//! Aggregates every agent's usage into a JSON payload for the web UI.

use std::collections::{BTreeMap, BTreeSet};

use csusage_core::{
    LoadedEntry, PricingMap, Result, TimestampMs, calculate_cost_for_usage_at, cli::SharedArgs,
    format_date_tz, parse_ts_timestamp, parse_tz,
};
use serde_json::{Value, json};

use crate::{adapter, cli::AgentReportKind, log_level};

/// One usage record reduced to the fields the dashboard aggregates on.
#[derive(Clone)]
struct DashboardEntry {
    agent: &'static str,
    session_id: String,
    timestamp_ms: i64,
    date: String,
    model: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cost_usd: f64,
}

/// Loads usage from every agent and aggregates it into the dashboard JSON.
pub fn load_dashboard(shared: &SharedArgs) -> Result<Value> {
    let entries = load_entries_all_agents(shared)?;
    Ok(build_dashboard(&entries))
}

fn load_entries_all_agents(shared: &SharedArgs) -> Result<Vec<DashboardEntry>> {
    // Suppress per-agent progress output; the web UI reports load state itself.
    let shared = SharedArgs {
        json: true,
        ..shared.clone()
    };
    let pricing = PricingMap::load_with_overrides(
        shared.offline,
        log_level() != Some(0),
        shared.pricing_overrides.iter(),
    );
    let timezone = parse_tz(shared.timezone.as_deref());
    let timezone = timezone.as_ref();
    let mut entries = Vec::new();
    for (agent, load) in agent_loaders() {
        for entry in load(&shared, &pricing)? {
            push_entry(&mut entries, agent, entry);
        }
    }
    // Codex does not go through LoadedEntry; load its events separately.
    let (events, _) = adapter::codex::load_codex_events_with_detection(&shared)?;
    for event in events {
        entries.push(codex_entry(&event, timezone, &pricing));
    }
    entries.sort_by_key(|entry| entry.timestamp_ms);
    Ok(entries)
}

type AgentLoader = fn(&SharedArgs, &PricingMap) -> Result<Vec<LoadedEntry>>;

fn agent_loaders() -> Vec<(&'static str, AgentLoader)> {
    vec![
        ("claude", |shared, _| {
            adapter::claude::load_entries(shared, None)
        }),
        ("opencode", |shared, _| {
            adapter::opencode::load_entries(shared, AgentReportKind::Daily)
        }),
        ("amp", |shared, pricing| {
            adapter::amp::load_entries(shared, pricing)
        }),
        ("droid", |shared, pricing| {
            adapter::droid::load_entries(shared, pricing)
        }),
        ("codebuff", |shared, pricing| {
            adapter::codebuff::load_entries(shared, pricing)
        }),
        ("hermes", |shared, pricing| {
            adapter::hermes::load_entries(shared, pricing)
        }),
        ("pi", |shared, pricing| {
            adapter::pi::load_entries(shared, None, Some(pricing))
        }),
        ("goose", |shared, pricing| {
            adapter::goose::load_entries(shared, pricing)
        }),
        ("kilo", |shared, pricing| {
            adapter::kilo::load_entries(shared, pricing)
        }),
        ("copilot", |shared, pricing| {
            adapter::copilot::load_entries(shared, pricing)
        }),
        ("gemini", |shared, pricing| {
            adapter::gemini::load_entries(shared, pricing)
        }),
        ("antigravity", |shared, pricing| {
            adapter::antigravity::load_entries(shared, pricing)
        }),
        ("kimi", |shared, pricing| {
            adapter::kimi::load_entries(shared, pricing)
        }),
        ("qwen", |shared, _| adapter::qwen::load_entries(shared)),
        ("openclaw", |shared, pricing| {
            adapter::openclaw::load_entries(shared, None, Some(pricing))
        }),
        ("grok", |shared, pricing| {
            adapter::grok::load_entries(shared, pricing)
        }),
        ("zcode", |shared, pricing| {
            adapter::zcode::load_entries(shared, pricing)
        }),
        ("claude-science", |shared, pricing| {
            adapter::claude_science::load_entries(shared, pricing)
        }),
        ("openhands", |shared, pricing| {
            adapter::openhands::load_entries(shared, pricing)
        }),
        ("dsh", |shared, pricing| {
            adapter::dsh::load_entries(shared, pricing)
        }),
    ]
}

fn push_entry(entries: &mut Vec<DashboardEntry>, agent: &'static str, entry: LoadedEntry) {
    let usage = &entry.data.message.usage;
    entries.push(DashboardEntry {
        agent,
        session_id: entry.session_id.to_string(),
        timestamp_ms: entry.timestamp.as_millis(),
        date: entry.date.clone(),
        model: entry.model.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        cache_write_tokens: usage.cache_creation_input_tokens,
        cost_usd: entry.cost,
    });
}

