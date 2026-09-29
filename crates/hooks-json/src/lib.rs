//! Installs and removes Ground Station hooks in agent hook configs that use
//! the `hooks.json` shape shared by Claude Code (`settings.json`) and Codex
//! (`hooks.json`):
//!
//! ```json
//! { "hooks": { "PreToolUse": [ { "matcher": "*", "hooks": [ { "type": "command", "command": "…" } ] } ] } }
//! ```
//!
//! Edits are surgical: other settings and other hooks are left untouched, and
//! key order is preserved. Ground Station's hooks are recognized by their
//! command (`…groundstation… hook <adapter>`), so reinstalling replaces them
//! and uninstalling removes exactly them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// How long an agent waits for a hook by default. The hook itself gives up
/// on the daemon well before this and spools instead.
pub const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// One hook event to register.
#[derive(Debug, Clone, Copy)]
pub struct HookSpec {
    pub event: &'static str,
    /// Add `"matcher": "*"` (for events filtered by tool name or similar).
    pub matcher: bool,
    pub timeout_secs: u64,
}

impl HookSpec {
    /// An event that isn't filtered by a matcher.
    pub const fn plain(event: &'static str) -> Self {
        Self {
            event,
            matcher: false,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }

    /// An event filtered by tool name; registered for every tool.
    pub const fn tool(event: &'static str) -> Self {
        Self {
            event,
            matcher: true,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }

    pub const fn timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

/// The hooks one adapter installs.
#[derive(Debug, Clone, Copy)]
pub struct HookSet {
    /// Adapter name, as in `groundstation hook <adapter>`.
    pub adapter: &'static str,
    pub events: &'static [HookSpec],
}

impl HookSet {
    /// The command the agent runs for every hook, given the absolute path of
    /// the `groundstation` binary.
    pub fn command(&self, groundstation: &Path) -> String {
        format!(
            "{} hook {}",
            shell_quote(&groundstation.to_string_lossy()),
            self.adapter
        )
    }

    /// Adds (or refreshes) a hook running `command` for every event.
    pub fn install(&self, settings: &mut Value, command: &str) -> Result<()> {
        self.uninstall(settings);
        let root = settings
            .as_object_mut()
            .context("config must be a JSON object")?;
        let hooks = root.entry("hooks").or_insert_with(|| json!({}));
        let hooks = hooks
            .as_object_mut()
            .context("\"hooks\" must be a JSON object")?;
        for spec in self.events {
            let groups = hooks.entry(spec.event).or_insert_with(|| json!([]));
            let groups = groups
                .as_array_mut()
                .with_context(|| format!("hooks.{} must be an array", spec.event))?;
            let mut group = Map::new();
            if spec.matcher {
                group.insert("matcher".into(), json!("*"));
            }
            group.insert(
                "hooks".into(),
                json!([{"type": "command", "command": command, "timeout": spec.timeout_secs}]),
            );
            groups.push(Value::Object(group));
        }
        Ok(())
    }

    /// Removes this adapter's hooks, pruning groups and events left empty.
    /// Returns how many hooks were removed.
    pub fn uninstall(&self, settings: &mut Value) -> usize {
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
                    list.retain(|h| !self.is_ours(h));
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

    fn is_ours(&self, hook: &Value) -> bool {
        let suffix = format!("hook {}", self.adapter);
        hook.get("command")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("groundstation") && c.trim_end().ends_with(&suffix))
    }
}

/// Reads a JSON config; a missing or empty file is an empty object.
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
/// Returns the backup's path, if there was a previous file.
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

    const HOOKS: HookSet = HookSet {
        adapter: "agent-a",
        events: &[
            HookSpec::plain("SessionStart"),
            HookSpec::tool("PreToolUse"),
            HookSpec::plain("SessionEnd").timeout(3),
        ],
    };
    const OTHER: HookSet = HookSet {
        adapter: "agent-b",
        events: &[HookSpec::plain("Stop")],
    };
    const CMD: &str = "/usr/local/bin/groundstation hook agent-a";

    fn count(settings: &Value) -> usize {
        settings["hooks"]
            .as_object()
            .unwrap()
            .values()
            .map(|g| g.as_array().unwrap().len())
            .sum()
    }

    #[test]
    fn install_is_idempotent_and_preserves_other_settings() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "./lint.sh"}]}]
            }
        });
        HOOKS.install(&mut settings, CMD).unwrap();
        HOOKS.install(&mut settings, CMD).unwrap();

        assert_eq!(settings["model"], "opus");
        let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "./lint.sh");
        assert_eq!(pre[1]["matcher"], "*");
        assert!(
            settings["hooks"]["SessionStart"][0]
                .get("matcher")
                .is_none()
        );
        assert_eq!(settings["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
        assert_eq!(count(&settings), HOOKS.events.len() + 1);
        // Key order is preserved: "model" stays first.
        assert_eq!(
            settings.as_object().unwrap().keys().next().unwrap(),
            "model"
        );
    }

    #[test]
    fn uninstall_leaves_foreign_hooks_and_other_adapters() {
        let mut settings = json!({
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "./lint.sh"}]}]
            }
        });
        let original = settings.clone();
        HOOKS.install(&mut settings, CMD).unwrap();
        OTHER
            .install(
                &mut settings,
                &OTHER.command(Path::new("/bin/groundstation")),
            )
            .unwrap();
        assert_eq!(HOOKS.uninstall(&mut settings), HOOKS.events.len());
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/bin/groundstation hook agent-b"
        );
        assert_eq!(OTHER.uninstall(&mut settings), 1);
        assert_eq!(settings, original);

        let mut only_ours = json!({"theme": "dark"});
        HOOKS.install(&mut only_ours, CMD).unwrap();
        HOOKS.uninstall(&mut only_ours);
        assert_eq!(only_ours, json!({"theme": "dark"}));
    }

    #[test]
    fn commands_quote_paths() {
        assert_eq!(
            HOOKS.command(Path::new("/opt/bin/groundstation")),
            "/opt/bin/groundstation hook agent-a"
        );
        let spaced = HOOKS.command(Path::new("/Users/Jo Doe/bin/groundstation"));
        assert_eq!(spaced, "'/Users/Jo Doe/bin/groundstation' hook agent-a");
        assert!(HOOKS.is_ours(&json!({ "command": spaced })));
    }

    #[test]
    fn read_write_round_trip_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/hooks.json");
        assert_eq!(read(&path).unwrap(), json!({}));
        assert!(write(&path, &json!({"a": 1})).unwrap().is_none());
        let backup = write(&path, &json!({"a": 2})).unwrap().unwrap();
        assert_eq!(read(&backup).unwrap(), json!({"a": 1}));
        assert_eq!(read(&path).unwrap(), json!({"a": 2}));
        std::fs::write(&path, "[1]").unwrap();
        assert!(read(&path).is_err());
    }
}
