//! Local persistence: the durable buffer between agents and the backend, and
//! the source for the local query API.
//!
//! Every write is idempotent. Event ids are stable across redeliveries, so
//! the spool, transcript re-reads and hook retries can all replay freely.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use groundstation_schema::{Event, EventKind, attr};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::api::TrajectorySummary;

const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE trajectories (
    id              TEXT PRIMARY KEY,
    agent           TEXT NOT NULL,
    agent_version   TEXT,
    title           TEXT,
    status          TEXT NOT NULL,
    cwd             TEXT,
    repository      TEXT,
    branch          TEXT,
    host            TEXT,
    repo_checked    INTEGER NOT NULL DEFAULT 0,
    started_ns      INTEGER NOT NULL,
    updated_ns      INTEGER NOT NULL,
    ended_ns        INTEGER
);
CREATE INDEX trajectories_updated ON trajectories(updated_ns DESC);

CREATE TABLE events (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    id              TEXT NOT NULL UNIQUE,
    trajectory_id   TEXT NOT NULL,
    kind            TEXT NOT NULL,
    ts_ns           INTEGER NOT NULL,
    span_id         TEXT,
    parent_span_id  TEXT,
    agent           TEXT NOT NULL,
    agent_version   TEXT,
    attributes      TEXT NOT NULL,
    raw             TEXT,
    received_ns     INTEGER NOT NULL,
    uploaded        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX events_trajectory ON events(trajectory_id, ts_ns);
CREATE INDEX events_span ON events(trajectory_id, span_id) WHERE span_id IS NOT NULL;
CREATE INDEX events_pending ON events(seq) WHERE uploaded = 0;

CREATE TABLE transcripts (
    path    TEXT PRIMARY KEY,
    offset  INTEGER NOT NULL
);
"#,
    // Adapter parse state carried between transcript reads (e.g. Codex's current model).
    "ALTER TABLE transcripts ADD COLUMN state TEXT;",
];

pub struct Store {
    conn: Mutex<Connection>,
}

/// An event plus the agent payload it was derived from, if retained.
pub struct NewEvent {
    pub event: Event,
    pub raw: Option<Value>,
    pub host: Option<String>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
            conn.execute_batch(sql)
                .with_context(|| format!("migration {}", i + 1))?;
            conn.pragma_update(None, "user_version", i as i64 + 1)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite inconsistent
        // (transactions roll back on drop), so poisoning is safe to ignore.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Inserts events in order, filling in span durations and trajectory
    /// metadata. Returns how many events were new or changed.
    pub fn insert(&self, events: Vec<NewEvent>) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = ns(Utc::now());
        let mut stored = 0;
        for NewEvent {
            mut event,
            raw,
            host,
        } in events
        {
            let ts = ns(event.timestamp);
            if event.kind.ends_span()
                && event.get(attr::DURATION_MS).is_none()
                && let Some(span) = &event.span_id
            {
                let started: Option<i64> = tx
                    .query_row(
                        "SELECT ts_ns FROM events
                             WHERE trajectory_id = ?1 AND span_id = ?2 AND kind LIKE '%.started'
                             ORDER BY ts_ns LIMIT 1",
                        params![event.trajectory_id, span],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(started) = started {
                    event.set(attr::DURATION_MS, (ts - started).max(0) / 1_000_000);
                }
            }

            let attributes = serde_json::to_string(&event.attributes)?;
            let raw = raw.map(|r| r.to_string());
            // Model calls are re-read from growing transcripts; the latest
            // version of a message wins. Everything else is write-once.
            let changed = tx.execute(
                "INSERT INTO events (id, trajectory_id, kind, ts_ns, span_id, parent_span_id,
                                     agent, agent_version, attributes, raw, received_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                     ts_ns = excluded.ts_ns, attributes = excluded.attributes,
                     agent_version = excluded.agent_version, uploaded = 0
                 WHERE excluded.kind = 'model.completed' AND events.attributes != excluded.attributes",
                params![
                    event.id.to_string(),
                    event.trajectory_id,
                    event.kind.as_str(),
                    ts,
                    event.span_id,
                    event.parent_span_id,
                    event.agent.name,
                    event.agent.version,
                    attributes,
                    raw,
                    now,
                ],
            )?;
            if changed == 0 {
                continue;
            }
            stored += 1;
            upsert_trajectory(&tx, &event, ts, host.as_deref())?;
        }
        tx.commit()?;
        Ok(stored)
    }

