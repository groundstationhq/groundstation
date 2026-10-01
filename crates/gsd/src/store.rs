//! Local persistence: the durable buffer between agents and the backend, and
//! the source for the local query API.
//!
//! Every write is idempotent. Event ids are stable across redeliveries, so
//! the spool, transcript re-reads and hook retries can all replay freely.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use groundstation_api::{ToolLatency, TrajectorySummary};
use groundstation_schema::{Event, EventKind, attr};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use crate::perms;

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
    // Who a transcript belongs to, so it can be re-read without waiting for a hook.
    "ALTER TABLE transcripts ADD COLUMN adapter TEXT;
     ALTER TABLE transcripts ADD COLUMN trajectory_id TEXT;",
    // Upload bookkeeping: `uploaded` is 0 while queued, 1 once accepted, 2 once
    // dropped after repeated rejection.
    "ALTER TABLE events ADD COLUMN upload_attempts INTEGER NOT NULL DEFAULT 0;",
];

pub struct Store {
    conn: Mutex<Connection>,
}

/// A transcript file and, when known, who it belongs to.
pub struct Transcript {
    pub path: String,
    pub adapter: Option<String>,
    pub trajectory_id: Option<String>,
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
            perms::create_dir_all(dir)?;
        }
        // Owner-only before SQLite opens it: the WAL and shm files SQLite adds
        // copy the database file's mode. Files an older gsd left more open are
        // tightened here too.
        perms::touch(path)?;
        for suffix in ["-wal", "-shm"] {
            let mut side = path.as_os_str().to_owned();
            side.push(suffix);
            let side = Path::new(&side);
            if side.exists() {
                perms::restrict(side, 0o600)?;
            }
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
                     agent_version = excluded.agent_version, uploaded = 0, upload_attempts = 0
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

    pub fn set_transcript_cursor(
        &self,
        path: &str,
        offset: u64,
        state: &Value,
        adapter: &str,
        trajectory_id: &str,
    ) -> Result<()> {
        let state = (!state.is_null()).then(|| state.to_string());
        self.conn().execute(
            "INSERT INTO transcripts (path, offset, state, adapter, trajectory_id) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET offset = excluded.offset, state = excluded.state,
                 adapter = excluded.adapter, trajectory_id = excluded.trajectory_id",
            params![path, offset as i64, state, adapter, trajectory_id],
        )?;
        Ok(())
    }

    /// Makes every transcript look as if read before owners were recorded.
    #[cfg(test)]
    pub fn forget_transcript_owners(&self) {
        self.conn()
            .execute(
                "UPDATE transcripts SET adapter = NULL, trajectory_id = NULL",
                [],
            )
            .unwrap();
    }

    /// Every transcript read so far, with its adapter and trajectory when known.
    pub fn transcripts(&self) -> Result<Vec<Transcript>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT path, adapter, trajectory_id FROM transcripts ORDER BY path")?;
        let rows = stmt.query_map([], |r| {
            Ok(Transcript {
                path: r.get(0)?,
                adapter: r.get(1)?,
                trajectory_id: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Transcript paths named by stored hooks, with the adapter and trajectory
    /// of the hook. Recovers owners of transcripts read before they were
    /// recorded; needs the path attribute or the raw payload to be kept.
    pub fn transcript_owners(&self) -> Result<Vec<Transcript>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT path, agent, trajectory_id FROM (
                 SELECT json_extract(attributes, '$.\"gs.transcript.path\"') AS path, agent, trajectory_id FROM events
                 UNION ALL SELECT json_extract(raw, '$.transcript_path'), agent, trajectory_id FROM events WHERE raw IS NOT NULL
                 UNION ALL SELECT json_extract(raw, '$.agent_transcript_path'), agent, trajectory_id FROM events WHERE raw IS NOT NULL
             ) WHERE path IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Transcript {
                path: r.get(0)?,
                adapter: r.get(1)?,
                trajectory_id: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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

    /// Counts a backend rejection against each event. An event rejected
    /// `max_attempts` times leaves the upload queue but stays in the store.
    /// Returns the sequence numbers that were dropped.
    pub fn record_rejections(&self, seqs: &[i64], max_attempts: i64) -> Result<Vec<i64>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut dropped = Vec::new();
        {
            let mut stmt = tx.prepare(
                "UPDATE events SET upload_attempts = upload_attempts + 1,
                     uploaded = CASE WHEN upload_attempts + 1 >= ?2 THEN 2 ELSE 0 END
                 WHERE seq = ?1 RETURNING uploaded",
            )?;
            for &seq in seqs {
                let state: Option<i64> = stmt
                    .query_row(params![seq, max_attempts], |r| r.get(0))
                    .optional()?;
                if state == Some(2) {
                    dropped.push(seq);
                }
            }
        }
        tx.commit()?;
        Ok(dropped)
    }

    /// Latency percentiles per tool category for tool calls that closed at or
    /// after `since_ns`. Uses the closing event's `gs.duration_ms`, so calls
    /// still open are not counted. Most-called categories first.
    pub fn tool_latency(&self, since_ns: i64) -> Result<Vec<ToolLatency>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT COALESCE(json_extract(attributes, '$.\"{cat}\"'), json_extract(attributes, '$.\"{name}\"'), 'tool'),
                    json_extract(attributes, '$.\"{dur}\"'), kind = 'tool.failed'
             FROM events
             WHERE kind IN ('tool.completed', 'tool.failed') AND ts_ns >= ?1
               AND json_extract(attributes, '$.\"{dur}\"') IS NOT NULL
             ORDER BY 1, 2",
            cat = attr::TOOL_CATEGORY,
            name = attr::GEN_AI_TOOL_NAME,
            dur = attr::DURATION_MS,
        ))?;
        let mut by_category: Vec<(String, Vec<i64>, u64)> = Vec::new();
        let rows = stmt.query_map(params![since_ns], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)? as i64,
                r.get::<_, bool>(2)?,
            ))
        })?;
        for row in rows {
            let (category, ms, failed) = row?;
            match by_category.last_mut() {
                Some((c, durations, fails)) if *c == category => {
                    durations.push(ms);
                    *fails += u64::from(failed);
                }
                _ => by_category.push((category, vec![ms], u64::from(failed))),
            }
        }
        let mut tools: Vec<ToolLatency> = by_category
            .into_iter()
            .map(|(category, durations, failed)| ToolLatency {
                category,
                calls: durations.len() as u64,
                failed,
                p50_ms: percentile(&durations, 0.5),
                p95_ms: percentile(&durations, 0.95),
                max_ms: *durations.last().unwrap_or(&0),
            })
            .collect();
        tools.sort_by(|a, b| {
            b.calls
                .cmp(&a.calls)
                .then_with(|| a.category.cmp(&b.category))
        });
        Ok(tools)
    }

    pub fn counts(&self) -> Result<Counts> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT (SELECT COUNT(*) FROM trajectories), (SELECT COUNT(*) FROM events),
                    (SELECT COUNT(*) FROM events WHERE uploaded = 0),
                    (SELECT COUNT(*) FROM events WHERE uploaded = 2)",
            [],
            |r| {
                Ok(Counts {
                    trajectories: count(r, 0)?,
                    events: count(r, 1)?,
                    pending_upload: count(r, 2)?,
                    dropped_upload: count(r, 3)?,
                })
            },
        )?)
    }
}

