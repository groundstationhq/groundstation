//! Generated files Ground Station owns inside an agent's config (OpenCode
//! plugins, pi extensions). A marker line identifies them, so reconnecting
//! and disconnecting never touch a file someone else wrote.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// Fills `"__KEY__"` placeholders (quotes included) in `template` with
/// JSON-quoted values, so any value is a valid JS string literal.
pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    values
        .iter()
        .fold(template.to_string(), |source, (key, value)| {
            let quoted = serde_json::to_string(value).expect("strings serialize");
            source.replace(&format!("\"__{key}__\""), &quoted)
        })
}

/// Writes `source` to `path`. Returns `true` if it replaced an earlier file
/// with `marker`. Refuses to overwrite a file without it.
pub fn install(path: &Path, source: &str, marker: &str) -> Result<bool> {
    let replaced = match std::fs::read_to_string(path) {
        Ok(existing) if existing.contains(marker) => true,
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

/// Removes the file at `path` if it has `marker`. Returns whether it was removed.
pub fn uninstall(path: &Path, marker: &str) -> Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing.contains(marker) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = "groundstation-test: x";

    #[test]
    fn render_quotes_values() {
        let out = render(
            "// groundstation-test: x\nconst A = \"__A__\";",
            &[("A", "a \"b\"")],
        );
        assert_eq!(out, "// groundstation-test: x\nconst A = \"a \\\"b\\\"\";");
    }

    #[test]
    fn install_and_uninstall_only_touch_marked_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins/gs.js");
        let source = format!("// {MARKER}\n");
        assert!(!install(&path, &source, MARKER).unwrap());
        assert!(install(&path, &source, MARKER).unwrap());
        assert!(uninstall(&path, MARKER).unwrap());
        assert!(!uninstall(&path, MARKER).unwrap());

        std::fs::write(&path, "export default {}").unwrap();
        assert!(install(&path, &source, MARKER).is_err());
        assert!(uninstall(&path, MARKER).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "export default {}");
    }
}
