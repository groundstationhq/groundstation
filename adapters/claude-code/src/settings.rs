//! Installs and removes Ground Station hooks in Claude Code `settings.json`.
//! Edits are surgical: other settings and other hooks are left untouched, and
//! key order is preserved.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// Hook events forwarded to gsd, and whether each takes a tool matcher.
pub const HOOK_EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("SessionEnd", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("PostToolUseFailure", true),
    ("Stop", false),
    ("SubagentStart", false),
    ("SubagentStop", false),
    ("PreCompact", false),
    ("Notification", false),
];

/// Seconds Claude Code waits for the hook. The hook itself gives up on the
/// daemon well before this and spools instead.
const HOOK_TIMEOUT_SECS: u64 = 10;

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

pub fn read(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => {
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            if !v.is_object() {
                bail!("{} is not a JSON object", path.display());
            }
            Ok(v)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes `settings`, keeping a copy of the previous file next to it.
pub fn write(path: &Path, settings: &Value) -> Result<Option<PathBuf>> {
    let backup = if path.exists() {
        let backup = path.with_extension("json.groundstation-backup");
        std::fs::copy(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
        Some(backup)
    } else {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        None
    };
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(backup)
}

/// The command Claude Code runs for every hook, given the absolute path of
/// the `groundstation` binary. [`uninstall`] recognizes exactly this shape.
pub fn hook_command(groundstation: &Path) -> String {
    format!(
        "{} hook {}",
        shell_quote(&groundstation.to_string_lossy()),
        crate::NAME
    )
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains("groundstation") && c.trim_end().ends_with("hook claude-code"))
}

/// Adds (or refreshes) a Ground Station hook for every event in [`HOOK_EVENTS`].
pub fn install(settings: &mut Value, command: &str) -> Result<()> {
    uninstall(settings);
    let root = settings
        .as_object_mut()
        .context("settings must be a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .context("\"hooks\" must be a JSON object")?;
    for (event, takes_matcher) in HOOK_EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        let groups = groups
            .as_array_mut()
            .with_context(|| format!("hooks.{event} must be an array"))?;
        let mut group = Map::new();
        if *takes_matcher {
            group.insert("matcher".into(), json!("*"));
        }
        group.insert(
            "hooks".into(),
            json!([{"type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS}]),
        );
        groups.push(Value::Object(group));
    }
    Ok(())
}

/// Removes every Ground Station hook, pruning groups and events left empty.
/// Returns how many hooks were removed.
pub fn uninstall(settings: &mut Value) -> usize {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return 0;
    };
    let mut removed = 0;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|h| !is_ours(h));
                removed += before - list.len();
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    removed
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/usr/local/bin/groundstation hook claude-code";

    #[test]
    fn install_is_idempotent_and_preserves_other_settings() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "./lint.sh"}]}]
            }
        });
        install(&mut settings, CMD).unwrap();
        install(&mut settings, CMD).unwrap();

        assert_eq!(settings["model"], "opus");
        let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "./lint.sh");
        assert_eq!(pre[1]["matcher"], "*");
        assert!(settings["hooks"]["Stop"][0].get("matcher").is_none());
        let ours: usize = settings["hooks"]
            .as_object()
            .unwrap()
            .values()
            .map(|g| g.as_array().unwrap().len())
            .sum();
        assert_eq!(ours, HOOK_EVENTS.len() + 1);
        // Key order is preserved: "model" stays first.
        assert_eq!(
            settings.as_object().unwrap().keys().next().unwrap(),
            "model"
        );
    }

    #[test]
    fn uninstall_leaves_foreign_hooks() {
        let mut settings = json!({
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "./lint.sh"}]}]
            }
        });
        let original = settings.clone();
        install(&mut settings, CMD).unwrap();
        assert_eq!(uninstall(&mut settings), HOOK_EVENTS.len());
        assert_eq!(settings, original);

        let mut only_ours = json!({"theme": "dark"});
        install(&mut only_ours, CMD).unwrap();
        uninstall(&mut only_ours);
        assert_eq!(only_ours, json!({"theme": "dark"}));
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(shell_quote("/a/b"), "/a/b");
        assert_eq!(
            hook_command(Path::new("/opt/bin/groundstation")),
            "/opt/bin/groundstation hook claude-code"
        );
        assert!(is_ours(
            &json!({"command": hook_command(Path::new("/x y/groundstation"))})
        ));
        assert_eq!(
            shell_quote("/Users/Jo Doe/bin/groundstation"),
            "'/Users/Jo Doe/bin/groundstation'"
        );
    }
}
