//! Normalizes sample extension payloads from `tests/fixtures/events/`, in the
//! shape `extension.js` sends them.
//!
//! Every event the extension forwards must have a fixture here, so a change
//! to the forwarded set can't skip coverage.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use groundstation_adapter_pi::{NAME, Pi, settings};
use groundstation_schema::{Adapter, Event, EventKind, HookEnvelope, attr};
use serde_json::json;
use uuid::Uuid;

const SESSION: &str = "019a3b7c-1d2e-7f40-8a55-6b7c8d9e0f1a";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events")
}

fn one(name: &str) -> Event {
    let path = fixtures().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let envelope = HookEnvelope {
        id: Uuid::now_v7(),
        observed_at: Utc::now(),
        payload: serde_json::from_str(&text).unwrap(),
    };
    let mut n = Pi.normalize(&envelope);
    assert_eq!(n.events.len(), 1, "{name}");
    assert!(n.transcripts.is_empty());
    n.events.remove(0)
}

#[test]
fn every_forwarded_event_has_a_fixture() {
    let on_disk: HashSet<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| {
            e.unwrap()
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for event in settings::EVENTS {
        assert!(
            on_disk.contains(*event),
            "missing tests/fixtures/events/{event}.json"
        );
    }
}

#[test]
fn events_normalize_to_expected_kinds() {
    let expected = [
        ("session_start", EventKind::AgentStarted),
        ("before_agent_start", EventKind::TurnUser),
        ("message_end", EventKind::ModelCompleted),
        ("tool_execution_start", EventKind::ToolStarted),
        ("tool_execution_end", EventKind::ToolFailed),
        ("tool_execution_end.error", EventKind::ToolFailed),
        ("agent_end", EventKind::TurnCompleted),
        ("agent_end.aborted", EventKind::TurnInterrupted),
        ("agent_end.error", EventKind::AgentFailed),
        ("session_compact", EventKind::ContextCompacted),
        ("session_shutdown", EventKind::AgentCompleted),
    ];
    for (name, kind) in expected {
        let ev = one(name);
        assert_eq!(ev.kind, kind, "{name}");
        assert_eq!(ev.trajectory_id, SESSION, "{name}");
        assert_eq!(ev.agent.name, NAME, "{name}");
        assert_eq!(ev.agent.version.as_deref(), Some("0.87.1"), "{name}");
        assert_eq!(
            ev.get(attr::CWD),
            Some(&json!("/Users/dev/src/shop")),
            "{name}"
        );
    }
}

#[test]
fn ids_and_timestamps_are_stable() {
    let ev = one("session_start");
    assert_eq!(ev.id, one("session_start").id);
    assert_eq!(
        ev.timestamp,
        DateTime::from_timestamp_millis(1790604191000).unwrap()
    );
}

#[test]
fn model_calls_carry_usage_cost_and_latency() {
    let ev = one("message_end");
    assert_eq!(ev.span_id.as_deref(), Some("msg_01AbCd"));
    assert_eq!(ev.get(attr::GEN_AI_PROVIDER), Some(&json!("anthropic")));
    assert_eq!(
        ev.get(attr::GEN_AI_RESPONSE_MODEL),
        Some(&json!("claude-opus-5-5"))
    );
    assert_eq!(ev.get(attr::GEN_AI_INPUT_TOKENS), Some(&json!(12)));
    // pi already counts reasoning inside output.
    assert_eq!(ev.get(attr::GEN_AI_OUTPUT_TOKENS), Some(&json!(340)));
    assert_eq!(ev.get(attr::REASONING_TOKENS), Some(&json!(60)));
    assert_eq!(ev.get(attr::CACHE_READ_TOKENS), Some(&json!(18000)));
    assert_eq!(ev.get(attr::CACHE_CREATION_TOKENS), Some(&json!(2100)));
    assert_eq!(ev.get(attr::COST_USD), Some(&json!(0.01844)));
    assert_eq!(ev.get(attr::DURATION_MS), Some(&json!(3800)));
    assert_eq!(ev.get(attr::MODEL_TOOL_USES), Some(&json!(1)));
    assert_eq!(
        ev.get(attr::GEN_AI_FINISH_REASONS),
        Some(&json!(["toolUse"]))
    );

    let failed = one("message_end.error");
    assert_eq!(failed.get(attr::ERROR_CATEGORY), Some(&json!("rate_limit")));
    assert!(failed.span_id.is_none());
}

#[test]
fn tools_carry_span_command_and_exit_code() {
    let start = one("tool_execution_start");
    let end = one("tool_execution_end");
    assert_eq!(start.span_id.as_deref(), Some("toolu_01Xy"));
    assert_eq!(start.span_id, end.span_id);
    assert_eq!(
        start.get(attr::SHELL_COMMAND),
        Some(&json!("cargo test -p checkout"))
    );
    assert_eq!(start.get(attr::TOOL_CATEGORY), Some(&json!("shell")));
    assert_eq!(end.get(attr::SHELL_EXIT_CODE), Some(&json!(101)));
    assert_eq!(end.get(attr::ERROR_CATEGORY), Some(&json!("tool_error")));

    let failed = one("tool_execution_end.error");
    assert_eq!(failed.get(attr::TOOL_CATEGORY), Some(&json!("file_read")));
    assert!(
        failed
            .get(attr::ERROR_MESSAGE)
            .unwrap()
            .as_str()
            .unwrap()
            .contains("ENOENT")
    );
}

#[test]
fn failures_are_classified() {
    let ev = one("agent_end.error");
    assert_eq!(
        ev.get(attr::ERROR_CATEGORY),
        Some(&json!("authentication_error"))
    );
}

#[test]
fn malformed_payloads_yield_nothing() {
    for payload in [
        json!({}),
        json!({"event": {"type": "agent_end"}}),
        json!("x"),
    ] {
        let env = HookEnvelope {
            id: Uuid::nil(),
            observed_at: Utc::now(),
            payload,
        };
        assert!(Pi.normalize(&env).events.is_empty());
    }
}
