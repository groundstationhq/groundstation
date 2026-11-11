//! Findings and how far detectors have looked. Written by the worker, read
//! and triaged through the api.

use std::borrow::Cow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use groundstation_wire::insights::{Evidence, Insight, InsightState, Severity};
use sha2::{Digest, Sha256};
use tokio_postgres::Row;
use tokio_postgres::types::Json;
use uuid::Uuid;

use super::Pool;
use crate::Result;

/// What a detector found, before it's stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub detector: &'static str,
    pub version: i32,
    /// What makes it this finding and not another, e.g. a trajectory id and a
    /// file path. Same detector and scope: same finding, updated.
    pub scope: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub recommendation: String,
    pub evidence: Vec<Evidence>,
    pub metrics: BTreeMap<String, f64>,
    /// When the newest event the finding describes arrived. The finding is
    /// deleted once its tenant no longer keeps that event.
    pub evidence_received_at: DateTime<Utc>,
}

impl Finding {
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(format!("{}\0{}", self.detector, self.scope));
        hex::encode(digest)
    }
}

/// Orders severities in SQL.
const RANK: &str = "CASE {} WHEN 'low' THEN 1 WHEN 'medium' THEN 2 WHEN 'high' THEN 3 ELSE 0 END";

fn rank(column: &str) -> String {
    RANK.replace("{}", column)
}

/// Stores a finding, or refreshes the one with its fingerprint; see [`store`].
pub async fn upsert(pool: &Pool, tenant: Uuid, f: &Finding, seen_at: DateTime<Utc>) -> Result<()> {
    store(pool, tenant, std::slice::from_ref(f), seen_at).await
}

/// Stores what one run found at `seen_at`, refreshing the findings already
/// stored with the same fingerprints, in one statement. A finding someone
/// dismissed or resolved stays so, unless it got worse since. One the worker
/// closed (it has no `state_severity`) opens again when it comes back. A run
/// older than the one that last refreshed a finding (another worker's)
/// leaves it as it is.
pub async fn store(
    pool: &Pool,
    tenant: Uuid,
    findings: &[Finding],
    seen_at: DateTime<Utc>,
) -> Result<()> {
    // Postgres updates a row only once per statement, so one finding per
    // fingerprint: the most severe.
    let mut unique: BTreeMap<String, &Finding> = BTreeMap::new();
    for f in findings {
        let kept = unique.entry(f.fingerprint()).or_insert(f);
        if f.severity > kept.severity {
            *kept = f;
        }
    }
    if unique.is_empty() {
        return Ok(());
    }
    let mut rows = Rows::default();
    for (fingerprint, f) in unique {
        rows.push(fingerprint, f);
    }
    let reopen = format!(
        "insights.state <> 'open' AND {} > {}",
        rank("EXCLUDED.severity"),
        rank("insights.state_severity")
    );
    let sql = format!(
        "INSERT INTO insights (id, tenant_id, detector, detector_version, fingerprint, severity,
                               title, body, recommendation, evidence, metrics,
                               evidence_received_at, first_seen_at, last_seen_at)
         SELECT f.id, $1, f.detector, f.version, f.fingerprint, f.severity, f.title, f.body,
                f.recommendation, f.evidence, f.metrics, f.received, $2, $2
         FROM unnest($3::uuid[], $4::text[], $5::int[], $6::text[], $7::text[], $8::text[],
                     $9::text[], $10::text[], $11::jsonb[], $12::jsonb[], $13::timestamptz[])
              AS f (id, detector, version, fingerprint, severity, title, body, recommendation,
                    evidence, metrics, received)
         ON CONFLICT (tenant_id, fingerprint) DO UPDATE SET
             detector_version = EXCLUDED.detector_version, severity = EXCLUDED.severity,
             title = EXCLUDED.title, body = EXCLUDED.body, recommendation = EXCLUDED.recommendation,
             evidence = EXCLUDED.evidence, metrics = EXCLUDED.metrics,
             evidence_received_at = EXCLUDED.evidence_received_at,
             last_seen_at = EXCLUDED.last_seen_at,
             state = CASE WHEN {reopen} THEN 'open' ELSE insights.state END,
             state_changed_at = CASE WHEN {reopen} THEN now() ELSE insights.state_changed_at END
         WHERE EXCLUDED.last_seen_at >= insights.last_seen_at"
    );
    pool.get()
        .await?
        .execute(
            &sql,
            &[
                &tenant,
                &seen_at,
                &rows.ids,
                &rows.detectors,
                &rows.versions,
                &rows.fingerprints,
                &rows.severities,
                &rows.titles,
                &rows.bodies,
                &rows.recommendations,
                &rows.evidence,
                &rows.metrics,
                &rows.received,
            ],
        )
        .await?;
    Ok(())
}

