//! Hook payloads and transcript lines → `groundstation.telemetry.v0` events.

use chrono::{DateTime, Utc};
use groundstation_schema::{
    Agent, ErrorCategory, Event, EventKind, HookEnvelope, Normalized, ToolCategory, attr,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::NAME as AGENT;

/// Namespace for event ids derived from transcript content.
const TRANSCRIPT_NS: Uuid = Uuid::from_u128(0x6773_2e74_7261_6e73_6372_6970_742e_7630);

pub fn normalize(envelope: &HookEnvelope) -> Normalized {
    let p = &envelope.payload;
    let (Some(session), Some(hook)) = (str_field(p, "session_id"), str_field(p, "hook_event_name"))
    else {
        return Normalized {
            events: Vec::new(),
            transcripts: Vec::new(),
        };
    };

    let mut n = 0u32;
    let mut event = |kind: EventKind| {
        n += 1;
        let id = Uuid::new_v5(&envelope.id, format!("{AGENT}/{n}").as_bytes());
        let mut ev = Event::new(id, session, kind, envelope.observed_at, Agent::named(AGENT));
        ev.set(attr::SESSION_ID, session);
        ev.set(attr::CWD, str_field(p, "cwd"));
        ev.set(attr::PERMISSION_MODE, str_field(p, "permission_mode"));
        ev
    };

    let events = match hook {
        "SessionStart" => {
            let source = str_field(p, "source").unwrap_or("startup");
            let kind = match source {
                "resume" | "compact" => EventKind::AgentResumed,
                _ => EventKind::AgentStarted,
            };
            let mut ev = event(kind);
            ev.set(attr::SESSION_SOURCE, source);
            ev.set(attr::TRANSCRIPT_PATH, str_field(p, "transcript_path"));
            ev.set("gen_ai.request.model", str_field(p, "model"));
            vec![ev]
        }
        "SessionEnd" => {
            let mut ev = event(EventKind::AgentCompleted);
            ev.set(attr::END_REASON, str_field(p, "reason"));
            vec![ev]
        }
        "UserPromptSubmit" => {
            let mut ev = event(EventKind::TurnUser);
            if let Some(prompt) = str_field(p, "prompt") {
                ev.set(attr::PROMPT_BYTES, prompt.len());
                ev.set(attr::PROMPT_TEXT, prompt);
            }
            vec![ev]
        }
        "PreToolUse" => {
            let mut ev = tool_event(event(EventKind::ToolStarted), p);
            if let Some(input) = p.get("tool_input") {
                ev.set(attr::TOOL_INPUT_BYTES, input.to_string().len());
                ev.set(attr::TOOL_INPUT, input.clone());
            }
            vec![ev]
        }
        "PostToolUse" => {
            let response = p.get("tool_response").unwrap_or(&Value::Null);
            let failed = response
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let kind = if failed {
                EventKind::ToolFailed
            } else {
                EventKind::ToolCompleted
            };
            let mut ev = tool_event(event(kind), p);
            tool_output(&mut ev, response);
            if failed {
                ev.set(attr::ERROR_CATEGORY, category_str(ErrorCategory::ToolError));
            }
            vec![ev]
        }
        "PostToolUseFailure" => {
            let mut ev = tool_event(event(EventKind::ToolFailed), p);
            let error = p.get("error").map(|e| match e {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            let interrupted = p
                .get("is_interrupt")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let category = if interrupted {
                ErrorCategory::UserCancelled
            } else {
                classify_error(error.as_deref().unwrap_or(""))
            };
            ev.set(attr::ERROR_CATEGORY, category_str(category));
            ev.set(attr::ERROR_MESSAGE, error);
            if let Some(response) = p.get("tool_response") {
                tool_output(&mut ev, response);
            }
            vec![ev]
        }
        "Stop" => vec![event(EventKind::TurnCompleted)],
        "SubagentStart" | "SubagentStop" => {
            let kind = if hook == "SubagentStart" {
                EventKind::SubagentStarted
            } else {
                EventKind::SubagentCompleted
            };
            let mut ev = event(kind);
            if let Some(agent_id) = str_field(p, "agent_id") {
                ev.span_id = Some(agent_id.to_string());
                ev.set(attr::SUBAGENT_ID, agent_id);
            }
            ev.set(attr::SUBAGENT_TYPE, str_field(p, "agent_type"));
            vec![ev]
        }
        "PreCompact" => {
            let mut ev = event(EventKind::ContextCompacted);
            ev.set(attr::COMPACTION_TRIGGER, str_field(p, "trigger"));
            vec![ev]
        }
        "Notification" => {
            let mut ev = event(EventKind::AgentNotification);
            ev.set(attr::NOTIFICATION_MESSAGE, str_field(p, "message"));
            ev.set("gs.notification.type", str_field(p, "notification_type"));
            vec![ev]
        }
        other => vec![event(EventKind::Custom(format!(
            "claude_code.{}",
            snake_case(other)
        )))],
    };

    let transcripts = ["transcript_path", "agent_transcript_path"]
        .iter()
        .filter_map(|key| str_field(p, key))
        .map(str::to_string)
        .collect();

    // PreToolUse fires constantly and never follows a new model response
    // that the previous hook didn't already see, so skip the transcript read.
    let transcripts = if hook == "PreToolUse" {
        Vec::new()
    } else {
        transcripts
    };

    Normalized {
        events,
        transcripts,
    }
}

/// Parses one transcript line into a `model.completed` event, if it records a
/// model response. A single response can span several lines (one per content
/// block); they share a message id and therefore an event id.
pub fn parse_transcript_line(session_id: &str, line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = v.get("message")?;
    let model = message.get("model")?.as_str()?;
    if model.starts_with('<') {
        // `<synthetic>` messages are written by Claude Code itself, not the API.
        return None;
    }
    let message_id = message.get("id")?.as_str()?;
    let usage = message.get("usage")?;
    let timestamp: DateTime<Utc> = v.get("timestamp")?.as_str()?.parse().ok()?;

    let id = Uuid::new_v5(
        &TRANSCRIPT_NS,
        format!("{session_id}/{message_id}").as_bytes(),
    );
    let agent = Agent {
        name: AGENT.to_string(),
        version: str_field(&v, "version").map(str::to_string),
    };
    let mut ev = Event::new(id, session_id, EventKind::ModelCompleted, timestamp, agent)
        .with_span(message_id);
    ev.set(attr::GEN_AI_PROVIDER, "anthropic");
    ev.set(attr::GEN_AI_RESPONSE_MODEL, model);
    ev.set(attr::GEN_AI_RESPONSE_ID, message_id);
    ev.set(
        attr::GEN_AI_INPUT_TOKENS,
        usage.get("input_tokens").cloned(),
    );
    ev.set(
        attr::GEN_AI_OUTPUT_TOKENS,
        usage.get("output_tokens").cloned(),
    );
    ev.set(
        attr::CACHE_READ_TOKENS,
        usage.get("cache_read_input_tokens").cloned(),
    );
    ev.set(
        attr::CACHE_CREATION_TOKENS,
        usage.get("cache_creation_input_tokens").cloned(),
    );
    if let Some(reason) = str_field(message, "stop_reason") {
        ev.set(attr::GEN_AI_FINISH_REASONS, json!([reason]));
    }
    ev.set(attr::REQUEST_ID, str_field(&v, "requestId"));
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        ev.set(attr::SIDECHAIN, true);
    }
    let tool_uses = message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| str_field(b, "type") == Some("tool_use"))
                .count()
        });
    if let Some(n) = tool_uses.filter(|n| *n > 0) {
        ev.set(attr::MODEL_TOOL_USES, n);
    }
    Some(ev)
}