fn codex_entry(
    event: &adapter::codex::CodexTokenUsageEvent,
    timezone: Option<&jiff::tz::TimeZone>,
    pricing: &PricingMap,
) -> DashboardEntry {
    let timestamp_ms = parse_ts_timestamp(&event.timestamp)
        .map(|timestamp| timestamp.as_millis())
        .unwrap_or_default();
    DashboardEntry {
        agent: "codex",
        session_id: event.session_id.clone(),
        timestamp_ms,
        date: format_date_tz(TimestampMs::from_millis(timestamp_ms), timezone),
        model: event.model.clone(),
        // Codex reports `input_tokens` inclusive of cached input; the CLI
        // rows expose the non-cached portion, so match that here.
        input_tokens: event.input_tokens.saturating_sub(
            event
                .cached_input_tokens
                .saturating_add(event.cache_creation_tokens),
        ),
        output_tokens: event.output_tokens,
        cache_read_tokens: event.cached_input_tokens,
        cache_write_tokens: event.cache_creation_tokens,
        cost_usd: calculate_cost_for_usage_at(
            event.model.as_deref(),
            csusage_core::TokenUsageRaw {
                input_tokens: event.input_tokens.saturating_sub(
                    event
                        .cached_input_tokens
                        .saturating_add(event.cache_creation_tokens),
                ),
                output_tokens: event.output_tokens,
                cache_creation_input_tokens: event.cache_creation_tokens,
                cache_read_input_tokens: event.cached_input_tokens,
                speed: None,
                cache_creation: None,
            },
            None,
            Some(TimestampMs::from_millis(timestamp_ms)),
            csusage_core::cli::CostMode::Auto,
            Some(pricing),
        ),
    }
}

/// A monotonic day number for a date; dates one day apart differ by one.
fn day_key(date: jiff::civil::Date) -> i64 {
    date.to_zoned(jiff::tz::TimeZone::UTC)
        .expect("UTC dates are always valid")
        .timestamp()
        .as_second()
        / 86_400
}

#[derive(Default)]
struct Totals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    sessions: BTreeSet<String>,
}

impl Totals {
    fn add(&mut self, entry: &DashboardEntry) {
        self.input_tokens += entry.input_tokens;
        self.output_tokens += entry.output_tokens;
        self.cache_read_tokens += entry.cache_read_tokens;
        self.cache_write_tokens += entry.cache_write_tokens;
        self.total_tokens += entry.input_tokens
            + entry.output_tokens
            + entry.cache_read_tokens
            + entry.cache_write_tokens;
        self.cost_usd += entry.cost_usd;
        self.sessions.insert(entry.session_id.clone());
    }
}

struct Session {
    agent: &'static str,
    session_id: String,
    first_ms: i64,
    last_ms: i64,
    totals: Totals,
    models: BTreeSet<String>,
}

