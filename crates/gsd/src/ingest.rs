//! Everything an event goes through between arriving and being stored.
//! All methods block (SQLite, file and git I/O); call them off the async runtime.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use groundstation_adapter_claude_code::ClaudeCode;
use groundstation_adapter_codex::Codex;
use groundstation_adapter_opencode::OpenCode;
use groundstation_schema::{Adapter, Batch, Event, HookEnvelope, SCHEMA, attr};

use crate::api::SpoolItem;
use crate::privacy::Privacy;
use crate::store::{NewEvent, Store};

/// Every adapter this build of gsd understands.
pub fn builtin_adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(ClaudeCode), Box::new(Codex), Box::new(OpenCode)]
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
    /// Transcripts being read right now, and whether another read was
    /// requested meanwhile. Concurrent hooks for one session coalesce into
    /// one reader instead of re-reading the same bytes in parallel.
    syncing: Mutex<HashMap<String, bool>>,
}

/// Work left after a hook's events are stored: reading transcripts and
/// resolving the repository. It can be slow (a long transcript's first read,
/// git), so the HTTP handler runs it after replying to the hook.
#[must_use = "call Ingestor::follow_up"]
pub struct FollowUp {
    adapter: &'static str,
    trajectory: String,
    transcripts: Vec<String>,
    cwds: Vec<(String, String)>,
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
            syncing: Mutex::default(),
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

    /// Normalizes a hook payload with the named adapter and stores the
    /// events. Returns how many were stored and the [`FollowUp`] to run next.
    pub fn ingest_hook(&self, adapter: &str, envelope: HookEnvelope) -> Result<(usize, FollowUp)> {
        let Some(registered) = self.adapter(adapter) else {
            bail!("unknown adapter {adapter:?}");
        };
        let normalized = registered.adapter.normalize(&envelope);
        let Some(trajectory) = normalized.events.first().map(|e| e.trajectory_id.clone()) else {
            bail!("payload not recognized by the {adapter} adapter");
        };
        let cwds = raw_cwds(&normalized.events);
        let stored = self.store_events(normalized.events, Some(&envelope.payload))?;
        let follow_up = FollowUp {
            adapter: registered.adapter.name(),
            trajectory,
            transcripts: normalized.transcripts,
            cwds,
        };
        Ok((stored, follow_up))
    }

    /// [`Self::ingest_hook`] and its [`FollowUp`], in one call.
    pub fn ingest_hook_now(&self, adapter: &str, envelope: HookEnvelope) -> Result<usize> {
        let (stored, follow_up) = self.ingest_hook(adapter, envelope)?;
        Ok(stored + self.follow_up(follow_up))
    }

    /// Reads the transcripts a hook pointed at and resolves the repository.
    /// Returns how many transcript events were stored. Never fails: this runs
    /// detached, and the next hook retries anything that went wrong.
    pub fn follow_up(&self, f: FollowUp) -> usize {
        let Some(registered) = self.adapter(f.adapter) else {
            return 0;
        };
        let mut stored = 0;
        for path in &f.transcripts {
            stored += self.sync_transcript_coalesced(registered, &f.trajectory, path);
        }
        for (id, cwd) in &f.cwds {
            self.enrich_repository(id, cwd);
        }
        stored
    }

    pub fn ingest_spooled(&self, item: SpoolItem) -> Result<usize> {
        match item {
            SpoolItem::Hook { adapter, envelope } => self.ingest_hook_now(&adapter, envelope),
            SpoolItem::Batch(batch) => self.ingest_batch(batch),
        }
    }