pub fn tool_category(name: &str) -> ToolCategory {
    match name {
        "Bash" | "BashOutput" | "KillShell" | "KillBash" => ToolCategory::Shell,
        "Read" | "NotebookRead" => ToolCategory::FileRead,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => ToolCategory::FileWrite,
        "Grep" | "Glob" | "LS" | "ToolSearch" => ToolCategory::Search,
        "WebFetch" | "WebSearch" => ToolCategory::Http,
        "Task" | "Agent" => ToolCategory::Subagent,
        "TodoWrite" | "TaskCreate" | "TaskUpdate" | "EnterPlanMode" | "ExitPlanMode" => {
            ToolCategory::Planning
        }
        n if n.starts_with("mcp__") => {
            if n.contains("browser") || n.contains("chrome") || n.contains("playwright") {
                ToolCategory::Browser
            } else {
                ToolCategory::Mcp
            }
        }
        _ => ToolCategory::Custom,
    }
}

fn tool_event(mut ev: Event, p: &Value) -> Event {
    let name = str_field(p, "tool_name").unwrap_or("unknown");
    if let Some(id) = str_field(p, "tool_use_id") {
        ev.span_id = Some(id.to_string());
        ev.set(attr::GEN_AI_TOOL_CALL_ID, id);
    }
    ev.set(attr::GEN_AI_TOOL_NAME, name);
    ev.set(attr::TOOL_CATEGORY, tool_category(name).as_str());

    let input = p.get("tool_input").unwrap_or(&Value::Null);
    let field = |k: &str| str_field(input, k);
    match name {
        "Bash" => ev.set(attr::SHELL_COMMAND, field("command")),
        "Read" => file(&mut ev, field("file_path"), "read"),
        "Write" => file(&mut ev, field("file_path"), "write"),
        "Edit" | "MultiEdit" => file(&mut ev, field("file_path"), "edit"),
        "NotebookEdit" => file(&mut ev, field("notebook_path"), "edit"),
        "Grep" | "Glob" => {
            ev.set(attr::SEARCH_PATTERN, field("pattern"));
            ev.set(attr::FILE_PATH, field("path"));
        }
        "WebFetch" => ev.set(attr::HTTP_URL, field("url")),
        "WebSearch" => ev.set(attr::SEARCH_PATTERN, field("query")),
        "Task" | "Agent" => ev.set(attr::SUBAGENT_TYPE, field("subagent_type")),
        _ => {}
    }
    ev
}

