//! Tool latency by category, as gsd computes it: closing tool events that
//! carry `gs.duration_ms`, nearest-rank percentiles, most-called first.

use chrono::{DateTime, Utc};
use clickhouse::Row;
use groundstation_backend::Result;
use groundstation_backend::ch::{TenantScope, deduplicated};
use groundstation_wire::ToolLatency;
use serde::Deserialize;

pub async fn tool_latency(scope: &TenantScope, since: DateTime<Utc>) -> Result<Vec<ToolLatency>> {
    let calls = deduplicated(
        "trajectory_id, event_id, kind, tool_category, tool_name, duration_ms",
        "kind IN ('tool.completed', 'tool.failed') AND ts >= {since:DateTime64(9, 'UTC')}
         AND duration_ms IS NOT NULL",
    );
    // Arrays are 1-based; rank = ceil(p * n), clamped to 1..=n.
    let rank = |p: f64| format!("arrayElement(d, toUInt64(greatest(1, ceil({p} * length(d)))))");
    let sql = format!(
        "SELECT category, length(d) AS calls, failed, {p50}, {p95}, arrayMax(d)
         FROM (
             SELECT multiIf(tool_category != '', tool_category, tool_name != '', tool_name, 'tool') AS category,
                    arraySort(groupArray(assumeNotNull(duration_ms))) AS d,
                    countIf(kind = 'tool.failed') AS failed
             FROM ({calls})
             GROUP BY category
         )
         ORDER BY calls DESC, category",
        p50 = rank(0.5),
        p95 = rank(0.95),
    );
    let rows: Vec<LatencyRow> = scope
        .query(&sql)?
        .param("since", since.format("%Y-%m-%d %H:%M:%S%.9f").to_string())
        .fetch_all()
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| ToolLatency {
            category: r.category,
            calls: r.calls,
            failed: r.failed,
            p50_ms: r.p50_ms,
            p95_ms: r.p95_ms,
            max_ms: r.max_ms,
        })
        .collect())
}

#[derive(Row, Deserialize)]
struct LatencyRow {
    category: String,
    calls: u64,
    failed: u64,
    p50_ms: i64,
    p95_ms: i64,
    max_ms: i64,
}
