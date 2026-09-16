//! Loads per-conversation token usage from the OpenHands event logs.

use std::{fs, path::Path, path::PathBuf, sync::Arc};

use csusage_core::{
    LoadedEntry, PricingMap, Result, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_for_usage_at, cli_error, format_date_tz, format_rfc3339_millis, parse_tz,
};
use serde_json::Value;

use crate::{
    cli::{CostMode, SharedArgs},
    paths,
};

/// One LLM usage snapshot read from a conversation's event log.
pub(super) struct UsageSnapshot {
    usage_id: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    recorded_cost_usd: Option<f64>,
    timestamp: String,
}

/// Loads OpenHands conversation usage from all discovered event logs.
pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("OpenHands"),
        shared.json,
        || load_entries_inner(shared, pricing),
    )
}

fn load_entries_inner(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let timezone = parse_tz(shared.timezone.as_deref());
    let mut entries = Vec::new();
    for events_dir in paths::conversation_dirs()? {
        match read_conversation(&events_dir) {
            Ok(snapshots) => {
                for snapshot in snapshots {
                    entries.push(snapshot_to_loaded(
                        snapshot,
                        events_dir.clone(),
                        timezone.as_ref(),
                        shared.mode,
                        pricing,
                    ));
                }
            }
            Err(error) => {
                // One unreadable or incompatible conversation must not hide
                // the usage recorded in the others.
                csusage_core::debug_log(
                    shared,
                    format!("skipping conversation {events_dir:?}: {error}"),
                );
            }
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

/// Extracts the final accumulated usage of every LLM in one conversation.
///
/// Stats events carry *accumulated* totals, so only the last snapshot per
/// LLM (`usage_to_metrics` key) is used; summing snapshots would
/// double-count.
fn read_conversation(events_dir: &Path) -> Result<Vec<UsageSnapshot>> {
    let mut event_files: Vec<std::path::PathBuf> = match fs::read_dir(events_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("event-"))
            })
            .collect(),
        Err(error) => return Err(cli_error(format!("{events_dir:?}: {error}"))),
    };
    event_files.sort();

    let mut last_timestamp = String::new();
    let mut latest: std::collections::BTreeMap<String, UsageSnapshot> =
        std::collections::BTreeMap::new();
    for path in event_files {
        let bytes = std::fs::read(&path)
            .map_err(|error| cli_error(format!("{}: {error}", path.display())))?;
        let event: Value = match serde_json::from_slice(&bytes) {
            Ok(event) => event,
            Err(_) => continue,
        };
        if event.get("kind").and_then(Value::as_str) != Some("ConversationStateUpdateEvent") {
            continue;
        }
        if event.get("key").and_then(Value::as_str) != Some("stats") {
            continue;
        }
        if let Some(timestamp) = event.get("timestamp").and_then(Value::as_str) {
            last_timestamp = timestamp.to_string();
        }
        let Some(usage_to_metrics) = event
            .pointer("/value/usage_to_metrics")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (usage_id, metrics) in usage_to_metrics {
            let Some(usage) = metrics.get("accumulated_token_usage") else {
                continue;
            };
            let tokens = |field: &str| {
                usage
                    .get(field)
                    .and_then(Value::as_i64)
                    .filter(|tokens| *tokens >= 0)
                    .map(|tokens| tokens as u64)
                    .unwrap_or_default()
            };
            latest.insert(
                usage_id.clone(),
                UsageSnapshot {
                    usage_id: usage_id.clone(),
                    model: metrics
                        .get("model_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_tokens: tokens("prompt_tokens"),
                    output_tokens: tokens("completion_tokens"),
                    cache_read_tokens: tokens("cache_read_tokens"),
                    cache_write_tokens: tokens("cache_write_tokens"),
                    recorded_cost_usd: metrics
                        .get("accumulated_cost")
                        .and_then(Value::as_f64)
                        .map(|cost| cost.max(0.0)),
                    timestamp: last_timestamp.clone(),
                },
            );
        }
    }
    Ok(latest.into_values().collect())
}

fn snapshot_to_loaded(
    snapshot: UsageSnapshot,
    events_dir: PathBuf,
    timezone: Option<&jiff::tz::TimeZone>,
    mode: CostMode,
    pricing: &PricingMap,
) -> LoadedEntry {
    let timestamp_ms = parse_naive_timestamp(&snapshot.timestamp);
    let usage = TokenUsageRaw {
        input_tokens: snapshot.input_tokens,
        output_tokens: snapshot.output_tokens,
        cache_creation_input_tokens: snapshot.cache_write_tokens,
        cache_read_input_tokens: snapshot.cache_read_tokens,
        speed: None,
        cache_creation: None,
    };
    let model = normalize_model(&snapshot.model).to_string();
    let missing_pricing_model = csusage_core::missing_pricing_model_for_usage(
        Some(&model),
        usage,
        snapshot.recorded_cost_usd,
        mode,
        Some(pricing),
    );
    let cost = calculate_cost_for_usage_at(
        Some(&model),
        usage,
        snapshot.recorded_cost_usd,
        Some(timestamp_ms),
        mode,
        Some(pricing),
    );
    let session_id = session_id(&events_dir);
    let data = UsageEntry {
        session_id: Some(session_id.to_string()),
        timestamp: format_rfc3339_millis(timestamp_ms),
        version: None,
        message: UsageMessage {
            usage,
            model: Some(model.clone()),
            id: Some(format!("{session_id}:{}", snapshot.usage_id)),
        },
        request_id: None,
        is_api_error_message: None,
        is_sidechain: None,
        cost_usd: None,
    };
    LoadedEntry {
        date: format_date_tz(timestamp_ms, timezone),
        timestamp: timestamp_ms,
        project: Arc::from("openhands"),
        session_id: Arc::from(session_id),
        project_path: Arc::from("OpenHands"),
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

/// OpenHands writes naive local ISO-8601 timestamps; interpret them in the
/// configured timezone (UTC by default).
fn parse_naive_timestamp(timestamp: &str) -> TimestampMs {
    timestamp
        .parse::<jiff::civil::DateTime>()
        .ok()
        .and_then(|datetime| datetime.to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map(|zoned| TimestampMs::from_millis(zoned.timestamp().as_millisecond()))
        .unwrap_or_else(|| TimestampMs::from_millis(0))
}

/// The conversation directory name doubles as the session id; the LLM usage
/// id keeps entries of multiple LLMs (agent, condenser, …) apart.
fn session_id(events_dir: &Path) -> String {
    events_dir
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// LiteLLM prefixes provider names to model ids (for example
/// `openai/gpt-5`); strip the provider so pricing lookups see the bare id.
pub(crate) fn normalize_model(model: &str) -> &str {
    match model.split_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_provider_prefixes() {
        assert_eq!(normalize_model("openai/gpt-5"), "gpt-5");
        assert_eq!(normalize_model("gpt-5"), "gpt-5");
        assert_eq!(normalize_model("provider/"), "provider/");
    }
}