/// Findings as the columns [`store`] sends, one array per column.
#[derive(Default)]
struct Rows {
    ids: Vec<Uuid>,
    detectors: Vec<&'static str>,
    versions: Vec<i32>,
    fingerprints: Vec<String>,
    severities: Vec<&'static str>,
    titles: Vec<String>,
    bodies: Vec<String>,
    recommendations: Vec<String>,
    evidence: Vec<Json<Vec<Evidence>>>,
    metrics: Vec<Json<BTreeMap<String, f64>>>,
    received: Vec<DateTime<Utc>>,
}

impl Rows {
    fn push(&mut self, fingerprint: String, f: &Finding) {
        // JSON has no infinity or NaN: they would be stored as null, which no
        // reader takes for a number. Odd client values can lead a detector's
        // arithmetic there, so such a metric is left out.
        let metrics = f
            .metrics
            .iter()
            .filter(|(_, v)| v.is_finite())
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let evidence = f
            .evidence
            .iter()
            .map(|e| Evidence {
                trajectory_id: storable(&e.trajectory_id).into_owned(),
                event_ids: e.event_ids.clone(),
                note: storable(&e.note).into_owned(),
            })
            .collect();
        self.ids.push(Uuid::now_v7());
        self.detectors.push(f.detector);
        self.versions.push(f.version);
        self.fingerprints.push(fingerprint);
        self.severities.push(f.severity.as_str());
        self.titles.push(storable(&f.title).into_owned());
        self.bodies.push(storable(&f.body).into_owned());
        self.recommendations
            .push(storable(&f.recommendation).into_owned());
        self.evidence.push(Json(evidence));
        self.metrics.push(Json(metrics));
        self.received.push(f.evidence_received_at);
    }
}

/// Text Postgres can store. Neither `text` nor `jsonb` holds U+0000, which an
/// event's strings (a file path, a trajectory id) can bring into a finding.
fn storable(text: &str) -> Cow<'_, str> {
    match text.contains('\0') {
        true => Cow::Owned(text.replace('\0', "\u{fffd}")),
        false => Cow::Borrowed(text),
    }
}

/// What the worker sets when it closes a finding itself: resolved by no one,
/// with no severity to stay under, so it opens again if it comes back.
const CLOSE: &str = "state = 'resolved', state_severity = NULL, state_changed_by = NULL,
                     state_changed_at = now()";

/// Closes the detector's open findings that its run at `seen_at` didn't find
/// again: what they describe stopped, or left the window the detector looks
/// at. Returns how many.
pub async fn resolve_unseen(
    pool: &Pool,
    tenant: Uuid,
    detector: &str,
    seen_at: DateTime<Utc>,
) -> Result<u64> {
    let closed = pool
        .get()
        .await?
        .execute(
            &format!(
                "UPDATE insights SET {CLOSE}
                 WHERE tenant_id = $1 AND detector = $2 AND state = 'open' AND last_seen_at < $3"
            ),
            &[&tenant, &detector, &seen_at],
        )
        .await?;
    Ok(closed)
}

/// For every tenant: closes open findings not seen for `window_days` (their
/// tenant stopped uploading, so no detector ran to close them), and deletes
/// findings of any state once the tenant no longer keeps the newest event
/// they describe, so a finding never outlives its events. Returns how many
/// were closed and deleted.
pub async fn sweep(pool: &Pool, window_days: i32) -> Result<(u64, u64)> {
    let conn = pool.get().await?;
    let closed = conn
        .execute(
            &format!(
                "UPDATE insights SET {CLOSE}
                 WHERE state = 'open' AND last_seen_at < now() - make_interval(days => $1)"
            ),
            &[&window_days],
        )
        .await?;
    let deleted = conn
        .execute(
            // Rows stored before `evidence_received_at` existed go a
            // retention period after they were last seen.
            "DELETE FROM insights i USING tenants t
             WHERE t.id = i.tenant_id
               AND COALESCE(i.evidence_received_at, i.last_seen_at)
                   < now() - make_interval(days => t.retention_days)",
            &[],
        )
        .await?;
    Ok((closed, deleted))
}

const COLUMNS: &str = "id, detector, severity, title, body, recommendation, evidence, metrics,
                       first_seen_at, last_seen_at, state";

