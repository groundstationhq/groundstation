//! Long stops after the agent asked for a person (`agent.notification`), such
//! as permission prompts nobody answered for minutes.
//!
//! Only notifications that block the agent count: a permission prompt or a
//! question, not Claude Code's note that its prompt has sat idle after a
//! finished turn. A stop lasts until the next event that isn't another
//! notification. Nothing marks the moment a person approves, so a stop that
//! ends when the approved tool finishes includes the tool's run, and the
//! finding says "up to".

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_schema::attr;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::{Found, Strongest, Window, arrived, ids, query};
use crate::format::{duration, plural, short};

pub const NAME: &str = "permission_wait";
const VERSION: i32 = 1;
/// Shorter stops are a person reading, not a person away.
const MIN_WAIT_MS: i64 = 60_000;
const MIN_WAITS: u64 = 3;
const MIN_TOTAL_MS: i64 = 10 * 60_000;
const MAX_EVIDENCE: usize = 20;
/// `gs.notification.type`s that stop the agent until a person answers.
/// Empty: events from before adapters recorded the type.
const BLOCKING: &[&str] = &[
    "",
    "permission_prompt",
    "permission_request",
    "elicitation_dialog",
];

#[derive(Debug, Row, Deserialize)]
pub struct Step {
    pub trajectory_id: String,
    pub event_id: String,
    pub kind: String,
    /// `gs.notification.type` of a notification.
    pub notice: String,
    /// `ts`, in nanoseconds.
    pub at: i64,
    /// `received_at`, in milliseconds.
    pub received: i64,
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let filter = "ts >= {since:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}";
    let blocking = BLOCKING
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let notice = format!(
        "JSONExtractString(attributes, '{}')",
        attr::NOTIFICATION_TYPE
    );
    let events = deduplicated(
        &format!(
            "trajectory_id, toString(event_id) AS id, kind,
             if(kind = 'agent.notification', {notice}, '') AS notice,
             toUnixTimestamp64Nano(ts) AS at, toUnixTimestamp64Milli(received_at) AS received"
        ),
        &format!(
            "{filter} AND trajectory_id IN (
                 SELECT trajectory_id FROM events
                 WHERE tenant_id = {{tenant:UUID}} AND kind = 'agent.notification'
                   AND {notice} IN ({blocking}) AND {filter})"
        ),
    );
    let mut rows = query(
        scope,
        &format!("SELECT * FROM ({events}) ORDER BY trajectory_id, at, id"),
        window,
    )?
    .fetch::<Step>()?;
    let mut detect = Detect::default();
    while let Some(row) = rows.next().await? {
        detect.push(row);
    }
    Ok(detect.finish())
}

/// [`Detect`] over rows already in memory.
#[cfg(test)]
fn detect(rows: impl IntoIterator<Item = Step>) -> Found {
    let mut detect = Detect::default();
    for row in rows {
        detect.push(row);
    }
    detect.finish()
}

/// A notification that stopped the agent, not yet followed by anything else.
struct Stop {
    at: i64,
    event_id: String,
    received: i64,
}

#[derive(Default)]
struct Waits {
    n: u64,
    total_ms: i64,
    longest_ms: i64,
    /// Whether a wait ended when a tool finished, so it includes the tool's run.
    with_tool: bool,
    ids: Vec<String>,
    received: i64,
}

/// Takes rows ordered by trajectory, `ts` and event id.
#[derive(Default)]
pub struct Detect {
    trajectory: String,
    stop: Option<Stop>,
    waits: Waits,
    found: Strongest,
}

impl Detect {
    pub fn push(&mut self, step: Step) {
        if step.trajectory_id != self.trajectory {
            self.end_trajectory();
            self.trajectory = step.trajectory_id;
        }
        if step.kind == "agent.notification" {
            if self.stop.is_none() && BLOCKING.contains(&step.notice.as_str()) {
                self.stop = Some(Stop {
                    at: step.at,
                    event_id: step.event_id,
                    received: step.received,
                });
            }
            return;
        }
        let Some(stop) = self.stop.take() else {
            return;
        };
        let ms = (step.at.saturating_sub(stop.at)) / 1_000_000;
        if ms >= MIN_WAIT_MS {
            let w = &mut self.waits;
            w.n += 1;
            w.total_ms = w.total_ms.saturating_add(ms);
            w.longest_ms = w.longest_ms.max(ms);
            w.with_tool |= matches!(step.kind.as_str(), "tool.completed" | "tool.failed");
            w.received = w.received.max(stop.received);
            if w.ids.len() < MAX_EVIDENCE {
                w.ids.push(stop.event_id);
            }
        }
    }

    fn end_trajectory(&mut self) {
        // A stop nothing followed is still open: how long it lasts is unknown.
        self.stop = None;
        let waits = std::mem::take(&mut self.waits);
        if waits.n >= MIN_WAITS || waits.total_ms >= MIN_TOTAL_MS {
            let size = waits.total_ms as f64;
            self.found.push(finding(&self.trajectory, &waits), size);
        }
    }

    pub fn finish(mut self) -> Found {
        self.end_trajectory();
        self.found.finish()
    }
}

