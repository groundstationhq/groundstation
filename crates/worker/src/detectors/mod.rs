//! Detectors: rules that turn a tenant's recent events into findings.
//!
//! Each is a ClickHouse query over the tenant's deduplicated events and a pure
//! function from its rows to findings, so the rules are tested on rows alone.
//! A finding names the events that show it, so the UI links straight to them.
//!
//! Detectors that look at single events read them as a stream, ordered so
//! that they hold only what one trajectory (or one file in it) needs: a
//! larger tenant takes longer, not more memory.

mod cache_miss;
mod cost_outlier;
mod failure_hotspot;
mod permission_wait;
mod repeated_read;
mod slow_tool;
mod test_rerun_loop;

use chrono::{DateTime, Duration, Utc};
use clickhouse::query::Query;
use groundstation_backend::Result;
use groundstation_backend::ch::TenantScope;
use groundstation_backend::pg::insights::Finding;
use uuid::Uuid;

/// The events a run looks at: those that happened in `[since, until)`.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    /// How many days before the run `since` is.
    pub days: i64,
}

impl Window {
    /// The `days` days up to `now`, and the events stamped up to
    /// [`MAX_FUTURE`](groundstation_wire::MAX_FUTURE) after it, which ingest
    /// accepts: left out, a fast clock's newest events would be looked at
    /// only once the machine uploads again.
    pub fn ending(now: DateTime<Utc>, days: i64) -> Window {
        Window {
            since: now - Duration::days(days),
            until: now + groundstation_wire::MAX_FUTURE,
            days,
        }
    }

    /// The `days` days just before this window.
    pub fn previous(&self) -> Window {
        Window {
            since: self.since - Duration::days(self.days),
            until: self.since,
            days: self.days,
        }
    }
}

/// A timestamp as a ClickHouse `DateTime64(9, 'UTC')` parameter.
pub fn param(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S%.9f").to_string()
}

/// A detector's query, with the window bound as `{since}` and `{until}`.
fn query(scope: &TenantScope, sql: &str, window: Window) -> Result<Query> {
    Ok(scope
        .query(sql)?
        .param("since", param(window.since))
        .param("until", param(window.until))
        // Values taken out of `attributes` are computed before rows are
        // sorted, so a sort carries them instead of the whole attributes of
        // every event (prompts and tool bodies included).
        .with_option("query_plan_execute_functions_after_sorting", "0"))
}

