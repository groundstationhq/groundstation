//! A trajectory that cost more than 95% of the tenant's trajectories with the
//! same agent and model, and more than twice their median, over the events in
//! the window. Only agents that report cost (`gs.cost.usd`) have any: the
//! OpenCode and pi adapters do; Claude Code and Codex don't.

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::{Found, Strongest, Window, arrived, ids, percentile, query};
use crate::format::{days, short, usd};

pub const NAME: &str = "cost_outlier";
const VERSION: i32 = 1;
const MIN_GROUP: usize = 20;

#[derive(Debug, Row, Deserialize)]
pub struct Cost {
    pub trajectory_id: String,
    pub agent: String,
    /// The model most of its calls used.
    pub model: String,
    pub cost_usd: f64,
    /// When its newest event arrived, in milliseconds.
    pub received: i64,
    pub calls: Vec<String>,
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let events = deduplicated(
        "trajectory_id, event_id, received_at, kind, agent, model, cost_usd",
        "ts >= {since:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}",
    );
    let rows: Vec<Cost> = query(
        scope,
        &format!(
            "SELECT trajectory_id, any(agent), arrayElement(topKIf(1)(model, model != ''), 1),
                    sum(cost_usd) AS cost, max(toUnixTimestamp64Milli(received_at)),
                    groupArrayIf(10)(toString(event_id), kind = 'model.completed')
             FROM ({events}) GROUP BY trajectory_id HAVING isFinite(cost) AND cost > 0"
        ),
        window,
    )?
    .fetch_all()
    .await?;
    Ok(detect(&rows, window.days))
}

/// `window_days`: how many days of events `rows` cover.
pub fn detect(rows: &[Cost], window_days: i64) -> Found {
    let mut groups: BTreeMap<(&str, &str), Vec<&Cost>> = BTreeMap::new();
    for r in rows
        .iter()
        .filter(|r| r.cost_usd.is_finite() && r.cost_usd > 0.0)
    {
        groups.entry((&r.agent, &r.model)).or_default().push(r);
    }
    let mut found = Strongest::default();
    for ((agent, model), group) in groups.into_iter().filter(|(_, g)| g.len() >= MIN_GROUP) {
        let mut costs: Vec<f64> = group.iter().map(|r| r.cost_usd).collect();
        costs.sort_by(f64::total_cmp);
        let (p95, median) = (percentile(&costs, 0.95), percentile(&costs, 0.5));
        for r in group {
            if r.cost_usd > p95 && r.cost_usd > 2.0 * median {
                let group = Group {
                    agent,
                    model,
                    p95,
                    median,
                };
                found.push(finding(r, &group, window_days), r.cost_usd);
            }
        }
    }
    found.finish()
}

struct Group<'a> {
    agent: &'a str,
    model: &'a str,
    p95: f64,
    median: f64,
}

fn finding(r: &Cost, g: &Group<'_>, window_days: i64) -> Finding {
    let agent = g.agent;
    let model = if g.model.is_empty() {
        "an unknown model"
    } else {
        g.model
    };
    Finding {
        detector: NAME,
        version: VERSION,
        scope: r.trajectory_id.clone(),
        severity: if r.cost_usd > 3.0 * g.p95 {
            Severity::High
        } else {
            Severity::Medium
        },
        title: format!("Trajectory cost more than 95% of {agent} trajectories on {model}"),
        body: format!(
            "In the last {}, trajectory {} cost {}. For {agent} on {model}, 95% of trajectories \
             cost {} or less, and the median is {}.",
            days(window_days),
            short(&r.trajectory_id),
            usd(r.cost_usd),
            usd(g.p95),
            usd(g.median)
        ),
        recommendation:
            "Open it and look for what made it long: retries of one command, files read \
            over and over, or a large context carried through many turns."
                .into(),
        evidence: vec![Evidence {
            trajectory_id: r.trajectory_id.clone(),
            event_ids: ids(&r.calls),
            note: format!("cost {}", usd(r.cost_usd)),
        }],
        metrics: BTreeMap::from([
            ("cost_usd".to_string(), r.cost_usd),
            ("p95_cost_usd".to_string(), g.p95),
            ("median_cost_usd".to_string(), g.median),
        ]),
        evidence_received_at: arrived(r.received),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(id: &str, model: &str, usd: f64) -> Cost {
        Cost {
            trajectory_id: id.into(),
            agent: "opencode".into(),
            model: model.into(),
            cost_usd: usd,
            received: 4_000,
            calls: Vec::new(),
        }
    }

    fn group(n: usize, model: &str) -> Vec<Cost> {
        (0..n)
            .map(|i| cost(&format!("t{i}"), model, 1.0 + i as f64 / 100.0))
            .collect()
    }

    #[test]
    fn compares_within_agent_and_model() {
        let mut rows = group(20, "m1");
        rows.push(cost("big", "m1", 9.0));
        // Expensive, but on another model with too few trajectories to judge.
        rows.push(cost("other", "m2", 50.0));
        let found = detect(&rows, 7);
        assert_eq!(found.findings.len(), 1);
        let f = &found.findings[0];
        assert_eq!(f.scope, "big");
        assert_eq!(f.severity, Severity::High);
        assert_eq!(
            f.title,
            "Trajectory cost more than 95% of opencode trajectories on m1"
        );
        assert_eq!(
            f.body,
            "In the last 7 days, trajectory big cost $9.00. For opencode on m1, 95% of \
             trajectories cost $1.19 or less, and the median is $1.10."
        );
        assert_eq!(f.evidence_received_at, arrived(4_000));
        let mut small = group(19, "m1");
        small.push(cost("big", "m1", 9.0));
        assert_eq!(detect(&small, 7).findings.len(), 1, "19 others are enough");
        small.pop();
        small.push(cost("medium", "m1", 3.0));
        assert_eq!(detect(&small, 7).findings[0].severity, Severity::Medium);
    }

    #[test]
    fn an_even_spread_has_no_outliers() {
        let rows: Vec<_> = (0..25)
            .map(|i| cost(&format!("t{i}"), "m", 1.0 + i as f64 / 25.0))
            .collect();
        assert!(detect(&rows, 7).findings.is_empty());
    }

    #[test]
    fn costs_that_are_not_numbers_are_left_out() {
        let mut rows = group(20, "m1");
        rows.push(cost("inf", "m1", f64::INFINITY));
        rows.push(cost("nan", "m1", f64::NAN));
        assert!(detect(&rows, 7).findings.is_empty());
    }
}
