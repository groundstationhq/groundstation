//! Everything an event goes through between arriving and being stored.
//! All methods block (SQLite, file and git I/O); call them off the async runtime.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use groundstation_schema::{Batch, Event, SCHEMA, attr};

use crate::adapters::claude_code;
use crate::api::{HookEnvelope, SpoolItem};
use crate::privacy::Privacy;
use crate::store::{NewEvent, Store};

pub struct Ingestor {
    pub store: Arc<Store>,
    privacy: Privacy,
    host: String,
    /// Directories transcripts may be read from. Paths arrive over HTTP, so
    /// the daemon refuses to open anything else.
    transcript_roots: Vec<PathBuf>,
}

impl Ingestor {
    pub fn new(store: Arc<Store>, privacy: Privacy) -> Self {
        let mut roots = vec![crate::config::home_dir().join(".claude")];
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            roots.push(PathBuf::from(dir));
        }
        let roots = roots
            .into_iter()
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        Self {
            store,
            privacy,
            host: gethostname::gethostname().to_string_lossy().into_owned(),
            transcript_roots: roots,
        }
    }

    #[cfg(test)]
    pub fn with_transcript_root(mut self, root: &Path) -> Self {
        self.transcript_roots.push(root.canonicalize().unwrap());
        self
    }

    pub fn ingest_batch(&self, batch: Batch) -> Result<usize> {
        if batch.schema != SCHEMA {
            bail!("unsupported schema {:?}, expected {SCHEMA:?}", batch.schema);
        }
        // Captured before the path policy can hash the working directory.
        let mut cwds: Vec<(String, String)> = batch
            .events
            .iter()
            .filter_map(|e| {
                let cwd = e.get(attr::CWD)?.as_str()?;
                Some((e.trajectory_id.clone(), cwd.to_string()))
            })
            .collect();
        cwds.sort();
        cwds.dedup_by(|a, b| a.0 == b.0);
        let stored = self.store_events(batch.events, None)?;
        for (id, cwd) in cwds {
            self.enrich_repository(&id, &cwd);
        }
        Ok(stored)
    }

    pub fn ingest_claude_code(&self, envelope: HookEnvelope) -> Result<usize> {
        let normalized = claude_code::normalize(&envelope);
        let Some(session) = normalized.events.first().map(|e| e.trajectory_id.clone()) else {
            bail!("not a Claude Code hook payload (missing session_id or hook_event_name)");
        };
        let mut stored = self.store_events(normalized.events, Some(&envelope.payload))?;
        for path in normalized.transcripts {
            match self.sync_transcript(&session, Path::new(&path)) {
                Ok(n) => stored += n,
                Err(e) => tracing::debug!(%path, "transcript not read: {e:#}"),
            }
        }
        if let Some(cwd) = envelope.payload.get("cwd").and_then(|c| c.as_str()) {
            self.enrich_repository(&session, cwd);
        }
        Ok(stored)
    }

    pub fn ingest_spooled(&self, item: SpoolItem) -> Result<usize> {
        match item {
            SpoolItem::ClaudeCode(envelope) => self.ingest_claude_code(envelope),
            SpoolItem::Batch(batch) => self.ingest_batch(batch),
        }
    }

    fn store_events(&self, events: Vec<Event>, raw: Option<&serde_json::Value>) -> Result<usize> {
        let raw = raw.and_then(|r| self.privacy.raw(r));
        let events = events
            .into_iter()
            .map(|mut event| {
                self.privacy.apply(&mut event);
                NewEvent {
                    event,
                    raw: raw.clone(),
                    host: Some(self.host.clone()),
                }
            })
            .collect();
        self.store.insert(events)
    }

    /// Reads model responses appended to a transcript since the last sync.
    fn sync_transcript(&self, session: &str, path: &Path) -> Result<usize> {
        let path = path
            .canonicalize()
            .with_context(|| format!("resolving {}", path.display()))?;
        let allowed = path.extension().is_some_and(|e| e == "jsonl")
            && self
                .transcript_roots
                .iter()
                .any(|root| path.starts_with(root));
        if !allowed {
            bail!("refusing to read transcript outside the Claude config directory");
        }
        let key = path.to_string_lossy();
        let mut file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len();
        let mut offset = self.store.transcript_offset(&key)?;
        if len < offset {
            offset = 0; // truncated or replaced
        }
        if len == offset {
            return Ok(0);
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::with_capacity((len - offset) as usize);
        file.take(len - offset).read_to_end(&mut buf)?;
        // Only consume complete lines; a partially written one is read next time.
        let Some(end) = buf.iter().rposition(|b| *b == b'\n') else {
            return Ok(0);
        };
        let events: Vec<Event> = String::from_utf8_lossy(&buf[..end])
            .lines()
            .filter_map(|line| claude_code::parse_transcript_line(session, line))
            .collect();
        let stored = self.store_events(events, None)?;
        self.store
            .set_transcript_offset(&key, offset + end as u64 + 1)?;
        Ok(stored)
    }

    /// Resolves the git repository and branch of a trajectory's working
    /// directory, once per trajectory. `cwd` is the unredacted path; the
    /// repository is stored under the path policy like any other path.
    fn enrich_repository(&self, trajectory_id: &str, cwd: &str) {
        if !self.store.take_repo_lookup(trajectory_id).unwrap_or(false) {
            return;
        }
        let git = |args: &[&str]| -> Option<String> {
            let out = Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .ok()?;
            let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
            (out.status.success() && !s.is_empty()).then_some(s)
        };
        let Some(repository) = git(&["rev-parse", "--show-toplevel"]) else {
            return;
        };
        let repository = self.privacy.path(&repository);
        let branch = git(&["branch", "--show-current"]);
        if let Err(e) =
            self.store
                .set_repository(trajectory_id, Some(&repository), branch.as_deref())
        {
            tracing::warn!("storing repository for {trajectory_id}: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, PathPolicy};
    use chrono::Utc;
    use serde_json::json;
    use std::io::Write;
    use uuid::Uuid;

    fn ingestor() -> Ingestor {
        ingestor_with(Config::default())
    }

    fn ingestor_with(config: Config) -> Ingestor {
        let store = Arc::new(Store::open_in_memory().unwrap());
        Ingestor::new(store, Privacy::with_env(&config, []).unwrap())
    }

    fn assistant_line(id: &str, out: u64) -> String {
        json!({
            "type": "assistant", "timestamp": "2026-09-28T14:03:14Z",
            "message": {"id": id, "model": "claude-opus-5-5", "usage": {"input_tokens": 1, "output_tokens": out}}
        })
        .to_string()
    }

    #[test]
    fn reads_transcripts_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let ing = ingestor().with_transcript_root(dir.path());
        let path = dir.path().join("sess.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", assistant_line("msg_1", 10)).unwrap();
        writeln!(f, "{}", assistant_line("msg_1", 10)).unwrap(); // second content block
        write!(f, "{}", &assistant_line("msg_2", 5)[..20]).unwrap(); // partial line
        f.flush().unwrap();

        assert_eq!(ing.sync_transcript("s", &path).unwrap(), 1);
        assert_eq!(ing.sync_transcript("s", &path).unwrap(), 0);

        writeln!(f, "{}", &assistant_line("msg_2", 5)[20..]).unwrap();
        f.flush().unwrap();
        assert_eq!(ing.sync_transcript("s", &path).unwrap(), 1);

        let (summary, _) = ing.store.trajectory("s").unwrap().unwrap();
        assert_eq!(summary.model_calls, 2);
        assert_eq!(summary.output_tokens, 15);
    }

    #[test]
    fn refuses_transcripts_outside_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        std::fs::write(&path, assistant_line("m", 1) + "\n").unwrap();
        assert!(ingestor().sync_transcript("s", &path).is_err());
    }

    #[test]
    fn claude_code_hook_end_to_end() {
        let ing = ingestor();
        let envelope = HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload: json!({
                "session_id": "s1", "hook_event_name": "UserPromptSubmit", "cwd": "/nonexistent",
                "prompt": "deploy with token ghp_0123456789abcdefghijklmnopqrstuvwxyz",
            }),
        };
        assert_eq!(ing.ingest_claude_code(envelope.clone()).unwrap(), 1);
        assert_eq!(ing.ingest_claude_code(envelope).unwrap(), 0);
        let (summary, events) = ing.store.trajectory("s1").unwrap().unwrap();
        assert_eq!(
            summary.title.as_deref(),
            Some("deploy with token [REDACTED]")
        );
        assert_eq!(summary.user_turns, 1);
        assert!(!serde_json::to_string(&events).unwrap().contains("ghp_"));
    }

    #[test]
    fn hashed_paths_still_resolve_repository() {
        let repo = tempfile::tempdir().unwrap();
        let ok = Command::new("git")
            .args(["init", "-q", "-b", "fix/checkout"])
            .arg(repo.path())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return; // git unavailable
        }
        let cwd = repo.path().canonicalize().unwrap().display().to_string();
        let mut config = Config::default();
        config.redaction.paths = PathPolicy::Hash;
        let ing = ingestor_with(config);
        let envelope = HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload: json!({"session_id": "s1", "hook_event_name": "Stop", "cwd": cwd}),
        };
        ing.ingest_claude_code(envelope).unwrap();
        let (summary, events) = ing.store.trajectory("s1").unwrap().unwrap();
        assert_eq!(summary.branch.as_deref(), Some("fix/checkout"));
        let stored_repo = summary.repository.unwrap();
        assert!(stored_repo.starts_with("sha256:"), "{stored_repo}");
        assert_eq!(summary.cwd.as_deref(), Some(stored_repo.as_str()));
        assert!(!serde_json::to_string(&events).unwrap().contains(&cwd));
    }
}
