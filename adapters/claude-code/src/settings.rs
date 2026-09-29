//! Where and which hooks `groundstation connect claude-code` installs. The
//! editing itself is shared with other agents in `groundstation-hooks-json`.

use std::path::PathBuf;

use anyhow::Result;
use groundstation_hooks_json::{HookSet, HookSpec};

/// Hook events forwarded to gsd.
pub const HOOKS: HookSet = HookSet {
    adapter: crate::NAME,
    events: &[
        HookSpec::plain("SessionStart"),
        HookSpec::plain("SessionEnd"),
        HookSpec::plain("UserPromptSubmit"),
        HookSpec::tool("PreToolUse"),
        HookSpec::tool("PostToolUse"),
        HookSpec::tool("PostToolUseFailure"),
        HookSpec::plain("Stop"),
        HookSpec::plain("SubagentStart"),
        HookSpec::plain("SubagentStop"),
        HookSpec::plain("PreCompact"),
        HookSpec::plain("Notification"),
    ],
};

/// Which Claude Code settings file to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// ~/.claude/settings.json: every project on this machine.
    User,
    /// .claude/settings.json: this project, committed for the whole team.
    Project,
    /// .claude/settings.local.json: this project, just for you.
    Local,
}

pub fn settings_path(scope: Scope) -> Result<PathBuf> {
    Ok(match scope {
        Scope::User => crate::config_dir().join("settings.json"),
        Scope::Project => std::env::current_dir()?.join(".claude/settings.json"),
        Scope::Local => std::env::current_dir()?.join(".claude/settings.local.json"),
    })
}