    /// Claims the one-time repository lookup for a trajectory. Returns `true`
    /// the first time it is called for a stored trajectory, `false` after.
    pub fn take_repo_lookup(&self, trajectory_id: &str) -> Result<bool> {
        let changed = self.conn().execute(
            "UPDATE trajectories SET repo_checked = 1 WHERE id = ?1 AND repo_checked = 0",
            params![trajectory_id],
        )?;
        Ok(changed == 1)
    }

    pub fn set_repository(
        &self,
        id: &str,
        repository: Option<&str>,
        branch: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE trajectories SET repository = COALESCE(?2, repository), branch = COALESCE(?3, branch)
             WHERE id = ?1",
            params![id, repository, branch],
        )?;
        Ok(())
    }

    /// How far a transcript has been read, and the adapter's parse state at that point.
    pub fn transcript_cursor(&self, path: &str) -> Result<(u64, Value)> {
        let row: Option<(i64, Option<String>)> = self
            .conn()
            .query_row(
                "SELECT offset, state FROM transcripts WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((offset, state)) = row else {
            return Ok((0, Value::Null));
        };
        let state = state
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        Ok((offset.max(0) as u64, state))
    }

    pub fn set_transcript_cursor(&self, path: &str, offset: u64, state: &Value) -> Result<()> {
        let state = (!state.is_null()).then(|| state.to_string());
        self.conn().execute(
            "INSERT INTO transcripts (path, offset, state) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET offset = excluded.offset, state = excluded.state",
            params![path, offset as i64, state],
        )?;
        Ok(())
    }

    pub fn list_trajectories(&self, limit: usize) -> Result<Vec<TrajectorySummary>> {
        let conn = self.conn();
        let sql = format!(
            "{SUMMARY_SELECT} WHERE t.id IN (SELECT id FROM trajectories ORDER BY updated_ns DESC LIMIT ?1)
             GROUP BY t.id ORDER BY t.updated_ns DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit as i64], summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Looks up a trajectory by id or unique id prefix.
    pub fn resolve_trajectory(&self, id_or_prefix: &str) -> Result<Resolved> {
        let conn = self.conn();
        let pattern = format!("{}%", id_or_prefix.replace(['%', '_'], ""));
        let mut stmt = conn.prepare(
            "SELECT id FROM trajectories WHERE id = ?1 OR id LIKE ?2 ORDER BY id = ?1 DESC LIMIT 5",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![id_or_prefix, pattern], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(match ids.as_slice() {
            [] => Resolved::NotFound,
            [one, ..] if one == id_or_prefix => Resolved::Found(one.clone()),
            [one] => Resolved::Found(one.clone()),
            _ => Resolved::Ambiguous(ids),
        })
    }

    pub fn trajectory(&self, id: &str) -> Result<Option<(TrajectorySummary, Vec<Event>)>> {
        let conn = self.conn();
        let sql = format!("{SUMMARY_SELECT} WHERE t.id = ?1 GROUP BY t.id");
        let Some(summary) = conn
            .query_row(&sql, params![id], summary_from_row)
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE trajectory_id = ?1 ORDER BY ts_ns, seq"
        ))?;
        let events = stmt
            .query_map(params![id], event_from_row)?
            .map(|r| r.map_err(anyhow::Error::from).and_then(|e| e))
            .collect::<Result<_>>()?;
        Ok(Some((summary, events)))
    }

    /// Oldest events not yet accepted by the backend, with their sequence numbers.
    pub fn pending_upload(&self, limit: usize) -> Result<Vec<(i64, Event)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT seq, {EVENT_COLUMNS} FROM events WHERE uploaded = 0 ORDER BY seq LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let seq: i64 = row.get(0)?;
            event_from_row_at(row, 1).map(|r| r.map(|e| (seq, e)))
        })?;
        rows.map(|r| r.map_err(anyhow::Error::from).and_then(|e| e))
            .collect()
    }

    pub fn mark_uploaded(&self, seqs: &[i64]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("UPDATE events SET uploaded = 1 WHERE seq = ?1")?;
            for seq in seqs {
                stmt.execute(params![seq])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT (SELECT COUNT(*) FROM trajectories), (SELECT COUNT(*) FROM events),
                    (SELECT COUNT(*) FROM events WHERE uploaded = 0)",
            [],
            |r| {
                Ok(Counts {
                    trajectories: count(r, 0)?,
                    events: count(r, 1)?,
                    pending_upload: count(r, 2)?,
                })
            },
        )?)
    }
}

