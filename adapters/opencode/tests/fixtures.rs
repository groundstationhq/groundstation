//! Normalizes sample plugin payloads from `tests/fixtures/events/`, in the
//! shape `plugin.js` sends them.
//!
//! Every event the plugin forwards must have a fixture here, so a change to
//! the forwarded set can't skip coverage.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use groundstation_adapter_opencode::{NAME, OpenCode, settings};
use groundstation_schema::{Adapter, Event, EventKind, HookEnvelope, attr};
use serde_json::json;
use uuid::Uuid;

const ROOT: &str = "ses_01J9ZK3Q7M2X8C4V6B1N5D0F2A";
const CHILD: &str = "ses_01J9ZK4R2T8Y6U1I3O5P7A9S0D";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events")
}

fn envelope(name: &str) -> HookEnvelope {
    let path = fixtures().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    HookEnvelope {
        id: Uuid::now_v7(),
        observed_at: Utc::now(),
        payload: serde_json::from_str(&text).unwrap(),
    }
}

fn one(name: &str) -> Event {
    let mut n = OpenCode.normalize(&envelope(name));
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
        ("session.created", EventKind::AgentStarted),
        ("session.inbox.enqueued", EventKind::TurnUser),
        ("session.step.ended", EventKind::ModelCompleted),
        ("session.step.failed", EventKind::ModelCompleted),
        ("session.tool.called", EventKind::ToolStarted),
        ("session.tool.success", EventKind::ToolFailed),
        ("session.tool.failed", EventKind::ToolFailed),
        ("session.compaction.started", EventKind::ContextCompacted),
        ("session.compaction.ended", EventKind::ModelCompleted),
        ("permission.asked", EventKind::AgentNotification),
        ("session.execution.succeeded", EventKind::TurnCompleted),
        ("session.execution.failed", EventKind::AgentFailed),
        ("session.execution.interrupted", EventKind::TurnInterrupted),
        ("session.deleted", EventKind::AgentCompleted),
    ];
    for (name, kind) in expected {
        let ev = one(name);
        assert_eq!(ev.kind, kind, "{name}");
        assert_eq!(ev.trajectory_id, ROOT, "{name}");
        assert_eq!(ev.agent.name, NAME, "{name}");
        assert_eq!(
            ev.get(attr::CWD),
            Some(&json!("/Users/dev/src/shop")),
            "{name}"
        );
        assert!(ev.get(attr::SIDECHAIN).is_none(), "{name}");
        assert_eq!(ev.agent.version.as_deref(), Some("2.0.19"), "{name}");
    }
}

#[test]
fn ids_and_timestamps_come_from_opencode() {
    let ev = one("session.created");
    assert_eq!(ev.id, one("session.created").id, "stable across redelivery");
    assert_eq!(
        ev.timestamp,
        DateTime::from_timestamp_millis(1790604191000).unwrap()
    );
    assert_eq!(ev.agent.version.as_deref(), Some("2.0.19"));
    assert_eq!(
        ev.get(attr::GEN_AI_REQUEST_MODEL),
        Some(&json!("claude-opus-5-5"))
    );
}

#[test]
fn model_steps_carry_tokens_cost_and_latency() {
    let ev = one("session.step.ended");
    assert_eq!(ev.span_id.as_deref(), Some("msg_a1"));
    assert_eq!(
        ev.get(attr::GEN_AI_RESPONSE_MODEL),
        Some(&json!("claude-opus-5-5"))
    );
    assert_eq!(ev.get(attr::GEN_AI_PROVIDER), Some(&json!("anthropic")));
    assert_eq!(ev.get(attr::GEN_AI_INPUT_TOKENS), Some(&json!(12)));
    // Output includes reasoning, which is also reported on its own.
    assert_eq!(ev.get(attr::GEN_AI_OUTPUT_TOKENS), Some(&json!(400)));
    assert_eq!(ev.get(attr::REASONING_TOKENS), Some(&json!(60)));
    assert_eq!(ev.get(attr::CACHE_READ_TOKENS), Some(&json!(18000)));
    assert_eq!(ev.get(attr::CACHE_CREATION_TOKENS), Some(&json!(2100)));
    assert_eq!(ev.get(attr::COST_USD), Some(&json!(0.0421)));
    assert_eq!(ev.get(attr::DURATION_MS), Some(&json!(3700)));
    assert_eq!(
        ev.get(attr::GEN_AI_FINISH_REASONS),
        Some(&json!(["tool-calls"]))
    );

    let failed = one("session.step.failed");
    assert_eq!(failed.get(attr::ERROR_CATEGORY), Some(&json!("rate_limit")));

    let compaction = one("session.compaction.ended");
    assert_eq!(
        compaction.get(attr::MODEL_PURPOSE),
        Some(&json!("compaction"))
    );
    assert_eq!(compaction.get(attr::COST_USD), Some(&json!(0.0105)));
}

