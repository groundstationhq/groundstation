//! The ClickHouse schema. Every statement is idempotent, so migrating is
//! running all of them, in order, at every start; two instances starting at
//! once can't conflict.
//!
//! Attribute names in the materialized columns come from
//! `groundstation_schema::attr`, so they are never written twice.

use groundstation_schema::attr;

use super::Clickhouse;
use crate::Result;

/// Role of the ingest user: insert events, read nothing.
pub const WRITER_ROLE: &str = "gs_writer";
/// Role of the api and worker users: read events, one tenant at a time.
pub const READER_ROLE: &str = "gs_reader";

pub async fn migrate(admin: &Clickhouse) -> Result<()> {
    for sql in statements(admin.database()) {
        admin.execute(&sql).await?;
    }
    Ok(())
}

pub fn statements(database: &str) -> Vec<String> {
    let db = &identifier(database);
    let mut all = vec![events(db), traj_index(db), traj_index_mv(db)];
    all.extend(changed_columns(db));
    all.extend(access(db));
    all
}

/// The database name as SQL: any name the operator chose, such as
/// `groundstation-prod`, quoted so it can't be read as anything else.
fn identifier(name: &str) -> String {
    format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
}

fn events(db: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {db}.events
(
    tenant_id       UUID,
    trajectory_id   String,
    event_id        UUID,
    kind            LowCardinality(String),
    ts              DateTime64(9, 'UTC'),
    span_id         Nullable(String),
    parent_span_id  Nullable(String),
    agent           LowCardinality(String),
    agent_version   LowCardinality(Nullable(String)),
    attributes      String,
    schema          LowCardinality(String),
    token_id        UUID,
    received_at     DateTime64(3, 'UTC'),
    retain_until    DateTime('UTC'),
    {hot}
)
ENGINE = ReplacingMergeTree(received_at)
PARTITION BY toYYYYMM(received_at)
ORDER BY (tenant_id, trajectory_id, event_id)
TTL retain_until DELETE",
        hot = hot_columns().join(",\n    "),
    )
}

/// Attributes extracted once, at insert, for aggregation.
fn hot_columns() -> Vec<String> {
    let string = |col: &str, key: &str| {
        format!("{col} String MATERIALIZED JSONExtractString(attributes, '{key}')")
    };
    let low = |col: &str, key: &str| {
        format!("{col} LowCardinality(String) MATERIALIZED JSONExtractString(attributes, '{key}')")
    };
    let uint = |col: &str, key: &str| {
        format!("{col} UInt64 MATERIALIZED JSONExtractUInt(attributes, '{key}')")
    };
    vec![
        string("repository", attr::VCS_REPOSITORY),
        string("branch", attr::VCS_BRANCH),
        string("revision", attr::VCS_REVISION),
        string("file_path", attr::FILE_PATH),
        cwd(),
        low("tool_category", attr::TOOL_CATEGORY),
        low("tool_name", attr::GEN_AI_TOOL_NAME),
        duration_ms(),
        uint("input_tokens", attr::GEN_AI_INPUT_TOKENS),
        uint("output_tokens", attr::GEN_AI_OUTPUT_TOKENS),
        uint("cache_read", attr::CACHE_READ_TOKENS),
        uint("cache_creation", attr::CACHE_CREATION_TOKENS),
        cost_usd(),
        low("model", attr::GEN_AI_RESPONSE_MODEL),
    ]
}

// ClickHouse computes these expressions while it inserts, for every row of
// every request in the same async-insert flush, and an exception there fails
// all of them: a 503 that gsd retries forever. So none may throw, whatever an
// attribute holds; both branches of an `if` are evaluated at insert.

/// A JSON number of milliseconds, cut to whole ones as gsd does. Anything
/// else, a string such as "NaN" included, is no duration.
fn duration_ms() -> String {
    format!(
        "duration_ms Nullable(Int64) MATERIALIZED if(JSONType(attributes, '{key}') IN ('Int64', 'UInt64', 'Double'), accurateCastOrNull(trunc(JSONExtractFloat(attributes, '{key}')), 'Int64'), NULL)",
        key = attr::DURATION_MS
    )
}

/// What the agent said the event cost. A value that isn't a finite number
/// ("inf", "1e400") counts as nothing, as gsd counts text that isn't one.
fn cost_usd() -> String {
    format!(
        "cost_usd Float64 MATERIALIZED if(isFinite(JSONExtractFloat(attributes, '{key}')), JSONExtractFloat(attributes, '{key}'), 0)",
        key = attr::COST_USD
    )
}

