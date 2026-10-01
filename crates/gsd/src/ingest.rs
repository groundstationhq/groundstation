//! Everything an event goes through between arriving and being stored.
//! All methods block (SQLite, file and git I/O); call them off the async runtime.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use groundstation_adapter_claude_code::ClaudeCode;
use groundstation_adapter_codex::Codex;
use groundstation_adapter_opencode::OpenCode;
use groundstation_adapter_pi::Pi;
use groundstation_api::{ResyncFailure, ResyncResponse, SpoolItem};
use groundstation_schema::{Adapter, Batch, Event, HookEnvelope, SCHEMA, attr};
use serde_json::Value;

use crate::git::Repo;
use crate::privacy::Privacy;
use crate::store::{NewEvent, Store};

/// Every adapter this build of gsd understands.
pub fn builtin_adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(ClaudeCode),
        Box::new(Codex),
        Box::new(OpenCode),
        Box::new(Pi),
    ]
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
    /// Last working directory seen per trajectory, for events that don't
    /// carry one, and when it was seen.
    cwds: Mutex<HashMap<String, (String, Instant)>>,
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
            cwds: Mutex::default(),
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
        let mut events = batch.events;
        let cwds = raw_cwds(&events);
        self.stamp_checkout(&mut events);
        let stored = self.store_events(events, None)?;
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
        let mut normalized = registered.adapter.normalize(&envelope);
        let Some(trajectory) = normalized.events.first().map(|e| e.trajectory_id.clone()) else {
            bail!("payload not recognized by the {adapter} adapter");
        };
        let cwds = raw_cwds(&normalized.events);
        self.stamp_checkout(&mut normalized.events);
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
            stored += self
                .sync_transcript_coalesced(registered, &f.trajectory, path, false)
                .unwrap_or(0);
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

    /// Re-reads every transcript of `adapter` from the start, so events
    /// already stored pick up what this version of the adapter derives from
    /// them. Model calls are updated in place; other events are write-once.
    pub fn resync(&self, adapter: &str) -> Result<ResyncResponse> {
        let Some(registered) = self.adapter(adapter) else {
            bail!("unknown adapter {adapter:?}");
        };
        // Transcripts read before their owner was recorded are matched to the
        // hooks that named them.
        let owners: HashMap<String, (String, String)> = self
            .store
            .transcript_owners()?
            .into_iter()
            .filter_map(|o| {
                let path = Path::new(&o.path).canonicalize().ok()?;
                Some((
                    path.to_string_lossy().into_owned(),
                    (o.adapter?, o.trajectory_id?),
                ))
            })
            .collect();
        let mut out = ResyncResponse::default();
        for t in self.store.transcripts()? {
            let owner = match (t.adapter, t.trajectory_id) {
                (Some(a), Some(id)) => Some((a, id)),
                _ => owners.get(&t.path).cloned(),
            };
            let Some((owner, trajectory)) = owner else {
                if registered
                    .roots
                    .iter()
                    .any(|r| Path::new(&t.path).starts_with(r))
                {
                    out.unmatched.push(t.path);
                }
                continue;
            };
            if owner != adapter {
                continue;
            }
            match self.sync_transcript_coalesced(registered, &trajectory, &t.path, true) {
                Ok(n) => {
                    out.transcripts += 1;
                    out.events += n;
                }
                Err(e) => out.failed.push(ResyncFailure {
                    path: t.path,
                    error: format!("{e:#}"),
                }),
            }
        }
        Ok(out)
    }

    /// Reads a transcript unless another thread already is, in which case
    /// that thread reads again when it finishes, so no appended line is missed.
    /// `from_start` reads it again from the first line instead, waiting for
    /// any read in progress so the two don't race on the cursor.
    fn sync_transcript_coalesced(
        &self,
        registered: &Registered,
        trajectory: &str,
        path: &str,
        mut from_start: bool,
    ) -> Result<usize> {
        let lock = || self.syncing.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let mut syncing = lock();
            match syncing.get_mut(path) {
                None => {
                    syncing.insert(path.to_string(), false);
                    break;
                }
                Some(again) if !from_start => {
                    *again = true;
                    return Ok(0);
                }
                Some(_) => {
                    drop(syncing);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        let mut result = Ok(0);
        loop {
            match self.sync_transcript(registered, trajectory, Path::new(path), from_start) {
                Ok(n) => {
                    if let Ok(stored) = &mut result {
                        *stored += n;
                    }
                }
                Err(e) => {
                    tracing::debug!(%path, "transcript not read: {e:#}");
                    if result.is_ok() {
                        result = Err(e);
                    }
                }
            }
            from_start = false;
            let mut syncing = lock();
            if syncing.get(path) == Some(&true) {
                syncing.insert(path.to_string(), false);
                continue;
            }
            syncing.remove(path);
            return result;
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

    /// Reads lines appended to a transcript since the last sync, or every
    /// line when `from_start`.
    fn sync_transcript(
        &self,
        registered: &Registered,
        trajectory: &str,
        path: &Path,
        from_start: bool,
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
        if from_start || len < offset {
            // Asked to, or truncated or replaced: start over.
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
        self.store.set_transcript_cursor(
            &key,
            offset + end as u64 + 1,
            &state,
            registered.adapter.name(),
            trajectory,
        )?;
        Ok(stored)
    }

    /// Resolves the git repository of a trajectory's working directory, once
    /// per trajectory: its top-level directory (stored under the path policy
    /// like any other path) and its name outside this machine. `cwd` is the
    /// unredacted path.
    fn enrich_repository(&self, trajectory_id: &str, cwd: &str) {
        if !self.store.take_repo_lookup(trajectory_id).unwrap_or(false) {
            return;
        }
        let Some(repo) = Repo::discover(Path::new(cwd)) else {
            return;
        };
        let repository = self.privacy.path(&repo.root.to_string_lossy());
        // The origin remote (`github.com/acme/shop`), else the directory name.
        let origin = repo
            .origin()
            .or_else(|| Some(repo.root.file_name()?.to_string_lossy().into_owned()))
            .map(|o| self.privacy.path(&o));
        if let Err(e) = self
            .store
            .set_repository(trajectory_id, &repository, origin.as_deref())
        {
            tracing::warn!("storing repository for {trajectory_id}: {e:#}");
        }
    }

    /// Stamps each event with the commit and branch checked out when it
    /// happened, so a trajectory that spans commits records each one. Runs on
    /// events as they arrive (hooks, SDK batches) and before the path policy
    /// can hide the working directory. Reading the repository's files is cheap,
    /// so every event looks. Events read from transcripts later are filled in
    /// by the store from the trajectory's timeline instead.
    fn stamp_checkout(&self, events: &mut [Event]) {
        let mut cwds = self.cwds.lock().unwrap_or_else(|e| e.into_inner());
        for event in events {
            if let Some(cwd) = event.get(attr::CWD).and_then(Value::as_str) {
                cwds.insert(
                    event.trajectory_id.clone(),
                    (cwd.to_string(), Instant::now()),
                );
            }
            if event.get(attr::VCS_REVISION).is_some() {
                continue; // the sender knew better
            }
            let Some((cwd, _)) = cwds.get(&event.trajectory_id) else {
                continue;
            };
            let Some(repo) = Repo::discover(Path::new(cwd)) else {
                continue;
            };
            let head = repo.head();
            if let Some(revision) = head.revision {
                event.set(attr::VCS_REVISION, revision);
            }
            if let Some(branch) = head.branch
                && event.get(attr::VCS_BRANCH).is_none()
            {
                event.set(attr::VCS_BRANCH, branch);
            }
        }
        if cwds.len() > 256 {
            cwds.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(3600));
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
    use std::process::Command;
    use uuid::Uuid;

    fn ingestor() -> Ingestor {
        ingestor_with(Config::default())
    }

    fn ingestor_with(config: Config) -> Ingestor {
        let store = Arc::new(Store::open_in_memory().unwrap());
        Ingestor::new(store, Privacy::with_env(&config, []).unwrap())
    }

    fn sync(ing: &Ingestor, path: &Path) -> Result<usize> {
        ing.sync_transcript(ing.adapter("claude-code").unwrap(), "s", path, false)
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
    fn resync_backfills_transcripts_read_by_an_older_version() {
        let dir = tempfile::tempdir().unwrap();
        let ing = ingestor().with_transcript_root(dir.path());
        let path = dir.path().join("s1.jsonl");
        let assistant = json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1", "timestamp": "2026-09-28T14:00:02Z",
            "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": {"input_tokens": 1, "output_tokens": 100}}
        })
        .to_string();
        std::fs::write(
            &path,
            format!(
                "{}\n{assistant}\n",
                json!({"type": "user", "uuid": "u1", "timestamp": "2026-09-28T14:00:00Z"})
            ),
        )
        .unwrap();
        let start = HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload: json!({
                "session_id": "s1", "hook_event_name": "SessionStart", "source": "startup",
                "transcript_path": path.to_str().unwrap(),
            }),
        };
        ing.ingest_hook_now("claude-code", start).unwrap();
        let duration = |ing: &Ingestor| {
            let (_, events) = ing.store.trajectory("s1").unwrap().unwrap();
            let model = events
                .iter()
                .find(|e| e.kind == groundstation_schema::EventKind::ModelCompleted)
                .unwrap();
            model.get(attr::DURATION_MS).cloned()
        };
        assert_eq!(duration(&ing), Some(json!(2000)));

        // What an older adapter stored: the same event without a duration,
        // from a transcript whose owner wasn't recorded.
        let old = ClaudeCode
            .parse_transcript_line("s1", &assistant, &mut serde_json::Value::Null)
            .unwrap();
        ing.store
            .insert(vec![NewEvent {
                event: old,
                raw: None,
                host: None,
            }])
            .unwrap();
        ing.store.forget_transcript_owners();
        assert_eq!(duration(&ing), None);
        // A transcript no stored hook names.
        let orphan = dir.path().join("orphan.jsonl");
        std::fs::write(&orphan, format!("{assistant}\n")).unwrap();
        sync(&ing, &orphan).unwrap();
        ing.store.forget_transcript_owners();

        assert_eq!(ing.resync("codex").unwrap().transcripts, 0);
        let r = ing.resync("claude-code").unwrap();
        assert_eq!((r.transcripts, r.events), (1, 1));
        assert!(r.failed.is_empty());
        assert_eq!(r.unmatched.len(), 1);
        assert!(r.unmatched[0].ends_with("orphan.jsonl"));
        assert_eq!(duration(&ing), Some(json!(2000)));

        // The owner is recorded now, so the next resync needs no hook lookup.
        let owned = ing.store.transcripts().unwrap();
        let t = owned.iter().find(|t| t.path.ends_with("s1.jsonl")).unwrap();
        assert_eq!(
            (t.adapter.as_deref(), t.trajectory_id.as_deref()),
            (Some("claude-code"), Some("s1"))
        );
        assert_eq!(ing.resync("claude-code").unwrap().events, 0);
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
    fn events_record_the_commit_checked_out_when_they_happened() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .status()
                .is_ok_and(|s| s.success())
        };
        if !git(&["init", "-q", "-b", "main"]) {
            return; // git unavailable
        }
        let head = || {
            let out = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let cwd = repo.path().canonicalize().unwrap().display().to_string();
        let ing = ingestor();
        let hook = |payload: serde_json::Value| HookEnvelope {
            id: Uuid::now_v7(),
            observed_at: Utc::now(),
            payload,
        };

        assert!(git(&["commit", "-q", "--allow-empty", "-m", "first"]));
        let first = head();
        ing.ingest_hook_now(
            "claude-code",
            hook(json!({"session_id": "s1", "hook_event_name": "SessionStart", "cwd": cwd})),
        )
        .unwrap();

        // A new commit within the cache window still shows up on the next
        // user turn, which always looks again.
        assert!(git(&["checkout", "-q", "-b", "feat/x"]));
        assert!(git(&["commit", "-q", "--allow-empty", "-m", "second"]));
        let second = head();
        ing.ingest_hook_now(
            "claude-code",
            hook(json!({"session_id": "s1", "hook_event_name": "UserPromptSubmit", "cwd": cwd, "prompt": "go"})),
        )
        .unwrap();

        let (summary, events) = ing.store.trajectory("s1").unwrap().unwrap();
        let checkout = |e: &Event| {
            (
                e.get(attr::VCS_REVISION)
                    .and_then(Value::as_str)
                    .map(str::to_string),
                e.get(attr::VCS_BRANCH)
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        };
        assert_eq!(checkout(&events[0]), (Some(first), Some("main".into())));
        assert_eq!(
            checkout(events.last().unwrap()),
            (Some(second.clone()), Some("feat/x".into()))
        );
        assert_eq!(summary.revision, Some(second));
        assert_eq!(summary.branch.as_deref(), Some("feat/x"));
    }

    #[test]
    fn a_booby_trapped_repository_runs_nothing() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .status()
                .is_ok_and(|s| s.success())
        };
        if !git(&["init", "-q", "-b", "main"]) {
            return; // git unavailable
        }
        let marker = repo.path().join("ran");
        let trap = format!("touch {}; true", marker.display());
        for key in [
            "core.fsmonitor",
            "core.pager",
            "core.sshCommand",
            "diff.external",
        ] {
            assert!(git(&["config", key, &trap]));
        }
        assert!(git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:acme/shop.git"
        ]));

        let cwd = repo.path().display().to_string();
        let ing = ingestor();
        ing.ingest_hook_now(
            "claude-code",
            HookEnvelope {
                id: Uuid::now_v7(),
                observed_at: Utc::now(),
                payload: json!({"session_id": "s1", "hook_event_name": "UserPromptSubmit", "cwd": cwd, "prompt": "go"}),
            },
        )
        .unwrap();
        assert!(
            !marker.exists(),
            "gsd ran something the repository configured"
        );
        let (summary, _) = ing.store.trajectory("s1").unwrap().unwrap();
        assert_eq!(summary.branch.as_deref(), Some("main"));
        let place = &ing.store.pending_upload(1).unwrap()[0].place;
        assert_eq!(place.origin.as_deref(), Some("github.com/acme/shop"));
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
