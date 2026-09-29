//! Where and which hooks `groundstation connect codex` installs.

use std::path::PathBuf;

use anyhow::Result;
use groundstation_hooks_json::{HookSet, HookSpec};

/// Hook events forwarded to gsd. `SessionEnd` and `Interrupt` are capped at
/// 3 seconds by Codex.
pub const HOOKS: HookSet = HookSet {
    adapter: crate::NAME,
    events: &[
        HookSpec::plain("SessionStart"),
        HookSpec::plain("SessionEnd").timeout(3),
        HookSpec::plain("UserPromptSubmit"),
        HookSpec::tool("PreToolUse"),
        HookSpec::tool("PostToolUse"),
        HookSpec::tool("PermissionRequest"),
        HookSpec::plain("Stop"),
        HookSpec::plain("SubagentStart"),
        HookSpec::plain("SubagentStop"),
        HookSpec::plain("PreCompact"),
        HookSpec::plain("Interrupt").timeout(3),
    ],
};

/// Which Codex `hooks.json` to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// ~/.codex/hooks.json (or `$CODEX_HOME`): every project on this machine.
    User,
    /// .codex/hooks.json: this project. Codex loads it only once the
    /// project's `.codex/` layer is trusted.
    Project,
}

pub fn hooks_path(scope: Scope) -> Result<PathBuf> {
    Ok(match scope {
        Scope::User => crate::codex_home().join("hooks.json"),
        Scope::Project => std::env::current_dir()?.join(".codex/hooks.json"),
    })
}