pub enum Resolved {
    Found(String),
    NotFound,
    Ambiguous(Vec<String>),
}

pub struct Counts {
    pub trajectories: u64,
    pub events: u64,
    pub pending_upload: u64,
}

fn upsert_trajectory(
    tx: &rusqlite::Transaction<'_>,
    event: &Event,
    ts: i64,
    host: Option<&str>,
) -> Result<()> {
    let status = match event.kind {
        EventKind::AgentCompleted => Some("completed"),
        EventKind::AgentFailed => Some("failed"),
        EventKind::AgentCancelled => Some("cancelled"),
        EventKind::AgentPaused => Some("paused"),
        EventKind::TurnCompleted | EventKind::TurnInterrupted => Some("idle"),
        EventKind::AgentNotification => Some("waiting"),
        EventKind::AgentStarted
        | EventKind::AgentResumed
        | EventKind::TurnUser
        | EventKind::ToolStarted
        | EventKind::ToolCompleted
        | EventKind::ToolFailed
        | EventKind::SubagentStarted
        | EventKind::ContextCompacted => Some("running"),
        // Model calls are read from transcripts after the fact; they say
        // nothing about what the agent is doing now.
        _ => None,
    };
    let ended = event.kind.is_terminal().then_some(ts);
    let title = match event.kind {
        EventKind::TurnUser => event
            .get(attr::PROMPT_TEXT)
            .and_then(Value::as_str)
            .map(title_from_prompt),
        _ => None,
    };
    let cwd = event.get(attr::CWD).and_then(Value::as_str);

    tx.execute(
        "INSERT INTO trajectories (id, agent, status, started_ns, updated_ns)
         VALUES (?1, ?2, 'running', ?3, ?3)
         ON CONFLICT(id) DO NOTHING",
        params![event.trajectory_id, event.agent.name, ts],
    )?;
    // Status follows the most recent event; late arrivals don't rewind it.
    // A resumed session reopens a completed trajectory.
    tx.execute(
        "UPDATE trajectories SET
             status = CASE WHEN ?2 IS NOT NULL AND ?3 >= updated_ns THEN ?2 ELSE status END,
             ended_ns = CASE
                 WHEN ?4 IS NOT NULL THEN MAX(COALESCE(ended_ns, 0), ?4)
                 WHEN ?2 = 'running' AND ?3 >= updated_ns THEN NULL
                 ELSE ended_ns END,
             started_ns = MIN(started_ns, ?3),
             updated_ns = MAX(updated_ns, ?3),
             agent_version = COALESCE(?5, agent_version),
             title = COALESCE(title, ?6),
             cwd = COALESCE(cwd, ?7),
             host = COALESCE(host, ?8)
         WHERE id = ?1",
        params![
            event.trajectory_id,
            status,
            ts,
            ended,
            event.agent.version,
            title,
            cwd,
            host
        ],
    )?;
    Ok(())
}

fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut title: String = line.chars().take(120).collect();
    if title.len() < line.len() {
        title.push('…');
    }
    title
}

const SUMMARY_SELECT: &str = r#"
SELECT t.id, t.agent, t.agent_version, t.title, t.status, t.cwd, t.repository, t.branch, t.host,
       t.started_ns, t.updated_ns, t.ended_ns,
       COUNT(e.id),
       COALESCE(SUM(e.kind = 'turn.user'), 0),
       COALESCE(SUM(e.kind = 'model.completed'), 0),
       COUNT(DISTINCT CASE WHEN e.kind LIKE 'tool.%' THEN COALESCE(e.span_id, e.id) END),
       COALESCE(SUM(e.kind = 'tool.failed'), 0),
       COALESCE(SUM(json_extract(e.attributes, '$."gen_ai.usage.input_tokens"')), 0),
       COALESCE(SUM(json_extract(e.attributes, '$."gen_ai.usage.output_tokens"')), 0),
       COALESCE(SUM(json_extract(e.attributes, '$."gs.usage.cache_read_input_tokens"')), 0),
       COALESCE(SUM(json_extract(e.attributes, '$."gs.usage.cache_creation_input_tokens"')), 0)
