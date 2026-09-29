//! `groundstation.telemetry.v0`: the wire format shared by agents, adapters,
//! the local daemon (`gsd`) and the Ground Station backend.
//!
//! A trajectory is one agent execution. Everything that happens inside it is an
//! [`Event`]. Events that have a duration come in pairs (`*.started` /
//! `*.completed` or `*.failed`) sharing a `span_id`, which maps directly onto
//! OpenTelemetry spans.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use uuid::Uuid;

pub mod attr;

/// Schema identifier carried on every batch.
pub const SCHEMA: &str = "groundstation.telemetry.v0";

/// A batch of events, as accepted by `POST /v1/events` on both `gsd` and the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub schema: String,
    pub events: Vec<Event>,
}

impl Batch {
    pub fn new(events: Vec<Event>) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            events,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Unique and stable: re-delivering the same event must reuse the same id.
    pub id: Uuid,
    pub trajectory_id: String,
    pub kind: EventKind,
    pub timestamp: DateTime<Utc>,
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// `gen_ai.*` where an OpenTelemetry convention exists, `gs.*` otherwise. See [`attr`].
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub attributes: Map<String, Value>,
}

impl Event {
    pub fn new(
        id: Uuid,
        trajectory_id: impl Into<String>,
        kind: EventKind,
        timestamp: DateTime<Utc>,
        agent: Agent,
    ) -> Self {
        Self {
            id,
            trajectory_id: trajectory_id.into(),
            kind,
            timestamp,
            agent,
            span_id: None,
            parent_span_id: None,
            attributes: Map::new(),
        }
    }

    pub fn with_span(mut self, span_id: impl Into<String>) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    /// Sets an attribute, skipping `null` so optional fields stay absent.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        let value = value.into();
        if !value.is_null() {
            self.attributes.insert(key.to_string(), value);
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.attributes.get(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Adapter name, e.g. `claude-code`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl Agent {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
        }
    }
}

macro_rules! event_kinds {
    ($($variant:ident => $name:literal),* $(,)?) => {
        /// What happened. Unknown kinds round-trip through [`EventKind::Custom`] so
        /// newer producers never break older daemons.
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum EventKind {
            $($variant,)*
            Custom(String),
        }

        impl EventKind {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $name,)*
                    Self::Custom(s) => s,
                }
            }
        }

        impl FromStr for EventKind {
            type Err = std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(match s {
                    $($name => Self::$variant,)*
                    other => Self::Custom(other.to_string()),
                })
            }
        }
    };
}

event_kinds! {
    AgentStarted => "agent.started",
    AgentResumed => "agent.resumed",
    AgentPaused => "agent.paused",
    AgentCompleted => "agent.completed",
    AgentFailed => "agent.failed",
    AgentCancelled => "agent.cancelled",
    AgentNotification => "agent.notification",
    TurnUser => "turn.user",
    TurnCompleted => "turn.completed",
    ModelStarted => "model.started",
    ModelCompleted => "model.completed",
    ToolStarted => "tool.started",
    ToolCompleted => "tool.completed",
    ToolFailed => "tool.failed",
    SubagentStarted => "subagent.started",
    SubagentCompleted => "subagent.completed",
    ContextCompacted => "context.compacted",
}

impl EventKind {
    /// Kinds that close a span opened by a `*.started` event.
    pub fn ends_span(&self) -> bool {
        matches!(
            self,
            Self::ToolCompleted | Self::ToolFailed | Self::ModelCompleted | Self::SubagentCompleted
        )
    }

    /// Kinds that end the trajectory.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::AgentCompleted | Self::AgentFailed | Self::AgentCancelled
        )
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(s.parse().unwrap_or_else(|never| match never {}))
    }
}

/// Coarse classification of tools, so dashboards can compare agents whose tool
/// names differ (`Bash` vs `shell` vs `exec_command`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Shell,
    FileRead,
    FileWrite,
    Search,
    Http,
    Browser,
    Subagent,
    Mcp,
    Planning,
    Custom,
}

impl ToolCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::FileRead => "file_read",
            Self::FileWrite => "file_write",
            Self::Search => "search",
            Self::Http => "http",
            Self::Browser => "browser",
            Self::Subagent => "subagent",
            Self::Mcp => "mcp",
            Self::Planning => "planning",
            Self::Custom => "custom",
        }
    }
}

/// Normalized error categories (`gs.error.category`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    ModelError,
    ToolError,
    Timeout,
    RateLimit,
    AuthenticationError,
    ContextOverflow,
    AgentCrash,
    PermissionError,
    LoopDetected,
    UserCancelled,
    ProviderError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_including_unknown() {
        for s in ["tool.started", "agent.completed", "vendor.something_new"] {
            let kind: EventKind = serde_json::from_value(Value::from(s)).unwrap();
            assert_eq!(serde_json::to_value(&kind).unwrap(), Value::from(s));
        }
        assert_eq!(
            "tool.started".parse::<EventKind>().unwrap(),
            EventKind::ToolStarted
        );
    }

    #[test]
    fn event_serializes_without_empty_fields() {
        let ev = Event::new(
            Uuid::nil(),
            "t1",
            EventKind::AgentStarted,
            DateTime::from_timestamp(0, 0).unwrap(),
            Agent::named("claude-code"),
        );
        let json = serde_json::to_value(&ev).unwrap();
        assert!(json.get("span_id").is_none());
        assert!(json.get("attributes").is_none());
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn set_skips_null() {
        let mut ev = Event::new(
            Uuid::nil(),
            "t1",
            EventKind::ToolStarted,
            Utc::now(),
            Agent::named("x"),
        );
        ev.set("a", Value::Null);
        ev.set("b", 1);
        assert!(ev.get("a").is_none());
        assert_eq!(ev.get("b"), Some(&Value::from(1)));
    }
}
