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
            Ok(text) => toml::from_str(&text).map_err(|e| parse_error(&path, &text, &e))?,
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
        if self.transport.mode == TransportMode::Cloud {
            let Some(endpoint) = self.upload_endpoint() else {
                bail!("transport.mode = \"cloud\" requires transport.endpoint");
            };
            check_endpoint(endpoint)?;
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

/// The upload token and the events go with every request, so the endpoint must
/// be `https://`. Plain `http://` is allowed only to this machine, for a
/// backend running locally.
/// Where a config file doesn't parse, and why, without the text of the line:
/// that line may hold the upload token, and the error is printed everywhere.
fn parse_error(path: &Path, text: &str, e: &toml::de::Error) -> anyhow::Error {
    let at = e
        .span()
        .map(|span| {
            let before = text.get(..span.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
            format!(", line {line}, column {column}")
        })
        .unwrap_or_default();
    anyhow::anyhow!("parsing {}{at}: {}", path.display(), e.message().trim())
}

pub fn check_endpoint(endpoint: &str) -> Result<()> {
    let url = reqwest::Url::parse(endpoint)
        .with_context(|| format!("transport.endpoint {endpoint:?} is not a URL"))?;
    // Checked before anything below repeats the endpoint.
    if !url.username().is_empty() || url.password().is_some() {
        bail!(
            "transport.endpoint must not carry a user name or password: \
             the upload token goes in transport.token"
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("transport.endpoint is the backend's address, without a ?query or #fragment");
    }
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        "http" => bail!(
            "transport.endpoint must use https:// (http:// only for localhost): \
             the upload token and your events would travel unencrypted to {endpoint}"
        ),
        other => bail!("transport.endpoint must be an https:// URL, not {other}://"),
    }
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

    #[test]
    fn upload_endpoints_must_be_https_unless_local() {
        let with = |endpoint: &str| {
            toml::from_str::<Config>(&format!(
                "[transport]\nmode = \"cloud\"\nendpoint = \"{endpoint}\""
            ))
            .unwrap()
            .validate()
        };
        for ok in [
            "https://ingest.groundstation.sh",
            "http://localhost:8080",
            "http://LOCALHOST",
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
            "http://[::1]:8080",
        ] {
            assert!(with(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://ingest.groundstation.sh",
            "http://192.168.1.10:8080",
            "http://localhost.evil.example",
            "ftp://ingest.groundstation.sh",
            "ingest.groundstation.sh",
            "https://ana:hunter22@ingest.groundstation.sh",
            "https://ana@ingest.groundstation.sh",
            "https://ingest.groundstation.sh/?x=1",
            "https://ingest.groundstation.sh/#top",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
        let refused = with("https://ana:hunter22@gs.example").unwrap_err();
        assert!(!format!("{refused:#}").contains("hunter22"));
        // Local-only mode never uploads, so the endpoint isn't checked.
        let local: Config =
            toml::from_str("[transport]\nendpoint = \"http://example.com\"").unwrap();
        assert!(local.validate().is_ok());
    }

    #[test]
    fn a_config_that_doesnt_parse_isnt_repeated_back() {
        let dir = std::env::temp_dir().join(format!("gsd-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // A token pasted without quotes, as it comes off the Tokens page.
        std::fs::write(
            &path,
            "[transport]\nmode = \"cloud\"\ntoken = gsi_secret_value\n",
        )
        .unwrap();
        let shown = format!("{:#}", Config::load(Some(&path)).unwrap_err());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!shown.contains("gsi_secret_value"), "{shown}");
        assert!(shown.contains("line 3, column 9"), "{shown}");
    }
}
