//! Extension payloads (pi events) → `groundstation.telemetry.v0` events.
//!
//! The extension forwards `{ event, event_id, at, session_id, cwd, version }`,
//! where `event` is a slimmed pi event (`{ type, ... }`) and `at` is when it
//! fired (Unix ms).

use chrono::DateTime;
use groundstation_schema::{
    Agent, ErrorCategory, Event, EventKind, HookEnvelope, Normalized, ToolCategory, attr,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::NAME as AGENT;

/// Namespace for event ids derived from extension event ids and pi response ids.
const EVENT_NS: Uuid = Uuid::from_u128(0x6773_2e70_692e_6576_656e_742e_7630_0001);

pub fn normalize(envelope: &HookEnvelope) -> Normalized {
    let p = &envelope.payload;
    let (Some(event), Some(session)) = (p.get("event"), str_field(p, "session_id")) else {
        return Normalized::default();
    };
    let Some(kind_name) = str_field(event, "type") else {
        return Normalized::default();
    };

    let timestamp = p
        .get("at")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or(envelope.observed_at);
    let id = match str_field(p, "event_id") {
        Some(event_id) => Uuid::new_v5(&EVENT_NS, event_id.as_bytes()),
        None => Uuid::new_v5(&envelope.id, AGENT.as_bytes()),
    };
    let new = |kind: EventKind| {
        let agent = Agent {
            name: AGENT.to_string(),
            version: str_field(p, "version").map(str::to_string),
        };
        let mut ev = Event::new(id, session, kind, timestamp, agent);
        ev.set(attr::SESSION_ID, session);
        ev.set(attr::CWD, str_field(p, "cwd"));
        ev
    };

    let ev = match kind_name {
        "session_start" => {
            let reason = str_field(event, "reason").unwrap_or("startup");
            let kind = match reason {
                "resume" | "reload" => EventKind::AgentResumed,
                _ => EventKind::AgentStarted,
            };
            let mut ev = new(kind);
            ev.set(attr::SESSION_SOURCE, reason);
            ev
        }
        "session_shutdown" => {
            let reason = str_field(event, "reason");
            // A reload keeps the same session running with fresh extensions.
            let kind = if reason == Some("reload") {
                EventKind::Custom("pi.session_reload".into())
            } else {
                EventKind::AgentCompleted
            };
            let mut ev = new(kind);
            ev.set(attr::END_REASON, reason);
            ev
        }
        "before_agent_start" => {
            let prompt = str_field(event, "prompt").unwrap_or("");
            let mut ev = new(EventKind::TurnUser);
            ev.set(attr::PROMPT_BYTES, prompt.len());
            ev.set(attr::PROMPT_TEXT, prompt);
            ev
        }
        "agent_end" => match str_field(event, "stopReason") {
            Some("aborted") => new(EventKind::TurnInterrupted),
            Some("error") => {
                let message = str_field(event, "errorMessage");
                let mut ev = new(EventKind::AgentFailed);
                ev.set(
                    attr::ERROR_CATEGORY,
                    category(classify(message.unwrap_or(""))),
                );
                ev.set(attr::ERROR_MESSAGE, message);
                ev
            }
            _ => new(EventKind::TurnCompleted),
        },
        "message_end" => model_event(new(EventKind::ModelCompleted), session, event),
        "tool_execution_start" => {
            let mut ev = tool_event(new(EventKind::ToolStarted), event);
            if let Some(args) = event.get("args") {
                ev.set(attr::TOOL_INPUT_BYTES, args.to_string().len());
                ev.set(attr::TOOL_INPUT, args.clone());
            }
            ev
        }
        "tool_execution_end" => {
            let exit = event
                .get("details")
                .and_then(|d| d.get("exitCode"))
                .and_then(Value::as_i64);
            let is_error = event
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let failed = is_error || exit.is_some_and(|c| c != 0);
            let kind = if failed {
                EventKind::ToolFailed
            } else {
                EventKind::ToolCompleted
            };
            let mut ev = tool_event(new(kind), event);
            ev.set(attr::SHELL_EXIT_CODE, exit);
            if let Some(output) = str_field(event, "output") {
                ev.set(attr::TOOL_OUTPUT_BYTES, output.len());
                ev.set(attr::TOOL_OUTPUT, output);
                if is_error {
                    ev.set(attr::ERROR_MESSAGE, output);
                }
            }
            if failed {
                ev.set(attr::ERROR_CATEGORY, category(ErrorCategory::ToolError));
            }
            ev
        }
        "session_compact" => {
            let mut ev = new(EventKind::ContextCompacted);
            ev.set(attr::COMPACTION_TRIGGER, str_field(event, "reason"));
            ev
        }
        other => new(EventKind::Custom(format!("pi.{other}"))),
    };
    Normalized {
        events: vec![ev],
        transcripts: Vec::new(),
    }
}

/// pi reports `input`, `cacheRead` and `cacheWrite` as disjoint counts and
/// `output` including reasoning, matching `gen_ai.usage.*` as used here.
fn model_event(mut ev: Event, session: &str, event: &Value) -> Event {
    let m = event.get("message").unwrap_or(&Value::Null);
    if let Some(response) = str_field(m, "responseId") {
        ev.id = Uuid::new_v5(&EVENT_NS, format!("{session}/{response}").as_bytes());
        ev.span_id = Some(response.to_string());
        ev.set(attr::GEN_AI_RESPONSE_ID, response);
    }
    ev.set(attr::GEN_AI_PROVIDER, str_field(m, "provider"));
    ev.set(attr::GEN_AI_REQUEST_MODEL, str_field(m, "model"));
    ev.set(
        attr::GEN_AI_RESPONSE_MODEL,
        str_field(m, "responseModel").or_else(|| str_field(m, "model")),
    );
    if let Some(usage) = m.get("usage") {
        let n = |k: &str| {
            usage
                .get(k)
                .and_then(Value::as_f64)
                .map(|f| f.max(0.0) as u64)
        };
        ev.set(attr::GEN_AI_INPUT_TOKENS, n("input"));
        ev.set(attr::GEN_AI_OUTPUT_TOKENS, n("output"));
        ev.set(attr::CACHE_READ_TOKENS, n("cacheRead"));
        ev.set(
            attr::CACHE_CREATION_TOKENS,
            n("cacheWrite").filter(|w| *w > 0),
        );
        ev.set(attr::REASONING_TOKENS, n("reasoning").filter(|r| *r > 0));
        ev.set(
            attr::COST_USD,
            usage
                .get("cost")
                .and_then(|c| c.get("total"))
                .and_then(Value::as_f64),
        );
    }
    ev.set(
        attr::DURATION_MS,
        event.get("durationMs").and_then(Value::as_i64),
    );
    let calls = m
        .get("toolCalls")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0);
    ev.set(attr::MODEL_TOOL_USES, calls);
    let stop = str_field(m, "stopReason");
    if let Some(stop) = stop {
        ev.set(attr::GEN_AI_FINISH_REASONS, json!([stop]));
    }
    if stop == Some("error") {
        let message = str_field(m, "errorMessage");
        ev.set(
            attr::ERROR_CATEGORY,
            category(classify(message.unwrap_or(""))),
        );
        ev.set(attr::ERROR_MESSAGE, message);
    }
    ev
}