FROM trajectories t LEFT JOIN events e ON e.trajectory_id = t.id
"#;

fn summary_from_row(r: &Row<'_>) -> rusqlite::Result<TrajectorySummary> {
    let started: i64 = r.get(9)?;
    let updated: i64 = r.get(10)?;
    let ended: Option<i64> = r.get(11)?;
    Ok(TrajectorySummary {
        id: r.get(0)?,
        agent: r.get(1)?,
        agent_version: r.get(2)?,
        title: r.get(3)?,
        status: r.get(4)?,
        cwd: r.get(5)?,
        repository: r.get(6)?,
        branch: r.get(7)?,
        host: r.get(8)?,
        started_at: from_ns(started),
        updated_at: from_ns(updated),
        ended_at: ended.map(from_ns),
        duration_ms: (ended.unwrap_or(updated) - started).max(0) / 1_000_000,
        event_count: count(r, 12)?,
        user_turns: count(r, 13)?,
        model_calls: count(r, 14)?,
        tool_calls: count(r, 15)?,
        tool_errors: count(r, 16)?,
        input_tokens: count(r, 17)?,
        output_tokens: count(r, 18)?,
        cache_read_tokens: count(r, 19)?,
        cache_creation_tokens: count(r, 20)?,
    })
}

fn count(r: &Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    Ok(r.get::<_, i64>(idx)?.max(0) as u64)
}

const EVENT_COLUMNS: &str =
    "id, trajectory_id, kind, ts_ns, span_id, parent_span_id, agent, agent_version, attributes";

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Event>> {
    event_from_row_at(row, 0)
}

fn event_from_row_at(row: &Row<'_>, at: usize) -> rusqlite::Result<Result<Event>> {
    let id: String = row.get(at)?;
    let kind: String = row.get(at + 2)?;
    let attributes: String = row.get(at + 8)?;
    let build = || -> Result<Event> {
        Ok(Event {
            id: id.parse()?,
            trajectory_id: row.get(at + 1)?,
            kind: kind.parse().unwrap_or_else(|never| match never {}),
            timestamp: from_ns(row.get(at + 3)?),
            span_id: row.get(at + 4)?,
            parent_span_id: row.get(at + 5)?,
            agent: groundstation_schema::Agent {
                name: row.get(at + 6)?,
                version: row.get(at + 7)?,
            },
            attributes: serde_json::from_str(&attributes)?,
        })
    };
    Ok(build())
}

fn ns(ts: DateTime<Utc>) -> i64 {
    ts.timestamp_nanos_opt().unwrap_or(i64::MAX)
}

