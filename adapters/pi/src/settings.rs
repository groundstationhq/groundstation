//! Installs and removes the Ground Station extension for pi.
//!
//! pi loads extensions from `<agent-dir>/extensions/` (`~/.pi/agent`, or
//! `$PI_CODING_AGENT_DIR`) and `<project>/.pi/extensions/`. The extension is
//! generated from `extension.js` with the gsd address and the `groundstation`
//! binary (the spool fallback) baked in.

use std::path::{Path, PathBuf};

use anyhow::Result;
use groundstation_hooks_json::managed;

const TEMPLATE: &str = include_str!("extension.js");
const MARKER: &str = "groundstation-extension: pi";
const FILE_NAME: &str = "groundstation.js";

/// pi events the extension forwards, for display. Kept in sync with
/// `extension.js` by a test.
pub const EVENTS: &[&str] = &[
    "session_start",
    "session_shutdown",
    "before_agent_start",
    "agent_end",
    "message_end",
    "tool_execution_start",
    "tool_execution_end",
    "session_compact",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// <agent-dir>/extensions/: every project on this machine.
    User,
    /// .pi/extensions/: this project. pi loads project extensions only for
    /// trusted projects.
    Project,
}

pub fn extension_path(scope: Scope) -> Result<PathBuf> {
    Ok(match scope {
        Scope::User => agent_dir().join("extensions").join(FILE_NAME),
        Scope::Project => std::env::current_dir()?
            .join(".pi/extensions")
            .join(FILE_NAME),
    })
}

/// pi's agent directory: `$PI_CODING_AGENT_DIR` or `~/.pi/agent`.
pub fn agent_dir() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".pi/agent")
        })
}

/// The extension source for a given gsd base URL and `groundstation` binary.
pub fn render(gsd_url: &str, groundstation: &Path) -> String {
    managed::render(
        TEMPLATE,
        &[
            ("GSD_URL", gsd_url),
            ("GROUNDSTATION", &groundstation.to_string_lossy()),
        ],
    )
}

/// Writes the extension to `path`. Returns `true` if it replaced an earlier
/// Ground Station extension. Refuses to overwrite a file that isn't ours.
pub fn install(path: &Path, source: &str) -> Result<bool> {
    managed::install(path, source, MARKER)
}

/// Removes the extension at `path` if it is ours. Returns whether it was removed.
pub fn uninstall(path: &Path) -> Result<bool> {
    managed::uninstall(path, MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_bakes_in_values() {
        let source = render("http://127.0.0.1:4318", Path::new("/bin/groundstation"));
        assert!(source.contains(r#"const GSD_URL = "http://127.0.0.1:4318";"#));
        assert!(source.contains(r#"const GROUNDSTATION = "/bin/groundstation";"#));
        assert!(source.contains(MARKER));
    }

    #[test]
    fn events_match_the_extension() {
        for event in EVENTS {
            assert!(
                TEMPLATE.contains(&format!("pi.on(\"{event}\"")),
                "{event} not handled"
            );
        }
        assert_eq!(TEMPLATE.matches("pi.on(\"").count(), EVENTS.len());
    }
}
