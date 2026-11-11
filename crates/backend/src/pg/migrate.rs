//! Schema migrations, applied in order at startup. One transaction holds an
//! advisory lock, so instances starting together apply each migration once.
//! Never edit a migration that has shipped; add a new one.
//!
//! Ingest and the worker can each connect as their own role, which gets only
//! what that service does. The roles are the operator's (see
//! `docker/postgres/roles.sh`); their privileges are granted here at every
//! start, to the ones that exist.

use super::Pool;
use crate::Result;

const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("migrations/0001_tenants.sql")),
    (2, include_str!("migrations/0002_accounts.sql")),
    (3, include_str!("migrations/0003_insights.sql")),
    (4, include_str!("migrations/0004_login_flows_expiry.sql")),
    (5, include_str!("migrations/0005_insight_evidence_age.sql")),
    (6, include_str!("migrations/0006_cli_approvals.sql")),
];

/// Arbitrary, but fixed: the advisory lock that serializes migrations.
const LOCK: i64 = 0x6773_6d69_6772_6174; // "gsmigrat"

/// The role ingest can connect as: resolve tokens, nothing else.
pub const INGEST_ROLE: &str = "gs_ingest";
/// The role the worker can connect as: read tenants, write findings.
pub const WORKER_ROLE: &str = "gs_worker";

/// What each service role may do: all of it, after revoking everything, so a
/// privilege taken out of this list is taken from existing databases too.
/// Ingest reads only the columns that resolve a token. Idempotent.
const GRANTS: &[(&str, &[&str])] = &[
    (
        INGEST_ROLE,
        &[
            "REVOKE ALL ON ALL TABLES IN SCHEMA public FROM gs_ingest",
            "GRANT SELECT (id, tenant_id, token_hash, revoked_at), UPDATE (last_used_at) ON ingest_tokens TO gs_ingest",
            "GRANT SELECT (id, retention_days, suspended_at) ON tenants TO gs_ingest",
        ],
    ),
    (
        WORKER_ROLE,
        &[
            "REVOKE ALL ON ALL TABLES IN SCHEMA public FROM gs_worker",
            "GRANT SELECT ON tenants TO gs_worker",
            "GRANT SELECT, INSERT, UPDATE, DELETE ON insights, insight_runs TO gs_worker",
        ],
    ),
];

pub async fn migrate(pool: &Pool) -> Result<()> {
    let mut conn = pool.get().await?;
    let tx = conn.transaction().await?;
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK])
        .await?;
    tx.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    integer PRIMARY KEY,
             applied_at timestamptz NOT NULL DEFAULT now()
         )",
    )
    .await?;
    let applied: i32 = tx
        .query_one(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            &[],
        )
        .await?
        .get(0);
    for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > applied) {
        tx.batch_execute(sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES ($1)",
            &[version],
        )
        .await?;
        tracing::info!(version, "applied postgres migration");
    }
    for (role, grants) in GRANTS {
        let exists = tx
            .query_opt("SELECT 1 FROM pg_roles WHERE rolname = $1", &[role])
            .await?
            .is_some();
        if exists {
            tx.batch_execute(&grants.join("; ")).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_consecutive_from_one() {
        for (i, (version, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*version, i as i32 + 1);
        }
    }

    #[test]
    fn grants_name_the_role_they_are_listed_under() {
        for (role, grants) in GRANTS {
            assert!(grants[0].starts_with("REVOKE ALL "), "{role}");
            for grant in *grants {
                let to = [format!("TO {role}"), format!("FROM {role}")];
                assert!(to.iter().any(|end| grant.ends_with(end)), "{grant}");
            }
        }
    }
}
