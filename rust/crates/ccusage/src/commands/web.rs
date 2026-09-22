//! Local web UI server for the `csusage web` command.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command as ProcessCommand,
    sync::Mutex,
    time::SystemTime,
};

use csusage_cli::WebArgs;

use crate::Result;

const INDEX_HTML: &str = include_str!("../../assets/web/index.html");
const REMOTE_CACHE_SECONDS: u64 = 60;

/// Runs the web UI server until interrupted.
pub(crate) fn run_web(args: WebArgs) -> Result<()> {
    let host = args.host.clone();
    let port = args.port;
    let cache = RemoteCache::default();
    let listener = TcpListener::bind((host.as_str(), port)).map_err(|error| {
        csusage_core::cli_error(format!("failed to bind {host}:{port}: {error}"))
    })?;
    println!("csusage web UI listening on http://{host}:{port}");
    println!("Press Ctrl+C to stop.");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                handle_connection(stream, &args, &cache);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, args: &WebArgs, cache: &RemoteCache) {
    let mut buffer = [0u8; 8192];
    let Ok(read) = stream.read(&mut buffer) else {
        return;
    };
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let (status, content_type, body) = match path.as_str() {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", INDEX_HTML.to_string()),
        "/api/usage" => match build_usage_payload(args, cache) {
            Ok(payload) => (
                "200 OK",
                "application/json",
                serde_json::to_string(&payload).unwrap_or_default(),
            ),
            Err(error) => (
                "500 Internal Server Error",
                "application/json",
                serde_json::json!({ "error": error.to_string() }).to_string(),
            ),
        },
        _ => ("404 Not Found", "text/plain; charset=utf-8", String::new()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.read(&mut [0u8; 0]);
}

/// The full dashboard payload: local usage plus one dashboard per remote.
fn build_usage_payload(args: &WebArgs, cache: &RemoteCache) -> Result<serde_json::Value> {
    let mut local = csusage_adapter_all::load_dashboard(&args.shared)?;
    for remote in &args.remote {
        let dashboard = cache.fetch(remote);
        merge_dashboard(&mut local, &dashboard, remote);
    }
    Ok(local)
}

/// Folds `add` into `base`, aggregating the totals so the web UI renders one
/// merged dashboard instead of one view per host. Sessions keep a `host` tag
/// so the recent-sessions table can attribute them.
fn merge_dashboard(base: &mut serde_json::Value, add: &serde_json::Value, host: &str) {
    for field in [
        "totalTokens",
        "totalInputTokens",
        "totalOutputTokens",
        "totalCacheReadTokens",
        "totalCacheWriteTokens",
        "totalCostUsd",
        "totalSessions",
    ] {
        if let (Some(target), Some(value)) = (
            base.get(field).and_then(serde_json::Value::as_i64),
            add.get(field).and_then(serde_json::Value::as_i64),
        ) {
            base[field] = serde_json::json!(target + value);
            continue;
        }
        let (Some(target), Some(value)) = (
            base.get(field).and_then(serde_json::Value::as_f64),
            add.get(field).and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        base[field] = serde_json::json!(target + value);
    }
    if let (Some(target), Some(value)) = (
        base.get("longestSessionMs")
            .and_then(serde_json::Value::as_i64),
        add.get("longestSessionMs")
            .and_then(serde_json::Value::as_i64),
    ) {
        base["longestSessionMs"] = serde_json::json!(target.max(value));
    }

    merge_groups(base, add, "agents", "agent");
    merge_groups(base, add, "models", "model");
    merge_groups(base, add, "days", "date");
    if let (Some(sessions), Some(add_sessions)) = (
        base.get_mut("sessions")
            .and_then(serde_json::Value::as_array_mut),
        add.get("sessions").and_then(serde_json::Value::as_array),
    ) {
        for mut session in add_sessions.iter().cloned() {
            if let Some(session) = session.as_object_mut() {
                session.insert("host".to_string(), serde_json::json!(host));
            }
            sessions.push(session);
        }
    }
    recompute_derived(base);
}

/// Merges a keyed array (agents / models / days) by summing every numeric field.
fn merge_groups(base: &mut serde_json::Value, add: &serde_json::Value, group: &str, key: &str) {
    let Some(add_group) = add.get(group).and_then(serde_json::Value::as_array) else {
        return;
    };
    let Some(base_group) = base
        .get_mut(group)
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for entry in add_group {
        let Some(entry_key) = entry.get(key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(target) = base_group.iter_mut().find(|candidate| {
            candidate.get(key).and_then(serde_json::Value::as_str) == Some(entry_key)
        }) else {
            base_group.push(entry.clone());
            continue;
        };
        for (field, value) in entry.as_object().into_iter().flatten() {
            if let Some(number) = value.as_i64() {
                let Some(existing) = target
                    .get(field.as_str())
                    .and_then(serde_json::Value::as_i64)
                else {
                    continue;
                };
                target[field.as_str()] = serde_json::json!(existing + number);
            } else if let (Some(existing), Some(number)) = (
                target
                    .get(field.as_str())
                    .and_then(serde_json::Value::as_f64),
                value.as_f64(),
            ) {
                target[field.as_str()] = serde_json::json!(existing + number);
            }
        }
    }
}

fn day_key(date: jiff::civil::Date) -> i64 {
    date.to_zoned(jiff::tz::TimeZone::UTC)
        .expect("UTC dates are always valid")
        .timestamp()
        .as_second()
        / 86_400
}

/// Recomputes peakDay and the streak counters over the merged day rows.
fn recompute_derived(base: &mut serde_json::Value) {
    let days = base
        .get("days")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dates: Vec<i64> = days
        .iter()
        .filter(|day| {
            day.get("totalTokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                > 0
        })
        .filter_map(|day| day.get("date")?.as_str())
        .filter_map(|date| date.parse::<jiff::civil::Date>().ok())
        .map(day_key)
        .collect::<std::collections::BTreeSet<i64>>()
        .into_iter()
        .collect();
    let (mut longest, mut run, mut previous) = (0, 0, None);
    for day in dates.iter() {
        run = if previous.is_some_and(|previous| *day == previous + 1) {
            run + 1
        } else {
            1
        };
        previous = Some(*day);
        longest = longest.max(run);
    }
    let today = day_key(
        jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date(),
    );
    let mut current = 0;
    for day in dates.iter().rev() {
        if *day >= today - 1 {
            current += 1;
        } else {
            break;
        }
    }
    base["currentStreakDays"] = serde_json::json!(current);
    base["longestStreakDays"] = serde_json::json!(longest);
    base["peakDay"] = days
        .iter()
        .filter(|day| {
            day.get("totalTokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                > 0
        })
        .max_by_key(|day| {
            day.get("totalTokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
        })
        .cloned()
        .unwrap_or_default();
}

/// Caches the SSH-fetched remote dashboards so page refreshes do not open a
/// new SSH connection each time.
#[derive(Default)]
struct RemoteCache {
    entries: Mutex<std::collections::HashMap<String, (SystemTime, serde_json::Value)>>,
}

impl RemoteCache {
    fn fetch(&self, remote: &str) -> serde_json::Value {
        let mut entries = self.entries.lock().unwrap();
        if let Some((fetched_at, dashboard)) = entries.get(remote) {
            if fetched_at.elapsed().unwrap_or_default().as_secs() < REMOTE_CACHE_SECONDS {
                return dashboard.clone();
            }
        }
        let dashboard = fetch_remote_dashboard(remote);
        entries.insert(remote.to_string(), (SystemTime::now(), dashboard.clone()));
        dashboard
    }
}

/// Pulls one remote host's dashboard over SSH.
///
/// The remote machine must have `csusage` installed; its `dashboard` command
/// emits the same JSON the local web UI serves.
fn fetch_remote_dashboard(remote: &str) -> serde_json::Value {
    let output = ProcessCommand::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("--")
        .arg(remote)
        .arg("sh")
        .arg("-lc")
        .arg("csusage dashboard --json")
        .output();
    let Ok(output) = output else {
        return remote_error("failed to spawn ssh".to_string());
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return remote_error(format!(
            "ssh {remote}: {}",
            stderr.chars().take(300).collect::<String>()
        ));
    }
    match serde_json::from_slice(&output.stdout) {
        Ok(dashboard) => dashboard,
        Err(error) => remote_error(format!("invalid csusage dashboard output: {error}")),
    }
}

fn remote_error(message: String) -> serde_json::Value {
    serde_json::json!({ "error": message })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::merge_dashboard;

    fn dashboard(agent_tokens: i64, dates: &[&str]) -> serde_json::Value {
        json!({
            "user": "tester",
            "totalTokens": agent_tokens,
            "totalInputTokens": agent_tokens,
            "totalOutputTokens": 10,
            "totalCacheReadTokens": 0,
            "totalCacheWriteTokens": 0,
            "totalCostUsd": 1.5,
            "totalSessions": 1,
            "longestSessionMs": 3600_000,
            "currentStreakDays": 1,
            "longestStreakDays": 1,
            "agents": [
                {"agent": "claude", "inputTokens": agent_tokens, "outputTokens": 10,
                 "cacheReadTokens": 0, "cacheWriteTokens": 0, "totalTokens": agent_tokens + 10,
                 "costUsd": 1.5, "sessions": 1},
                {"agent": "dsh", "inputTokens": 5, "outputTokens": 0,
                 "cacheReadTokens": 0, "cacheWriteTokens": 0, "totalTokens": 5,
                 "costUsd": 0.0, "sessions": 1}
            ],
            "models": [
                {"model": "glm-5.3", "inputTokens": agent_tokens, "outputTokens": 10,
                 "totalTokens": agent_tokens + 10, "costUsd": 1.5, "sessions": 1}
            ],
            "days": dates
                .iter()
                .map(|date| json!({
                    "date": date,
                    "inputTokens": agent_tokens,
                    "outputTokens": 10,
                    "cacheReadTokens": 0,
                    "cacheWriteTokens": 0,
                    "totalTokens": agent_tokens + 10,
                    "costUsd": 1.5,
                    "sessions": 1
                }))
                .collect::<Vec<_>>(),
            "sessions": [
                {"agent": "claude", "sessionId": "s1", "totalTokens": agent_tokens + 10, "costUsd": 1.5}
            ],
        })
    }

    #[test]
    fn merge_dashboard_sums_totals_and_groups() {
        let mut base = dashboard(100, &["2026-01-01"]);
        let remote = dashboard(50, &["2026-01-01", "2026-01-02"]);
        merge_dashboard(&mut base, &remote, "macmini");

        assert_eq!(base["totalTokens"], json!(150));
        assert_eq!(base["totalSessions"], json!(2));
        assert_eq!(base["totalCostUsd"], json!(3.0));
        assert_eq!(base["longestSessionMs"], json!(3600_000));

        let agents = base["agents"].as_array().unwrap();
        let claude = agents
            .iter()
            .find(|entry| entry["agent"] == "claude")
            .unwrap();
        assert_eq!(claude["inputTokens"], json!(150));
        let dsh = agents.iter().find(|entry| entry["agent"] == "dsh").unwrap();
        assert_eq!(dsh["inputTokens"], json!(10));
        assert_eq!(dsh["sessions"], json!(2));

        let days = base["days"].as_array().unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0]["date"], json!("2026-01-01"));
        assert_eq!(days[0]["inputTokens"], json!(150));
        assert_eq!(days[1]["date"], json!("2026-01-02"));
        assert_eq!(days[1]["inputTokens"], json!(50));

        let models = base["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["totalTokens"], json!(170));

        let sessions = base["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[1]["host"], json!("macmini"));

        assert_eq!(base["currentStreakDays"], json!(0));
        assert_eq!(base["longestStreakDays"], json!(2));
        assert_eq!(base["peakDay"]["date"], json!("2026-01-01"));
    }
}
