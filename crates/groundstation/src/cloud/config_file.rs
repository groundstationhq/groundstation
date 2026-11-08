//! Turning cloud mode on and off in `config.toml`, keeping everything else in
//! the file (comments included) as it was. The file holds the token, so it's
//! written owner-only, and whole or not at all.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gsd::config::{Config, TransportMode};
use toml_edit::{DocumentMut, Item, Table, value};

/// What `[transport]` says now.
pub struct Current {
    pub cloud: bool,
    pub endpoint: Option<String>,
    pub token: bool,
}

pub fn current(text: &str) -> Result<Current> {
    let config: Config = toml::from_str(text).context("config.toml doesn't parse")?;
    Ok(Current {
        cloud: config.transport.mode == TransportMode::Cloud,
        endpoint: config.transport.endpoint.filter(|e| !e.is_empty()),
        token: config.transport.token.is_some(),
    })
}

pub fn connect(text: &str, endpoint: &str, token: &str) -> Result<String> {
    edit(text, |transport| {
        transport["mode"] = value("cloud");
        transport["endpoint"] = value(endpoint);
        transport["token"] = value(token);
    })
}

/// Back to local-only. The endpoint stays, for the next `cloud login`.
pub fn disconnect(text: &str) -> Result<String> {
    edit(text, |transport| {
        transport["mode"] = value("local-only");
        transport.remove("token");
    })
}

fn edit(text: &str, change: impl FnOnce(&mut Table)) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("config.toml isn't valid TOML")?;
    // `transport = { … }` is as valid as `[transport]`.
    if let Some(inline) = doc.get("transport").and_then(Item::as_inline_table) {
        let table = inline.clone().into_table();
        doc["transport"] = Item::Table(table);
    }
    let transport = doc
        .entry("transport")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .context("transport in config.toml isn't a table")?;
    change(transport);
    let out = doc.to_string();
    // Never write a file gsd would refuse to start with.
    toml::from_str::<Config>(&out).context("the edited config.toml doesn't parse")?;
    Ok(out)
}

pub fn read(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The file a write to `path` changes: where a symlink points, so the link
/// stays a link.
pub fn target(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(target) => Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(std::path::absolute(path)?),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Fails now, before a token exists, if `path` couldn't be written.
pub fn check_writable(path: &Path) -> Result<()> {
    let target = target(path)?;
    if target.parent().is_none_or(|dir| !dir.exists()) {
        // `write` makes it, if its parents allow.
        return Ok(());
    }
    let probe = scratch(&target);
    open_new(&probe).with_context(|| format!("{} can't be written", target.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Replaces the file `path` names (or links to) with `text`, owner-only:
/// written beside it, then renamed over it, so a failed write leaves the old
/// file whole and the token is never readable by others. A new directory is
/// made owner-only; an existing one is left as it is. Returns the file written.
pub fn write(path: &Path, text: &str) -> Result<PathBuf> {
    let target = target(path)?;
    if let Some(dir) = target.parent().filter(|dir| !dir.exists()) {
        gsd::perms::create_dir_all(dir)?;
    }
    let scratch = scratch(&target);
    let written = open_new(&scratch)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&scratch, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&scratch);
        return Err(e).with_context(|| format!("writing {}", target.display()));
    }
    Ok(target)
}

fn scratch(target: &Path) -> PathBuf {
    let name = target.file_name().map_or_else(
        || "config.toml".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    target.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

fn open_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXISTING: &str = "# my settings\n[redaction]\nexclude = [\"prompt\"] # keep prompts here\n\n[transport]\nbatch_size = 100\n";

    #[test]
    fn sets_cloud_mode_and_keeps_the_rest() {
        let out = connect(EXISTING, "https://gs.example", "gsi_abc").unwrap();
        assert!(out.starts_with("# my settings\n"));
        assert!(out.contains("exclude = [\"prompt\"] # keep prompts here"));
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.upload_endpoint(), Some("https://gs.example"));
        assert_eq!(config.transport.token.as_deref(), Some("gsi_abc"));
        assert_eq!(config.transport.batch_size, 100);

        let back: Config = toml::from_str(&disconnect(&out).unwrap()).unwrap();
        assert_eq!(
            (back.upload_endpoint(), back.transport.token.as_deref()),
            (None, None)
        );
        assert_eq!(
            back.transport.endpoint.as_deref(),
            Some("https://gs.example")
        );
    }

    #[test]
    fn works_on_an_empty_file() {
        let config: Config = toml::from_str(&connect("", "https://x", "t").unwrap()).unwrap();
        assert_eq!(config.upload_endpoint(), Some("https://x"));
    }

    #[test]
    fn works_on_an_inline_transport_table() {
        let text = "transport = { mode = \"cloud\", endpoint = \"https://x\", token = \"t\" }\n";
        let back: Config = toml::from_str(&disconnect(text).unwrap()).unwrap();
        assert_eq!(
            (back.upload_endpoint(), back.transport.token.as_deref()),
            (None, None)
        );
        assert!(connect("transport = { batch_size = 7 }", "https://x", "t").is_ok());
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn writes_owner_only_through_links_and_leaves_the_directory_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("write");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let real = dir.join("dotfiles.toml");
        std::fs::write(&real, EXISTING).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let written = write(&link, "[transport]\ntoken = \"t\"\n").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(written, std::fs::canonicalize(&real).unwrap());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "[transport]\ntoken = \"t\"\n"
        );
        assert_eq!(mode(&real), 0o600);
        assert_eq!(mode(&dir), 0o755, "an existing directory keeps its mode");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "no scratch file left"
        );

        // A new directory is made owner-only.
        let fresh = dir.join("new").join("config.toml");
        write(&fresh, "").unwrap();
        assert_eq!(mode(fresh.parent().unwrap()), 0o700);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_is_found_before_a_token_exists() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("readonly");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let checked = check_writable(&dir.join("config.toml"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(checked.is_err());
        assert!(check_writable(&std::env::temp_dir().join("gs-cfg-absent/config.toml")).is_ok());
    }

    #[test]
    fn a_bare_file_name_is_in_the_working_directory() {
        // `--config config.toml`: its parent is "", which nothing can create.
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            target(Path::new("config.toml")).unwrap(),
            here.join("config.toml")
        );
    }
}
