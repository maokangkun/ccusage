//! Conversation discovery for the OpenHands adapter.
//!
//! OpenHands persists each conversation as a directory of JSON event files
//! (`events/event-00000-<id>.json`). Token usage lives in
//! `ConversationStateUpdateEvent` entries with `key: "stats"`. Users can
//! point the adapter at an explicit persistence directory with
//! `OPENHANDS_DIR`; otherwise well-known roots are scanned for conversation
//! directories.

use std::{env, fs, path::PathBuf};

use crate::Result;

pub(crate) const OPENHANDS_DIR_ENV: &str = "OPENHANDS_DIR";

/// Roots (relative to the user's home directory) that may hold OpenHands
/// persistence data.
const DEFAULT_OPENHANDS_ROOTS: [&str; 2] = [".openhands", ".config/openhands"];

/// Subdirectories (relative to a persistence root) that hold conversation
/// directories. The desktop app nests them under `agent-canvas`; the SDK
/// keeps them in `workspace/conversations`.
const CONVERSATION_DIRS: [&str; 3] = [
    "agent-canvas/dev_conversations",
    "agent-canvas/conversations",
    "conversations",
];

fn candidate_roots() -> Vec<PathBuf> {
    if let Some(value) = env::var_os(OPENHANDS_DIR_ENV) {
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
            DEFAULT_OPENHANDS_ROOTS
                .into_iter()
                .map(move |root| home.join(root))
        })
        .collect()
}

/// Returns the event-log directory of every conversation found.
///
/// An explicit `OPENHANDS_DIR` override is exclusive: only the listed roots
/// are considered, and discovery does not fall back to well-known locations.
pub(crate) fn conversation_dirs() -> Result<Vec<PathBuf>> {
    let mut conversations = Vec::new();
    for root in candidate_roots() {
        if root.join("events").is_dir() {
            // The override may point straight at a conversation directory.
            push_unique(&mut conversations, root.join("events"));
            continue;
        }
        for nested in CONVERSATION_DIRS {
            let conversations_root = root.join(nested);
            if let Ok(entries) = fs::read_dir(&conversations_root) {
                for entry in entries.filter_map(std::result::Result::ok) {
                    let path = entry.path().join("events");
                    if path.is_dir() {
                        push_unique(&mut conversations, path);
                    }
                }
            }
        }
        // Also accept a persistence root whose direct children are the
        // conversation directories (OH_PERSISTENCE_DIR style).
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.filter_map(std::result::Result::ok) {
                if !entry.file_type().is_ok_and(|type_| type_.is_dir()) {
                    continue;
                }
                let path = entry.path().join("events");
                if path.is_dir() {
                    push_unique(&mut conversations, path);
                }
            }
        }
    }
    conversations.sort();
    Ok(conversations)
}

fn push_unique(conversations: &mut Vec<PathBuf>, path: PathBuf) {
    if !conversations.contains(&path) {
        conversations.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;

    fn prepare(root: &std::path::Path, layout: &[(&str, &str)]) -> Vec<String> {
        for (dir, file) in layout {
            let events = root.join(dir).join("events");
            std::fs::create_dir_all(&events).unwrap();
            std::fs::write(events.join(file), "{}").unwrap();
        }
        unsafe { std::env::set_var(OPENHANDS_DIR_ENV, root) };
        let dirs = conversation_dirs().unwrap();
        let mut found = dirs
            .iter()
            .map(|path| {
                path.parent()
                    .unwrap()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect::<Vec<_>>();
        found.sort();
        found
    }

    #[test]
    fn discovers_conversations_in_known_layouts() {
        let temp = TempDir::new().unwrap();
        let paths = prepare(
            temp.path(),
            &[
                (
                    "agent-canvas/dev_conversations/conv-a",
                    "event-00000-a.json",
                ),
                ("conversations/conv-b", "event-00000-b.json"),
            ],
        );
        assert_eq!(
            paths,
            vec![
                "agent-canvas/dev_conversations/conv-a".to_string(),
                "conversations/conv-b".to_string()
            ]
        );
    }

    #[test]
    fn override_may_point_at_a_conversation_directory() {
        let temp = TempDir::new().unwrap();
        let conv = temp.path().join("agent-canvas/dev_conversations/conv-a");
        let events = conv.join("events");
        std::fs::create_dir_all(&events).unwrap();
        std::fs::write(events.join("event-00000-a.json"), "{}").unwrap();
        unsafe { std::env::set_var(OPENHANDS_DIR_ENV, &conv) };
        let dirs = conversation_dirs().unwrap();
        assert_eq!(dirs, vec![events]);
    }
}