/// pi's built-in tools; anything else comes from an extension.
pub fn tool_category(name: &str) -> ToolCategory {
    match name {
        "bash" => ToolCategory::Shell,
        "read" => ToolCategory::FileRead,
        "edit" | "write" => ToolCategory::FileWrite,
        "grep" | "find" | "ls" => ToolCategory::Search,
        n if n.contains("browser") || n.contains("playwright") || n.contains("chrome") => {
            ToolCategory::Browser
        }
        n if n.starts_with("mcp") => ToolCategory::Mcp,
        _ => ToolCategory::Custom,
    }
}

fn tool_event(mut ev: Event, event: &Value) -> Event {
    let name = str_field(event, "toolName").unwrap_or("unknown");
    if let Some(id) = str_field(event, "toolCallId") {
        ev.span_id = Some(id.to_string());
        ev.set(attr::GEN_AI_TOOL_CALL_ID, id);
    }
    ev.set(attr::GEN_AI_TOOL_NAME, name);
    let category = tool_category(name);
    ev.set(attr::TOOL_CATEGORY, category.as_str());

    let args = event.get("args").unwrap_or(&Value::Null);
    match name {
        "bash" => ev.set(attr::SHELL_COMMAND, str_field(args, "command")),
        "read" | "edit" | "write" => {
            ev.set(attr::FILE_PATH, str_field(args, "path"));
            ev.set(attr::FILE_OPERATION, name);
        }
        "grep" | "find" | "ls" => {
            ev.set(attr::SEARCH_PATTERN, str_field(args, "pattern"));
            ev.set(attr::FILE_PATH, str_field(args, "path"));
        }
        _ => {}
    }
    ev
}

fn classify(message: &str) -> ErrorCategory {
    let m = message.to_ascii_lowercase();
    if m.contains("rate limit") || m.contains("429") {
        ErrorCategory::RateLimit
    } else if m.contains("unauthorized") || m.contains("api key") || m.contains("401") {
        ErrorCategory::AuthenticationError
    } else if m.contains("context") || m.contains("too long") {
        ErrorCategory::ContextOverflow
    } else if m.contains("timeout") || m.contains("timed out") {
        ErrorCategory::Timeout
    } else {
        ErrorCategory::ProviderError
    }
}

fn category(c: ErrorCategory) -> Value {
    serde_json::to_value(c).expect("enum serializes")
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}
