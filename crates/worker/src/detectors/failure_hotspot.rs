//! A tool category failing more than one call in five, over enough calls to
//! mean something.

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::{Found, Strongest, Window, arrived, ids, query};
use crate::format::{count, days, percent, plural};

pub const NAME: &str = "tool_failure_hotspot";
const VERSION: i32 = 1;
const MIN_CALLS: u64 = 20;
const MAX_RATE: f64 = 0.2;
const MAX_TRAJECTORIES: usize = 5;
/// Failed calls linked per trajectory.
const MAX_EXAMPLES: usize = 20;

/// The category of a call: its tool category, else its tool name, else
/// [`UNNAMED`].
pub const CATEGORY: &str =
    "multiIf(tool_category != '', tool_category, tool_name != '', tool_name, 'tool')";
/// What [`CATEGORY`] calls a tool with neither a category nor a name.
const UNNAMED: &str = "tool";

#[derive(Debug, Row, Deserialize)]
pub struct Category {
    pub category: String,
    pub calls: u64,
    pub failed: u64,
    /// When the newest failed call arrived, in milliseconds.
    pub received: i64,
    /// The trajectories with the most failed calls, most first, as (minus
    /// their failed calls, trajectory, some of those calls' event ids).
    pub worst: Vec<(i64, String, Vec<String>)>,
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let calls = deduplicated(
        &format!("trajectory_id, event_id, received_at, kind, {CATEGORY} AS category"),
        "kind IN ('tool.completed', 'tool.failed')
         AND ts >= {since:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}",
    );
    let rows: Vec<Category> = query(
        scope,
        &format!(
            "SELECT category, sum(in_trajectory) AS calls, sum(failed_in_trajectory) AS failed,
                    max(newest),
                    groupArraySortedIf({MAX_TRAJECTORIES})(
                        (-toInt64(failed_in_trajectory), trajectory_id, examples),
                        failed_in_trajectory > 0)
             FROM (
                 SELECT category, trajectory_id, count() AS in_trajectory,
                        countIf(kind = 'tool.failed') AS failed_in_trajectory,
                        maxIf(toUnixTimestamp64Milli(received_at), kind = 'tool.failed') AS newest,
                        groupArrayIf({MAX_EXAMPLES})(toString(event_id), kind = 'tool.failed') AS examples
                 FROM ({calls}) GROUP BY category, trajectory_id
             )
             GROUP BY category HAVING calls >= {MIN_CALLS}"
        ),
        window,
    )?
    .fetch_all()
    .await?;
    Ok(detect(&rows, window.days))
}

/// `window_days`: how many days of calls `rows` cover.
pub fn detect(rows: &[Category], window_days: i64) -> Found {
    let mut found = Strongest::default();
    for c in rows {
        if c.calls >= MIN_CALLS && c.failed as f64 / c.calls as f64 > MAX_RATE {
            found.push(finding(c, window_days), c.failed as f64);
        }
    }
    found.finish()
}

fn finding(c: &Category, window_days: i64) -> Finding {
    let rate = c.failed as f64 / c.calls as f64;
    let label = if c.category == UNNAMED {
        "unnamed"
    } else {
        &c.category
    };
    Finding {
        detector: NAME,
        version: VERSION,
        scope: c.category.clone(),
        severity: if rate > 0.5 { Severity::High } else { Severity::Medium },
        title: format!("{} of {label} tool calls failed", percent(rate)),
        body: format!(
            "{} of {} {label} calls failed in the last {}.",
            count(c.failed),
            count(c.calls),
            days(window_days)
        ),
        recommendation: "Open the failed calls below. Failures of one kind usually share a cause: \
            a wrong path, a missing permission, or a tool the agent misuses. Fix the cause, or tell \
            the agent how in its instructions."
            .into(),
        evidence: c
            .worst
            .iter()
            .map(|(failed, trajectory, examples)| Evidence {
                trajectory_id: trajectory.clone(),
                event_ids: ids(examples),
                note: plural(failed.unsigned_abs(), "failed call", "failed calls"),
            })
            .collect(),
        metrics: BTreeMap::from([
            ("calls".to_string(), c.calls as f64),
            ("failed".to_string(), c.failed as f64),
            ("failure_rate".to_string(), rate),
        ]),
        evidence_received_at: arrived(c.received),
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn category(calls: u64, failed: u64) -> Category {
        // Failures spread over three trajectories, most in the first.
        let worst = (0..3u64)
            .map(|t| {
                let n = failed / 3 + u64::from(t < failed % 3);
                let ids = (0..n.min(MAX_EXAMPLES as u64))
                    .map(|_| Uuid::now_v7().to_string())
                    .collect();
                (-(n as i64), format!("t{t}"), ids)
            })
            .filter(|(n, _, _)| *n < 0)
            .collect();
        Category {
            category: "shell".into(),
            calls,
            failed,
            received: 5_000,
            worst,
        }
    }

    #[test]
    fn flags_categories_failing_more_than_one_in_five() {
        assert!(detect(&[category(100, 20)], 7).findings.is_empty());
        assert!(
            detect(&[category(19, 15)], 7).findings.is_empty(),
            "too few calls to judge"
        );
        let found = detect(&[category(40, 12)], 14);
        let f = &found.findings[0];
        assert_eq!(f.title, "30% of shell tool calls failed");
        assert_eq!(f.body, "12 of 40 shell calls failed in the last 14 days.");
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.evidence.len(), 3);
        assert_eq!(f.evidence[0].event_ids.len(), 4);
        assert_eq!(f.evidence[0].note, "4 failed calls");
        assert_eq!(f.evidence_received_at, arrived(5_000));
        let tiers = [
            (100, 21, Severity::Medium),
            (40, 20, Severity::Medium),
            (40, 21, Severity::High),
        ];
        for (calls, failed, severity) in tiers {
            let found = detect(&[category(calls, failed)], 7);
            assert_eq!(found.findings[0].severity, severity, "{failed} of {calls}");
        }
        assert_eq!(detect(&[category(20, 5)], 7).findings.len(), 1, "25% of 20");
    }

    #[test]
    fn notes_count_every_failure_in_the_trajectory() {
        let mut c = category(400, 300);
        c.worst = vec![
            (
                -280,
                "big".into(),
                vec![Uuid::now_v7().to_string(); MAX_EXAMPLES],
            ),
            (-1, "one".into(), vec![Uuid::now_v7().to_string()]),
        ];
        let f = &detect(&[c], 7).findings[0];
        assert_eq!(f.evidence[0].note, "280 failed calls");
        assert_eq!(f.evidence[1].note, "1 failed call");
    }

    #[test]
    fn tools_without_a_category_or_name_are_unnamed() {
        let mut c = category(40, 30);
        c.category = UNNAMED.into();
        let f = &detect(&[c], 1).findings[0];
        assert_eq!(f.title, "75% of unnamed tool calls failed");
        assert_eq!(f.body, "30 of 40 unnamed calls failed in the last day.");
        assert_eq!(f.scope, "tool");
    }
}
