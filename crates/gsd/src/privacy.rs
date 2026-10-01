//! `[redaction]` is applied inside the daemon, before anything touches disk,
//! so nothing that leaves an agent is stored or uploaded unfiltered.

use std::path::Path;

use anyhow::{Context, Result, bail};
use groundstation_schema::Event;
use groundstation_schema::attr;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{Config, PathPolicy};

pub const REDACTED: &str = "[REDACTED]";

/// Environment values shorter than this are too likely to appear by chance
/// (`true`, `8080`, `dev`) to be redacted literally.
const MIN_ENV_VALUE_LEN: usize = 8;

/// Credential formats that are unambiguous enough to redact everywhere.
/// Each entry is `(pattern, replacement)`; replacements may keep a prefix.
const SECRET_PATTERNS: &[(&str, &str)] = &[
    (
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        REDACTED,
    ),
    (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
    (r"\bgh[pousr]_[A-Za-z0-9]{36,}\b", REDACTED),
    (r"\bgithub_pat_[A-Za-z0-9_]{22,}\b", REDACTED),
    (r"\bglpat-[A-Za-z0-9_\-]{20,}\b", REDACTED),
    (r"\bsk-ant-[A-Za-z0-9_\-]{20,}", REDACTED),
    (r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{20,}", REDACTED),
    (r"\b[rps]k_(?:live|test)_[A-Za-z0-9]{20,}\b", REDACTED),
    (r"\bwhsec_[A-Za-z0-9]{20,}\b", REDACTED),
    (r"\bxox[abprs]-[A-Za-z0-9\-]{10,}", REDACTED),
    (r"\bAIza[0-9A-Za-z_\-]{35}\b", REDACTED),
    (
        r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
        REDACTED,
    ),
    (
        r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/\-]{20,}=*",
        "$1 [REDACTED]",
    ),
    (r"(?i)\b(basic)\s+[A-Za-z0-9+/]{16,}=*", "$1 [REDACTED]"),
    (r"(?i)(://[^/\s:@]+:)[^@\s/]+@", "${1}[REDACTED]@"),
    (
        r#"(?i)\b([A-Z0-9_]*(?:password|passwd|secret|api_?key|access_?key|auth_?token|private_?key)[A-Z0-9_]*)(["']?\s*[:=]\s*["']?)[^\s"'`,;]{4,}"#,
        "$1$2[REDACTED]",
    ),
];

pub struct Privacy {
    exclude: Vec<&'static str>,
    paths: PathPolicy,
    max_content_bytes: usize,
    store_raw: bool,
    rules: Vec<(Regex, String)>,
    /// Secret key for `paths = "hash"`, so hashes can't be reversed by
    /// guessing likely paths or compared across machines. See [`path_key`].
    path_key: [u8; 32],
}

impl Privacy {
    /// Builds the policy from config, reading environment values for
    /// `redaction.env` from this process.
    pub fn new(config: &Config) -> Result<Self> {
        Self::with_env(config, std::env::vars())
    }

    pub fn with_env(
        config: &Config,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self> {
        let r = &config.redaction;
        let mut rules = Vec::new();
        if r.secrets {
            for (pattern, replacement) in SECRET_PATTERNS {
                let re = Regex::new(pattern).expect("built-in pattern");
                rules.push((re, replacement.to_string()));
            }
        }
        if !r.env.is_empty() {
            let alternatives = env_name_alternatives(&r.env)?;
            let names = Regex::new(&format!("(?i)^(?:{alternatives})$"))?;
            // Literal values first, so a value is removed even where its name isn't next to it.
            let mut values: Vec<String> = env
                .into_iter()
                .filter(|(name, value)| names.is_match(name) && value.len() >= MIN_ENV_VALUE_LEN)
                .map(|(_, value)| value)
                .collect();
            values.sort_by_key(|v| std::cmp::Reverse(v.len()));
            values.dedup();
            for value in values {
                rules.push((Regex::new(&regex::escape(&value))?, REDACTED.to_string()));
            }
            let assignment = format!(r#"(?i)\b({alternatives})(["']?\s*[:=]\s*["']?)[^\s"'`,;]+"#);
            rules.push((Regex::new(&assignment)?, "$1$2[REDACTED]".to_string()));
        }
        for pattern in &r.patterns {
            let re = Regex::new(pattern)
                .with_context(|| format!("invalid redaction.patterns entry {pattern:?}"))?;
            rules.push((re, REDACTED.to_string()));
        }
        let exclude = resolve_exclude(&r.exclude)?;
        Ok(Self {
            store_raw: config.capture.raw && exclude.is_empty() && r.paths == PathPolicy::Keep,
            exclude,
            paths: r.paths,
            max_content_bytes: config.capture.max_content_bytes,
            rules,
            path_key: [0; 32],
        })
    }

    /// Hashes paths with this install's secret key (see [`path_key`]).
    pub fn with_path_key(mut self, key: [u8; 32]) -> Self {
        self.path_key = key;
        self
    }

    /// Drops excluded fields, applies the path policy, then redacts secrets
    /// in every remaining string.
    pub fn apply(&self, event: &mut Event) {
        for key in &self.exclude {
            event.attributes.remove(*key);
        }
        if self.paths == PathPolicy::Hash {
            for key in attr::PATHS {
                if let Some(Value::String(s)) = event.attributes.get_mut(*key) {
                    *s = self.hash(s);
                }
            }
            if let Some(Value::Object(input)) = event.attributes.get_mut(attr::TOOL_INPUT) {
                for key in attr::TOOL_INPUT_PATH_KEYS {
                    if let Some(Value::String(s)) = input.get_mut(*key) {
                        *s = self.hash(s);
                    }
                }
            }
        }
        for value in event.attributes.values_mut() {
            self.scrub(value);
        }
    }

    /// A local path as it may be stored (e.g. a trajectory's repository).
    pub fn path(&self, path: &str) -> String {
        match self.paths {
            PathPolicy::Keep => path.to_string(),
            PathPolicy::Hash => self.hash(path),
        }
    }

    /// A stable stand-in for a path: HMAC-SHA256 under the install's key,
    /// truncated. The same path always gives the same value on this machine.
    fn hash(&self, s: &str) -> String {
        let mut inner_pad = [0x36u8; 64];
        let mut outer_pad = [0x5cu8; 64];
        for (i, k) in self.path_key.iter().enumerate() {
            inner_pad[i] ^= k;
            outer_pad[i] ^= k;
        }
        let inner = Sha256::new()
            .chain_update(inner_pad)
            .chain_update(s.as_bytes())
            .finalize();
        let mac = Sha256::new()
            .chain_update(outer_pad)
            .chain_update(inner)
            .finalize();
        format!("sha256:{}", &hex::encode(mac)[..16])
    }

    /// The original agent payload, if the policy allows keeping it.
    pub fn raw(&self, raw: &Value) -> Option<Value> {
        self.store_raw.then(|| {
            let mut raw = raw.clone();
            self.scrub(&mut raw);
            raw
        })
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = std::borrow::Cow::Borrowed(text);
        for (re, replacement) in &self.rules {
            if let std::borrow::Cow::Owned(s) = re.replace_all(&out, replacement.as_str()) {
                out = std::borrow::Cow::Owned(s);
            }
        }
        out.into_owned()
    }

    fn scrub(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                let mut redacted = self.redact(s);
                truncate(&mut redacted, self.max_content_bytes);
                *s = redacted;
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.scrub(v)),
            Value::Object(map) => map.values_mut().for_each(|v| self.scrub(v)),
            _ => {}
        }
    }
}

/// Translates `*`-globs over environment variable names into regex alternatives.
fn env_name_alternatives(globs: &[String]) -> Result<String> {
    let mut alternatives = Vec::new();
    for glob in globs {
        if glob.is_empty()
            || !glob
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '*')
        {
            bail!(
                "redaction.env: {glob:?} is not a variable name glob (letters, digits, `_` and `*`)"
            );
        }
        alternatives.push(glob.replace('*', "[A-Za-z0-9_]*"));
    }
    Ok(alternatives.join("|"))
}

