//! Browser sessions, sign-ins in progress and computers approved for the CLI.
//! Each is keyed by the hash of a secret the browser or the CLI holds, so the
//! table alone grants nothing.

use chrono::{Duration, Utc};
use deadpool_postgres::GenericClient;
use groundstation_backend::pg::Pool;
use groundstation_backend::{Result, secret};
use uuid::Uuid;

pub const SESSION_PREFIX: &str = "gss_";
const FLOW_TTL: Duration = Duration::minutes(10);
/// What the browser hands the CLI after an approval, to collect its token.
pub const CLI_CODE_PREFIX: &str = "gsc_";
/// The CLI collects its token as soon as the browser reaches it.
const CLI_CODE_TTL: Duration = Duration::minutes(2);

/// The person behind a request, and the tenant they're looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub admin: bool,
}

/// Starts a session and returns its id, which only the browser keeps.
pub async fn create(pool: &Pool, user: Uuid, tenant: Uuid, ttl: Duration) -> Result<String> {
    let id = secret::generate(SESSION_PREFIX);
    let conn = pool.get().await?;
    // Sign-ins are rare enough to sweep expired rows on each one.
    conn.batch_execute(
        "DELETE FROM sessions WHERE expires_at < now();
         DELETE FROM login_flows WHERE expires_at < now();",
    )
    .await?;
    conn.execute(
        "INSERT INTO sessions (id_hash, user_id, tenant_id, expires_at) VALUES ($1, $2, $3, $4)",
        &[&secret::hash(&id), &user, &tenant, &(Utc::now() + ttl)],
    )
    .await?;
    Ok(id)
}

/// A live session, and whether its tenant may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Live(Viewer),
    Suspended,
}

/// Who a session belongs to, if it's live and they still belong to its tenant.
pub async fn viewer(pool: &Pool, id: &str) -> Result<Option<Session>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            "SELECT s.user_id, s.tenant_id, m.role = 'admin', t.suspended_at IS NOT NULL
             FROM sessions s
             JOIN memberships m ON m.tenant_id = s.tenant_id AND m.user_id = s.user_id
             JOIN tenants t ON t.id = s.tenant_id
             WHERE s.id_hash = $1 AND s.expires_at > now()",
            &[&secret::hash(id)],
        )
        .await?;
    Ok(row.map(|r| match r.get(3) {
        true => Session::Suspended,
        false => Session::Live(Viewer {
            user_id: r.get(0),
            tenant_id: r.get(1),
            admin: r.get(2),
        }),
    }))
}

pub async fn delete(pool: &Pool, id: &str) -> Result<()> {
    pool.get()
        .await?
        .execute(
            "DELETE FROM sessions WHERE id_hash = $1",
            &[&secret::hash(id)],
        )
        .await?;
    Ok(())
}

/// What a sign-in needs to remember while the browser is at the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub pkce_verifier: String,
    pub nonce: String,
    pub return_to: String,
}

/// Anyone can start a sign-in, so each start also removes the expired ones:
/// the table holds at most what was started in the last ten minutes.
pub async fn save_flow(pool: &Pool, state: &str, flow: &Flow) -> Result<()> {
    let conn = pool.get().await?;
    conn.execute("DELETE FROM login_flows WHERE expires_at < now()", &[])
        .await?;
    conn.execute(
        "INSERT INTO login_flows (state_hash, pkce_verifier, nonce, return_to, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        &[
            &secret::hash(state),
            &flow.pkce_verifier,
            &flow.nonce,
            &flow.return_to,
            &(Utc::now() + FLOW_TTL),
        ],
    )
    .await?;
    Ok(())
}

/// Removes and returns the flow for `state`; a second call finds nothing.
pub async fn take_flow(pool: &Pool, state: &str) -> Result<Option<Flow>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            "DELETE FROM login_flows WHERE state_hash = $1 AND expires_at > now()
             RETURNING pkce_verifier, nonce, return_to",
            &[&secret::hash(state)],
        )
        .await?;
    Ok(row.map(|r| Flow {
        pkce_verifier: r.get(0),
        nonce: r.get(1),
        return_to: r.get(2),
    }))
}

/// A computer someone approved for `groundstation cloud login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub label: String,
}

/// Records an approval and returns the one-time code that collects its token,
/// for the CLI holding the secret behind `challenge`. Each approval also
/// removes the expired ones, which the CLI never collected.
pub async fn save_approval(pool: &Pool, approval: &Approval, challenge: &str) -> Result<String> {
    let code = secret::generate(CLI_CODE_PREFIX);
    let conn = pool.get().await?;
    conn.execute("DELETE FROM cli_approvals WHERE expires_at < now()", &[])
        .await?;
    conn.execute(
        "INSERT INTO cli_approvals (code_hash, tenant_id, user_id, label, challenge, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        &[
            &secret::hash(&code),
            &approval.tenant_id,
            &approval.user_id,
            &approval.label,
            &challenge,
            &(Utc::now() + CLI_CODE_TTL),
        ],
    )
    .await?;
    Ok(code)
}

/// Removes and returns the approval for `code`, if `verifier` is the secret
/// its challenge was made from; a second call finds nothing. A wrong
/// verifier leaves the approval for the CLI that holds the right one.
pub async fn take_approval(
    client: &impl GenericClient,
    code: &str,
    verifier: &str,
) -> Result<Option<Approval>> {
    let row = client
        .query_opt(
            "DELETE FROM cli_approvals
             WHERE code_hash = $1 AND challenge = $2 AND expires_at > now()
             RETURNING user_id, tenant_id, label",
            &[&secret::hash(code), &challenge(verifier)],
        )
        .await?;
    Ok(row.map(|r| Approval {
        user_id: r.get(0),
        tenant_id: r.get(1),
        label: r.get(2),
    }))
}

/// The challenge the CLI sends for its secret `verifier`: SHA-256, in hex.
pub fn challenge(verifier: &str) -> String {
    hex::encode(secret::hash(verifier))
}

/// Who made an approval, if they still belong to its tenant.
pub struct Approver {
    pub session: Session,
    /// Their email, or name, as the CLI shows it.
    pub who: Option<String>,
    /// The tenant's name.
    pub workspace: String,
}

pub async fn approver(
    client: &impl GenericClient,
    approval: &Approval,
) -> Result<Option<Approver>> {
    let row = client
        .query_opt(
            "SELECT m.role = 'admin', t.suspended_at IS NOT NULL, COALESCE(u.email, u.name), t.name
             FROM memberships m
             JOIN tenants t ON t.id = m.tenant_id
             JOIN users u ON u.id = m.user_id
             WHERE m.tenant_id = $1 AND m.user_id = $2",
            &[&approval.tenant_id, &approval.user_id],
        )
        .await?;
    Ok(row.map(|r| Approver {
        session: match r.get(1) {
            true => Session::Suspended,
            false => Session::Live(Viewer {
                user_id: approval.user_id,
                tenant_id: approval.tenant_id,
                admin: r.get(0),
            }),
        },
        who: r.get(2),
        workspace: r.get(3),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_verifiers_sha256_in_hex() {
        // The same vector as the CLI's test, which makes it.
        assert_eq!(
            challenge("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
