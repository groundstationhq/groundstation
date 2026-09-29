//! Installs and removes the Ground Station plugin file for OpenCode v2.
//!
//! OpenCode has no command hooks; it loads JS/TS plugins from
//! `~/.config/opencode/plugins/` and `<project>/.opencode/plugins/`. The
//! plugin is generated from `plugin.js` with the gsd address and the
//! `groundstation` binary (the spool fallback) baked in.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const TEMPLATE: &str = include_str!("plugin.js");
/// Marks a plugin file as ours, so reconnect and disconnect never touch
/// a file someone else wrote.
const MARKER: &str = "groundstation-plugin: opencode";
const FILE_NAME: &str = "groundstation.js";

/// OpenCode events the plugin forwards, for display. Kept in sync with the
/// plugin's `FORWARD` set by a test.
pub const EVENTS: &[&str] = &[
    "session.created",
    "session.deleted",
    "session.inbox.enqueued",
    "session.execution.succeeded",
    "session.execution.failed",
    "session.execution.interrupted",
    "session.step.ended",
    "session.step.failed",
    "session.tool.called",
    "session.tool.success",
    "session.tool.failed",
    "session.compaction.started",
    "session.compaction.ended",
    "permission.asked",
];

/// The oldest OpenCode major version whose plugin API this adapter targets.
pub const MIN_MAJOR_VERSION: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// ~/.config/opencode/plugins/: every project on this machine.
    User,
    /// .opencode/plugins/: this project.
    Project,
}

pub fn plugin_path(scope: Scope) -> Result<PathBuf> {
    Ok(match scope {
        Scope::User => config_dir().join("plugins").join(FILE_NAME),
        Scope::Project => std::env::current_dir()?
            .join(".opencode/plugins")
            .join(FILE_NAME),
    })
}

/// OpenCode's global config directory: `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("opencode")
}

/// The plugin source for a given gsd base URL and `groundstation` binary.
pub fn render(gsd_url: &str, groundstation: &Path) -> String {
    let quote = |s: &str| serde_json::to_string(s).expect("strings serialize");
    TEMPLATE
        .replace("\"__GSD_URL__\"", &quote(gsd_url))
        .replace(
            "\"__GROUNDSTATION__\"",
            &quote(&groundstation.to_string_lossy()),
        )
}

/// Writes the plugin to `path`. Returns `true` if it replaced an earlier
/// Ground Station plugin. Refuses to overwrite a file that isn't ours.
pub fn install(path: &Path, source: &str) -> Result<bool> {
    let replaced = match std::fs::read_to_string(path) {
        Ok(existing) if existing.contains(MARKER) => true,
        Ok(_) => bail!(
            "{} exists and wasn't written by Ground Station; not overwriting it",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, source).with_context(|| format!("writing {}", path.display()))?;
    Ok(replaced)
}

/// Removes the plugin at `path` if it is ours. Returns whether it was removed.
pub fn uninstall(path: &Path) -> Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing.contains(MARKER) => {
            std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
            Ok(true)
        }
        Ok(_) => bail!(
            "{} wasn't written by Ground Station; leaving it",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Checks `opencode --version` output (`opencode v2.0.19`) against
/// [`MIN_MAJOR_VERSION`]. Returns the version.
pub fn check_version(output: &str) -> Result<String> {
    let version = output
        .split_whitespace()
        .find_map(|word| {
            let v = word.trim_start_matches('v');
            v.chars().next().filter(char::is_ascii_digit).map(|_| v)
        })
        .with_context(|| format!("unrecognized `opencode --version` output: {output:?}"))?;
    let major: u64 = version
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .with_context(|| format!("unrecognized OpenCode version {version:?}"))?;
    if major < MIN_MAJOR_VERSION {
        bail!(
            "OpenCode {version} is not supported: Ground Station needs OpenCode {MIN_MAJOR_VERSION} or newer (v1 plugins don't load in v2 and vice versa). Upgrade with `npm i -g @opencode/cli`."
        );
    }
    Ok(version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_bakes_in_quoted_values() {
        let source = render(
            "http://127.0.0.1:4318",
            Path::new("/Users/Jo \"D\"/bin/groundstation"),
        );
        assert!(source.contains(r#"const GSD_URL = "http://127.0.0.1:4318";"#));
        assert!(source.contains(r#"const GROUNDSTATION = "/Users/Jo \"D\"/bin/groundstation";"#));
        assert!(!source.contains("__GSD_URL__") && !source.contains("__GROUNDSTATION__"));
        assert!(source.contains(MARKER));
    }

    #[test]
    fn events_match_the_plugin() {
        for event in EVENTS {
            assert!(
                TEMPLATE.contains(&format!("\"{event}\",")),
                "{event} not forwarded by plugin.js"
            );
        }
        let forwarded = TEMPLATE
            .split("const FORWARD")
            .nth(1)
            .unwrap()
            .split("]);")
            .next()
            .unwrap();
        assert_eq!(forwarded.matches("\",").count(), EVENTS.len());
    }

    #[test]
    fn install_and_uninstall_only_touch_our_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins/groundstation.js");
        let source = render("http://127.0.0.1:4318", Path::new("/bin/groundstation"));
        assert!(!install(&path, &source).unwrap());
        assert!(install(&path, &source).unwrap());
        assert!(uninstall(&path).unwrap());
        assert!(!uninstall(&path).unwrap());

        std::fs::write(&path, "export default {}").unwrap();
        assert!(install(&path, &source).is_err());
        assert!(uninstall(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "export default {}");
    }

    #[test]
    fn versions() {
        assert_eq!(check_version("opencode v2.0.19\n").unwrap(), "2.0.19");
        assert_eq!(check_version("2.1.0").unwrap(), "2.1.0");
        let err = check_version("1.18.27").unwrap_err().to_string();
        assert!(err.contains("needs OpenCode 2"), "{err}");
        assert!(check_version("garbage").is_err());
    }
}
