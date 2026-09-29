//! `~/.config/groundstation/config.toml` and on-disk locations.
//!
//! Directories follow XDG on every platform (the product promises
//! `~/.config/groundstation`), with `GROUNDSTATION_CONFIG_DIR` and
//! `GROUNDSTATION_DATA_DIR` as overrides for tests and custom installs.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:4318";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub redaction: RedactionConfig,
    pub transport: TransportConfig,
    pub capture: CaptureConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub listen: SocketAddr,
    /// Defaults to `$XDG_DATA_HOME/groundstation`.
    pub data_dir: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.parse().expect("valid default address"),
            data_dir: None,
        }
    }
}

/// What is removed or rewritten before anything is stored or uploaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedactionConfig {
    /// Replace well-known credential formats (Stripe, AWS, GitHub, OpenAI,
    /// Anthropic, Slack, JWT, PEM keys, passwords, URL credentials) with `[REDACTED]`.
    pub secrets: bool,
    /// How local filesystem paths (working directory, repository, file paths) are kept.
    pub paths: PathPolicy,
    /// Environment variable name globs (`*` wildcard). `NAME=value` assignments
    /// for matching names are redacted wherever they appear, as are the values
    /// those variables hold in gsd's own environment.
    pub env: Vec<String>,
    /// Content fields to drop entirely, e.g. `prompt` or `tool.output.body`.
    /// Measurements such as byte counts are kept.
    pub exclude: Vec<String>,
    /// Extra regexes; every match is replaced with `[REDACTED]`.
    pub patterns: Vec<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            secrets: true,
            paths: PathPolicy::Keep,
            env: ["*_KEY", "*_TOKEN", "*_SECRET", "DATABASE_URL"]
                .map(String::from)
                .to_vec(),
            exclude: Vec::new(),
            patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathPolicy {
    #[default]
    Keep,
    /// Replace each path with a stable hash: the same file still groups
    /// together, but its name never leaves the daemon.
    Hash,
}

/// Where events go after they are stored locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransportConfig {
    pub mode: TransportMode,
    /// Base URL of the Ground Station backend; required in `cloud` mode.
    pub endpoint: Option<String>,
    /// Also read from `GROUNDSTATION_TOKEN`.
    pub token: Option<String>,
    pub batch_size: usize,
    pub flush_interval_secs: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            mode: TransportMode::LocalOnly,
            endpoint: None,
            token: None,
            batch_size: 500,
            flush_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportMode {
    /// Every byte stays on this machine.
    #[default]
    LocalOnly,
    /// Upload events to `transport.endpoint` in compressed batches.
    Cloud,
}

impl TransportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local-only",
            Self::Cloud => "cloud",
        }
    }
}

/// How much of each event is kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// Longest string kept per field; sizes are always recorded. 0 = unlimited.
    pub max_content_bytes: usize,
    /// Keep the agent's original payload next to each normalized event (for
    /// re-processing). Only honored when `redaction.exclude` is empty and
    /// `redaction.paths = "keep"`, since raw payloads can't be filtered by field.
    pub raw: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            max_content_bytes: 64 * 1024,
            raw: true,
        }
    }
}

impl Config {
    /// Loads `path`, or the default config file if `path` is `None`. A missing
    /// default file is not an error. `GROUNDSTATION_LISTEN` overrides the
    /// address and `GROUNDSTATION_TOKEN` the upload token.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let (path, required) = match path {
            Some(p) => (p.to_path_buf(), true),
            None => (config_file(), false),
        };
        let mut config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => Config::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if let Ok(listen) = std::env::var("GROUNDSTATION_LISTEN") {
            config.daemon.listen = listen
                .parse()
                .with_context(|| format!("GROUNDSTATION_LISTEN={listen}"))?;
        }
        if let Ok(token) = std::env::var("GROUNDSTATION_TOKEN") {
            config.transport.token = Some(token);
        }
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.transport.mode == TransportMode::Cloud && self.upload_endpoint().is_none() {
            bail!("transport.mode = \"cloud\" requires transport.endpoint");
        }
        Ok(())
    }

    /// Where to upload events, or `None` in local-only mode.
    pub fn upload_endpoint(&self) -> Option<&str> {
        match self.transport.mode {
            TransportMode::LocalOnly => None,
            TransportMode::Cloud => self.transport.endpoint.as_deref().filter(|e| !e.is_empty()),
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.daemon.data_dir.clone().unwrap_or_else(data_dir)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join("gsd.db")
    }

    pub fn spool_dir(&self) -> PathBuf {
        self.data_dir().join("spool")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.data_dir().join("gsd.pid")
    }

    pub fn log_file(&self) -> PathBuf {
        self.data_dir().join("gsd.log")
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.daemon.listen)
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(fallback))
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("GROUNDSTATION_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg("XDG_CONFIG_HOME", ".config").join("groundstation"))
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("GROUNDSTATION_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg("XDG_DATA_HOME", ".local/share").join("groundstation"))
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_example_parses() {
        let config: Config = toml::from_str(
            r#"
            [redaction]
            secrets  = true
            paths    = "hash"
            env      = ["*_KEY", "*_TOKEN", "*_SECRET", "DATABASE_URL"]
            exclude  = ["prompt", "tool.output.body"]

            [transport]
            mode     = "local-only"
            "#,
        )
        .unwrap();
        assert_eq!(config.redaction.paths, PathPolicy::Hash);
        assert_eq!(config.redaction.exclude, ["prompt", "tool.output.body"]);
        assert_eq!(config.transport.mode, TransportMode::LocalOnly);
        assert_eq!(config.upload_endpoint(), None);
        assert_eq!(config.daemon.listen.to_string(), DEFAULT_LISTEN);
        assert!(config.capture.raw);
    }

    #[test]
    fn rejects_typos_and_incomplete_cloud_mode() {
        assert!(toml::from_str::<Config>("[redaction]\nexclud = [\"prompt\"]").is_err());
        assert!(toml::from_str::<Config>("[redation]\nsecrets = true").is_err());

        let cloud: Config = toml::from_str("[transport]\nmode = \"cloud\"").unwrap();
        assert!(cloud.validate().is_err());
        let cloud: Config =
            toml::from_str("[transport]\nmode = \"cloud\"\nendpoint = \"https://x\"").unwrap();
        assert_eq!(cloud.upload_endpoint(), Some("https://x"));
    }
}