fn finding(trajectory: &str, w: &Waits) -> Finding {
    let up_to = if w.with_tool { "up to " } else { "" };
    let times = match w.n {
        1 => "once".to_string(),
        n => format!("{} times", crate::format::count(n)),
    };
    let tool = if w.with_tool {
        " A wait that ended when the approved tool finished includes the time the tool ran."
    } else {
        ""
    };
    Finding {
        detector: NAME,
        version: VERSION,
        scope: trajectory.to_string(),
        severity: if w.total_ms >= 30 * 60_000 {
            Severity::Medium
        } else {
            Severity::Low
        },
        title: format!("Waited {up_to}{} for a person", duration(w.total_ms)),
        body: format!(
            "In trajectory {}, the agent stopped {times} to wait for input, {up_to}{} in all. \
             The longest wait was {up_to}{}.{tool}",
            short(trajectory),
            duration(w.total_ms),
            duration(w.longest_ms)
        ),
        recommendation: "If these were permission prompts, allow the commands the agent runs \
            routinely in its settings, so it doesn't stop to ask."
            .into(),
        evidence: vec![Evidence {
            trajectory_id: trajectory.to_string(),
            event_ids: ids(&w.ids),
            note: plural(w.n, "wait", "waits"),
        }],
        metrics: BTreeMap::from([
            ("waits".to_string(), w.n as f64),
            ("wait_ms".to_string(), w.total_ms as f64),
            ("longest_wait_ms".to_string(), w.longest_ms as f64),
        ]),
        evidence_received_at: arrived(w.received),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(trajectory: &str, kind: &str, at_secs: i64) -> Step {
        Step {
            trajectory_id: trajectory.into(),
            event_id: uuid::Uuid::now_v7().to_string(),
            kind: kind.into(),
            notice: String::new(),
            at: at_secs * 1_000_000_000,
            received: at_secs * 1_000,
        }
    }

    fn notice(trajectory: &str, notice: &str, at_secs: i64) -> Step {
        Step {
            notice: notice.into(),
            ..step(trajectory, "agent.notification", at_secs)
        }
    }

    #[test]
    fn adds_up_long_stops_after_notifications() {
        let rows = vec![
            step("t1", "agent.notification", 0),
            step("t1", "tool.started", 700), // 11m 40s
            step("t1", "agent.notification", 800),
            step("t1", "tool.started", 830), // 30s: someone was there
            step("t2", "agent.notification", 0),
            step("t2", "tool.started", 90),
            step("t3", "agent.notification", 0), // nothing after: open-ended
        ];
        let found = detect(rows);
        assert_eq!(found.findings.len(), 1);
        let f = &found.findings[0];
        assert_eq!(f.title, "Waited 11m 40s for a person");
        assert!(f.body.contains("stopped once to wait"));
        assert_eq!(f.evidence[0].event_ids.len(), 1);
        assert_eq!(f.evidence[0].note, "1 wait");
        assert_eq!(f.severity, Severity::Low);
        assert_eq!(f.evidence_received_at, arrived(0));
    }

    #[test]
    fn three_long_stops_count_even_if_short_in_total() {
        let mut rows = Vec::new();
        for i in 0..3 {
            rows.push(step("t", "agent.notification", i * 100));
            rows.push(step("t", "turn.user", i * 100 + 61));
        }
        let f = &detect(rows).findings[0];
        assert_eq!(f.title, "Waited 3m 03s for a person");
        assert!(f.body.contains("stopped 3 times"));
        assert_eq!(f.evidence[0].note, "3 waits");
        let two: Vec<_> = (0..2)
            .flat_map(|i| {
                [
                    step("t", "agent.notification", i * 100),
                    step("t", "turn.user", i * 100 + 61),
                ]
            })
            .collect();
        assert!(detect(two).findings.is_empty());
    }

    #[test]
    fn half_an_hour_in_all_is_medium() {
        let rows = vec![
            notice("t", "permission_prompt", 0),
            step("t", "turn.user", 30 * 60),
        ];
        assert_eq!(detect(rows).findings[0].severity, Severity::Medium);
    }

    #[test]
    fn an_idle_prompt_after_a_finished_turn_is_no_wait() {
        let rows = vec![
            step("t", "turn.completed", 0),
            notice("t", "idle_prompt", 60),
            step("t", "turn.user", 2 * 3600),
            notice("t", "auth_success", 2 * 3600 + 10),
            step("t", "turn.user", 3 * 3600),
        ];
        assert!(detect(rows).findings.is_empty());
    }

    #[test]
    fn notifications_back_to_back_are_one_stop() {
        let rows = vec![
            notice("t", "permission_prompt", 0),
            notice("t", "idle_prompt", 300),
            notice("t", "permission_prompt", 400),
            step("t", "turn.user", 700),
        ];
        let f = &detect(rows).findings[0];
        assert_eq!(f.title, "Waited 11m 40s for a person");
        assert_eq!(f.metrics["waits"], 1.0);
    }

    #[test]
    fn a_wait_that_ends_with_the_tool_is_at_most_that_long() {
        let rows = vec![
            step("t", "tool.started", 0),
            notice("t", "permission_request", 2),
            step("t", "tool.completed", 702),
        ];
        let f = &detect(rows).findings[0];
        assert_eq!(f.title, "Waited up to 11m 40s for a person");
        assert!(
            f.body
                .contains("up to 11m 40s in all. The longest wait was up to 11m 40s.")
        );
        assert!(f.body.ends_with("includes the time the tool ran."));
    }
}