fn insight(row: &Row) -> Result<Insight> {
    let severity: &str = row.try_get("severity")?;
    let state: &str = row.try_get("state")?;
    let Json(evidence) = row.try_get::<_, Json<Vec<Evidence>>>("evidence")?;
    // Rows written before `upsert` left out non-finite metrics hold nulls.
    let Json(metrics) = row.try_get::<_, Json<BTreeMap<String, Option<f64>>>>("metrics")?;
    Ok(Insight {
        id: row.try_get("id")?,
        detector: row.try_get("detector")?,
        severity: match severity {
            "high" => Severity::High,
            "medium" => Severity::Medium,
            _ => Severity::Low,
        },
        title: row.try_get("title")?,
        body: row.try_get("body")?,
        recommendation: row.try_get("recommendation")?,
        evidence,
        metrics: metrics
            .into_iter()
            .filter_map(|(name, value)| Some((name, value?)))
            .collect(),
        first_seen_at: row.try_get("first_seen_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
        state: match state {
            "dismissed" => InsightState::Dismissed,
            "resolved" => InsightState::Resolved,
            _ => InsightState::Open,
        },
    })
}

/// Most severe, then most recently seen, first.
pub async fn list(
    pool: &Pool,
    tenant: Uuid,
    state: Option<InsightState>,
    limit: i64,
) -> Result<Vec<Insight>> {
    let state = state.map(InsightState::as_str);
    let rows = pool
        .get()
        .await?
        .query(
            &format!(
                "SELECT {COLUMNS} FROM insights
                 WHERE tenant_id = $1 AND ($2::text IS NULL OR state = $2)
                 ORDER BY {} DESC, last_seen_at DESC, id LIMIT $3",
                rank("severity")
            ),
            &[&tenant, &state, &limit],
        )
        .await?;
    rows.iter().map(insight).collect()
}

pub async fn get(pool: &Pool, tenant: Uuid, id: Uuid) -> Result<Option<Insight>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            &format!("SELECT {COLUMNS} FROM insights WHERE tenant_id = $1 AND id = $2"),
            &[&tenant, &id],
        )
        .await?;
    row.as_ref().map(insight).transpose()
}

/// Records a person's triage. Returns false if the tenant has no such finding.
pub async fn set_state(
    pool: &Pool,
    tenant: Uuid,
    id: Uuid,
    state: InsightState,
    by: Uuid,
) -> Result<bool> {
    let changed = pool
        .get()
        .await?
        .execute(
            "UPDATE insights SET state = $3, state_severity = severity,
                 state_changed_by = $4, state_changed_at = now()
             WHERE tenant_id = $1 AND id = $2",
            &[&tenant, &id, &state.as_str(), &by],
        )
        .await?;
    Ok(changed == 1)
}

/// The newest `received_at` a detector has already looked at for a tenant.
pub async fn watermark(pool: &Pool, tenant: Uuid, detector: &str) -> Result<Option<DateTime<Utc>>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            "SELECT watermark FROM insight_runs WHERE tenant_id = $1 AND detector = $2",
            &[&tenant, &detector],
        )
        .await?;
    Ok(row.map(|r| r.get(0)))
}

pub async fn record_run(
    pool: &Pool,
    tenant: Uuid,
    detector: &str,
    watermark: DateTime<Utc>,
) -> Result<()> {
    pool.get()
        .await?
        .execute(
            "INSERT INTO insight_runs (tenant_id, detector, watermark, last_run_at)
             VALUES ($1, $2, $3, now())
             ON CONFLICT (tenant_id, detector) DO UPDATE SET
                 watermark = GREATEST(insight_runs.watermark, EXCLUDED.watermark),
                 last_run_at = now()",
            &[&tenant, &detector, &watermark],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_depend_on_detector_and_scope_only() {
        let finding = |detector, scope: &str, title: &str| Finding {
            detector,
            version: 1,
            scope: scope.into(),
            severity: Severity::Low,
            title: title.into(),
            body: String::new(),
            recommendation: String::new(),
            evidence: Vec::new(),
            metrics: BTreeMap::new(),
            evidence_received_at: DateTime::UNIX_EPOCH,
        };
        let a = finding("repeated_read", "t1\0src/a.rs", "Read 3 times");
        assert_eq!(
            a.fingerprint(),
            finding("repeated_read", "t1\0src/a.rs", "Read 9 times").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            finding("repeated_read", "t1\0src/b.rs", "x").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            finding("slow_tool", "t1\0src/a.rs", "x").fingerprint()
        );
    }
}
