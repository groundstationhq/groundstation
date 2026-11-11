//! The same file read three or more times in a trajectory with no write to it
//! in between: the agent keeps losing it from context.
//!
//! Reads count per agent context (`gs.session.id`; a subagent has its own),
//! and a read of some lines (an `offset` and `limit` in the tool input) is a
//! repeat only if it reads lines already read: paging through a long file
//! isn't losing it. A write to the file ends every context's run of reads of
//! it, and a write that names no file, or several (a patch), ends them all.
//! The tool input is read only to tell which lines; nothing of it is copied
//! into the finding.

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_schema::attr;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::{Found, Strongest, Window, arrived, ids, query};
use crate::format::{count, plural, short};

pub const NAME: &str = "repeated_read";
const VERSION: i32 = 1;
const MIN_READS: usize = 3;
const MAX_EVIDENCE: usize = 20;
/// How many lines agents read when not told (Claude Code, OpenCode), and
/// what a read with no `limit` in its input is taken to cover.
const DEFAULT_LINES: u64 = 2_000;
/// Line ranges remembered per run. Past this, a read is a repeat only if it
/// overlaps one of these.
const MAX_RANGES: usize = 256;

#[derive(Debug, Row, Deserialize)]
pub struct Access {
    pub trajectory_id: String,
    /// Empty for a write that names no file, or several.
    pub path: String,
    pub session: String,
    pub write: bool,
    /// `ts`, in nanoseconds.
    pub at: i64,
    pub event_id: String,
    /// `received_at`, in milliseconds.
    pub received: i64,
    /// `offset` and `limit` from the tool input, 0 when it has none.
    pub line_offset: i64,
    pub line_limit: i64,
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let calls = deduplicated(
        &format!(
            "trajectory_id, toString(event_id) AS id, toUnixTimestamp64Nano(ts) AS at,
             toUnixTimestamp64Milli(received_at) AS received,
             tool_category = 'file_write' AS is_write,
             if(is_write AND (file_path = '' OR JSONExtractUInt(attributes, '{patch}') > 1),
                '', file_path) AS path,
             JSONExtractString(attributes, '{session}') AS session,
             JSONExtractInt(attributes, '{input}', 'offset') AS line_offset,
             JSONExtractInt(attributes, '{input}', 'limit') AS line_limit",
            patch = attr::PATCH_FILES,
            session = attr::SESSION_ID,
            input = attr::TOOL_INPUT,
        ),
        "kind = 'tool.started'
         AND (tool_category = 'file_read' AND file_path != '' OR tool_category = 'file_write')
         AND ts >= {since:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}",
    );
    let mut rows = query(
        scope,
        &format!(
            "SELECT trajectory_id, path, session, is_write, at, id, received, line_offset, line_limit
             FROM ({calls}) ORDER BY trajectory_id, path, at, id"
        ),
        window,
    )?
    .fetch::<Access>()?;
    let mut detect = Detect::default();
    while let Some(row) = rows.next().await? {
        detect.push(row);
    }
    Ok(detect.finish())
}

/// [`Detect`] over rows already in memory.
#[cfg(test)]
fn detect(rows: impl IntoIterator<Item = Access>) -> Found {
    let mut detect = Detect::default();
    for row in rows {
        detect.push(row);
    }
    detect.finish()
}

/// Reads in a row: no write to the file between them.
#[derive(Debug, Default)]
struct Run {
    reads: usize,
    /// Reads of lines this run had read already.
    repeats: usize,
    /// Lines read, as `[from, to)`.
    ranges: Vec<(u64, u64)>,
    /// The last read, as (`ts`, event id).
    last: (i64, String),
    ids: Vec<String>,
    received: i64,
}

impl Run {
    fn worse_than(&self, other: &Run) -> bool {
        (self.repeats, self.reads) > (other.repeats, other.reads)
    }
}

/// Takes rows ordered by trajectory, path, `ts` and event id, so each file's
/// reads come together and in order, after the writes in the trajectory that
/// name no file.
#[derive(Default)]
pub struct Detect {
    trajectory: String,
    path: String,
    /// The writes in this trajectory that end every run, as (`ts`, event id),
    /// in order.
    resets: Vec<(i64, String)>,
    /// The run each context is in, for this path.
    runs: BTreeMap<String, Run>,
    /// The worst run this path has had.
    worst: Option<Run>,
    found: Strongest,
}

