use std::path::Path;

/// Creates the session log used by dsh report tests. The layout mirrors the
/// dsh home: `sessions/<escaped-cwd>/session-<id>/session.v3.jsonl.zstd`,
/// where each log is a zstd-compressed JSONL event stream.
pub fn create_fixture(sessions_root: impl AsRef<Path>) {
    write_session(
        sessions_root.as_ref().join("--home-user-workspace--"),
        "session-11111111-1111-4111-8111-111111111111",
        &[
            header(1_000, "session-11111111-1111-4111-8111-111111111111"),
            request_header(1_100, "deepseek-flash"),
            assistant_step(1_200, 1, 1, 100, 10),
            assistant_step(1_300, 1, 2, 200, 20),
            request_header(1_400, "glm-5.3-flash"),
            assistant_step(1_500, 2, 1, 50, 5),
        ],
    );
    write_session(
        sessions_root.as_ref().join("--home-user-workspace--"),
        "session-22222222-2222-4222-8222-222222222222",
        &[
            header(2_000, "session-22222222-2222-4222-8222-222222222222"),
            request_header(2_100, "deepseek-flash"),
            assistant_step(2_200, 1, 1, 300, 30),
        ],
    );
}

fn write_session(session: std::path::PathBuf, _id: &str, events: &[serde_json::Value]) {
    let session = session.join(_id);
    let mut text = String::new();
    for event in events {
        text.push_str(&serde_json::to_string(event).unwrap());
        text.push('\n');
    }
    // dsh writes zstd-compressed logs; tests emit a single raw-block frame,
    // which any zstd decoder accepts without a compressor.
    let bytes = text.as_bytes();
    // Magic, then a frame header descriptor with an 8-byte content size and
    // the single-segment flag set, then the content size itself.
    let mut zstd = vec![0x28, 0xB5, 0x2F, 0xFD, 0xE0];
    zstd.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    zstd.extend_from_slice(&((bytes.len() as u32) << 3 | 1).to_le_bytes()[..3]);
    zstd.extend_from_slice(bytes);

    let log = session.join("session.v3.jsonl.zstd");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(log, zstd).unwrap();
}

fn header(time: i64, id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "session",
        "id": id,
        "time": time,
        "cwd": "/home/user/workspace",
    })
}

fn request_header(time: i64, model: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "request/header",
        "time": time,
        "data": {"header": {"config": {"model": model}}},
    })
}

fn assistant_step(time: i64, turn: u64, step: u64, input: u64, output: u64) -> serde_json::Value {
    serde_json::json!({
        "type": "assistant/message",
        "time": time,
        "data": {
            "turn": turn,
            "step": step,
            "usage": {
                "inputTokens": input,
                "outputTokens": output,
                "totalTokens": input + output,
            },
        },
    })
}