    /// Reads a transcript unless another thread already is, in which case
    /// that thread reads again when it finishes, so no appended line is missed.
    fn sync_transcript_coalesced(
        &self,
        registered: &Registered,
        trajectory: &str,
        path: &str,
    ) -> usize {
        let lock = || self.syncing.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut syncing = lock();
            if let Some(again) = syncing.get_mut(path) {
                *again = true;
                return 0;
            }
            syncing.insert(path.to_string(), false);
        }
        let mut stored = 0;
        loop {
            match self.sync_transcript(registered, trajectory, Path::new(path)) {
                Ok(n) => stored += n,
                Err(e) => tracing::debug!(%path, "transcript not read: {e:#}"),
            }
            let mut syncing = lock();
            if syncing.get(path) == Some(&true) {
                syncing.insert(path.to_string(), false);
                continue;
            }
            syncing.remove(path);
            return stored;
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
        let (mut offset, mut state) = self.store.transcript_cursor(&key)?;
        if len < offset {
            // Truncated or replaced: start over.
            offset = 0;
            state = serde_json::Value::Null;
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
            .filter_map(|line| {
                registered
                    .adapter
                    .parse_transcript_line(trajectory, line, &mut state)
            })
            .collect();
        let stored = self.store_events(events, None)?;
        self.store
            .set_transcript_cursor(&key, offset + end as u64 + 1, &state)?;
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
        assert_eq!(
            ing.ingest_hook_now("claude-code", envelope.clone())
                .unwrap(),
            1
        );
        assert_eq!(ing.ingest_hook_now("claude-code", envelope).unwrap(), 0);
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
        let err = ingestor().ingest_hook_now("nope", envelope).unwrap_err();
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
        ing.ingest_hook_now("claude-code", envelope).unwrap();
        let (summary, events) = ing.store.trajectory("s1").unwrap().unwrap();
        assert_eq!(summary.branch.as_deref(), Some("fix/checkout"));
        let stored_repo = summary.repository.unwrap();
        assert!(stored_repo.starts_with("sha256:"), "{stored_repo}");
        assert_eq!(summary.cwd.as_deref(), Some(stored_repo.as_str()));
        assert!(!serde_json::to_string(&events).unwrap().contains(&cwd));
    }

    #[test]
    fn codex_rollout_state_survives_between_reads() {
        let dir = tempfile::tempdir().unwrap();
        let ing = ingestor().with_transcript_root(dir.path());
        let rollout = dir.path().join("rollout.jsonl");
        let line = |v: serde_json::Value| v.to_string() + "\n";
        std::fs::write(
            &rollout,
            line(json!({"timestamp": "2026-09-28T14:03:11Z", "type": "session_meta", "payload": {"cli_version": "0.155.0"}}))
                + &line(json!({"timestamp": "2026-09-28T14:03:11Z", "type": "turn_context", "payload": {"model": "gpt-6-codex"}})),
        )
        .unwrap();
        let hook = |name: &str| HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload: json!({"session_id": "cx", "hook_event_name": name, "transcript_path": rollout}),
        };
        ing.ingest_hook_now("codex", hook("SessionStart")).unwrap();

        // The usage record arrives in a later read; the model comes from saved state.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap();
        write!(
            f,
            "{}",
            line(json!({"timestamp": "2026-09-28T14:03:13Z", "type": "token_usage_record", "payload": {
                "session_id": "cx", "thread_id": "cx", "response_id": "resp_1",
                "usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 7}}}))
        )
        .unwrap();
        assert_eq!(ing.ingest_hook_now("codex", hook("Stop")).unwrap(), 2);

        let (summary, events) = ing.store.trajectory("cx").unwrap().unwrap();
        assert_eq!(summary.agent, "codex");
        assert_eq!(summary.agent_version.as_deref(), Some("0.155.0"));
        assert_eq!(
            (
                summary.input_tokens,
                summary.cache_read_tokens,
                summary.output_tokens
            ),
            (40, 60, 7)
        );
        let model = events
            .iter()
            .find(|e| e.kind == groundstation_schema::EventKind::ModelCompleted)
            .unwrap();
        assert_eq!(
            model.get(attr::GEN_AI_RESPONSE_MODEL),
            Some(&json!("gpt-6-codex"))
        );
    }

    #[test]
    fn opencode_cost_and_subagents_roll_up() {
        let ing = ingestor();
        let event = |id: &str,
                     created: i64,
                     session: &str,
                     kind: &str,
                     data: serde_json::Value,
                     extra: serde_json::Value| {
            let mut payload = json!({
                "event": {"id": id, "created": created, "type": kind, "data": data},
                "root_session_id": "ses_root", "directory": "/nonexistent",
            });
            payload["event"]["data"]["sessionID"] = json!(session);
            payload
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            HookEnvelope {
                id: Uuid::now_v7(),
                observed_at: Utc::now(),
                payload,
            }
        };
        let usage = |cost: f64| {
            json!({"assistantMessageID": format!("m{cost}"), "cost": cost,
            "tokens": {"input": 10, "output": 5, "reasoning": 0, "cache": {"read": 100, "write": 0}}})
        };
        let t0 = 1_790_604_191_000;
        for env in [
            event(
                "evt_1",
                t0,
                "ses_root",
                "session.created",
                json!({"version": "2.0.19"}),
                json!({}),
            ),
            event(
                "evt_2",
                t0 + 10,
                "ses_root",
                "session.step.ended",
                usage(0.25),
                json!({"step": {"started": t0 + 5}}),
            ),
            event(
                "evt_3",
                t0 + 20,
                "ses_child",
                "session.created",
                json!({"parentID": "ses_root", "agent": "explore"}),
                json!({}),
            ),
            event(
                "evt_4",
                t0 + 30,
                "ses_child",
                "session.step.ended",
                usage(0.5),
                json!({}),
            ),
            event(
                "evt_5",
                t0 + 40,
                "ses_root",
                "session.execution.succeeded",
                json!({}),
                json!({}),
            ),
        ] {
            ing.ingest_hook_now("opencode", env).unwrap();
        }
        let (summary, _) = ing.store.trajectory("ses_root").unwrap().unwrap();
        assert_eq!(summary.agent_version.as_deref(), Some("2.0.19"));
        assert_eq!(summary.model_calls, 2);
        assert!(
            (summary.cost_usd - 0.75).abs() < 1e-9,
            "{}",
            summary.cost_usd
        );
        assert_eq!(summary.cache_read_tokens, 200);
        assert_eq!(summary.status, "idle");
        assert!(
            ing.store.trajectory("ses_child").unwrap().is_none(),
            "child folded into root"
        );
    }
}