impl Detect {
    pub fn push(&mut self, row: Access) {
        if row.trajectory_id != self.trajectory {
            self.end_path();
            self.trajectory = row.trajectory_id;
            self.resets.clear();
        }
        if row.path.is_empty() {
            // Sorted first in its trajectory: a write naming no file, or several.
            self.resets.push((row.at, row.event_id));
            return;
        }
        if row.path != self.path {
            self.end_path();
            self.path = row.path;
        }
        if row.write {
            for (_, run) in std::mem::take(&mut self.runs) {
                self.keep(run);
            }
            return;
        }
        let read = (row.at, row.event_id);
        let mut run = self.runs.remove(&row.session).unwrap_or_default();
        if run.reads > 0 && written_between(&self.resets, &run.last, &read) {
            self.keep(std::mem::take(&mut run));
        }
        let from = row.line_offset.max(0) as u64;
        let lines = match row.line_limit {
            n if n > 0 => n as u64,
            _ => DEFAULT_LINES,
        };
        let range = (from, from.saturating_add(lines));
        if run.ranges.iter().any(|r| r.0 < range.1 && range.0 < r.1) {
            run.repeats += 1;
        }
        if run.ranges.len() < MAX_RANGES {
            run.ranges.push(range);
        }
        run.reads += 1;
        run.received = run.received.max(row.received);
        if run.ids.len() < MAX_EVIDENCE {
            run.ids.push(read.1.clone());
        }
        run.last = read;
        self.runs.insert(row.session, run);
    }

    fn keep(&mut self, run: Run) {
        if self.worst.as_ref().is_none_or(|w| run.worse_than(w)) {
            self.worst = Some(run);
        }
    }

    fn end_path(&mut self) {
        for (_, run) in std::mem::take(&mut self.runs) {
            self.keep(run);
        }
        if let Some(run) = self.worst.take()
            && run.repeats + 1 >= MIN_READS
        {
            let finding = finding(&self.trajectory, &self.path, &run);
            self.found.push(finding, run.repeats as f64);
        }
    }

    pub fn finish(mut self) -> Found {
        self.end_path();
        self.found.finish()
    }
}

/// Whether a write that ends every run came between two reads.
fn written_between(
    resets: &[(i64, String)],
    before: &(i64, String),
    after: &(i64, String),
) -> bool {
    let next = resets.partition_point(|w| w <= before);
    resets.get(next).is_some_and(|w| w < after)
}

