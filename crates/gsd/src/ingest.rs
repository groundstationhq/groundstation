//! Everything an event goes through between arriving and being stored.
//! All methods block (SQLite, file and git I/O); call them off the async runtime.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use groundstation_adapter_claude_code::ClaudeCode;
use groundstation_schema::{Adapter, Batch, Event, HookEnvelope, SCHEMA, attr};

use crate::api::SpoolItem;
use crate::privacy::Privacy;
use crate::store::{NewEvent, Store};

/// Every adapter this build of gsd understands.
pub fn builtin_adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(ClaudeCode)]
}

struct Registered {
    adapter: Box<dyn Adapter>,
    /// Canonicalized [`Adapter::transcript_roots`].
    roots: Vec<PathBuf>,
}

pub struct Ingestor {
    pub store: Arc<Store>,
    privacy: Privacy,
    host: String,
    adapters: Vec<Registered>,
}

impl Ingestor {
    pub fn new(store: Arc<Store>, privacy: Privacy) -> Self {
        let adapters = builtin_adapters()
            .into_iter()
            .map(|adapter| Registered {
                roots: adapter
                    .transcript_roots()
                    .into_iter()
                    .filter_map(|p| p.canonicalize().ok())
                    .collect(),
                adapter,
            })
            .collect();
        Self {
            store,
            privacy,
            host: gethostname::gethostname().to_string_lossy().into_owned(),
            adapters,
        }
    }

    #[cfg(test)]
    pub fn with_transcript_root(mut self, root: &Path) -> Self {
        for registered in &mut self.adapters {
            registered.roots.push(root.canonicalize().unwrap());
        }
        self
    }

    pub fn adapter_names(&self) -> Vec<&'static str> {
        self.adapters.iter().map(|r| r.adapter.name()).collect()
    }

    pub fn has_adapter(&self, name: &str) -> bool {
        self.adapter(name).is_some()
    }

    fn adapter(&self, name: &str) -> Option<&Registered> {
        self.adapters.iter().find(|r| r.adapter.name() == name)
    }

    pub fn ingest_batch(&self, batch: Batch) -> Result<usize> {
        if batch.schema != SCHEMA {
            bail!("unsupported schema {:?}, expected {SCHEMA:?}", batch.schema);
        }
        let cwds = raw_cwds(&batch.events);
        let stored = self.store_events(batch.events, None)?;
        for (id, cwd) in cwds {
            self.enrich_repository(&id, &cwd);
        }
        Ok(stored)
    }

    /// Normalizes a hook payload with the named adapter, stores the events,
    /// then reads any transcripts the payload points at.
    pub fn ingest_hook(&self, adapter: &str, envelope: HookEnvelope) -> Result<usize> {
        let Some(registered) = self.adapter(adapter) else {
            bail!("unknown adapter {adapter:?}");
        };
        let normalized = registered.adapter.normalize(&envelope);
        let Some(trajectory) = normalized.events.first().map(|e| e.trajectory_id.clone()) else {
            bail!("payload not recognized by the {adapter} adapter");
        };
        let cwds = raw_cwds(&normalized.events);
        let mut stored = self.store_events(normalized.events, Some(&envelope.payload))?;
        for path in normalized.transcripts {
            match self.sync_transcript(registered, &trajectory, Path::new(&path)) {
                Ok(n) => stored += n,
                Err(e) => tracing::debug!(%path, "transcript not read: {e:#}"),
            }
        }
        for (id, cwd) in cwds {
            self.enrich_repository(&id, &cwd);
        }
        Ok(stored)
    }

    pub fn ingest_spooled(&self, item: SpoolItem) -> Result<usize> {
        match item {
            SpoolItem::Hook { adapter, envelope } => self.ingest_hook(&adapter, envelope),
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

    /// Reads lines appended to a transcript since the last sync.
    fn sync_transcript(
        &self,
        registered: &Registered,
        trajectory: &str,
        path: &Path,
    ) -> Result<usize> {
        let path = path
            .canonicalize()
            .with_context(|| format!("resolving {}", path.display()))?;
        let allowed = path.extension().is_some_and(|e| e == "jsonl")
            && registered.roots.iter().any(|root| path.starts_with(root));
        if !allowed {
            bail!(
                "refusing to read a transcript outside the {} adapter's directories",
                registered.adapter.name()
            );
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
            .filter_map(|line| registered.adapter.parse_transcript_line(trajectory, line))
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

/// The working directory of each trajectory, captured before the path
/// policy can hash it, for the git lookup.
fn raw_cwds(events: &[Event]) -> Vec<(String, String)> {
    let mut cwds: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| {
            let cwd = e.get(attr::CWD)?.as_str()?;
            Some((e.trajectory_id.clone(), cwd.to_string()))
        })
        .collect();
    cwds.sort();
    cwds.dedup_by(|a, b| a.0 == b.0);
    cwds
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

    fn sync(ing: &Ingestor, path: &Path) -> Result<usize> {
        ing.sync_transcript(ing.adapter("claude-code").unwrap(), "s", path)
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

        assert_eq!(sync(&ing, &path).unwrap(), 1);
        assert_eq!(sync(&ing, &path).unwrap(), 0);

        writeln!(f, "{}", &assistant_line("msg_2", 5)[20..]).unwrap();
        f.flush().unwrap();
        assert_eq!(sync(&ing, &path).unwrap(), 1);

        let (summary, _) = ing.store.trajectory("s").unwrap().unwrap();
        assert_eq!(summary.model_calls, 2);
        assert_eq!(summary.output_tokens, 15);
    }

    #[test]
    fn refuses_transcripts_outside_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        std::fs::write(&path, assistant_line("m", 1) + "\n").unwrap();
        assert!(sync(&ingestor(), &path).is_err());
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
        assert_eq!(ing.ingest_hook("claude-code", envelope.clone()).unwrap(), 1);
        assert_eq!(ing.ingest_hook("claude-code", envelope).unwrap(), 0);
        let (summary, events) = ing.store.trajectory("s1").unwrap().unwrap();
        assert_eq!(
            summary.title.as_deref(),
            Some("deploy with token [REDACTED]")
        );
        assert_eq!(summary.user_turns, 1);
        assert!(!serde_json::to_string(&events).unwrap().contains("ghp_"));
    }

    #[test]
    fn unknown_adapters_are_rejected() {
        let envelope = HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload: json!({}),
        };
        let err = ingestor().ingest_hook("codex", envelope).unwrap_err();
        assert!(err.to_string().contains("unknown adapter"), "{err}");
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
        ing.ingest_hook("claude-code", envelope).unwrap();
        let (summary, events) = ing.store.trajectory("s1").unwrap().unwrap();
        assert_eq!(summary.branch.as_deref(), Some("fix/checkout"));
        let stored_repo = summary.repository.unwrap();
        assert!(stored_repo.starts_with("sha256:"), "{stored_repo}");
        assert_eq!(summary.cwd.as_deref(), Some(stored_repo.as_str()));
        assert!(!serde_json::to_string(&events).unwrap().contains(&cwd));
    }
}