#[test]
fn tools_carry_span_name_and_exit_code() {
    let called = one("session.tool.called");
    let done = one("session.tool.success");
    assert_eq!(called.span_id.as_deref(), Some("call_bash1"));
    assert_eq!(called.span_id, done.span_id);
    assert_eq!(called.get(attr::GEN_AI_TOOL_NAME), Some(&json!("bash")));
    assert_eq!(called.get(attr::TOOL_CATEGORY), Some(&json!("shell")));
    assert_eq!(
        called.get(attr::SHELL_COMMAND),
        Some(&json!("cargo test -p checkout"))
    );
    // A non-zero exit is a failure, as for the other agents.
    assert_eq!(done.get(attr::SHELL_EXIT_CODE), Some(&json!(101)));
    assert_eq!(done.get(attr::ERROR_CATEGORY), Some(&json!("tool_error")));
    assert_eq!(
        done.get(attr::TOOL_OUTPUT),
        Some(&json!("test checkout::retry ... FAILED"))
    );

    let failed = one("session.tool.failed");
    assert_eq!(failed.get(attr::GEN_AI_TOOL_NAME), Some(&json!("read")));
    assert_eq!(failed.get(attr::TOOL_CATEGORY), Some(&json!("file_read")));
    assert_eq!(
        failed.get(attr::ERROR_MESSAGE),
        Some(&json!("File not found: src/checkout/missing.rs"))
    );
}

#[test]
fn failures_are_classified() {
    let ev = one("session.execution.failed");
    assert_eq!(
        ev.get(attr::ERROR_CATEGORY),
        Some(&json!("authentication_error"))
    );
    assert_eq!(
        one("session.execution.interrupted").get("gs.interrupt.reason"),
        Some(&json!("user"))
    );
    let perm = one("permission.asked");
    assert_eq!(
        perm.get(attr::NOTIFICATION_TYPE),
        Some(&json!("permission_request"))
    );
    assert_eq!(perm.get(attr::GEN_AI_TOOL_NAME), Some(&json!("bash")));
}

#[test]
fn subagents_fold_into_the_root_trajectory() {
    let started = one("subagent.created");
    assert_eq!(started.kind, EventKind::SubagentStarted);
    assert_eq!(started.trajectory_id, ROOT);
    assert_eq!(started.span_id.as_deref(), Some(CHILD));
    assert_eq!(started.get(attr::SUBAGENT_TYPE), Some(&json!("explore")));

    let model = one("subagent.step.ended");
    assert_eq!(model.trajectory_id, ROOT);
    assert_eq!(model.get(attr::SIDECHAIN), Some(&json!(true)));
    assert_eq!(
        model.get(attr::GEN_AI_RESPONSE_MODEL),
        Some(&json!("claude-haiku-4-5"))
    );

    let done = one("subagent.execution.succeeded");
    assert_eq!(done.kind, EventKind::SubagentCompleted);
    assert_eq!(done.span_id.as_deref(), Some(CHILD));
}

#[test]
fn malformed_payloads_yield_nothing() {
    for payload in [
        json!({}),
        json!({"event": {"type": "session.idle"}}),
        json!("x"),
    ] {
        let env = HookEnvelope {
            id: Uuid::nil(),
            observed_at: Utc::now(),
            payload,
        };
        assert!(OpenCode.normalize(&env).events.is_empty());
    }
}