/// Nearest-rank percentile of an ascending list; 0 for an empty list.
fn percentile(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
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
    /// Events the backend rejected too often to keep retrying.
    pub dropped_upload: u64,
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
       COALESCE(SUM(json_extract(e.attributes, '$."gs.usage.cache_creation_input_tokens"')), 0),
       COALESCE(SUM(json_extract(e.attributes, '$."gs.cost.usd"')), 0.0)
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
        cost_usd: r.get::<_, f64>(21)?,
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
    fn an_updated_event_gets_fresh_upload_attempts() {
        let store = Store::open_in_memory().unwrap();
        let mut model = ev("a", EventKind::ModelCompleted, Utc::now());
        model.set(attr::GEN_AI_OUTPUT_TOKENS, 1);
        store.insert(vec![new(model.clone())]).unwrap();
        let seq = store.pending_upload(1).unwrap()[0].0;
        assert!(store.record_rejections(&[seq], 2).unwrap().is_empty());
        assert_eq!(store.record_rejections(&[seq], 2).unwrap(), vec![seq]);
        assert_eq!(store.counts().unwrap().dropped_upload, 1);

        model.set(attr::GEN_AI_OUTPUT_TOKENS, 2);
        store.insert(vec![new(model)]).unwrap();
        let counts = store.counts().unwrap();
        assert_eq!((counts.pending_upload, counts.dropped_upload), (1, 0));
        assert!(store.record_rejections(&[seq], 2).unwrap().is_empty());
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
    fn tool_latency_percentiles_by_category() {
        let store = Store::open_in_memory().unwrap();
        let t0 = Utc::now();
        let mut events = Vec::new();
        for (i, ms) in [10, 20, 30, 40, 1000].into_iter().enumerate() {
            let span = format!("shell-{i}");
            let mut open = ev(
                "t1",
                EventKind::ToolStarted,
                t0 + Duration::seconds(i as i64),
            )
            .with_span(&span);
            open.set(attr::TOOL_CATEGORY, "shell");
            let mut close = ev(
                "t1",
                if i == 4 {
                    EventKind::ToolFailed
                } else {
                    EventKind::ToolCompleted
                },
                t0 + Duration::seconds(i as i64) + Duration::milliseconds(ms),
            )
            .with_span(&span);
            close.set(attr::TOOL_CATEGORY, "shell");
            events.push(new(open));
            events.push(new(close));
        }
        // A closing event without a span still counts when it carries a duration.
        let mut read = ev("t1", EventKind::ToolCompleted, t0);
        read.set(attr::GEN_AI_TOOL_NAME, "Read");
        read.set(attr::DURATION_MS, 7);
        events.push(new(read));
        // Still open: not counted.
        let mut open = ev("t1", EventKind::ToolStarted, t0).with_span("late");
        open.set(attr::TOOL_CATEGORY, "shell");
        events.push(new(open));
        store.insert(events).unwrap();

        let tools = store.tool_latency(0).unwrap();
        assert_eq!(tools.len(), 2);
        let shell = &tools[0];
        assert_eq!(
            (shell.category.as_str(), shell.calls, shell.failed),
            ("shell", 5, 1)
        );
        assert_eq!((shell.p50_ms, shell.p95_ms, shell.max_ms), (30, 1000, 1000));
        assert_eq!(
            (tools[1].category.as_str(), tools[1].calls, tools[1].p50_ms),
            ("Read", 1, 7)
        );
        // The window excludes older calls.
        let recent = store.tool_latency(ns(t0 + Duration::seconds(3))).unwrap();
        assert_eq!(recent.iter().map(|t| t.calls).sum::<u64>(), 2);

        assert_eq!(percentile(&[], 0.5), 0);
        assert_eq!(percentile(&[5], 0.95), 5);
        assert_eq!(percentile(&[1, 2, 3, 4], 0.5), 2);
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