/// Maps `redaction.exclude` entries to content attributes. An entry names a
/// field with or without its `gs.` prefix, or a dotted prefix of several
/// (`tool` covers `tool.input.body` and `tool.output.body`). Unknown entries
/// are an error: a typo must not silently keep content that was meant to go.
fn resolve_exclude(entries: &[String]) -> Result<Vec<&'static str>> {
    let mut keys = Vec::new();
    for entry in entries {
        let name = entry.trim();
        let name = name.strip_prefix("gs.").unwrap_or(name);
        let matched: Vec<&'static str> = attr::CONTENT
            .iter()
            .copied()
            .filter(|key| {
                let key = key.strip_prefix("gs.").unwrap_or(key);
                key == name
                    || key
                        .strip_prefix(name)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            .collect();
        if matched.is_empty() {
            let valid: Vec<&str> = attr::CONTENT
                .iter()
                .map(|k| k.strip_prefix("gs.").unwrap_or(k))
                .collect();
            bail!(
                "redaction.exclude: {entry:?} matches no content field; valid fields are {} (or a prefix such as \"tool\")",
                valid.join(", ")
            );
        }
        keys.extend(matched);
    }
    keys.sort_unstable();
    keys.dedup();
    Ok(keys)
}

/// This install's key for hashing paths, kept in `<data_dir>/path-key` and
/// created on first use. Readable by the owner only; deleting it changes
/// every hash from then on.
pub fn path_key(data_dir: &Path) -> Result<[u8; 32]> {
    let file = data_dir.join("path-key");
    match std::fs::read(&file) {
        Ok(bytes) => bytes.try_into().map_err(|_| {
            anyhow::anyhow!(
                "{} is not a 32-byte key; delete it to make a new one",
                file.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut key))
                .context("reading random bytes for the path key")?;
            crate::perms::write(&file, &key)?;
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

fn truncate(s: &mut String, max: usize) {
    if max == 0 || s.len() <= max {
        return;
    }
    let dropped = s.len() - max;
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(&format!("…[truncated {dropped} bytes]"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use groundstation_schema::{Agent, EventKind};
    use serde_json::json;
    use uuid::Uuid;

    fn privacy(f: impl FnOnce(&mut Config)) -> Privacy {
        let mut config = Config::default();
        f(&mut config);
        Privacy::with_env(&config, []).unwrap()
    }

    fn event() -> Event {
        Event::new(
            Uuid::nil(),
            "t",
            EventKind::ToolStarted,
            Utc::now(),
            Agent::named("test"),
        )
    }

    #[test]
    fn redacts_known_secrets() {
        let p = privacy(|_| {});
        let cases = [
            (
                "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE",
                "AKIAIOSFODNN7EXAMPLE",
            ),
            ("token ghp_0123456789abcdefghijklmnopqrstuvwxyz", "ghp_0123"),
            (
                "ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuv",
                "sk-ant",
            ),
            ("stripe sk_live_0123456789abcdefghijklmn", "sk_live"),
            (
                "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz123'",
                "abcdefghijklmnop",
            ),
            ("postgres://admin:hunter2@db.internal/app", "hunter2"),
            (r#"{"password": "correct-horse"}"#, "correct-horse"),
        ];
        for (input, secret) in cases {
            let out = p.redact(input);
            assert!(!out.contains(secret), "{input:?} -> {out:?}");
            assert!(out.contains(REDACTED), "{input:?} -> {out:?}");
        }
        assert_eq!(p.redact("cargo test --workspace"), "cargo test --workspace");
        assert_eq!(p.redact("Bearer"), "Bearer");
    }

    #[test]
    fn secrets_can_be_turned_off() {
        let p = privacy(|c| c.redaction.secrets = false);
        let s = "token ghp_0123456789abcdefghijklmnopqrstuvwxyz";
        assert_eq!(p.redact(s), s);
    }

    #[test]
    fn env_names_and_values() {
        let mut config = Config::default();
        config.redaction.env = vec!["*_TOKEN".into(), "DATABASE_URL".into()];
        let env = [
            (
                "DATABASE_URL".to_string(),
                "mysql://prod-db.internal/app".to_string(),
            ),
            ("DEPLOY_TOKEN".to_string(), "dtk-9f8e7d6c5b4a".to_string()),
            ("HOME".to_string(), "/home/someone-long".to_string()),
            ("SHORT_TOKEN".to_string(), "abc".to_string()),
        ];
        let p = Privacy::with_env(&config, env).unwrap();

        assert_eq!(
            p.redact("FOO_TOKEN=abc123 make"),
            "FOO_TOKEN=[REDACTED] make"
        );
        assert_eq!(
            p.redact(r#"{"npm_token": "xyz"}"#),
            r#"{"npm_token": "[REDACTED]"}"#
        );
        assert_eq!(
            p.redact("connect to mysql://prod-db.internal/app"),
            "connect to [REDACTED]"
        );
        assert_eq!(
            p.redact("using dtk-9f8e7d6c5b4a now"),
            "using [REDACTED] now"
        );
        assert_eq!(p.redact("cd /home/someone-long"), "cd /home/someone-long");
        assert_eq!(p.redact("echo $FOO_TOKEN abc"), "echo $FOO_TOKEN abc");

        config.redaction.env = vec!["BAD-NAME".into()];
        assert!(Privacy::with_env(&config, []).is_err());
    }

    #[test]
    fn scrubs_nested_attributes() {
        let p = privacy(|_| {});
        let mut ev = event();
        ev.set(
            attr::TOOL_INPUT,
            json!({"command": "echo sk-ant-api03-abcdefghijklmnopqrstuv"}),
        );
        p.apply(&mut ev);
        assert_eq!(
            ev.get(attr::TOOL_INPUT).unwrap()["command"],
            "echo [REDACTED]"
        );
    }

    #[test]
    fn excludes_fields_but_keeps_measurements() {
        let p = privacy(|c| c.redaction.exclude = vec!["prompt".into(), "tool.output.body".into()]);
        let mut ev = event();
        ev.set(attr::PROMPT_TEXT, "fix the tests");
        ev.set(attr::PROMPT_BYTES, 13);
        ev.set(attr::TOOL_INPUT, json!({"command": "ls"}));
        ev.set(attr::TOOL_OUTPUT, "secret source code");
        ev.set(attr::TOOL_OUTPUT_BYTES, 18);
        p.apply(&mut ev);
        assert!(ev.get(attr::PROMPT_TEXT).is_none());
        assert!(ev.get(attr::TOOL_OUTPUT).is_none());
        assert_eq!(ev.get(attr::PROMPT_BYTES), Some(&json!(13)));
        assert_eq!(ev.get(attr::TOOL_OUTPUT_BYTES), Some(&json!(18)));
        assert!(ev.get(attr::TOOL_INPUT).is_some());
        assert!(p.raw(&json!({"prompt": "x"})).is_none());
    }

    #[test]
    fn exclude_prefixes_and_typos() {
        let keys = resolve_exclude(&["tool".into(), "gs.shell.command".into()]).unwrap();
        assert_eq!(
            keys,
            [attr::SHELL_COMMAND, attr::TOOL_INPUT, attr::TOOL_OUTPUT]
        );
        let err = resolve_exclude(&["tool.outptu".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("tool.output.body"), "{err}");
        // "prompt.t" is not a whole segment of "prompt.text".
        assert!(resolve_exclude(&["prompt.t".into()]).is_err());
    }

    #[test]
    fn hashes_paths() {
        let p = privacy(|c| c.redaction.paths = PathPolicy::Hash);
        let mut ev = event();
        ev.set(attr::CWD, "/home/u/secret-project");
        ev.set(attr::FILE_PATH, "/home/u/secret-project/src/a.rs");
        ev.set(
            attr::TOOL_INPUT,
            json!({"file_path": "/home/u/secret-project/src/a.rs", "limit": 10}),
        );
        p.apply(&mut ev);
        let file = ev
            .get(attr::FILE_PATH)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(file.starts_with("sha256:") && file.len() == 23, "{file}");
        assert_eq!(ev.get(attr::TOOL_INPUT).unwrap()["file_path"], json!(file));
        assert_eq!(ev.get(attr::TOOL_INPUT).unwrap()["limit"], json!(10));
        assert!(
            !serde_json::to_string(&ev)
                .unwrap()
                .contains("secret-project")
        );
        assert_eq!(p.path("/home/u/secret-project/src/a.rs"), file);
        assert!(p.raw(&json!({})).is_none());
    }

    #[test]
    fn path_hashes_depend_on_the_install_key() {
        let hashed = |key: [u8; 32]| {
            privacy(|c| c.redaction.paths = PathPolicy::Hash)
                .with_path_key(key)
                .path("/Users/ana/src/shop")
        };
        assert_eq!(hashed([1; 32]), hashed([1; 32]));
        assert_ne!(hashed([1; 32]), hashed([2; 32]));
        // Not the plain digest anyone could compute from a guessed path.
        let plain = hex::encode(Sha256::digest(b"/Users/ana/src/shop"));
        assert!(!plain.starts_with(&hashed([1; 32])["sha256:".len()..]));
    }

    #[test]
    fn the_path_key_is_made_once_and_kept_private() {
        let dir = tempfile::tempdir().unwrap();
        let key = path_key(dir.path()).unwrap();
        assert_eq!(path_key(dir.path()).unwrap(), key);
        assert_ne!(key, [0; 32]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("path-key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(dir.path().join("path-key"), b"short").unwrap();
        assert!(path_key(dir.path()).is_err());
    }

    #[test]
    fn truncates_on_char_boundary() {
        let p = privacy(|c| c.capture.max_content_bytes = 5);
        let mut ev = event();
        ev.set(attr::TOOL_OUTPUT, "héllo world");
        p.apply(&mut ev);
        let out = ev.get(attr::TOOL_OUTPUT).unwrap().as_str().unwrap();
        assert!(out.starts_with("héll…[truncated"), "{out}");
    }
}
