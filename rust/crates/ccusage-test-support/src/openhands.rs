use std::path::Path;

/// Creates the event log used by OpenHands report tests. The layout mirrors
/// the OpenHands persistence store: one directory per conversation, each
/// holding `event-<idx>-<id>.json` files whose stats snapshots carry
/// accumulated token usage per LLM.
pub fn create_fixture(conversations_root: impl AsRef<Path>) {
    write_conversation(
        conversations_root.as_ref().join("conv-1"),
        &[
            // An early snapshot must not double-count.
            stats_event(
                "2026-01-01T00:00:00.000000",
                "default",
                "openai/gpt-5",
                100,
                10,
                0,
                0,
                Some(0.5),
            ),
            // A second LLM in the same conversation.
            stats_event(
                "2026-01-02T00:00:00.000000",
                "condenser",
                "gpt-5-mini",
                0,
                0,
                0,
                0,
                None,
            ),
            // The final snapshot wins: totals are 200/20, not 100/10.
            stats_event(
                "2026-01-02T00:00:01.000000",
                "default",
                "openai/gpt-5",
                200,
                20,
                30,
                10,
                Some(1.5),
            ),
            non_stats_event(),
        ],
    );
    write_conversation(
        conversations_root.as_ref().join("conv-2"),
        &[stats_event(
            "2026-01-03T00:00:00.000000",
            "default",
            "anthropic/claude-sonnet-4-5",
            50,
            5,
            0,
            0,
            None,
        )],
    );
}

fn write_conversation(conversation: std::path::PathBuf, events: &[serde_json::Value]) {
    let events_dir = conversation.join("events");
    std::fs::create_dir_all(&events_dir).unwrap();
    for (index, event) in events.iter().enumerate() {
        let path = events_dir.join(format!("event-{index:05}-id{index}.json"));
        std::fs::write(path, serde_json::to_string(event).unwrap()).unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn stats_event(
    timestamp: &str,
    usage_id: &str,
    model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read: i64,
    cache_write: i64,
    cost: Option<f64>,
) -> serde_json::Value {
    let metrics = serde_json::json!({
        "model_name": model,
        "accumulated_cost": cost.unwrap_or(0.0),
        "max_budget_per_task": None::<serde_json::Value>,
        "accumulated_token_usage": {
            "model": model,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "cache_read_tokens": cache_read,
            "cache_write_tokens": cache_write,
            "reasoning_tokens": 0,
            "context_window": 0,
            "per_turn_token": prompt_tokens + completion_tokens,
            "response_id": ""
        }
    });
    serde_json::json!({
        "id": format!("event-{usage_id}-{timestamp}"),
        "timestamp": timestamp,
        "source": "environment",
        "parent_id": null,
        "kind": "ConversationStateUpdateEvent",
        "key": "stats",
        "value": {"usage_to_metrics": {usage_id: metrics}}
    })
}

fn non_stats_event() -> serde_json::Value {
    serde_json::json!({
        "id": "event-other",
        "timestamp": "2026-01-02T00:00:02.000000",
        "source": "environment",
        "parent_id": null,
        "kind": "MessageEvent",
        "key": "",
        "value": {}
    })
}