/// `NULL` when the event has no working directory: an empty one is the
/// repository's root.
fn cwd() -> String {
    format!(
        "cwd Nullable(String) MATERIALIZED if(JSONType(attributes, '{key}') = 'String', JSONExtractString(attributes, '{key}'), NULL)",
        key = attr::CWD
    )
}

/// Hot columns added or redefined after `events` first shipped, for tables
/// created before. An added column is computed from `attributes` when parts
/// written earlier are read. A redefined one applies to rows inserted from
/// then on; changing only the expression rewrites no data, and doing it again
/// changes nothing.
fn changed_columns(db: &str) -> Vec<String> {
    let added = [cwd()]
        .into_iter()
        .map(|column| format!("ALTER TABLE {db}.events ADD COLUMN IF NOT EXISTS {column}"));
    let redefined = [duration_ms(), cost_usd()]
        .into_iter()
        .map(|column| format!("ALTER TABLE {db}.events MODIFY COLUMN IF EXISTS {column}"));
    added.chain(redefined).collect()
}

/// The most recently updated trajectories, without scanning events. Holds only
/// aggregates that don't change when the same event is inserted twice: a
/// materialized view fires on every insert, before duplicates merge.
///
/// The view runs as the user that created it, so the ingest user needs no
/// read access to `events` for it to work.
fn traj_index(db: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {db}.traj_index
(
    tenant_id      UUID,
    trajectory_id  String,
    agent          SimpleAggregateFunction(any, LowCardinality(String)),
    started_at     SimpleAggregateFunction(min, DateTime64(9, 'UTC')),
    updated_at     SimpleAggregateFunction(max, DateTime64(9, 'UTC')),
    retain_until   SimpleAggregateFunction(max, DateTime('UTC'))
)
ENGINE = AggregatingMergeTree
ORDER BY (tenant_id, trajectory_id)
TTL retain_until DELETE"
    )
}

fn traj_index_mv(db: &str) -> String {
    format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS {db}.traj_index_mv TO {db}.traj_index
DEFINER = CURRENT_USER SQL SECURITY DEFINER AS
SELECT tenant_id, trajectory_id, any(agent) AS agent, min(ts) AS started_at,
       max(ts) AS updated_at, max(retain_until) AS retain_until
FROM {db}.events
GROUP BY tenant_id, trajectory_id"
    )
}

/// Roles, grants and the row policy. Users are the operator's: they are
/// created with a password and granted one of these roles (see
/// `docker/clickhouse/users.sh`).
fn access(db: &str) -> Vec<String> {
    // A reader sees the rows of the tenant named by the `SQL_tenant_id`
    // setting, and nothing when it's missing.
    let policy = |table: &str| {
        format!(
            "CREATE ROW POLICY OR REPLACE gs_tenant_isolation ON {db}.{table} FOR SELECT
             USING tenant_id = toUUIDOrZero(getSettingOrDefault('SQL_tenant_id', ''))
             TO {READER_ROLE}"
        )
    };
    vec![
        format!("CREATE ROLE IF NOT EXISTS {WRITER_ROLE}"),
        format!("CREATE ROLE IF NOT EXISTS {READER_ROLE}"),
        format!("GRANT INSERT ON {db}.events TO {WRITER_ROLE}"),
        format!("GRANT SELECT ON {db}.events TO {READER_ROLE}"),
        format!("GRANT SELECT ON {db}.traj_index TO {READER_ROLE}"),
        policy("events"),
        policy("traj_index"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_statement_is_idempotent() {
        for sql in statements("db") {
            let idempotent = [
                "IF NOT EXISTS",
                "OR REPLACE",
                "GRANT ",
                "MODIFY COLUMN IF EXISTS",
            ]
            .iter()
            .any(|marker| sql.contains(marker));
            assert!(idempotent, "not safe to run twice:\n{sql}");
        }
    }

    #[test]
    fn any_database_name_is_quoted() {
        assert_eq!(identifier("groundstation-prod"), "`groundstation-prod`");
        assert_eq!(identifier("a`b\\c"), "`a\\`b\\\\c`");
        for sql in statements("groundstation-prod") {
            assert!(!sql.contains(" groundstation-prod."), "{sql}");
        }
    }

    #[test]
    fn hot_columns_read_the_schema_attribute_names() {
        let ddl = events("db");
        for key in [
            attr::VCS_REVISION,
            attr::GEN_AI_INPUT_TOKENS,
            attr::CACHE_READ_TOKENS,
            attr::DURATION_MS,
            attr::COST_USD,
            attr::CWD,
        ] {
            assert!(ddl.contains(&format!("'{key}'")), "{key} missing");
        }
    }
}
