//! Loads per-step token usage from dsh (DeepSeek Harness) session logs.

use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

use csusage_core::{
    LoadedEntry, PricingMap, Result, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_for_usage_at, cli_error, format_date_tz, format_rfc3339_millis, parse_tz,
};

use serde_json::Value;

use crate::{
    cli::{CostMode, SharedArgs},
    paths,
};

/// One settled assistant step: the final usage sample of a (turn, step) pair,
/// plus the model active at the time. Each step reports its whole usage and
/// later samples replace earlier ones, so keeping the last per step matches
/// dsh's own token-meter fold.
pub(super) struct StepUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    model: String,
    timestamp_ms: i64,
}

/// Loads dsh session usage from all discovered event logs.
pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(crate::progress::UsageLoadAgent("DSH"), shared.json, || {
        load_entries_inner(shared, pricing)
    })
}

fn load_entries_inner(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let timezone = parse_tz(shared.timezone.as_deref());
    let mut entries = Vec::new();
    for (log, session) in paths::session_logs()?
        .into_iter()
        .map(|log| (log.clone(), session_id(&log)))
    {
        match read_session(&log) {
            Ok(steps) => {
                for step in steps {
                    entries.push(step_to_loaded(
                        step,
                        &session,
                        timezone.as_ref(),
                        shared.mode,
                        pricing,
                    ));
                }
            }
            Err(error) => {
                // One unreadable or incompatible session must not hide the
                // usage recorded in the others.
                csusage_core::debug_log(shared, format!("skipping session {session}: {error}"));
            }
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

/// Extracts the final usage sample of every (turn, step) pair in one session.
fn read_session(log: &Path) -> Result<Vec<StepUsage>> {
    let bytes = fs::read(log).map_err(|error| cli_error(format!("{}: {error}", log.display())))?;
    let decoded =
        zstd::decode_all(&bytes[..]).map_err(|error| cli_error(format!("{log:?}: {error}")))?;
    let text = String::from_utf8_lossy(&decoded);

    let mut model = String::new();
    let mut steps: BTreeMap<(u64, u64), StepUsage> = BTreeMap::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let timestamp_ms = event
            .get("time")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        match event.get("type").and_then(Value::as_str) {
            Some("session") => {}
            Some("request/header") => {
                if let Some(configured) = event
                    .pointer("/data/header/config/model")
                    .and_then(Value::as_str)
                {
                    model = configured.to_string();
                }
            }
            Some("assistant/message") | Some("assistant/attempt") => {
                let Some(usage) = usage_of(&event) else {
                    continue;
                };
                let data = event.get("data").unwrap_or(&Value::Null);
                let step = (
                    data.get("turn").and_then(Value::as_u64).unwrap_or_default(),
                    data.get("step").and_then(Value::as_u64).unwrap_or_default(),
                );
                let tokens = |field: &str| {
                    usage
                        .get(field)
                        .and_then(Value::as_i64)
                        .filter(|tokens| *tokens >= 0)
                        .map(|tokens| tokens as u64)
                        .unwrap_or_default()
                };
                steps.insert(
                    step,
                    StepUsage {
                        input_tokens: tokens("inputTokens"),
                        output_tokens: tokens("outputTokens"),
                        cache_read_tokens: tokens("cacheReadTokens"),
                        cache_write_tokens: tokens("cacheWriteTokens"),
                        model: model.clone(),
                        timestamp_ms,
                    },
                );
            }
            _ => {}
        }
    }
    Ok(steps.into_values().collect())
}

/// The usage sample of an assistant settlement: embedded directly, or the
/// last usage chunk of the stream.
fn usage_of(event: &Value) -> Option<&Value> {
    let data = event.get("data")?;
    if data.get("usage").is_some_and(Value::is_object) {
        return data.get("usage");
    }
    let stream = data.get("stream")?.as_array()?;
    stream
        .iter()
        .rev()
        .find(|chunk| chunk.get("type").and_then(Value::as_str) == Some("usage"))
        .and_then(|chunk| chunk.get("usage"))
        .filter(|usage| usage.is_object())
}

fn session_id(log: &Path) -> String {
    log.parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn step_to_loaded(
    step: StepUsage,
    session: &str,
    timezone: Option<&jiff::tz::TimeZone>,
    mode: CostMode,
    pricing: &PricingMap,
) -> LoadedEntry {
    let timestamp_ms = TimestampMs::from_millis(step.timestamp_ms);
    let usage = TokenUsageRaw {
        input_tokens: step.input_tokens,
        output_tokens: step.output_tokens,
        cache_creation_input_tokens: step.cache_write_tokens,
        cache_read_input_tokens: step.cache_read_tokens,
        speed: None,
        cache_creation: None,
    };
    let cost = calculate_cost_for_usage_at(
        Some(&step.model),
        usage,
        None,
        Some(timestamp_ms),
        mode,
        Some(pricing),
    );
    let missing_pricing_model = csusage_core::missing_pricing_model_for_usage(
        Some(&step.model),
        usage,
        None,
        mode,
        Some(pricing),
    );
    LoadedEntry {
        date: format_date_tz(timestamp_ms, timezone),
        timestamp: timestamp_ms,
        project: Arc::from("dsh"),
        session_id: Arc::from(session),
        project_path: Arc::from("DSH"),
        cost,
        extra_total_tokens: 0,
        credits: None,
        message_count: None,
        model: Some(step.model.clone()),
        usage_limit_reset_time: None,
        missing_pricing_model,
        data: UsageEntry {
            session_id: Some(session.to_string()),
            timestamp: format_rfc3339_millis(timestamp_ms),
            version: None,
            message: UsageMessage {
                usage,
                model: Some(step.model.clone()),
                id: None,
            },
            request_id: None,
            is_api_error_message: None,
            is_sidechain: None,
            cost_usd: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_fixture_frame() {
        // Regression guard: the test fixture hand-builds a zstd raw-block
        // frame; make sure the loader's decoder accepts it.
        let temp = assert_fs::TempDir::new().unwrap();
        csusage_test_support::dsh::create_fixture(temp.path());
        let log = temp
            .path()
            .join("--home-user-workspace--")
            .join("session-11111111-1111-4111-8111-111111111111")
            .join("session.v3.jsonl.zstd");
        assert!(log.is_file(), "fixture log missing at {log:?}");
        let steps = read_session(&log).unwrap();
        assert_eq!(steps.len(), 3, "expected three settled steps");
        assert_eq!(steps[0].input_tokens, 100);
        assert_eq!(steps[0].model, "deepseek-flash");
        assert_eq!(steps[1].input_tokens, 200);
        assert_eq!(steps[2].model, "glm-5.3-flash");
    }
}
