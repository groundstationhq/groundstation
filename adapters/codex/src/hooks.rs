//! Hook payloads and rollout lines → `groundstation.telemetry.v0` events.

use chrono::{DateTime, Utc};
use groundstation_schema::{
    Agent, ErrorCategory, Event, EventKind, HookEnvelope, Normalized, ToolCategory, attr,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::NAME as AGENT;

/// Namespace for event ids derived from rollout content.
const ROLLOUT_NS: Uuid = Uuid::from_u128(0x6773_2e63_6f64_6578_2e72_6f6c_6c6f_7574);

/// Patch headers in `apply_patch` input that name a file.
const PATCH_FILE_HEADERS: &[&str] = &[
    "*** Add File: ",
    "*** Update File: ",
    "*** Delete File: ",
    "*** Move to: ",
];

pub fn normalize(envelope: &HookEnvelope) -> Normalized {
    let p = &envelope.payload;
    let (Some(session), Some(hook)) = (str_field(p, "session_id"), str_field(p, "hook_event_name"))
    else {
        return Normalized::default();
    };

    let mut n = 0u32;
    let mut event = |kind: EventKind| {
        n += 1;
        let id = Uuid::new_v5(&envelope.id, format!("{AGENT}/{n}").as_bytes());
        let mut ev = Event::new(id, session, kind, envelope.observed_at, Agent::named(AGENT));
        ev.set(attr::SESSION_ID, session);
        ev.set(attr::TURN_ID, str_field(p, "turn_id"));
        ev.set(attr::CWD, str_field(p, "cwd"));
        ev.set(attr::PERMISSION_MODE, str_field(p, "permission_mode"));
        ev.set(attr::GEN_AI_REQUEST_MODEL, str_field(p, "model"));
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
            // Codex has no failure hook; a non-zero exit code is the failure signal.
            let response = p.get("tool_response").unwrap_or(&Value::Null);
            let exit_code = exit_code(response);
            let failed = exit_code.is_some_and(|c| c != 0);
            let kind = if failed {
                EventKind::ToolFailed
            } else {
                EventKind::ToolCompleted
            };
            let mut ev = tool_event(event(kind), p);
            ev.set(attr::SHELL_EXIT_CODE, exit_code);
            if failed {
                ev.set(attr::ERROR_CATEGORY, category_str(ErrorCategory::ToolError));
            }
            if !response.is_null() {
                ev.set(attr::TOOL_OUTPUT_BYTES, response_bytes(response));
                ev.set(attr::TOOL_OUTPUT, response.clone());
            }
            vec![ev]
        }
        "PermissionRequest" => {
            let mut ev = event(EventKind::AgentNotification);
            ev.set(attr::NOTIFICATION_TYPE, "permission_request");
            ev.set(attr::GEN_AI_TOOL_NAME, str_field(p, "tool_name"));
            ev.set(attr::GEN_AI_TOOL_CALL_ID, str_field(p, "tool_use_id"));
            vec![ev]
        }
        "Stop" => vec![event(EventKind::TurnCompleted)],
        "Interrupt" => vec![event(EventKind::TurnInterrupted)],
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
        other => vec![event(EventKind::Custom(format!(
            "codex.{}",
            snake_case(other)
        )))],
    };

    // PreToolUse never follows a model response that the previous hook
    // didn't already see, so it skips the rollout read.
    let transcripts = if hook == "PreToolUse" {
        Vec::new()
    } else {
        ["transcript_path", "agent_transcript_path"]
            .iter()
            .filter_map(|key| str_field(p, key))
            .map(str::to_string)
            .collect()
    };

    Normalized {
        events,
        transcripts,
    }
}

/// Parses one rollout line. `session_meta` and `turn_context` lines only
/// update `state` (CLI version, current model); each `token_usage_record`
/// becomes a `model.completed` event.
///
/// Codex reports `input_tokens` including cached tokens and `output_tokens`
/// including reasoning; they are split here so `gen_ai.usage.input_tokens`
/// means uncached input for every agent.
pub fn parse_transcript_line(session_id: &str, line: &str, state: &mut Value) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    let payload = v.get("payload")?;
    if !state.is_object() {
        *state = json!({});
    }
    match v.get("type")?.as_str()? {
        "session_meta" => {
            if let Some(version) = str_field(payload, "cli_version") {
                state["version"] = json!(version);
            }
            None
        }
        "turn_context" => {
            if let Some(model) = str_field(payload, "model") {
                state["model"] = json!(model);
            }
            None
        }
        "token_usage_record" => usage_event(session_id, &v, payload, state),
        _ => None,
    }
}