fn from_ns(ns: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_nanos(ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use groundstation_schema::Agent;
    use uuid::Uuid;

    fn ev(traj: &str, kind: EventKind, at: DateTime<Utc>) -> Event {
        Event::new(Uuid::now_v7(), traj, kind, at, Agent::named("test"))
    }

    fn new(event: Event) -> NewEvent {
        NewEvent {
            event,
            raw: None,
            host: Some("host-a".into()),
        }
    }

    #[test]
    fn computes_span_duration_and_summary() {
        let store = Store::open_in_memory().unwrap();
        let t0 = Utc::now();
        let mut prompt = ev("t1", EventKind::TurnUser, t0);
        prompt.set(
            attr::PROMPT_TEXT,
            "\n  Fix the flaky checkout tests\nplease",
        );
        prompt.set(attr::CWD, "/repo");
        let started =
            ev("t1", EventKind::ToolStarted, t0 + Duration::seconds(1)).with_span("tool-1");
        let completed = ev(
            "t1",
            EventKind::ToolFailed,
            t0 + Duration::milliseconds(48_193),
        )
        .with_span("tool-1");
        let mut model = ev("t1", EventKind::ModelCompleted, t0 + Duration::seconds(50));
        model.set(attr::GEN_AI_INPUT_TOKENS, 10);
        model.set(attr::GEN_AI_OUTPUT_TOKENS, 200);
        model.set(attr::CACHE_READ_TOKENS, 18_000);
        let stop = ev("t1", EventKind::TurnCompleted, t0 + Duration::seconds(51));

        let stored = store
            .insert(vec![
                new(prompt),
                new(started),
                new(completed),
                new(model),
                new(stop),
            ])
            .unwrap();
        assert_eq!(stored, 5);

        let (summary, events) = store.trajectory("t1").unwrap().unwrap();
        assert_eq!(
            summary.title.as_deref(),
            Some("Fix the flaky checkout tests")
        );
        assert_eq!(summary.status, "idle");
        assert_eq!(summary.cwd.as_deref(), Some("/repo"));
        assert_eq!(summary.host.as_deref(), Some("host-a"));
        assert_eq!(summary.tool_calls, 1);
        assert_eq!(summary.tool_errors, 1);
        assert_eq!(summary.model_calls, 1);
        assert_eq!(summary.total_tokens(), 18_210);
        assert_eq!(summary.duration_ms, 51_000);
        let failed = events
            .iter()
            .find(|e| e.kind == EventKind::ToolFailed)
            .unwrap();
        assert_eq!(failed.get(attr::DURATION_MS), Some(&Value::from(47_193)));
    }

    #[test]
    fn redelivery_is_idempotent_but_model_updates_apply() {
        let store = Store::open_in_memory().unwrap();
        let start = ev("t1", EventKind::AgentStarted, Utc::now());
        assert_eq!(store.insert(vec![new(start.clone())]).unwrap(), 1);
        assert_eq!(store.insert(vec![new(start)]).unwrap(), 0);

        let mut model = ev("t1", EventKind::ModelCompleted, Utc::now());
        model.set(attr::GEN_AI_OUTPUT_TOKENS, 1);
        assert_eq!(store.insert(vec![new(model.clone())]).unwrap(), 1);
        store.mark_uploaded(&[1, 2]).unwrap();
        assert_eq!(store.insert(vec![new(model.clone())]).unwrap(), 0);
        model.set(attr::GEN_AI_OUTPUT_TOKENS, 42);
        assert_eq!(store.insert(vec![new(model)]).unwrap(), 1);

        let (summary, _) = store.trajectory("t1").unwrap().unwrap();
        assert_eq!(summary.output_tokens, 42);
        assert_eq!(summary.event_count, 2);
        assert_eq!(store.pending_upload(10).unwrap().len(), 1);
    }

    #[test]
    fn late_events_do_not_rewind_status() {
        let store = Store::open_in_memory().unwrap();
        let t0 = Utc::now();
        store
            .insert(vec![new(ev(
                "t1",
                EventKind::AgentCompleted,
                t0 + Duration::seconds(5),
            ))])
            .unwrap();
        store
            .insert(vec![new(ev("t1", EventKind::ToolStarted, t0))])
            .unwrap();
        let (summary, _) = store.trajectory("t1").unwrap().unwrap();
        assert_eq!(summary.status, "completed");
        assert!(summary.ended_at.is_some());

        store
            .insert(vec![new(ev(
                "t1",
                EventKind::AgentResumed,
                t0 + Duration::seconds(9),
            ))])
            .unwrap();
        let (summary, _) = store.trajectory("t1").unwrap().unwrap();
        assert_eq!(summary.status, "running");
        assert!(summary.ended_at.is_none());
    }

    #[test]
    fn resolves_prefixes() {
        let store = Store::open_in_memory().unwrap();
        for id in ["abc-1", "abc-2", "xyz"] {
            store
                .insert(vec![new(ev(id, EventKind::AgentStarted, Utc::now()))])
                .unwrap();
        }
        assert!(
            matches!(store.resolve_trajectory("xy").unwrap(), Resolved::Found(id) if id == "xyz")
        );
        assert!(
            matches!(store.resolve_trajectory("abc").unwrap(), Resolved::Ambiguous(ids) if ids.len() == 2)
        );
        assert!(matches!(
            store.resolve_trajectory("nope").unwrap(),
            Resolved::NotFound
        ));
    }
}
