//! Ground Station adapter for [Claude Code](https://claude.com/claude-code).
//!
//! Two halves:
//!
//! * **Connecting** ([`settings`]): adds or removes the `groundstation hook
//!   claude-code` command in a Claude Code `settings.json`, leaving every
//!   other setting and hook untouched.
//! * **Normalizing** ([`ClaudeCode`]): turns what Claude Code emits into
//!   `groundstation.telemetry.v0` events. Two sources are combined into one
//!   trajectory per Claude Code session:
//!   - **Hooks**: lifecycle, user turns and tool calls, stamped the moment
//!     they fire.
//!   - **The session transcript** (`transcript_path` in every hook payload):
//!     one line per model response, carrying the model name and token usage
//!     that hooks don't expose.
//!
//! Sample payloads live in `tests/fixtures/`. When Claude Code changes a
//! payload, add a fixture first.

use std::path::PathBuf;

use groundstation_schema::{Adapter, Event, HookEnvelope, Normalized};
use serde_json::Value;

mod hooks;
pub mod settings;

pub use hooks::tool_category;

/// Adapter name: `agent.name`, the CLI argument and the ingest URL segment.
pub const NAME: &str = "claude-code";

pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        NAME
    }

    fn normalize(&self, envelope: &HookEnvelope) -> Normalized {
        hooks::normalize(envelope)
    }

    /// Claude Code lines are self-contained, so no state is kept.
    fn parse_transcript_line(
        &self,
        trajectory_id: &str,
        line: &str,
        _: &mut Value,
    ) -> Option<Event> {
        hooks::parse_transcript_line(trajectory_id, line)
    }

    fn transcript_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![home_dir().join(".claude")];
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            roots.push(PathBuf::from(dir));
        }
        roots
    }
}

/// Claude Code's user configuration directory: `$CLAUDE_CONFIG_DIR` or `~/.claude`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".claude"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
