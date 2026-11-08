//! A tool category whose p95 latency more than doubled against the window
//! before.

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::failure_hotspot::CATEGORY;
use super::{Found, Strongest, Window, arrived, ids, param, query};
use crate::format::{count, days, duration};

pub const NAME: &str = "slow_tool";
const VERSION: i32 = 1;
const MIN_CALLS: u64 = 20;
const MIN_RATIO: f64 = 2.0;
const SLOWEST: usize = 5;

#[derive(Debug, Row, Deserialize)]
pub struct Latency {
    pub category: String,
    /// Whether the row is for the window being checked or the one before.
    pub current: bool,
    pub calls: u64,
    pub p95_ms: i64,
    /// When the newest call arrived, in milliseconds.
    pub received: i64,
    /// The slowest calls, slowest first, as (minus ms, trajectory, event id).
    pub slowest: Vec<(i64, String, String)>,
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let before = window.previous();
    let calls = deduplicated(
        &format!(
            "trajectory_id, event_id, ts, received_at, {CATEGORY} AS category,
             assumeNotNull(duration_ms) AS ms"
        ),
        "kind IN ('tool.completed', 'tool.failed') AND duration_ms >= 0
         AND ts >= {before:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}",
    );
    // The p95 sorts one number per call; the slowest calls are kept five at
    // a time, never all of them.
    let rows: Vec<Latency> = query(
        scope,
        &format!(
            "SELECT category, ts >= {{since:DateTime64(9, 'UTC')}} AS current, count() AS calls,
                    arrayElement(arraySort(groupArray(ms)),
                                 toUInt64(greatest(1, ceil(0.95 * count())))) AS p95_ms,
                    max(toUnixTimestamp64Milli(received_at)),
                    groupArraySorted({SLOWEST})((-ms, trajectory_id, toString(event_id)))
             FROM ({calls}) GROUP BY category, current"
        ),
        window,
    )?
    .param("before", param(before.since))
    .fetch_all()
    .await?;
    Ok(detect(&rows, window.days))
}

/// `window_days`: how many days of calls the current rows cover.
pub fn detect(rows: &[Latency], window_days: i64) -> Found {
    let previous: BTreeMap<&str, &Latency> = rows
        .iter()
        .filter(|r| !r.current)
        .map(|r| (r.category.as_str(), r))
        .collect();
    let mut found = Strongest::default();
    for now in rows.iter().filter(|r| r.current && r.calls >= MIN_CALLS) {
        let Some(before) = previous.get(now.category.as_str()) else {
            continue;
        };
        let slower = before.calls >= MIN_CALLS
            && before.p95_ms > 0
            && now.p95_ms as f64 > MIN_RATIO * before.p95_ms as f64;
        if slower {
            let ratio = now.p95_ms as f64 / before.p95_ms as f64;
            found.push(finding(now, before, window_days), ratio);
        }
    }
    found.finish()
}

fn finding(now: &Latency, before: &Latency, window_days: i64) -> Finding {
    let ratio = now.p95_ms as f64 / before.p95_ms as f64;
    let label = match now.category.as_str() {
        "tool" => "unnamed",
        category => category,
    };
    Finding {
        detector: NAME,
        version: VERSION,
        scope: now.category.clone(),
        severity: if ratio > 4.0 {
            Severity::High
        } else {
            Severity::Medium
        },
        title: format!(
            "{} tool calls got {ratio:.1}× slower at p95",
            capitalized(label)
        ),
        body: format!(
            "p95 went from {} to {} across {} calls in the last {}.",
            duration(before.p95_ms),
            duration(now.p95_ms),
            count(now.calls),
            days(window_days)
        ),
        recommendation: "Open the slowest calls below. A slower test suite, network or machine \
            shows up here first; so does an agent that started running heavier commands."
            .into(),
        evidence: now
            .slowest
            .iter()
            .map(|(ms, trajectory, id)| Evidence {
                trajectory_id: trajectory.clone(),
                event_ids: ids([id]),
                note: format!("took {}", duration(ms.saturating_neg())),
            })
            .collect(),
        metrics: BTreeMap::from([
            ("p95_ms".to_string(), now.p95_ms as f64),
            ("previous_p95_ms".to_string(), before.p95_ms as f64),
            ("calls".to_string(), now.calls as f64),
        ]),
        evidence_received_at: arrived(now.received),
    }
}

/// A title starts with a capital (STYLEGUIDE §11, sentence case).
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latency(current: bool, calls: u64, p95_ms: i64) -> Latency {
        Latency {
            category: "shell".into(),
            current,
            calls,
            p95_ms,
            received: 9_000,
            slowest: vec![(-p95_ms * 2, "t1".into(), uuid::Uuid::now_v7().to_string())],
        }
    }

    #[test]
    fn flags_a_p95_that_more_than_doubled() {
        let found = detect(&[latency(true, 30, 9_000), latency(false, 25, 3_000)], 7);
        let f = &found.findings[0];
        assert_eq!(f.title, "Shell tool calls got 3.0× slower at p95");
        assert_eq!(
            f.body,
            "p95 went from 3.0s to 9.0s across 30 calls in the last 7 days."
        );
        assert_eq!(f.evidence[0].note, "took 18.0s");
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.evidence_received_at, arrived(9_000));
        assert!(
            detect(&[latency(true, 30, 6_000), latency(false, 25, 3_000)], 7)
                .findings
                .is_empty(),
            "exactly doubled"
        );
        assert!(
            detect(&[latency(true, 30, 9_000), latency(false, 19, 3_000)], 7)
                .findings
                .is_empty()
        );
        assert!(
            detect(&[latency(true, 19, 9_000), latency(false, 25, 3_000)], 7)
                .findings
                .is_empty()
        );
        assert!(
            detect(&[latency(true, 30, 9_000)], 7).findings.is_empty(),
            "nothing to compare with"
        );
    }

    #[test]
    fn more_than_four_times_slower_is_high() {
        let high = detect(&[latency(true, 30, 12_001), latency(false, 25, 3_000)], 7);
        assert_eq!(high.findings[0].severity, Severity::High);
        let medium = detect(&[latency(true, 30, 12_000), latency(false, 25, 3_000)], 7);
        assert_eq!(medium.findings[0].severity, Severity::Medium);
    }
}
