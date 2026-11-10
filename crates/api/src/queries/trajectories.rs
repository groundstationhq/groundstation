//! Trajectory lists, summaries and event timelines.

use chrono::{DateTime, Utc};
use clickhouse::Row;
use clickhouse::query::RowCursor;
use groundstation_backend::Result;
use groundstation_backend::ch::{EventRow, Query, TenantScope, deduplicated};
use groundstation_schema::{Event, EventKind, attr};
use groundstation_wire::TrajectorySummary;
use groundstation_wire::summary::{STATUS_BY_KIND, STATUS_RUNNING, cost_usd, title_from_prompt};
use serde::Deserialize;

pub enum Resolved {
    Found(String),
    NotFound,
    Ambiguous(Vec<String>),
}

/// Which trajectories to summarize.
enum Which<'a> {
    /// The `limit` most recently updated, chosen inside the query: a list of
    /// ids passed as a parameter would have to fit in a URL.
    Latest(u32),
    One(&'a str),
}

impl Which<'_> {
    const LATEST: &'static str = "trajectory_id IN (
        SELECT trajectory_id FROM traj_index WHERE tenant_id = {tenant:UUID}
        GROUP BY trajectory_id ORDER BY max(updated_at) DESC, trajectory_id
        LIMIT {limit:UInt32})";
    const ONE: &'static str = "trajectory_id = {id:String}";

    fn filter(&self) -> &'static str {
        match self {
            Self::Latest(_) => Self::LATEST,
            Self::One(_) => Self::ONE,
        }
    }

    fn bind(&self, query: Query) -> Query {
        match self {
            Self::Latest(limit) => query.param("limit", limit),
            Self::One(id) => query.param("id", id),
        }
    }
}

/// The `limit` most recently updated trajectories, newest first.
pub async fn list(scope: &TenantScope, limit: u32) -> Result<Vec<TrajectorySummary>> {
    let mut summaries = summarize(scope, Which::Latest(limit)).await?;
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
    Ok(summaries)
}

/// Looks up a trajectory by id or unique id prefix, as gsd does.
pub async fn resolve(scope: &TenantScope, id_or_prefix: &str) -> Result<Resolved> {
    let ids: Vec<String> = scope
        .query(
            "SELECT DISTINCT trajectory_id FROM traj_index
             WHERE tenant_id = {tenant:UUID} AND startsWith(trajectory_id, {prefix:String})
             ORDER BY trajectory_id = {prefix:String} DESC, trajectory_id
             LIMIT 5",
        )?
        .param("prefix", id_or_prefix)
        .fetch_all()
        .await?;
    Ok(match ids.as_slice() {
        [] => Resolved::NotFound,
        [one, ..] if one == id_or_prefix => Resolved::Found(one.clone()),
        [one] => Resolved::Found(one.clone()),
        _ => Resolved::Ambiguous(ids),
    })
}

/// A trajectory's summary.
pub async fn summary(scope: &TenantScope, id: &str) -> Result<Option<TrajectorySummary>> {
    Ok(summarize(scope, Which::One(id)).await?.pop())
}

/// A trajectory's events in time order, a row at a time: a timeline can be
/// far larger than anything worth holding in memory at once. Unhurried: the
/// rows leave as fast as whoever reads them takes them, and a large timeline
/// on a slow connection takes longer than an interactive read may.
pub fn events(scope: &TenantScope, id: &str) -> Result<RowCursor<EventRow>> {
    let sql = format!(
        "SELECT ?fields FROM ({}) ORDER BY ts, received_at, event_id",
        deduplicated("*", "trajectory_id = {id:String}")
    );
    Ok(scope.unhurried().query(&sql)?.param("id", id).fetch()?)
}

/// A trajectory's summary and all its events in time order.
pub async fn detail(
    scope: &TenantScope,
    id: &str,
) -> Result<Option<(TrajectorySummary, Vec<Event>)>> {
    let Some(summary) = summary(scope, id).await? else {
        return Ok(None);
    };
    let mut rows = events(scope, id)?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        events.push(row.to_event()?);
    }
    Ok(Some((summary, events)))
}

/// One summary per trajectory, computed from its deduplicated events by the
/// rules in `groundstation_wire::summary`, as if they had arrived in time order.
///
/// Two aggregations, so that listing trajectories doesn't read every event's
/// `attributes` (prompts and tool bodies, by far the largest column): the
/// numbers come from the materialized columns, and only prompts' attributes
/// are read for titles. One query: ClickHouse builds the set of trajectories
/// an identical `IN (subquery)` names once per query, so both aggregations
/// read the same ones, even while new trajectories arrive.
async fn summarize(scope: &TenantScope, which: Which<'_>) -> Result<Vec<TrajectorySummary>> {
    let (kinds, statuses): (Vec<&str>, Vec<&str>) = STATUS_BY_KIND.iter().copied().unzip();
    let terminal: Vec<&str> = kinds
        .iter()
        .copied()
        .filter(|k| k.parse::<EventKind>().is_ok_and(|k| k.is_terminal()))
        .collect();
    let rows = which
        .bind(scope.query(&summary_sql(which.filter()))?)
        .param("kinds", &kinds)
        .param("statuses", &statuses)
        .param("terminal", &terminal)
        .fetch_all::<SummaryRow>()
        .await?;
    Ok(rows.into_iter().map(SummaryRow::into_summary).collect())
}