fn file(ev: &mut Event, path: Option<&str>, operation: &str) {
    ev.set(attr::FILE_PATH, path);
    ev.set(attr::FILE_OPERATION, operation);
}

fn tool_output(ev: &mut Event, response: &Value) {
    if response.is_null() {
        return;
    }
    ev.set(attr::TOOL_OUTPUT_BYTES, response.to_string().len());
    if let Value::Object(obj) = response {
        shell_output(ev, obj);
    }
    ev.set(attr::TOOL_OUTPUT, response.clone());
}

fn shell_output(ev: &mut Event, obj: &Map<String, Value>) {
    if let Some(stdout) = obj.get("stdout").and_then(Value::as_str) {
        ev.set(attr::SHELL_STDOUT_BYTES, stdout.len());
    }
    if let Some(stderr) = obj.get("stderr").and_then(Value::as_str) {
        ev.set(attr::SHELL_STDERR_BYTES, stderr.len());
    }
    if let Some(interrupted) = obj.get("interrupted").and_then(Value::as_bool) {
        ev.set(attr::SHELL_INTERRUPTED, interrupted);
    }
    for key in ["exit_code", "exitCode", "returnCode"] {
        if let Some(code) = obj.get(key).and_then(Value::as_i64) {
            ev.set(attr::SHELL_EXIT_CODE, code);
            break;
        }
    }
}

fn classify_error(message: &str) -> ErrorCategory {
    let m = message.to_ascii_lowercase();
    if m.contains("timed out") || m.contains("timeout") {
        ErrorCategory::Timeout
    } else if m.contains("permission") || m.contains("denied") || m.contains("not allowed") {
        ErrorCategory::PermissionError
    } else if m.contains("interrupted") || m.contains("cancel") {
        ErrorCategory::UserCancelled
    } else {
        ErrorCategory::ToolError
    }
}

