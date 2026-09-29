//! `groundstation update`: installs a GitHub Release over an install that
//! `install.sh` made, then restarts gsd so the new daemon is the one running.
//!
//! The layout is the installer's: releases unpack under
//! `$GROUNDSTATION_HOME/releases/<name>/`, `$GROUNDSTATION_HOME/current`
//! points at the active one, and `$GROUNDSTATION_INSTALL_DIR/{groundstation,gsd}`
//! link into `current`. An install made any other way (`cargo install`, a
//! package manager) is left alone with a hint on how to upgrade it.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub const REPO: &str = "groundstationhq/groundstation";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const BINARIES: [&str; 2] = ["groundstation", "gsd"];

/// A release version, ordered like semver: `1.0.0-rc.1 < 1.0.0 < 1.0.1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_string())),
            Some(_) => return None,
            None => (s, None),
        };
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

/// Where the installer put things. Mirrors `install.sh` exactly.
#[derive(Debug, Clone)]
pub struct Layout {
    /// `$GROUNDSTATION_HOME`, default `$XDG_DATA_HOME/groundstation` or `~/.local/share/groundstation`.
    pub home: PathBuf,
    /// `$GROUNDSTATION_INSTALL_DIR`, default `~/.local/bin`.
    pub bin: PathBuf,
    /// `$GROUNDSTATION_RELEASE_BASE`, default the GitHub Releases page.
    pub release_base: String,
}

impl Layout {
    pub fn from_env() -> Self {
        let home_dir = gsd::config::home_dir();
        let home = std::env::var_os("GROUNDSTATION_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| home_dir.join(".local/share"))
                    .join("groundstation")
            });
        let bin = std::env::var_os("GROUNDSTATION_INSTALL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".local/bin"));
        let release_base = std::env::var("GROUNDSTATION_RELEASE_BASE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("https://github.com/{REPO}/releases"));
        Self {
            home,
            bin,
            release_base,
        }
    }

    pub fn releases(&self) -> PathBuf {
        self.home.join("releases")
    }

    pub fn current(&self) -> PathBuf {
        self.home.join("current")
    }
}

/// How this `groundstation` binary got onto the machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// Under the installer's `releases/` directory: `update` can replace it.
    Managed,
    /// Under `~/.cargo/bin`: upgrade with cargo.
    Cargo,
    /// Anywhere else (a package manager, a dev build).
    Other,
}

/// Classifies `exe` (the running binary, symlinks resolved) against `layout`.
pub fn detect(exe: &Path, layout: &Layout) -> Install {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let exe = canon(exe);
    if exe.starts_with(canon(&layout.releases())) {
        return Install::Managed;
    }
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| gsd::config::home_dir().join(".cargo"))
        .join("bin");
    if exe.starts_with(canon(&cargo_bin)) {
        return Install::Cargo;
    }
    Install::Other
}

/// The release target for this machine. An x86_64 build running under
/// Rosetta still gets the native arm64 release, like the installer.
pub fn target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") if rosetta() => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        (os, arch) => bail!("no release build for {os} {arch}"),
    }
}

fn rosetta() -> bool {
    std::process::Command::new("sysctl")
        .args(["-n", "sysctl.proc_translated"])
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
}

/// The version `releases/latest` points at, from its `VERSION` asset.
pub async fn latest(client: &reqwest::Client, layout: &Layout) -> Result<Version> {
    let url = format!("{}/latest/download/VERSION", layout.release_base);
    let text = fetch(client, &url).await?;
    let text = String::from_utf8_lossy(&text);
    Version::parse(&text).with_context(|| format!("{url} does not hold a version: {text:?}"))
}

