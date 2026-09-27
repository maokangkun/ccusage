//! Snapshot state for the Claude Science adapter.
//!
//! Claude Science stores usage as a per-session *cumulative* total. Daily
//! reporting therefore needs the previous observation of each session to
//! diff against. The state file accumulates one usage row per observed
//! (session, day) pair, so every run re-emits the complete per-day history
//! instead of a one-shot delta that would vanish on the next read.

use std::collections::BTreeMap;
use std::path::PathBuf;

use csusage_core::cli_error;

use crate::home::home_dir;

/// Per-day totals for one frame, as last emitted.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct DayTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_cost_usd: Option<f64>,
}

#[derive(Default)]
pub(super) struct SnapshotState {
    /// frame id -> (day -> per-day usage rows)
    pub frames: BTreeMap<String, BTreeMap<String, DayTotals>>,
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
    let Ok(parsed) = serde_json::from_str::<SnapshotStateFile>(&raw) else {
        return SnapshotState::default();
    };
    SnapshotState {
        frames: parsed.frames,
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SnapshotStateFile {
    #[serde(default)]
    frames: BTreeMap<String, BTreeMap<String, DayTotals>>,
}

pub(super) fn save_state(
    path: &std::path::Path,
    state: &SnapshotState,
) -> csusage_core::Result<()> {
    let file = SnapshotStateFile {
        frames: state.frames.clone(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| cli_error(format!("cannot create {}: {error}", parent.display())))?;
    }
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(&file)
        .map_err(|error| cli_error(format!("serialize state: {error}")))?;
    std::fs::write(&tmp, body)
        .map_err(|error| cli_error(format!("write state {}: {error}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|error| cli_error(format!("rename state {}: {error}", path.display())))
}
