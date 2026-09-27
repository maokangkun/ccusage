//! Snapshot state for the Claude Science adapter.
//!
//! Claude Science stores usage as a per-session *cumulative* total. Daily
//! reporting therefore needs the previous observation of each session to
//! diff against; this module persists those observations between runs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use csusage_core::cli_error;

use crate::home::home_dir;

#[derive(Default)]
pub(super) struct SessionTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub recorded_cost_usd: Option<f64>,
}

#[derive(Default)]
pub(super) struct SnapshotState {
    pub sessions: BTreeMap<String, SessionTotals>,
}

pub(super) fn state_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CSUSAGE_CLAUDE_SCIENCE_STATE") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cache")));
    match base {
        Some(dir) => dir.join("csusage").join("claude-science-state.json"),
        None => PathBuf::from(".csusage-claude-science-state.json"),
    }
}

pub(super) fn load_state(path: &std::path::Path) -> SnapshotState {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return SnapshotState::default();
    };
    let Ok(parsed) = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&raw) else {
        return SnapshotState::default();
    };
    let mut state = SnapshotState::default();
    for (session_id, entry) in parsed {
        let get = |field: &str| {
            entry
                .get(field)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default()
        };
        let cost = entry.get("cost").and_then(serde_json::Value::as_f64);
        state.sessions.insert(
            session_id,
            SessionTotals {
                input_tokens: get("in"),
                output_tokens: get("out"),
                cache_read_tokens: get("read"),
                cache_write_tokens: get("write"),
                recorded_cost_usd: cost,
            },
        );
    }
    state
}

pub(super) fn save_state(
    path: &std::path::Path,
    state: &SnapshotState,
) -> csusage_core::Result<()> {
    let mut parsed = serde_json::Map::new();
    for (session_id, totals) in &state.sessions {
        let mut entry = serde_json::Map::new();
        entry.insert("in".into(), totals.input_tokens.into());
        entry.insert("out".into(), totals.output_tokens.into());
        entry.insert("read".into(), totals.cache_read_tokens.into());
        entry.insert("write".into(), totals.cache_write_tokens.into());
        if let Some(cost) = totals.recorded_cost_usd {
            entry.insert("cost".into(), serde_json::json!(cost));
        }
        parsed.insert(session_id.clone(), serde_json::Value::Object(entry));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| cli_error(format!("cannot create {}: {error}", parent.display())))?;
    }
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(&serde_json::Value::Object(parsed))
        .map_err(|error| cli_error(format!("serialize state: {error}")))?;
    std::fs::write(&tmp, body)
        .map_err(|error| cli_error(format!("write state {}: {error}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|error| cli_error(format!("rename state {}: {error}", path.display())))
}
