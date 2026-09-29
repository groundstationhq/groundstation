//! Ground Station adapter for [OpenAI Codex](https://developers.openai.com/codex)
//! (CLI, IDE extension and desktop app, which share `~/.codex`).
//!
//! Two halves:
//!
//! * **Connecting** ([`settings`]): adds or removes the `groundstation hook
//!   codex` command in a Codex `hooks.json`. Codex runs non-managed hooks only
//!   after the user reviews and trusts them (`/hooks`).
//! * **Normalizing** ([`Codex`]): turns what Codex emits into
//!   `groundstation.telemetry.v0` events, one trajectory per Codex session:
//!   - **Hooks**: lifecycle, user turns, tool calls (shell, `apply_patch`,
//!     MCP and local function tools; hosted tools such as web search don't
//!     fire hooks), permission prompts and interrupts.
//!   - **The session rollout** (`transcript_path`,
//!     `~/.codex/sessions/…/rollout-*.jsonl`): one `token_usage_record` per
//!     model response, with the model taken from the preceding `turn_context`.
//!
//! Sample payloads live in `tests/fixtures/`. When Codex changes a payload,
//! add a fixture first.

use std::path::PathBuf;

use groundstation_schema::{Adapter, Event, HookEnvelope, Normalized};
use serde_json::Value;

mod hooks;
pub mod settings;

pub use hooks::tool_category;

/// Adapter name: `agent.name`, the CLI argument and the ingest URL segment.
pub const NAME: &str = "codex";

pub struct Codex;

impl Adapter for Codex {
    fn name(&self) -> &'static str {
        NAME
    }

    fn normalize(&self, envelope: &HookEnvelope) -> Normalized {
        hooks::normalize(envelope)
    }

    fn parse_transcript_line(
        &self,
        trajectory_id: &str,
        line: &str,
        state: &mut Value,
    ) -> Option<Event> {
        hooks::parse_transcript_line(trajectory_id, line, state)
    }

    fn transcript_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![home_dir().join(".codex")];
        if let Some(dir) = std::env::var_os("CODEX_HOME") {
            roots.push(PathBuf::from(dir));
        }
        roots
    }
}

/// Codex's configuration directory: `$CODEX_HOME` or `~/.codex`.
pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
