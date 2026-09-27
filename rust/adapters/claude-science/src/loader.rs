//! Loads per-frame token usage from the Claude Science metadata database.

use std::sync::Arc;

use csusage_core::{
    LoadedEntry, PricingMap, Result, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_for_usage_at, cli_error, debug_log, format_date_tz, format_rfc3339_millis,
    parse_tz,
};
use sqlite::{Connection, State};

use crate::{
    cli::{CostMode, SharedArgs},
    paths,
};

/// Per-frame usage record straight from the database.
pub(super) struct FrameUsage {
    id: String,
    session_id: String,
    model: String,
    project_name: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    recorded_cost_usd: Option<f64>,
    timestamp_ms: TimestampMs,
    created_ms: i64,
}

/// Loads Claude Science frame usage from all discovered databases.
pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("Claude Science"),
        shared.json,
        || load_entries_inner(shared, pricing),
    )
}

fn load_entries_inner(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let timezone = parse_tz(shared.timezone.as_deref());
    let mut entries = Vec::new();
    let state_path = crate::state::state_path();
    let mut state = crate::state::load_state(&state_path);
    for path in paths::database_paths()? {
        match read_frames(&path) {
            Ok(frames) => {
                for frame in frames {
                    // Observations are keyed per frame (the primary usage
                    // record), not per session: a root frame carries the
                    // session cumulative total while sibling frames carry
                    // their own records, so a session key would make them
                    // overwrite each other.
                    let frame_key = frame.id.clone();
                    for day_row in diff_against_snapshot(&mut state, &frame_key, frame) {
                        entries.push(frame_to_loaded(
                            day_row,
                            timezone.as_ref(),
                            shared.mode,
                            pricing,
                        ));
                    }
                }
            }
            Err(error) => {
                // One unreadable or incompatible database must not hide the
                // usage recorded in the others.
                debug_log(shared, format!("skipping database {path:?}: {error}"));
            }
        }
    }
    if let Err(error) = crate::state::save_state(&state_path, &state) {
        debug_log(shared, format!("failed to persist snapshot state: {error}"));
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

/// Claude Science records usage as a per-frame cumulative total. The snapshot
/// state accumulates one usage row per observed (frame, day) pair, so every
/// run re-emits the complete per-day history:
///
/// - the first observation of a frame spanning multiple days is spread evenly
///   across its `created_at`..=`updated_at` range so history does not pile
///   onto the last day;
/// - later observations replace the last day's row with the grown total and
///   add rows for any new days;
/// - the returned rows carry one usage row per day of recorded activity, and
///   re-running the report yields the same numbers.
fn diff_against_snapshot(
    state: &mut crate::state::SnapshotState,
    frame_key: &str,
    frame: FrameUsage,
) -> Vec<FrameUsage> {
    let day_ms = 86_400_000;
    let last_seen = frame.timestamp_ms.as_millis();
    let created = frame.created_ms.max(0);
    let span_days = ((last_seen - created) / day_ms).clamp(0, 365) as u64;
    let parts = span_days + 1;
    let first_share = |total: u64| total / parts + u64::from(!total.is_multiple_of(parts));

    // Evenly split the cumulative total across the observed day range.
    let mut day_rows: Vec<(i64, crate::state::DayTotals)> = Vec::new();
    for index in 0..parts {
        let day = created + index as i64 * day_ms;
        day_rows.push((day, crate::state::DayTotals::default()));
    }

    if parts > 1 {
        // The first day keeps an even share and the last day absorbs the
        // remainder, so the per-day rows sum to the observed totals exactly.
        let head_cost = frame.recorded_cost_usd.map(|cost| cost / parts as f64);
        let Some(((day0, first), rest)) = day_rows.split_first_mut() else {
            unreachable!("day_rows has {parts} entries");
        };
        let Some((_, last)) = rest.last_mut() else {
            unreachable!("day_rows rest is empty");
        };
        let _ = day0;
        first.input_tokens = first_share(frame.input_tokens);
        first.output_tokens = first_share(frame.output_tokens);
        first.cache_read_tokens = first_share(frame.cache_read_tokens);
        first.cache_write_tokens = first_share(frame.cache_write_tokens);
        first.recorded_cost_usd = head_cost;
        last.input_tokens = frame.input_tokens - first.input_tokens;
        last.output_tokens = frame.output_tokens - first.output_tokens;
        last.cache_read_tokens = frame.cache_read_tokens - first.cache_read_tokens;
        last.cache_write_tokens = frame.cache_write_tokens - first.cache_write_tokens;
        last.recorded_cost_usd = frame
            .recorded_cost_usd
            .zip(head_cost)
            .map(|(current, head)| current - head);
    } else if let Some((_, only)) = day_rows.first_mut() {
        only.input_tokens = frame.input_tokens;
        only.output_tokens = frame.output_tokens;
        only.cache_read_tokens = frame.cache_read_tokens;
        only.cache_write_tokens = frame.cache_write_tokens;
        only.recorded_cost_usd = frame.recorded_cost_usd;
    }

    state.frames.insert(frame_key.to_string(), {
        day_rows
            .iter()
            .map(|(day, row)| (format!("{day}"), row.clone()))
            .collect()
    });
    day_rows
        .into_iter()
        .map(|(timestamp_ms, row)| FrameUsage {
            id: frame.id.clone(),
            session_id: frame.session_id.clone(),
            model: frame.model.clone(),
            project_name: frame.project_name.clone(),
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_read_tokens: row.cache_read_tokens,
            cache_write_tokens: row.cache_write_tokens,
            recorded_cost_usd: row.recorded_cost_usd,
            timestamp_ms: TimestampMs::from_millis(timestamp_ms),
            created_ms: frame.created_ms,
        })
        .collect()
}

fn read_frames(path: &std::path::Path) -> Result<Vec<FrameUsage>> {
    let connection = Connection::open_with_flags(
        path,
        sqlite::OpenFlags::new().with_read_only().with_no_mutex(),
    )
    .map_err(|error| cli_error(format!("failed to open {}: {error}", path.display())))?;
    let query = if projects_table_exists(&connection) {
        "SELECT frames.id, COALESCE(frames.root_frame_id, frames.id), frames.model, \
         frames.input_tokens, frames.output_tokens, frames.cache_read_tokens, \
         frames.cache_write_tokens, frames.total_cost, frames.updated_at, projects.name, \
         frames.created_at \
         FROM frames LEFT JOIN projects ON projects.id = frames.project_id \
         WHERE frames.input_tokens IS NOT NULL AND frames.output_tokens IS NOT NULL"
    } else {
        "SELECT frames.id, COALESCE(frames.root_frame_id, frames.id), frames.model, \
         frames.input_tokens, frames.output_tokens, frames.cache_read_tokens, \
         frames.cache_write_tokens, frames.total_cost, frames.updated_at, NULL, \
         frames.created_at \
         FROM frames \
         WHERE frames.input_tokens IS NOT NULL AND frames.output_tokens IS NOT NULL"
    };
    let mut statement = connection
        .prepare(query)
        .map_err(|error| cli_error(format!("{}: {error}", path.display())))?;
    let mut frames = Vec::new();
    while let State::Row = statement
        .next()
        .map_err(|error| cli_error(format!("{}: {error}", path.display())))?
    {
        let timestamp_ms = statement
            .read::<Option<i64>, _>(8)
            .ok()
            .flatten()
            .unwrap_or_default();
        if timestamp_ms <= 0 {
            continue;
        }
        frames.push(FrameUsage {
            id: statement
                .read::<String, _>(0)
                .map_err(|error| cli_error(format!("{}: {error}", path.display())))?,
            session_id: statement
                .read::<String, _>(1)
                .map_err(|error| cli_error(format!("{}: {error}", path.display())))?,
            model: statement
                .read::<Option<String>, _>(2)
                .ok()
                .flatten()
                .unwrap_or_default(),
            input_tokens: non_negative(statement.read::<Option<i64>, _>(3).ok().flatten()),
            output_tokens: non_negative(statement.read::<Option<i64>, _>(4).ok().flatten()),
            cache_read_tokens: non_negative(statement.read::<Option<i64>, _>(5).ok().flatten()),
            cache_write_tokens: non_negative(statement.read::<Option<i64>, _>(6).ok().flatten()),
            recorded_cost_usd: statement.read::<Option<f64>, _>(7).ok().flatten(),
            timestamp_ms: TimestampMs::from_millis(timestamp_ms),
            project_name: statement.read::<Option<String>, _>(9).ok().flatten(),
            created_ms: statement
                .read::<Option<i64>, _>(10)
                .ok()
                .flatten()
                .unwrap_or_default(),
        });
    }
    Ok(frames)
}

fn non_negative(value: Option<i64>) -> u64 {
    value.unwrap_or_default().max(0) as u64
}

fn projects_table_exists(connection: &Connection) -> bool {
    let Ok(mut statement) = connection
        .prepare("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'")
    else {
        return false;
    };
    if statement.next().is_err() {
        return false;
    }
    matches!(statement.read::<i64, _>(0), Ok(count) if count == 1)
}

/// Claude Science may route models through a proxy that prefixes model names
/// with a routing namespace (for example `cs-switch-direct:claude-sonnet-4-5`).
/// Strip the namespace so downstream pricing lookups see the bare model name.
pub(crate) fn normalize_model(model: &str) -> &str {
    model.split_once(':').map_or(model, |(_, rest)| rest)
}

fn frame_to_loaded(
    frame: FrameUsage,
    timezone: Option<&jiff::tz::TimeZone>,
    mode: CostMode,
    pricing: &PricingMap,
) -> LoadedEntry {
    let usage = TokenUsageRaw {
        input_tokens: frame.input_tokens,
        output_tokens: frame.output_tokens,
        cache_creation_input_tokens: frame.cache_write_tokens,
        cache_read_input_tokens: frame.cache_read_tokens,
        speed: None,
        cache_creation: None,
    };
    let model = normalize_model(&frame.model).to_string();
    let missing_pricing_model = csusage_core::missing_pricing_model_for_usage(
        Some(&model),
        usage,
        frame.recorded_cost_usd,
        mode,
        Some(pricing),
    );
    let cost = calculate_cost_for_usage_at(
        Some(&model),
        usage,
        frame.recorded_cost_usd,
        Some(frame.timestamp_ms),
        mode,
        Some(pricing),
    );
    let data = UsageEntry {
        session_id: Some(frame.session_id.clone()),
        timestamp: format_rfc3339_millis(frame.timestamp_ms),
        version: None,
        message: UsageMessage {
            usage,
            model: Some(model.clone()),
            id: Some(frame.id.clone()),
        },
        cost_usd: None,
        request_id: None,
        is_api_error_message: None,
        is_sidechain: None,
    };
    LoadedEntry {
        date: format_date_tz(frame.timestamp_ms, timezone),
        timestamp: frame.timestamp_ms,
        project: Arc::from(frame.project_name.as_deref().unwrap_or("claude-science")),
        session_id: Arc::from(frame.session_id),
        project_path: Arc::from("Claude Science"),
        cost,
        extra_total_tokens: 0,
        credits: None,
        message_count: None,
        model: Some(model),
        usage_limit_reset_time: None,
        missing_pricing_model,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_proxy_prefixes() {
        assert_eq!(
            normalize_model("cs-switch-direct:claude-sonnet-4-5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(normalize_model("claude-sonnet-4-5"), "claude-sonnet-4-5");
    }
}
