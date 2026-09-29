//! Plugin payloads (OpenCode v2 events) → `groundstation.telemetry.v0` events.
//!
//! The plugin forwards `{ event, tool?, step?, root_session_id?, directory?, version? }`,
//! where `event` is OpenCode's own event (`{ id, created, type, location, data }`)
//! and the other fields carry what the plugin remembered from earlier events.

use chrono::DateTime;
use groundstation_schema::{
    Agent, ErrorCategory, Event, EventKind, HookEnvelope, Normalized, ToolCategory, attr,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::NAME as AGENT;

/// Namespace for event ids derived from OpenCode event ids.
const EVENT_NS: Uuid = Uuid::from_u128(0x6773_2e6f_7065_6e63_6f64_652e_6576_7430);

pub fn normalize(envelope: &HookEnvelope) -> Normalized {
    let p = &envelope.payload;
    let Some(event) = p.get("event") else {
        return Normalized::default();
    };
    let data = event.get("data").unwrap_or(&Value::Null);
    let (Some(kind_name), Some(session)) = (str_field(event, "type"), str_field(data, "sessionID"))
    else {
        return Normalized::default();
    };
    let trajectory = str_field(p, "root_session_id").unwrap_or(session);
    let child = trajectory != session;

    let timestamp = event
        .get("created")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or(envelope.observed_at);
    let id = match str_field(event, "id") {
        Some(event_id) => Uuid::new_v5(&EVENT_NS, event_id.as_bytes()),
        None => Uuid::new_v5(&envelope.id, AGENT.as_bytes()),
    };
    let new = |kind: EventKind| {
        let agent = Agent {
            name: AGENT.to_string(),
            version: str_field(p, "version").map(str::to_string),
        };
        let mut ev = Event::new(id, trajectory, kind, timestamp, agent);
        ev.set(attr::SESSION_ID, session);
        let cwd = event
            .get("location")
            .and_then(|l| str_field(l, "directory"))
            .or_else(|| str_field(p, "directory"));
        ev.set(attr::CWD, cwd);
        if child {
            ev.set(attr::SIDECHAIN, true);
        }
        ev
    };

    let ev = match kind_name {
        "session.created" if child => {
            let mut ev = new(EventKind::SubagentStarted).with_span(session);
            ev.set(attr::SUBAGENT_ID, session);
            ev.set(attr::SUBAGENT_TYPE, str_field(data, "agent"));
            ev
        }
        "session.created" => {
            let mut ev = new(EventKind::AgentStarted);
            if let Some(version) = str_field(data, "version") {
                ev.agent.version = Some(version.to_string());
            }
            ev.set(
                attr::GEN_AI_REQUEST_MODEL,
                data.get("model").and_then(model_id),
            );
            ev
        }
        "session.deleted" if child => new(EventKind::Custom("opencode.subagent.deleted".into())),
        "session.deleted" => {
            let mut ev = new(EventKind::AgentCompleted);
            ev.set(attr::END_REASON, "deleted");
            ev
        }
        "session.inbox.enqueued" => {
            let text = data
                .get("item")
                .and_then(|i| i.get("payload"))
                .and_then(|p| str_field(p, "text"))
                .unwrap_or("");
            // A subagent's prompt is written by the parent agent, not the user.
            let kind = if child {
                EventKind::Custom("opencode.subagent.prompt".into())
            } else {
                EventKind::TurnUser
            };
            let mut ev = new(kind);
            ev.set(attr::PROMPT_BYTES, text.len());
            ev.set(attr::PROMPT_TEXT, text);
            ev
        }
        "session.execution.succeeded" if child => {
            new(EventKind::SubagentCompleted).with_span(session)
        }
        "session.execution.succeeded" => new(EventKind::TurnCompleted),
        "session.execution.failed" => {
            let kind = if child {
                EventKind::SubagentCompleted
            } else {
                EventKind::AgentFailed
            };
            let mut ev = new(kind);
            if child {
                ev.span_id = Some(session.to_string());
            }
            set_error(&mut ev, data.get("error"), classify_execution_error);
            ev
        }
        "session.execution.interrupted" => {
            let mut ev = if child {
                new(EventKind::SubagentCompleted).with_span(session)
            } else {
                new(EventKind::TurnInterrupted)
            };
            ev.set("gs.interrupt.reason", str_field(data, "reason"));
            ev
        }
        "session.step.ended" | "session.step.failed" => {
            let mut ev = new(EventKind::ModelCompleted);
            if let Some(message) = str_field(data, "assistantMessageID") {
                ev.span_id = Some(message.to_string());
                ev.set(attr::GEN_AI_RESPONSE_ID, message);
            }
            let step = p.get("step").unwrap_or(&Value::Null);
            if let Some(model) = step.get("model") {
                ev.set(attr::GEN_AI_RESPONSE_MODEL, model_id(model));
                ev.set(attr::GEN_AI_PROVIDER, str_field(model, "providerID"));
            }
            if let Some(started) = step.get("started").and_then(Value::as_i64) {
                ev.set(
                    attr::DURATION_MS,
                    (timestamp.timestamp_millis() - started).max(0),
                );
            }
            usage(&mut ev, data);
            if let Some(finish) = str_field(data, "finish") {
                ev.set(attr::GEN_AI_FINISH_REASONS, json!([finish]));
            }
            if kind_name == "session.step.failed" {
                set_error(&mut ev, data.get("error"), classify_execution_error);
            }
            ev
        }
        "session.compaction.started" => {
            let mut ev = new(EventKind::ContextCompacted);
            ev.set(attr::COMPACTION_TRIGGER, str_field(data, "reason"));
            ev
        }
        "session.compaction.ended" => {
            // The summarizing call is a model call too, and it costs money.
            let mut ev = new(EventKind::ModelCompleted);
            ev.set(attr::MODEL_PURPOSE, "compaction");
            if let Some(model) = data.get("model") {
                ev.set(attr::GEN_AI_RESPONSE_MODEL, model_id(model));
                ev.set(attr::GEN_AI_PROVIDER, str_field(model, "providerID"));
            }
            usage(&mut ev, data);
            ev
        }
        "session.tool.called" => {
            let mut ev = tool_event(new(EventKind::ToolStarted), p, data);
            if let Some(input) = data.get("input") {
                ev.set(attr::TOOL_INPUT_BYTES, input.to_string().len());
                ev.set(attr::TOOL_INPUT, input.clone());
            }
            ev
        }
        "session.tool.success" | "session.tool.failed" => {
            let exit = data.get("metadata").and_then(exit_code);
            let failed = kind_name == "session.tool.failed" || exit.is_some_and(|c| c != 0);
            let kind = if failed {
                EventKind::ToolFailed
            } else {
                EventKind::ToolCompleted
            };
            let mut ev = tool_event(new(kind), p, data);
            ev.set(attr::SHELL_EXIT_CODE, exit);
            if let Some(output) = content_text(data.get("content")) {
                ev.set(attr::TOOL_OUTPUT_BYTES, output.len());
                ev.set(attr::TOOL_OUTPUT, output);
            }
            if kind_name == "session.tool.failed" {
                set_error(&mut ev, data.get("error"), |_| ErrorCategory::ToolError);
            } else if failed {
                ev.set(attr::ERROR_CATEGORY, category(ErrorCategory::ToolError));
            }
            ev
        }
        "permission.asked" => {
            let mut ev = new(EventKind::AgentNotification);
            ev.set(attr::NOTIFICATION_TYPE, "permission_request");
            ev.set(attr::GEN_AI_TOOL_NAME, str_field(data, "action"));
            ev.set(attr::NOTIFICATION_MESSAGE, str_field(data, "message"));
            ev
        }
        other => new(EventKind::Custom(format!("opencode.{other}"))),
    };

    Normalized {
        events: vec![ev],
        transcripts: Vec::new(),
    }
}

/// OpenCode tool names (lowercase in v2).
pub fn tool_category(name: &str) -> ToolCategory {
    match name {
        "bash" | "shell" => ToolCategory::Shell,
        "read" => ToolCategory::FileRead,
        "write" | "edit" | "multiedit" | "patch" | "apply_patch" => ToolCategory::FileWrite,
        "grep" | "glob" | "list" | "ls" | "codesearch" => ToolCategory::Search,
        "webfetch" | "websearch" => ToolCategory::Http,
        "task" => ToolCategory::Subagent,
        "todowrite" | "todoread" | "plan" => ToolCategory::Planning,
        n if n.contains("browser") || n.contains("playwright") || n.contains("chrome") => {
            ToolCategory::Browser
        }
        _ => ToolCategory::Custom,
    }
}

fn tool_event(mut ev: Event, p: &Value, data: &Value) -> Event {
    let name = str_field(p, "tool").unwrap_or("unknown");
    if let Some(id) = str_field(data, "id") {
        ev.span_id = Some(id.to_string());
        ev.set(attr::GEN_AI_TOOL_CALL_ID, id);
    }
    ev.set(attr::GEN_AI_TOOL_NAME, name);
    let category = tool_category(name);
    ev.set(attr::TOOL_CATEGORY, category.as_str());

    let input = data.get("input").unwrap_or(&Value::Null);
    let field = |k: &str| str_field(input, k);
    match category {
        ToolCategory::Shell => ev.set(attr::SHELL_COMMAND, field("command")),
        ToolCategory::FileRead | ToolCategory::FileWrite => {
            ev.set(attr::FILE_PATH, field("filePath").or_else(|| field("path")));
            let op = if category == ToolCategory::FileRead {
                "read"
            } else {
                "edit"
            };
            ev.set(
                attr::FILE_OPERATION,
                if name == "write" { "write" } else { op },
            );
        }
        ToolCategory::Search => {
            ev.set(
                attr::SEARCH_PATTERN,
                field("pattern").or_else(|| field("query")),
            );
            ev.set(attr::FILE_PATH, field("path"));
        }
        ToolCategory::Http => {
            ev.set(attr::HTTP_URL, field("url"));
            ev.set(attr::SEARCH_PATTERN, field("query"));
        }
        ToolCategory::Subagent => ev.set(
            attr::SUBAGENT_TYPE,
            field("subagent_type").or_else(|| field("agent")),
        ),
        _ => {}
    }
    ev
}

/// OpenCode reports input, output, reasoning and cache tokens as disjoint
/// counts. `gen_ai.usage.output_tokens` includes reasoning, as for other agents.
fn usage(ev: &mut Event, data: &Value) {
    ev.set(attr::COST_USD, data.get("cost").and_then(Value::as_f64));
    let Some(tokens) = data.get("tokens") else {
        return;
    };
    let n = |v: Option<&Value>| {
        v.and_then(Value::as_f64)
            .map(|f| f.max(0.0) as u64)
            .unwrap_or(0)
    };
    let cache = tokens.get("cache").unwrap_or(&Value::Null);
    let reasoning = n(tokens.get("reasoning"));
    ev.set(attr::GEN_AI_INPUT_TOKENS, n(tokens.get("input")));
    ev.set(
        attr::GEN_AI_OUTPUT_TOKENS,
        n(tokens.get("output")) + reasoning,
    );
    ev.set(attr::CACHE_READ_TOKENS, n(cache.get("read")));
    let written = n(cache.get("write"));
    if written > 0 {
        ev.set(attr::CACHE_CREATION_TOKENS, written);
    }
    if reasoning > 0 {
        ev.set(attr::REASONING_TOKENS, reasoning);
    }
}

fn set_error(ev: &mut Event, error: Option<&Value>, classify: fn(&Value) -> ErrorCategory) {
    let Some(error) = error else {
        return;
    };
    ev.set(attr::ERROR_CATEGORY, category(classify(error)));
    ev.set(attr::ERROR_MESSAGE, str_field(error, "message"));
    ev.set("gs.error.type", str_field(error, "type"));
}

fn classify_execution_error(error: &Value) -> ErrorCategory {
    let status = error.get("status").and_then(Value::as_i64);
    let text = format!(
        "{} {}",
        str_field(error, "type").unwrap_or(""),
        str_field(error, "message").unwrap_or("")
    )
    .to_ascii_lowercase();
    match status {
        Some(429) => ErrorCategory::RateLimit,
        Some(401 | 403) => ErrorCategory::AuthenticationError,
        _ if text.contains("rate") && text.contains("limit") => ErrorCategory::RateLimit,
        _ if text.contains("context") || text.contains("too long") => {
            ErrorCategory::ContextOverflow
        }
        _ if text.contains("abort") || text.contains("cancel") => ErrorCategory::UserCancelled,
        _ if text.contains("timeout") || text.contains("timed out") => ErrorCategory::Timeout,
        _ => ErrorCategory::ProviderError,
    }
}

fn model_id(model: &Value) -> Option<String> {
    match model {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => str_field(model, "id").map(str::to_string),
        _ => None,
    }
}

fn content_text(content: Option<&Value>) -> Option<String> {
    let items = content?.as_array()?;
    let text: Vec<&str> = items.iter().filter_map(|c| str_field(c, "text")).collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

fn exit_code(metadata: &Value) -> Option<i64> {
    ["exit", "exitCode", "exit_code"]
        .iter()
        .find_map(|k| metadata.get(*k).and_then(Value::as_i64))
}

fn category(c: ErrorCategory) -> Value {
    serde_json::to_value(c).expect("enum serializes")
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}
