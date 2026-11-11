//! A trajectory that read far less of its input from the prompt cache than the
//! tenant's trajectories usually do, over the events in the window.

use std::collections::BTreeMap;

use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_backend::pg::insights::Finding;
use groundstation_wire::insights::{Evidence, Severity};
use serde::Deserialize;

use super::{Found, Strongest, Window, arrived, ids, percentile, query};
use crate::format::{days, percent, short, tokens};

pub const NAME: &str = "cache_miss";
const VERSION: i32 = 1;
/// Below this, cache behavior is noise.
const MIN_INPUT: f64 = 50_000.0;
const MIN_TRAJECTORIES: usize = 10;
/// "Far below" the median: under a third of it.
const FRACTION_OF_MEDIAN: f64 = 1.0 / 3.0;

/// Token sums, as floats: token counts come from clients, and integer sums
/// of odd values would wrap.
#[derive(Debug, Row, Deserialize)]
pub struct Usage {
    pub trajectory_id: String,
    pub input: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
    /// When the newest model call arrived, in milliseconds.
    pub received: i64,
    pub calls: Vec<String>,
}

impl Usage {
    fn total(&self) -> f64 {
        self.input + self.cache_read + self.cache_creation
    }

    fn cached(&self) -> f64 {
        self.cache_read / self.total().max(1.0)
    }
}

pub async fn run(scope: &TenantScope, window: Window) -> Result<Found> {
    let calls = deduplicated(
        "trajectory_id, event_id, received_at, input_tokens, cache_read, cache_creation",
        "kind = 'model.completed'
         AND ts >= {since:DateTime64(9, 'UTC')} AND ts < {until:DateTime64(9, 'UTC')}",
    );
    let rows: Vec<Usage> = query(
        scope,
        &format!(
            "SELECT trajectory_id, sum(toFloat64(input_tokens)) AS input,
                    sum(toFloat64(cache_read)) AS cache_read,
                    sum(toFloat64(cache_creation)) AS cache_creation,
                    max(toUnixTimestamp64Milli(received_at)), groupArray(10)(toString(event_id))
             FROM ({calls}) GROUP BY trajectory_id
             HAVING input + cache_read + cache_creation >= {MIN_INPUT}"
        ),
        window,
    )?
    .fetch_all()
    .await?;
    Ok(detect(&rows, window.days))
}

/// `window_days`: how many days of calls `rows` cover.
pub fn detect(rows: &[Usage], window_days: i64) -> Found {
    let rows: Vec<&Usage> = rows.iter().filter(|r| r.total() >= MIN_INPUT).collect();
    let mut found = Strongest::default();
    if rows.len() < MIN_TRAJECTORIES {
        return found.finish();
    }
    let mut shares: Vec<f64> = rows.iter().map(|r| r.cached()).collect();
    shares.sort_by(f64::total_cmp);
    let median = percentile(&shares, 0.5);
    for r in rows {
        if r.cached() < median * FRACTION_OF_MEDIAN {
            found.push(finding(r, median, window_days), r.total());
        }
    }
    found.finish()
}

fn finding(r: &Usage, median: f64, window_days: i64) -> Finding {
    Finding {
        detector: NAME,
        version: VERSION,
        scope: r.trajectory_id.clone(),
        severity: if r.total() >= 1_000_000.0 {
            Severity::Medium
        } else {
            Severity::Low
        },
        title: format!(
            "Only {} of input tokens came from the cache",
            percent(r.cached())
        ),
        body: format!(
            "In the last {}, trajectory {} read {} of its {} input tokens from the prompt cache. \
             The median trajectory reads {}.",
            days(window_days),
            short(&r.trajectory_id),
            percent(r.cached()),
            tokens(r.total() as u64),
            percent(median)
        ),
        recommendation: "Cache hits need a stable start of the prompt. Changing instructions or \
            tools mid-trajectory, or pausing longer than the cache lasts, makes the model read \
            everything again at full price."
            .into(),
        evidence: vec![Evidence {
            trajectory_id: r.trajectory_id.clone(),
            event_ids: ids(&r.calls),
            note: format!("{} cached", percent(r.cached())),
        }],
        metrics: BTreeMap::from([
            ("cached_share".to_string(), r.cached()),
            ("median_cached_share".to_string(), median),
            ("input_tokens".to_string(), r.total()),
        ]),
        evidence_received_at: arrived(r.received),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(id: &str, input: f64, cache_read: f64) -> Usage {
        Usage {
            trajectory_id: id.into(),
            input,
            cache_read,
            cache_creation: 0.0,
            received: 3_000,
            calls: vec![uuid::Uuid::now_v7().to_string()],
        }
    }

    fn warm(n: usize) -> Vec<Usage> {
        (0..n)
            .map(|i| usage(&format!("t{i}"), 20_000.0, 80_000.0))
            .collect()
    }

    #[test]
    fn flags_trajectories_far_below_the_median() {
        let mut rows = warm(10);
        rows.push(usage("cold", 95_000.0, 5_000.0));
        let found = detect(&rows, 7);
        assert_eq!(found.findings.len(), 1);
        let f = &found.findings[0];
        assert_eq!(f.scope, "cold");
        assert_eq!(f.title, "Only 5.0% of input tokens came from the cache");
        assert_eq!(
            f.body,
            "In the last 7 days, trajectory cold read 5.0% of its 100k input tokens from the \
             prompt cache. The median trajectory reads 80%."
        );
        assert_eq!(f.severity, Severity::Low);
        assert_eq!(f.evidence_received_at, arrived(3_000));
        // Just under a third of the median, and just over.
        let mut edge = warm(10);
        edge.push(usage("under", 73_400.0, 26_600.0));
        edge.push(usage("over", 73_300.0, 26_700.0));
        let found = detect(&edge, 7);
        assert_eq!(found.findings.len(), 1);
        assert_eq!(found.findings[0].scope, "under");
        let mut big = warm(10);
        big.push(usage("big", 1_000_000.0, 0.0));
        assert_eq!(detect(&big, 7).findings[0].severity, Severity::Medium);
    }

    #[test]
    fn needs_enough_trajectories_and_tokens() {
        let mut rows = warm(8);
        rows.push(usage("cold", 95_000.0, 5_000.0));
        assert!(detect(&rows, 7).findings.is_empty(), "nine trajectories");
        rows.extend(warm(1));
        assert_eq!(detect(&rows, 7).findings.len(), 1, "ten trajectories");
        let small: Vec<_> = (0..20)
            .map(|i| usage(&format!("t{i}"), 100.0, 0.0))
            .collect();
        assert!(detect(&small, 7).findings.is_empty());
    }

    #[test]
    fn huge_token_counts_stay_shares() {
        let mut rows = warm(10);
        rows.push(Usage {
            cache_creation: u64::MAX as f64,
            ..usage("odd", u64::MAX as f64, 1.0)
        });
        let f = &detect(&rows, 7).findings[0];
        assert!(f.metrics["cached_share"] >= 0.0 && f.metrics["cached_share"] <= 1.0);
        assert_eq!(f.title, "Only <0.1% of input tokens came from the cache");
    }
}