/// When an event arrived, from `toUnixTimestamp64Milli(received_at)`.
fn arrived(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

/// The most findings one detector keeps for a tenant. The api lists at most
/// 500 findings and the UI shows 50; past this, storing them only costs.
pub const MAX_FINDINGS: usize = 200;

/// What a detector found for a tenant.
#[derive(Debug, Default)]
pub struct Found {
    /// The most severe findings, at most [`MAX_FINDINGS`], most severe first.
    pub findings: Vec<Finding>,
    /// How many it left out to stay under [`MAX_FINDINGS`].
    pub dropped: usize,
}

/// Keeps a detector's [`MAX_FINDINGS`] most severe findings and, among
/// equally severe ones, the largest, so a tenant with many costs no more
/// memory or writes than one with a few.
#[derive(Default)]
struct Strongest {
    kept: Vec<(Finding, f64)>,
    dropped: usize,
}

impl Strongest {
    /// `size`: how much of what the detector looks for the finding shows.
    fn push(&mut self, finding: Finding, size: f64) {
        self.kept.push((finding, size));
        if self.kept.len() >= 2 * MAX_FINDINGS {
            self.prune();
        }
    }

    fn prune(&mut self) {
        // Stable, so among equals the first found stays.
        self.kept.sort_by(|(a, a_size), (b, b_size)| {
            b.severity.cmp(&a.severity).then(b_size.total_cmp(a_size))
        });
        if self.kept.len() > MAX_FINDINGS {
            self.dropped += self.kept.len() - MAX_FINDINGS;
            self.kept.truncate(MAX_FINDINGS);
        }
    }

    fn finish(mut self) -> Found {
        self.prune();
        Found {
            findings: self.kept.into_iter().map(|(f, _)| f).collect(),
            dropped: self.dropped,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detector {
    RepeatedRead,
    TestRerunLoop,
    FailureHotspot,
    SlowTool,
    CacheMiss,
    CostOutlier,
    PermissionWait,
}

impl Detector {
    pub const ALL: [Detector; 7] = [
        Self::RepeatedRead,
        Self::TestRerunLoop,
        Self::FailureHotspot,
        Self::SlowTool,
        Self::CacheMiss,
        Self::CostOutlier,
        Self::PermissionWait,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::RepeatedRead => repeated_read::NAME,
            Self::TestRerunLoop => test_rerun_loop::NAME,
            Self::FailureHotspot => failure_hotspot::NAME,
            Self::SlowTool => slow_tool::NAME,
            Self::CacheMiss => cache_miss::NAME,
            Self::CostOutlier => cost_outlier::NAME,
            Self::PermissionWait => permission_wait::NAME,
        }
    }

    pub async fn run(self, scope: &TenantScope, window: Window) -> Result<Found> {
        match self {
            Self::RepeatedRead => repeated_read::run(scope, window).await,
            Self::TestRerunLoop => test_rerun_loop::run(scope, window).await,
            Self::FailureHotspot => failure_hotspot::run(scope, window).await,
            Self::SlowTool => slow_tool::run(scope, window).await,
            Self::CacheMiss => cache_miss::run(scope, window).await,
            Self::CostOutlier => cost_outlier::run(scope, window).await,
            Self::PermissionWait => permission_wait::run(scope, window).await,
        }
    }
}

/// Event ids as read back with `toString(event_id)`.
fn ids<'a>(raw: impl IntoIterator<Item = &'a String>) -> Vec<Uuid> {
    raw.into_iter().filter_map(|s| s.parse().ok()).collect()
}

/// Nearest-rank percentile of an ascending list, as the stats endpoints use.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use groundstation_wire::insights::Severity;

    use super::*;

    #[test]
    fn every_detector_has_its_own_name() {
        let mut names: Vec<_> = Detector::ALL.iter().map(|d| d.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Detector::ALL.len());
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.5), 2.0);
    }

    #[test]
    fn windows_reach_as_far_ahead_as_ingest_accepts() {
        let now = DateTime::UNIX_EPOCH + Duration::days(100);
        let w = Window::ending(now, 7);
        assert_eq!(w.since, now - Duration::days(7));
        assert_eq!(w.until, now + Duration::hours(24));
        let before = w.previous();
        assert_eq!(
            (before.since, before.until, before.days),
            (now - Duration::days(14), w.since, 7)
        );
    }

    fn finding(scope: String, severity: Severity) -> Finding {
        Finding {
            detector: "d",
            version: 1,
            scope,
            severity,
            title: String::new(),
            body: String::new(),
            recommendation: String::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            evidence_received_at: DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn keeps_the_most_severe_and_largest_findings() {
        let mut found = Strongest::default();
        for i in 0..1000 {
            found.push(finding(format!("low{i}"), Severity::Low), i as f64);
        }
        found.push(finding("high".into(), Severity::High), 0.0);
        found.push(finding("medium".into(), Severity::Medium), 0.0);
        let found = found.finish();
        assert_eq!(found.findings.len(), MAX_FINDINGS);
        assert_eq!(found.dropped, 1002 - MAX_FINDINGS);
        assert_eq!(found.findings[0].scope, "high");
        assert_eq!(found.findings[1].scope, "medium");
        assert_eq!(found.findings[2].scope, "low999");
        assert_eq!(found.findings[MAX_FINDINGS - 1].scope, "low802");
    }
}