/// The `hash` column of `SHA256SUMS` for `asset`.
pub fn checksum_for<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (name.trim_start_matches('*') == asset && hash.len() == 64).then_some(hash)
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("{url} returned {status}");
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Downloads, verifies and unpacks `version` under `releases/`, and returns
/// the release directory. A release already unpacked is reused.
pub async fn install(
    client: &reqwest::Client,
    layout: &Layout,
    version: &Version,
    say: &dyn Fn(&str),
) -> Result<PathBuf> {
    let name = format!("groundstation-{version}-{}", target()?);
    let release_dir = layout.releases().join(&name);
    if BINARIES.iter().all(|b| is_executable(&release_dir.join(b))) {
        say(&format!("{version} is already unpacked; refreshing links"));
        return Ok(release_dir);
    }

    let asset = format!("{name}.tar.gz");
    let base = format!("{}/download/v{version}", layout.release_base);
    say(&format!("Downloading {asset}"));
    let archive = fetch(client, &format!("{base}/{asset}")).await?;
    let sums = fetch(client, &format!("{base}/SHA256SUMS")).await?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = checksum_for(&sums, &asset)
        .with_context(|| format!("{asset} is not listed in SHA256SUMS"))?;
    let actual = sha256_hex(&archive);
    if actual != expected {
        bail!("checksum mismatch for {asset}\n  expected {expected}\n  actual   {actual}");
    }
    say("Checksum verified");

    gsd::perms::create_dir_all(&layout.releases())?;
    let staging = layout
        .releases()
        .join(format!(".staging.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let unpacked = (|| -> Result<()> {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(&archive[..]));
        tar.unpack(&staging).context("unpacking the archive")?;
        for bin in BINARIES {
            if !is_executable(&staging.join(&name).join(bin)) {
                bail!("archive is missing {bin}");
            }
        }
        let _ = std::fs::remove_dir_all(&release_dir);
        std::fs::rename(staging.join(&name), &release_dir)
            .with_context(|| format!("moving the release into {}", release_dir.display()))?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    unpacked?;
    Ok(release_dir)
}

/// Points `current` at `release_dir` and the launch symlinks at `current`,
/// the way the installer does, so the links stay valid across upgrades.
#[cfg(unix)]
pub fn link(layout: &Layout, release_dir: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;
    let current = layout.current();
    let tmp = layout.home.join(".current.tmp");
    let _ = std::fs::remove_file(&tmp);
    symlink(release_dir, &tmp)?;
    std::fs::rename(&tmp, &current).with_context(|| format!("updating {}", current.display()))?;
    std::fs::create_dir_all(&layout.bin)?;
    for bin in BINARIES {
        let link = layout.bin.join(bin);
        if std::fs::symlink_metadata(&link).is_ok() {
            std::fs::remove_file(&link).with_context(|| format!("replacing {}", link.display()))?;
        }
        symlink(current.join(bin), &link)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn link(_layout: &Layout, _release_dir: &Path) -> Result<()> {
    bail!("groundstation update supports macOS and Linux; re-run the installer instead")
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_order() {
        let v = |s| Version::parse(s).unwrap();
        assert_eq!(v("v0.1.1").to_string(), "0.1.1");
        assert!(v("0.1.1") > v("0.1.0"));
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-rc.1") < v("1.0.0-rc.2"));
        assert_eq!(v("0.1.1"), v("v0.1.1\n"));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1.2.3-", "latest"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        assert!(Version::parse(CURRENT).is_some());
    }

    #[test]
    fn checksums_are_looked_up_by_asset_name() {
        let sums = "\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  groundstation-0.1.1-aarch64-apple-darwin.tar.gz
fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210  groundstation-0.1.1-x86_64-unknown-linux-musl.tar.gz
";
        assert_eq!(
            checksum_for(sums, "groundstation-0.1.1-x86_64-unknown-linux-musl.tar.gz"),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
        );
        assert_eq!(checksum_for(sums, "groundstation-0.1.1-arm.tar.gz"), None);
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn detects_where_the_binary_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout {
            home: dir.path().join("share"),
            bin: dir.path().join("bin"),
            release_base: "file:///nowhere".into(),
        };
        let managed = layout
            .releases()
            .join("groundstation-0.1.1-x/groundstation");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"").unwrap();
        assert_eq!(detect(&managed, &layout), Install::Managed);
        assert_eq!(
            detect(Path::new("/opt/homebrew/bin/groundstation"), &layout),
            Install::Other
        );
        // The launch symlink resolves into releases/, so it counts as managed too.
        #[cfg(unix)]
        {
            std::fs::create_dir_all(&layout.bin).unwrap();
            std::os::unix::fs::symlink(&managed, layout.bin.join("groundstation")).unwrap();
            assert_eq!(
                detect(&layout.bin.join("groundstation"), &layout),
                Install::Managed
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_current_and_launchers() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout {
            home: dir.path().join("share"),
            bin: dir.path().join("bin"),
            release_base: String::new(),
        };
        let release = layout.releases().join("groundstation-9.9.9-x");
        std::fs::create_dir_all(&release).unwrap();
        for bin in BINARIES {
            std::fs::write(release.join(bin), b"#!/bin/sh\n").unwrap();
        }
        // A stale launcher from an older layout is replaced, not appended to.
        std::fs::create_dir_all(&layout.bin).unwrap();
        std::fs::write(layout.bin.join("gsd"), b"old").unwrap();
        link(&layout, &release).unwrap();
        assert_eq!(std::fs::read_link(layout.current()).unwrap(), release);
        for bin in BINARIES {
            let launcher = layout.bin.join(bin);
            assert_eq!(
                std::fs::read_link(&launcher).unwrap(),
                layout.current().join(bin)
            );
            assert_eq!(
                std::fs::read(launcher.canonicalize().unwrap()).unwrap(),
                b"#!/bin/sh\n"
            );
        }
        // Relinking to another release swaps atomically.
        let newer = layout.releases().join("groundstation-9.9.10-x");
        std::fs::create_dir_all(&newer).unwrap();
        link(&layout, &newer).unwrap();
        assert_eq!(std::fs::read_link(layout.current()).unwrap(), newer);
    }
}