fn usage_event(session_id: &str, line: &Value, payload: &Value, state: &Value) -> Option<Event> {
    let response_id = str_field(payload, "response_id")?;
    let usage = payload.get("usage")?;
    let timestamp: DateTime<Utc> = str_field(line, "timestamp")?.parse().ok()?;
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);

    let id = Uuid::new_v5(
        &ROLLOUT_NS,
        format!("{session_id}/{response_id}").as_bytes(),
    );
    let agent = Agent {
        name: AGENT.to_string(),
        version: str_field(state, "version").map(str::to_string),
    };
    let mut ev = Event::new(id, session_id, EventKind::ModelCompleted, timestamp, agent)
        .with_span(response_id);
    ev.set(attr::GEN_AI_PROVIDER, "openai");
    ev.set(attr::GEN_AI_RESPONSE_MODEL, str_field(state, "model"));
    ev.set(attr::GEN_AI_RESPONSE_ID, response_id);
    let cached = n("cached_input_tokens");
    ev.set(
        attr::GEN_AI_INPUT_TOKENS,
        n("input_tokens").saturating_sub(cached),
    );
    ev.set(attr::GEN_AI_OUTPUT_TOKENS, n("output_tokens"));
    ev.set(attr::CACHE_READ_TOKENS, cached);
    let written = n("cache_write_input_tokens");
    if written > 0 {
        ev.set(attr::CACHE_CREATION_TOKENS, written);
    }
    let reasoning = n("reasoning_output_tokens");
    if reasoning > 0 {
        ev.set(attr::REASONING_TOKENS, reasoning);
    }
    ev.set(attr::TURN_ID, str_field(payload, "turn_id"));
    // Subagent threads share the root session id but have their own thread id.
    let thread = str_field(payload, "thread_id");
    if thread.is_some() && thread != str_field(payload, "session_id") {
        ev.set(attr::SIDECHAIN, true);
    }
    Some(ev)
}

pub fn tool_category(name: &str) -> ToolCategory {
    match name {
        "Bash" | "shell" | "exec_command" | "local_shell" => ToolCategory::Shell,
        "apply_patch" | "Edit" | "Write" => ToolCategory::FileWrite,
        "update_plan" => ToolCategory::Planning,
        "spawn_agent" | "Agent" => ToolCategory::Subagent,
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
    let category = tool_category(name);
    ev.set(attr::TOOL_CATEGORY, category.as_str());

    let input = p.get("tool_input").unwrap_or(&Value::Null);
    match category {
        ToolCategory::Shell => ev.set(attr::SHELL_COMMAND, command_text(input)),
        ToolCategory::FileWrite => {
            let files = patch_files(input);
            let path = str_field(input, "file_path")
                .map(str::to_string)
                .or_else(|| files.first().cloned());
            ev.set(attr::FILE_PATH, path);
            ev.set(attr::FILE_OPERATION, "edit");
            if !files.is_empty() {
                ev.set(attr::PATCH_FILES, files.len());
            }
        }
        ToolCategory::Subagent => ev.set(attr::SUBAGENT_TYPE, str_field(input, "agent_type")),
        _ => {}
    }
    ev
}

/// Shell commands arrive as a string or, in some Codex versions, an argv array.
fn command_text(input: &Value) -> Option<String> {
    match input.get("command")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(argv) => Some(
            argv.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// Files named in an `apply_patch` input, in order.
fn patch_files(input: &Value) -> Vec<String> {
    let text = ["command", "input", "patch"]
        .iter()
        .find_map(|k| str_field(input, k))
        .unwrap_or("");
    text.lines()
        .filter_map(|line| {
            PATCH_FILE_HEADERS
                .iter()
                .find_map(|h| line.strip_prefix(h))
                .map(|p| p.trim().to_string())
        })
        .collect()
}

/// The exit code in a tool response, if it reports one. Codex function
/// outputs are sometimes JSON encoded as a string (`{"output", "metadata"}`).
fn exit_code(response: &Value) -> Option<i64> {
    let decoded;
    let response = match response {
        Value::String(s) => {
            decoded = serde_json::from_str::<Value>(s).ok()?;
            &decoded
        }
        other => other,
    };
    [
        response.get("exit_code"),
        response.get("exitCode"),
        response.get("metadata").and_then(|m| m.get("exit_code")),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_i64)
}

fn response_bytes(response: &Value) -> usize {
    match response {
        Value::String(s) => s.len(),
        other => other.to_string().len(),
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

    #[test]
    fn patch_files_are_extracted() {
        let input = json!({"command": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch\n"});
        assert_eq!(patch_files(&input), ["src/a.rs", "src/b.rs"]);
        assert!(patch_files(&json!({})).is_empty());
    }

    #[test]
    fn exit_codes_in_all_shapes() {
        assert_eq!(exit_code(&json!({"exit_code": 2})), Some(2));
        assert_eq!(exit_code(&json!({"metadata": {"exit_code": 0}})), Some(0));
        assert_eq!(
            exit_code(&json!(r#"{"output":"x","metadata":{"exit_code":1}}"#)),
            Some(1)
        );
        assert_eq!(exit_code(&json!("plain text output")), None);
        assert_eq!(exit_code(&Value::Null), None);
    }

    #[test]
    fn argv_commands_are_joined() {
        let input = json!({"command": ["bash", "-lc", "cargo test"]});
        assert_eq!(command_text(&input).as_deref(), Some("bash -lc cargo test"));
    }

    #[test]
    fn unknown_hooks_are_kept() {
        let envelope = HookEnvelope {
            id: Uuid::nil(),
            observed_at: Utc::now(),
            payload: json!({"session_id": "s", "hook_event_name": "PostCompact"}),
        };
        let n = normalize(&envelope);
        assert_eq!(
            n.events[0].kind,
            EventKind::Custom("codex.post_compact".into())
        );
    }
}
