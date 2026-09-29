//! Normalizes recorded Claude Code payloads from `tests/fixtures/`.
//!
//! Every hook event installed by `groundstation connect claude-code` must have
//! a fixture here, so a change to the installed hooks can't skip coverage.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::Utc;
use groundstation_adapter_claude_code::{ClaudeCode, NAME, settings};
use groundstation_schema::{Adapter, EventKind, HookEnvelope, attr};
use serde_json::{Value, json};
use uuid::Uuid;

const SESSION: &str = "0f6b1c4e-3a55-4f7e-9d7a-2c1f6b8e4d21";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn hook(name: &str) -> HookEnvelope {
    let path = fixtures().join(format!("hooks/{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    HookEnvelope {
        id: Uuid::now_v7(),
        observed_at: Utc::now(),
        payload: serde_json::from_str(&text).unwrap(),
    }
}

#[test]
fn every_installed_hook_has_a_fixture() {
    let on_disk: HashSet<String> = std::fs::read_dir(fixtures().join("hooks"))
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
    for spec in settings::HOOKS.events {
        let event = spec.event;
        assert!(
            on_disk.contains(event),
            "missing tests/fixtures/hooks/{event}.json"
        );
    }
}

#[test]
fn hooks_normalize_to_expected_kinds() {
    let expected = [
        ("SessionStart", EventKind::AgentStarted),
        ("UserPromptSubmit", EventKind::TurnUser),
        ("PreToolUse", EventKind::ToolStarted),
        ("PostToolUse", EventKind::ToolCompleted),
        ("PostToolUseFailure", EventKind::ToolFailed),
        ("Stop", EventKind::TurnCompleted),
        ("SubagentStart", EventKind::SubagentStarted),
        ("SubagentStop", EventKind::SubagentCompleted),
        ("PreCompact", EventKind::ContextCompacted),
        ("Notification", EventKind::AgentNotification),
        ("SessionEnd", EventKind::AgentCompleted),
    ];
    for (name, kind) in expected {
        let n = ClaudeCode.normalize(&hook(name));
        assert_eq!(n.events.len(), 1, "{name}");
        let ev = &n.events[0];
        assert_eq!(ev.kind, kind, "{name}");
        assert_eq!(ev.trajectory_id, SESSION, "{name}");
        assert_eq!(ev.agent.name, NAME, "{name}");
        assert_eq!(
            ev.get(attr::CWD),
            Some(&json!("/Users/dev/src/shop")),
            "{name}"
        );
    }
}

#[test]
fn tool_calls_carry_span_and_details() {
    let pre = &ClaudeCode.normalize(&hook("PreToolUse")).events[0];
    let post = &ClaudeCode.normalize(&hook("PostToolUse")).events[0];
    assert_eq!(pre.span_id, post.span_id);
    assert_eq!(
        pre.get(attr::SHELL_COMMAND),
        Some(&json!("cargo test -p checkout"))
    );
    assert_eq!(pre.get(attr::TOOL_CATEGORY), Some(&json!("shell")));
    assert_eq!(post.get(attr::SHELL_STDERR_BYTES), Some(&json!(0)));

    let failed = &ClaudeCode.normalize(&hook("PostToolUseFailure")).events[0];
    assert_eq!(failed.get(attr::ERROR_CATEGORY), Some(&json!("timeout")));
}

#[test]
fn transcript_paths_are_reported() {
    // PreToolUse never follows a model response the previous hook missed.
    assert!(
        ClaudeCode
            .normalize(&hook("PreToolUse"))
            .transcripts
            .is_empty()
    );
    let stop = ClaudeCode.normalize(&hook("SubagentStop"));
    assert_eq!(
        stop.transcripts.len(),
        2,
        "session and subagent transcripts"
    );
}

#[test]
fn transcript_yields_one_event_per_model_response() {
    let text = std::fs::read_to_string(fixtures().join("transcript.jsonl")).unwrap();
    let mut state = Value::Null;
    let events: Vec<_> = text
        .lines()
        .filter_map(|line| ClaudeCode.parse_transcript_line(SESSION, line, &mut state))
        .collect();

    // Two content-block lines of msg_01AbCdEf share an id; the daemon keeps
    // the latest. The <synthetic> line and non-assistant lines are skipped.
    let ids: HashSet<_> = events.iter().map(|e| e.id).collect();
    assert_eq!(events.len(), 3);
    assert_eq!(ids.len(), 2);

    let last = events
        .iter()
        .rfind(|e| e.span_id.as_deref() == Some("msg_01AbCdEf"))
        .unwrap();
    assert_eq!(last.kind, EventKind::ModelCompleted);
    assert_eq!(last.agent.version.as_deref(), Some("2.1.3"));
    assert_eq!(
        last.get(attr::GEN_AI_RESPONSE_MODEL),
        Some(&json!("claude-opus-5-5"))
    );
    assert_eq!(last.get(attr::GEN_AI_OUTPUT_TOKENS), Some(&json!(96)));
    assert_eq!(last.get(attr::CACHE_READ_TOKENS), Some(&json!(16021)));
    assert_eq!(
        last.get(attr::GEN_AI_FINISH_REASONS),
        Some(&json!(["tool_use"]))
    );
    assert_eq!(last.get(attr::MODEL_TOOL_USES), Some(&Value::from(1)));
    // From the prompt (14:03:11.002) to the response's last block (14:03:13.912).
    assert_eq!(last.get(attr::DURATION_MS), Some(&json!(2910)));
    // From the tool result (14:04:01.004) to the only block (14:04:03.440).
    let next = events
        .iter()
        .find(|e| e.span_id.as_deref() == Some("msg_01GhIjKl"))
        .unwrap();
    assert_eq!(next.get(attr::DURATION_MS), Some(&json!(2436)));
}