/// Each trajectory's first prompt, for its title. Trajectories without one
/// (or whose prompts were excluded from upload) have no row. A title is a
/// line of at most 120 characters, so the first few thousand are plenty.
fn first_prompt_sql(which: &str) -> String {
    let prompts = deduplicated(
        &format!(
            "trajectory_id, event_id, ts,
             substringUTF8(JSONExtractString(attributes, '{key}'), 1, 4096) AS prompt,
             JSONType(attributes, '{key}') = 'String' AS is_prompt",
            key = attr::PROMPT_TEXT
        ),
        &format!("{which} AND kind = 'turn.user'"),
    );
    format!(
        "SELECT trajectory_id, toNullable(argMin(prompt, ts)) AS prompt FROM ({prompts})
         WHERE is_prompt GROUP BY trajectory_id"
    )
}

/// The numbers of each trajectory `which` names, and its first prompt: NULL
/// for one without.
fn summary_sql(which: &str) -> String {
    format!(
        "SELECT numbers.*, prompts.prompt
         FROM ({}) AS numbers
         LEFT JOIN ({}) AS prompts ON numbers.trajectory_id = prompts.trajectory_id",
        numbers_sql(which),
        first_prompt_sql(which)
    )
}

fn numbers_sql(which: &str) -> String {
    let events = deduplicated(
        "trajectory_id, event_id, kind, ts, span_id, agent, agent_version, cwd,
         repository, branch, revision, input_tokens, output_tokens, cache_read, cache_creation,
         cost_usd, transform(kind, {kinds:Array(String)}, {statuses:Array(String)}, '') AS status",
        which,
    );
    format!(
        "SELECT
             trajectory_id,
             argMin(agent, ts),
             argMaxIf(agent_version, ts, agent_version IS NOT NULL),
             argMaxIf(status, ts, status != ''),
             maxIf(ts, kind IN {{terminal:Array(String)}}),
             countIf(kind IN {{terminal:Array(String)}}),
             maxIf(ts, status = '{STATUS_RUNNING}'),
             countIf(status = '{STATUS_RUNNING}'),
             argMinIf(cwd, ts, cwd IS NOT NULL),
             anyIf(repository, repository != ''),
             argMaxIf(branch, ts, branch != ''),
             argMaxIf(revision, ts, revision != ''),
             min(ts),
             max(ts),
             count(),
             countIf(kind = 'turn.user'),
             countIf(kind = 'model.completed'),
             uniqExactIf(ifNull(span_id, toString(event_id)), startsWith(kind, 'tool.')),
             countIf(kind = 'tool.failed'),
             sum(input_tokens),
             sum(output_tokens),
             sum(cache_read),
             sum(cache_creation),
             sumKahan(cost_usd)
         FROM ({events})
         GROUP BY trajectory_id"
    )
}

/// The columns of [`summary_sql`], in order.
#[derive(Row, Deserialize)]
struct SummaryRow {
    id: String,
    agent: String,
    agent_version: Option<String>,
    status: String,
    #[serde(with = "clickhouse::serde::chrono::datetime64::nanos")]
    last_terminal: DateTime<Utc>,
    terminals: u64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::nanos")]
    last_running: DateTime<Utc>,
    runnings: u64,
    cwd: Option<String>,
    repository: String,
    branch: String,
    revision: String,
    #[serde(with = "clickhouse::serde::chrono::datetime64::nanos")]
    started_at: DateTime<Utc>,
    #[serde(with = "clickhouse::serde::chrono::datetime64::nanos")]
    updated_at: DateTime<Utc>,
    event_count: u64,
    user_turns: u64,
    model_calls: u64,
    tool_calls: u64,
    tool_errors: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cost_usd: f64,
    first_prompt: Option<String>,
}

impl SummaryRow {
    fn into_summary(self) -> TrajectorySummary {
        // A terminal event ends the trajectory unless a later event resumed
        // it. (With no running event, last_running is 1970, the column's zero.)
        let resumed = self.runnings > 0 && self.last_running > self.last_terminal;
        let ended_at = (self.terminals > 0 && !resumed).then_some(self.last_terminal);
        let end = ended_at.unwrap_or(self.updated_at);
        let some = |s: String| (!s.is_empty()).then_some(s);
        TrajectorySummary {
            id: self.id,
            agent: self.agent,
            agent_version: self.agent_version,
            title: self.first_prompt.as_deref().map(title_from_prompt),
            status: some(self.status).unwrap_or_else(|| STATUS_RUNNING.to_string()),
            cwd: self.cwd,
            repository: some(self.repository),
            branch: some(self.branch),
            revision: some(self.revision),
            // Hostnames never leave the machine.
            host: None,
            started_at: self.started_at,
            updated_at: self.updated_at,
            ended_at,
            duration_ms: (end - self.started_at).num_milliseconds().max(0),
            event_count: self.event_count,
            user_turns: self.user_turns,
            model_calls: self.model_calls,
            tool_calls: self.tool_calls,
            tool_errors: self.tool_errors,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cost_usd: cost_usd(self.cost_usd),
        }
    }
}