fn finding(trajectory: &str, path: &str, run: &Run) -> Finding {
    let n = run.reads as u64;
    let body = if run.repeats + 1 == run.reads {
        format!(
            "In trajectory {}, the agent read {path} {} times and never wrote to it in between.",
            short(trajectory),
            count(n)
        )
    } else {
        format!(
            "In trajectory {}, the agent read {path} {} times without writing to it, \
             {} of them of lines it had already read.",
            short(trajectory),
            count(n),
            count(run.repeats as u64)
        )
    };
    Finding {
        detector: NAME,
        version: VERSION,
        scope: format!("{trajectory}\0{path}"),
        severity: match run.repeats + 1 {
            0..5 => Severity::Low,
            5..10 => Severity::Medium,
            _ => Severity::High,
        },
        title: format!("Read {path} {} times without changing it", count(n)),
        body,
        recommendation: "Each re-read costs a tool call and the tokens to read the file again. \
            The file likely fell out of context: keep it shorter, or point the agent at the lines it needs."
            .into(),
        evidence: vec![Evidence {
            trajectory_id: trajectory.to_string(),
            event_ids: ids(&run.ids),
            note: format!("{} of {path}", plural(n, "read", "reads")),
        }],
        metrics: BTreeMap::from([
            ("reads".to_string(), n as f64),
            ("repeated_reads".to_string(), run.repeats as f64),
        ]),
        evidence_received_at: arrived(run.received),
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    /// Rows as the query orders them, from accesses listed in time order.
    fn ordered(mut rows: Vec<Access>) -> Vec<Access> {
        for (i, r) in rows.iter_mut().enumerate() {
            r.at = i as i64;
        }
        rows.sort_by(|a, b| {
            (&a.trajectory_id, &a.path, a.at, &a.event_id).cmp(&(
                &b.trajectory_id,
                &b.path,
                b.at,
                &b.event_id,
            ))
        });
        rows
    }

    fn access(trajectory: &str, write: bool, path: &str) -> Access {
        Access {
            trajectory_id: trajectory.into(),
            path: path.into(),
            session: "s".into(),
            write,
            at: 0,
            event_id: Uuid::now_v7().to_string(),
            received: 1_000,
            line_offset: 0,
            line_limit: 0,
        }
    }

    fn read(p: &str) -> Access {
        access("t1", false, p)
    }

    fn write(p: &str) -> Access {
        access("t1", true, p)
    }

    fn lines(p: &str, offset: i64, limit: i64) -> Access {
        Access {
            line_offset: offset,
            line_limit: limit,
            ..read(p)
        }
    }

    #[test]
    fn counts_reads_between_writes() {
        let rows = ordered(vec![
            read("a.rs"),
            read("a.rs"),
            write("a.rs"), // resets the run
            read("a.rs"),
            read("b.rs"),
            read("a.rs"),
            read("a.rs"),
            read("b.rs"),
            access("t2", false, "a.rs"),
        ]);
        let found = detect(rows);
        assert_eq!(found.findings.len(), 1);
        let f = &found.findings[0];
        assert_eq!(f.title, "Read a.rs 3 times without changing it");
        assert_eq!(
            f.body,
            "In trajectory t1, the agent read a.rs 3 times and never wrote to it in between."
        );
        assert_eq!(f.evidence[0].event_ids.len(), 3);
        assert_eq!(f.evidence[0].note, "3 reads of a.rs");
        assert_eq!(f.evidence[0].trajectory_id, "t1");
        assert_eq!(f.severity, Severity::Low);
        assert_eq!(f.scope, "t1\0a.rs");
        assert_eq!(f.evidence_received_at, arrived(1_000));
    }

    #[test]
    fn severity_grows_with_reads() {
        let tiers = [
            (4, Severity::Low),
            (5, Severity::Medium),
            (9, Severity::Medium),
            (10, Severity::High),
        ];
        for (n, severity) in tiers {
            let rows = ordered((0..n).map(|_| read("x")).collect());
            let f = &detect(rows).findings[0];
            assert_eq!(f.severity, severity, "{n} reads");
            assert_eq!(f.evidence[0].event_ids.len(), n.min(MAX_EVIDENCE));
        }
        let many = ordered((0..1_500).map(|_| read("x")).collect());
        let f = &detect(many).findings[0];
        assert_eq!(f.title, "Read x 1,500 times without changing it");
        assert_eq!(f.evidence[0].event_ids.len(), MAX_EVIDENCE);
    }

    #[test]
    fn a_write_naming_no_file_ends_every_run() {
        // OpenCode's patch tools carry no path; Codex names only the first
        // file of a patch, and the query blanks that path too.
        let rows = ordered(vec![
            read("a.rs"),
            read("a.rs"),
            write(""),
            read("a.rs"),
            read("b.rs"),
            read("b.rs"),
        ]);
        assert!(detect(rows).findings.is_empty());
    }

    #[test]
    fn paging_through_a_file_is_not_rereading_it() {
        // Disjoint chunks, the first with no limit (the agent's default).
        let paged = ordered(vec![
            lines("big.rs", 0, 0),
            lines("big.rs", 2001, 2000),
            lines("big.rs", 4001, 2000),
            lines("big.rs", 6001, 2000),
        ]);
        assert!(detect(paged).findings.is_empty());
        // The same chunks again are repeats.
        let again = ordered(vec![
            lines("big.rs", 0, 100),
            lines("big.rs", 100, 100),
            lines("big.rs", 0, 100),
            lines("big.rs", 50, 100),
        ]);
        let f = &detect(again).findings[0];
        assert_eq!(f.title, "Read big.rs 4 times without changing it");
        assert!(
            f.body.ends_with(
                "4 times without writing to it, 2 of them of lines it had already read."
            )
        );
        assert_eq!(f.metrics["repeated_reads"], 2.0);
    }

    #[test]
    fn subagents_read_in_their_own_context() {
        let in_session = |s: &str| Access {
            session: s.into(),
            ..read("lib.rs")
        };
        let rows = ordered(vec![
            in_session("child1"),
            in_session("child2"),
            in_session("child3"),
        ]);
        assert!(detect(rows).findings.is_empty());
        // A write in one context ends the others' runs too.
        let rows = ordered(vec![
            in_session("a"),
            in_session("a"),
            Access {
                session: "b".into(),
                ..write("lib.rs")
            },
            in_session("a"),
        ]);
        assert!(detect(rows).findings.is_empty());
    }

    #[test]
    fn a_long_run_takes_linear_time() {
        let rows = ordered((0..200_000).map(|_| read("x")).collect());
        let started = std::time::Instant::now();
        let f = &detect(rows).findings[0];
        assert_eq!(f.metrics["reads"], 200_000.0);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
