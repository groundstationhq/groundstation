//! Normalizes sample Codex payloads from `tests/fixtures/`.
//!
//! Every hook event installed by `groundstation connect codex` must have a
//! fixture here, so a change to the installed hooks can't skip coverage.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::Utc;
use groundstation_adapter_codex::{Codex, NAME, settings};
use groundstation_schema::{Adapter, EventKind, HookEnvelope, attr};
use serde_json::{Value, json};
use uuid::Uuid;

const SESSION: &str = "019a2f3c-7b1e-7d40-9c55-5e8f1a2b3c4d";

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
        assert!(
            on_disk.contains(spec.event),
            "missing tests/fixtures/hooks/{}.json",
            spec.event
        );
    }
}

#[test]
fn hooks_normalize_to_expected_kinds() {
    let expected = [
        ("SessionStart", EventKind::AgentStarted),
        ("UserPromptSubmit", EventKind::TurnUser),
        ("PreToolUse", EventKind::ToolStarted),
        ("PostToolUse", EventKind::ToolFailed),
        ("PermissionRequest", EventKind::AgentNotification),
        ("Stop", EventKind::TurnCompleted),
        ("SubagentStart", EventKind::SubagentStarted),
        ("SubagentStop", EventKind::SubagentCompleted),
        ("PreCompact", EventKind::ContextCompacted),
        ("Interrupt", EventKind::TurnInterrupted),
        ("SessionEnd", EventKind::AgentCompleted),
    ];
    for (name, kind) in expected {
        let n = Codex.normalize(&hook(name));
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
        assert_eq!(
            ev.get(attr::GEN_AI_REQUEST_MODEL),
            Some(&json!("gpt-6-codex")),
            "{name}"
        );
    }
    let prompt = &Codex.normalize(&hook("UserPromptSubmit")).events[0];
    assert_eq!(prompt.get(attr::TURN_ID), Some(&json!("turn_01")));
}

#[test]
fn shell_calls_carry_span_command_and_exit_code() {
    let pre = &Codex.normalize(&hook("PreToolUse")).events[0];
    let post = &Codex.normalize(&hook("PostToolUse")).events[0];
    assert_eq!(pre.span_id.as_deref(), Some("call_Bf3kQ9"));
    assert_eq!(pre.span_id, post.span_id);
    assert_eq!(
        pre.get(attr::SHELL_COMMAND),
        Some(&json!("cargo test -p checkout"))
    );
    assert_eq!(pre.get(attr::TOOL_CATEGORY), Some(&json!("shell")));
    assert_eq!(post.get(attr::SHELL_EXIT_CODE), Some(&json!(101)));
    assert_eq!(post.get(attr::ERROR_CATEGORY), Some(&json!("tool_error")));
}

#[test]
fn apply_patch_reports_files() {
    let ev = &Codex.normalize(&hook("PreToolUse.apply_patch")).events[0];
    assert_eq!(ev.get(attr::TOOL_CATEGORY), Some(&json!("file_write")));
    assert_eq!(
        ev.get(attr::FILE_PATH),
        Some(&json!("src/checkout/retry.rs"))
    );
    assert_eq!(ev.get(attr::PATCH_FILES), Some(&json!(2)));
    assert!(ev.get(attr::SHELL_COMMAND).is_none());
}

#[test]
fn permission_requests_name_the_tool() {
    let ev = &Codex.normalize(&hook("PermissionRequest")).events[0];
    assert_eq!(
        ev.get(attr::NOTIFICATION_TYPE),
        Some(&json!("permission_request"))
    );
    assert_eq!(ev.get(attr::GEN_AI_TOOL_NAME), Some(&json!("Bash")));
}

#[test]
fn transcript_paths_are_reported() {
    assert!(Codex.normalize(&hook("PreToolUse")).transcripts.is_empty());
    let stop = Codex.normalize(&hook("SubagentStop"));
    assert_eq!(stop.transcripts.len(), 2, "session and subagent rollouts");
}

#[test]
fn rollout_yields_one_event_per_model_response() {
    let text = std::fs::read_to_string(fixtures().join("rollout.jsonl")).unwrap();
    let mut state = Value::Null;
    let events: Vec<_> = text
        .lines()
        .filter_map(|line| Codex.parse_transcript_line(SESSION, line, &mut state))
        .collect();

    // Two token_usage_record lines; token_count and other lines are ignored.
    assert_eq!(events.len(), 2);
    let first = &events[0];
    assert_eq!(first.kind, EventKind::ModelCompleted);
    assert_eq!(first.span_id.as_deref(), Some("resp_0a1b2c3d4e5f"));
    assert_eq!(first.agent.version.as_deref(), Some("0.155.0"));
    assert_eq!(
        first.get(attr::GEN_AI_RESPONSE_MODEL),
        Some(&json!("gpt-6-codex"))
    );
    // input_tokens (18701) includes the 4864 cached tokens.
    assert_eq!(first.get(attr::GEN_AI_INPUT_TOKENS), Some(&json!(13837)));
    assert_eq!(first.get(attr::CACHE_READ_TOKENS), Some(&json!(4864)));
    assert_eq!(first.get(attr::GEN_AI_OUTPUT_TOKENS), Some(&json!(119)));
    assert_eq!(first.get(attr::REASONING_TOKENS), Some(&json!(101)));
    assert!(first.get(attr::CACHE_CREATION_TOKENS).is_none());
    assert!(first.get(attr::SIDECHAIN).is_none());

    // The second response ran on a subagent thread.
    assert_eq!(events[1].get(attr::SIDECHAIN), Some(&json!(true)));

    // Ids are stable across re-reads, and state survives between reads.
    let mut state2 = state.clone();
    let again = text
        .lines()
        .rev()
        .find_map(|line| Codex.parse_transcript_line(SESSION, line, &mut state2))
        .unwrap();
    assert_eq!(again.id, events[1].id);
    assert_eq!(
        again.get(attr::GEN_AI_RESPONSE_MODEL),
        Some(&json!("gpt-6-codex"))
    );
}