fn category_str(c: ErrorCategory) -> Value {
    serde_json::to_value(c).expect("enum serializes")
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: Value) -> HookEnvelope {
        HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload,
        }
    }

    fn base(hook: &str) -> Value {
        json!({
            "session_id": "sess-1",
            "transcript_path": "/home/u/.claude/projects/p/sess-1.jsonl",
            "cwd": "/home/u/repo",
            "permission_mode": "default",
            "hook_event_name": hook,
        })
    }

    fn with(hook: &str, extra: Value) -> Value {
        let mut v = base(hook);
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    fn one(payload: Value) -> Event {
        let mut n = normalize(&envelope(payload));
        assert_eq!(n.events.len(), 1);
        n.events.remove(0)
    }

    #[test]
    fn session_lifecycle() {
        let ev = one(with("SessionStart", json!({"source": "resume"})));
        assert_eq!(ev.kind, EventKind::AgentResumed);
        assert_eq!(ev.trajectory_id, "sess-1");
        assert_eq!(ev.get(attr::CWD), Some(&json!("/home/u/repo")));

        let ev = one(with("SessionEnd", json!({"reason": "prompt_input_exit"})));
        assert_eq!(ev.kind, EventKind::AgentCompleted);
        assert_eq!(ev.get(attr::END_REASON), Some(&json!("prompt_input_exit")));
    }

    #[test]
    fn bash_tool_span() {
        let pre = one(with(
            "PreToolUse",
            json!({"tool_name": "Bash", "tool_use_id": "toolu_1", "tool_input": {"command": "cargo test"}}),
        ));
        assert_eq!(pre.kind, EventKind::ToolStarted);
        assert_eq!(pre.span_id.as_deref(), Some("toolu_1"));
        assert_eq!(pre.get(attr::SHELL_COMMAND), Some(&json!("cargo test")));
        assert_eq!(pre.get(attr::TOOL_CATEGORY), Some(&json!("shell")));

        let post = one(with(
            "PostToolUse",
            json!({
                "tool_name": "Bash", "tool_use_id": "toolu_1",
                "tool_input": {"command": "cargo test"},
                "tool_response": {"stdout": "ok\n", "stderr": "warn", "interrupted": false},
            }),
        ));
        assert_eq!(post.kind, EventKind::ToolCompleted);
        assert_eq!(post.span_id.as_deref(), Some("toolu_1"));
        assert_eq!(post.get(attr::SHELL_STDOUT_BYTES), Some(&json!(3)));
        assert_eq!(post.get(attr::SHELL_STDERR_BYTES), Some(&json!(4)));
    }

    #[test]
    fn tool_failure() {
        let ev = one(with(
            "PostToolUseFailure",
            json!({"tool_name": "Read", "tool_use_id": "toolu_2", "tool_input": {"file_path": "/x"},
                   "error": "Command timed out after 2m"}),
        ));
        assert_eq!(ev.kind, EventKind::ToolFailed);
        assert_eq!(ev.get(attr::ERROR_CATEGORY), Some(&json!("timeout")));
        assert_eq!(ev.get(attr::FILE_PATH), Some(&json!("/x")));
    }

    #[test]
    fn unknown_hooks_are_kept() {
        let ev = one(base("PermissionRequest"));
        assert_eq!(
            ev.kind,
            EventKind::Custom("claude_code.permission_request".into())
        );
    }

    #[test]
    fn stable_ids_per_envelope() {
        let env = envelope(base("Stop"));
        assert_eq!(normalize(&env).events[0].id, normalize(&env).events[0].id);
        assert_ne!(
            normalize(&env).events[0].id,
            normalize(&envelope(base("Stop"))).events[0].id
        );
    }

    #[test]
    fn invalid_payload_yields_nothing() {
        assert!(
            normalize(&envelope(json!({"hook_event_name": "Stop"})))
                .events
                .is_empty()
        );
        assert!(normalize(&envelope(json!("garbage"))).events.is_empty());
    }

    #[test]
    fn transcript_usage() {
        let line = json!({
            "type": "assistant", "sessionId": "sess-1", "version": "2.1.0", "requestId": "req_1",
            "timestamp": "2026-09-28T14:03:14.821Z", "isSidechain": false,
            "message": {
                "id": "msg_1", "model": "claude-opus-5-5", "stop_reason": "tool_use",
                "content": [{"type": "text", "text": "hi"}, {"type": "tool_use", "id": "toolu_1"}],
                "usage": {"input_tokens": 12, "output_tokens": 340,
                          "cache_read_input_tokens": 18000, "cache_creation_input_tokens": 500}
            }
        })
        .to_string();
        let ev = parse_transcript_line("sess-1", &line).unwrap();
        assert_eq!(ev.kind, EventKind::ModelCompleted);
        assert_eq!(ev.agent.version.as_deref(), Some("2.1.0"));
        assert_eq!(ev.get(attr::GEN_AI_OUTPUT_TOKENS), Some(&json!(340)));
        assert_eq!(ev.get(attr::CACHE_READ_TOKENS), Some(&json!(18000)));
        assert_eq!(ev.get(attr::MODEL_TOOL_USES), Some(&json!(1)));
        assert_eq!(ev.id, parse_transcript_line("sess-1", &line).unwrap().id);

        assert!(parse_transcript_line("sess-1", r#"{"type":"user","message":{}}"#).is_none());
        assert!(parse_transcript_line("sess-1", "not json").is_none());
        let synthetic = line.replace("claude-opus-5-5", "<synthetic>");
        assert!(parse_transcript_line("sess-1", &synthetic).is_none());
    }
}