/// Builds the dashboard JSON payload from per-entry records.
fn build_dashboard(entries: &[DashboardEntry]) -> Value {
    let mut days: BTreeMap<String, Totals> = BTreeMap::new();
    let mut agents: BTreeMap<&'static str, Totals> = BTreeMap::new();
    let mut models: BTreeMap<String, Totals> = BTreeMap::new();
    let mut sessions: BTreeMap<(&'static str, String), Session> = BTreeMap::new();

    for entry in entries {
        days.entry(entry.date.clone()).or_default().add(entry);
        agents.entry(entry.agent).or_default().add(entry);
        if let Some(model) = &entry.model {
            models.entry(model.clone()).or_default().add(entry);
        }
        let session = sessions
            .entry((entry.agent, entry.session_id.clone()))
            .or_insert_with(|| Session {
                agent: entry.agent,
                session_id: entry.session_id.clone(),
                first_ms: entry.timestamp_ms,
                last_ms: entry.timestamp_ms,
                totals: Totals::default(),
                models: BTreeSet::new(),
            });
        session.first_ms = session.first_ms.min(entry.timestamp_ms);
        session.last_ms = session.last_ms.max(entry.timestamp_ms);
        session.totals.add(entry);
        if let Some(model) = &entry.model {
            session.models.insert(model.clone());
        }
    }

    let days_json: Vec<Value> = days
        .iter()
        .map(|(date, totals)| totals_json(date, totals))
        .collect();
    let agents_json: Vec<Value> = agents
        .iter()
        .map(|(agent, totals)| {
            json!({
                "agent": agent,
                "inputTokens": totals.input_tokens,
                "outputTokens": totals.output_tokens,
                "cacheReadTokens": totals.cache_read_tokens,
                "cacheWriteTokens": totals.cache_write_tokens,
                "totalTokens": totals.total_tokens,
                "costUsd": totals.cost_usd,
                "sessions": totals.sessions.len(),
            })
        })
        .collect();

    let mut sessions_json: Vec<Value> = sessions
        .values()
        .map(|session| {
            json!({
                "agent": session.agent,
                "sessionId": session.session_id,
                "startMs": session.first_ms,
                "endMs": session.last_ms,
                "durationMs": session.last_ms - session.first_ms,
                "inputTokens": session.totals.input_tokens,
                "outputTokens": session.totals.output_tokens,
                "cacheReadTokens": session.totals.cache_read_tokens,
                "cacheWriteTokens": session.totals.cache_write_tokens,
                "totalTokens": session.totals.total_tokens,
                "costUsd": session.totals.cost_usd,
                "models": session.models,
            })
        })
        .collect();
    sessions_json.sort_by_key(|session| session["endMs"].as_i64().unwrap_or_default());
    sessions_json.reverse();

    let total_sessions = sessions_json.len();
    let (grand_input, grand_output, grand_read, grand_write) =
        days.values().fold((0u64, 0u64, 0u64, 0u64), |acc, day| {
            (
                acc.0 + day.input_tokens,
                acc.1 + day.output_tokens,
                acc.2 + day.cache_read_tokens,
                acc.3 + day.cache_write_tokens,
            )
        });
    let (grand_total, grand_cost) = days.values().fold((0u64, 0.0f64), |acc, day| {
        (acc.0 + day.total_tokens, acc.1 + day.cost_usd)
    });
    let peak_day = days
        .iter()
        .max_by_key(|(_, totals)| totals.total_tokens)
        .map(|(date, totals)| json!({"date": date, "totalTokens": totals.total_tokens}));
    let longest_session = sessions_json
        .iter()
        .map(|session| session["durationMs"].as_i64().unwrap_or_default())
        .max()
        .unwrap_or(0);
    let today = day_key(
        jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date(),
    );
    let (current_streak, longest_streak) = day_streaks(days.keys(), today);
    let models_json: Vec<Value> = models
        .iter()
        .map(|(model, totals)| {
            json!({
                "model": model,
                "inputTokens": totals.input_tokens,
                "outputTokens": totals.output_tokens,
                "cacheReadTokens": totals.cache_read_tokens,
                "cacheWriteTokens": totals.cache_write_tokens,
                "totalTokens": totals.total_tokens,
                "costUsd": totals.cost_usd,
                "sessions": totals.sessions.len(),
            })
        })
        .collect();

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string());

    json!({
        "user": user,
        "generatedAt": jiff::Timestamp::now().to_string(),
        "totalTokens": grand_total,
        "totalInputTokens": grand_input,
        "totalOutputTokens": grand_output,
        "totalCacheReadTokens": grand_read,
        "totalCacheWriteTokens": grand_write,
        "totalCostUsd": grand_cost,
        "totalSessions": total_sessions,
        "peakDay": peak_day,
        "longestSessionMs": longest_session,
        "currentStreakDays": current_streak,
        "longestStreakDays": longest_streak,
        "agents": agents_json,
        "models": models_json,
        "days": days_json,
        "sessions": sessions_json,
    })
}

fn totals_json(date: &str, totals: &Totals) -> Value {
    json!({
        "date": date,
        "inputTokens": totals.input_tokens,
        "outputTokens": totals.output_tokens,
        "cacheReadTokens": totals.cache_read_tokens,
        "cacheWriteTokens": totals.cache_write_tokens,
        "totalTokens": totals.total_tokens,
        "costUsd": totals.cost_usd,
        "sessions": totals.sessions.len(),
    })
}

/// Returns (current streak, longest streak) in days. `today` is the ordinal
/// of the current day; the current streak may start today or yesterday.
fn day_streaks(dates: impl Iterator<Item = impl AsRef<str>>, today: i64) -> (u64, u64) {
    let sorted: Vec<i64> = dates
        .filter_map(|date| date.as_ref().parse::<jiff::civil::Date>().ok().map(day_key))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let longest = longest_run(&sorted);
    let current = match sorted.last() {
        Some(&last) if last == today || last == today - 1 => {
            let streak_end = last;
            let mut current = 0u64;
            while sorted.binary_search(&(streak_end - current as i64)).is_ok() {
                current += 1;
            }
            current
        }
        _ => 0,
    };
    (current, longest)
}

fn longest_run(ordinals: &[i64]) -> u64 {
    let mut longest = 0u64;
    let mut run = 0u64;
    let mut previous: Option<i64> = None;
    for &ordinal in ordinals {
        run = match previous {
            Some(previous) if ordinal == previous + 1 => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        previous = Some(ordinal);
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_streaks_counts_runs() {
        let dates = ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-05"];
        let today = day_key(jiff::civil::date(2026, 1, 5));
        let (current, longest) = day_streaks(dates.iter(), today);
        assert_eq!(longest, 3);
        assert_eq!(current, 1);
        let (current, longest) = day_streaks(dates.iter(), today + 1);
        assert_eq!((current, longest), (1, 3));
        let (current, _) = day_streaks(dates.iter(), today + 2);
        assert_eq!(current, 0);
    }

    #[test]
    fn day_streaks_empty() {
        let (current, longest) = day_streaks(Vec::<String>::new().iter(), 1);
        assert_eq!((current, longest), (0, 0));
    }
}
