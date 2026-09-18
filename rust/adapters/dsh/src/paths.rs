//! Session discovery for the dsh (DeepSeek Harness) adapter.
//!
//! dsh persists each session under `~/.dsh/sessions/<escaped-cwd>/session-<id>/`
//! as a zstd-compressed JSONL event log. Users can point the adapter at an
//! explicit directory with `DSH_HOME`: it accepts the dsh home itself, a
//! sessions root, an escaped-cwd directory, or a single session directory.

use std::{env, fs, path::PathBuf};

use crate::Result;

pub(crate) const DSH_HOME_ENV: &str = "DSH_HOME";

fn candidate_roots() -> Vec<PathBuf> {
    if let Some(value) = env::var_os(DSH_HOME_ENV) {
        // Keep non-Unicode overrides byte-for-byte: only a UTF-8 value can
        // carry the comma-separated list form.
        return match value.to_str() {
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .collect(),
            None => vec![PathBuf::from(value)],
        };
    }

    crate::home::home_dir()
        .into_iter()
        .flat_map(|home| {
            [".dsh", ".config/dsh"]
                .into_iter()
                .map(move |root| home.join(root))
        })
        .collect()
}

/// Returns the event-log file of every session found.
///
/// An explicit `DSH_HOME` override is exclusive: only the listed roots are
/// considered, and discovery does not fall back to well-known locations.
pub(crate) fn session_logs() -> Result<Vec<PathBuf>> {
    let mut logs = Vec::new();
    for root in candidate_roots() {
        if root.is_file() {
            // The override may point straight at a session log.
            push_unique(&mut logs, root);
            continue;
        }
        collect_session_logs(&root, &mut logs);
    }
    logs.sort();
    Ok(logs)
}

fn collect_session_logs(root: &std::path::Path, logs: &mut Vec<PathBuf>) {
    // Root may be the dsh home, a sessions root, an escaped-cwd directory,
    // or a session directory itself.
    if root.join("session.v3.jsonl.zstd").is_file() {
        push_unique(logs, root.join("session.v3.jsonl.zstd"));
        return;
    }
    let sessions = if root.join("sessions").is_dir() {
        vec![root.join("sessions")]
    } else {
        vec![root.to_path_buf()]
    };
    for sessions_root in sessions {
        // Layout: sessions/<escaped-cwd>/session-<id>/session.v3.jsonl.zstd
        for escaped_cwd in read_children(&sessions_root) {
            for session_dir in read_children(&escaped_cwd) {
                let log = session_dir.join("session.v3.jsonl.zstd");
                if log.is_file() {
                    push_unique(logs, log);
                }
            }
        }
    }
}

fn read_children(directory: &std::path::Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

fn push_unique(logs: &mut Vec<PathBuf>, path: PathBuf) {
    if !logs.contains(&path) {
        logs.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The tests set a process-wide environment variable, so they must not
    /// run concurrently.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_session(root: &std::path::Path, escaped: &str, session: &str) {
        let log = root.join("sessions").join(escaped).join(session);
        std::fs::create_dir_all(&log).unwrap();
        std::fs::write(log.join("session.v3.jsonl.zstd"), b"").unwrap();
    }

    #[test]
    fn discovers_sessions_under_an_escaped_cwd() {
        let _guard = env_lock();
        let temp = assert_fs::TempDir::new().unwrap();
        write_session(
            temp.path(),
            "--home-user-workspace--",
            "session-11111111-1111-4111-8111-111111111111",
        );
        unsafe { std::env::set_var(DSH_HOME_ENV, temp.path()) };
        let logs = session_logs().unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].ends_with("session.v3.jsonl.zstd"));
    }

    #[test]
    fn override_may_point_at_a_session_directory() {
        let _guard = env_lock();
        let temp = assert_fs::TempDir::new().unwrap();
        write_session(
            temp.path(),
            "--home-user-workspace--",
            "session-22222222-2222-4222-8222-222222222222",
        );
        let session = temp
            .path()
            .join("sessions")
            .join("--home-user-workspace--")
            .join("session-22222222-2222-4222-8222-222222222222");
        unsafe { std::env::set_var(DSH_HOME_ENV, &session) };
        let logs = session_logs().unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].ends_with("session.v3.jsonl.zstd"));
    }
}
