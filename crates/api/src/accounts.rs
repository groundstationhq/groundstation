//! People, how they sign in, and the tenants they belong to.

use deadpool_postgres::GenericClient;
use groundstation_backend::Result;
use groundstation_backend::pg::{Pool, tenants};
use uuid::Uuid;

use crate::config::NewUsers;

/// Who signed in, as the identity provider (or the admin password) says.
#[derive(Debug, Clone)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

impl Identity {
    /// The single user of an instance that signs in with the admin password.
    pub fn admin() -> Self {
        Self {
            issuer: "local".into(),
            subject: "admin".into(),
            email: None,
            name: Some("admin".into()),
        }
    }

    fn label(&self) -> &str {
        self.email
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or(&self.subject)
    }
}

/// What happens to someone signing in for the first time.
#[derive(Debug, Clone, Copy)]
pub struct Signup {
    pub new_users: NewUsers,
    pub retention_days: i32,
}

/// The tenant everyone joins with `NewUsers::Shared`. Fixed, so concurrent
/// first sign-ins can't create two.
pub const SHARED_TENANT: Uuid = Uuid::from_u128(1);

/// Where someone who signed in may work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignedIn {
    /// The user, and the tenant to sign them into.
    Member(Uuid, Uuid),
    /// Every tenant they belong to is suspended.
    Suspended,
}

/// Finds or creates the user with this identity, and the tenant to sign them
/// into: their oldest membership of a tenant that isn't suspended, or a new
/// one per `signup` if they have none at all.
pub async fn sign_in(pool: &Pool, identity: &Identity, signup: Signup) -> Result<SignedIn> {
    let mut conn = pool.get().await?;
    let tx = conn.transaction().await?;
    let user = match find_user(&tx, identity).await? {
        Some(user) => user,
        None => create_user(&tx, identity).await?,
    };
    let signed_in = match first_tenant(&tx, user).await? {
        Some((tenant, false)) => SignedIn::Member(user, tenant),
        Some((_, true)) => SignedIn::Suspended,
        None => SignedIn::Member(user, join(&tx, user, identity, signup).await?),
    };
    tx.commit().await?;
    Ok(signed_in)
}

async fn find_user(tx: &impl GenericClient, identity: &Identity) -> Result<Option<Uuid>> {
    let row = tx
        .query_opt(
            "SELECT user_id FROM identities WHERE issuer = $1 AND subject = $2",
            &[&identity.issuer, &identity.subject],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let user: Uuid = row.get(0);
    // Keep the profile as the provider has it now.
    tx.execute(
        "UPDATE users SET email = COALESCE($2, email), name = COALESCE($3, name) WHERE id = $1",
        &[&user, &identity.email, &identity.name],
    )
    .await?;
    Ok(Some(user))
}

async fn create_user(tx: &impl GenericClient, identity: &Identity) -> Result<Uuid> {
    let user = Uuid::now_v7();
    tx.execute(
        "INSERT INTO users (id, email, name) VALUES ($1, $2, $3)",
        &[&user, &identity.email, &identity.name],
    )
    .await?;
    tx.execute(
        "INSERT INTO identities (issuer, subject, user_id) VALUES ($1, $2, $3)",
        &[&identity.issuer, &identity.subject, &user],
    )
    .await?;
    Ok(user)
}

/// The user's oldest membership, preferring a tenant that isn't suspended,
/// and whether that one is.
async fn first_tenant(tx: &impl GenericClient, user: Uuid) -> Result<Option<(Uuid, bool)>> {
    let row = tx
        .query_opt(
            "SELECT m.tenant_id, t.suspended_at IS NOT NULL
             FROM memberships m JOIN tenants t ON t.id = m.tenant_id
             WHERE m.user_id = $1
             ORDER BY t.suspended_at IS NOT NULL, m.created_at LIMIT 1",
            &[&user],
        )
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

async fn join(
    tx: &impl GenericClient,
    user: Uuid,
    identity: &Identity,
    signup: Signup,
) -> Result<Uuid> {
    // The first person in a tenant administers it.
    let (tenant, admin) = match signup.new_users {
        NewUsers::Personal => {
            let tenant = tenants::insert(tx, identity.label(), signup.retention_days).await?;
            (tenant.id, true)
        }
        NewUsers::Shared => {
            let created =
                tenants::insert_fixed(tx, SHARED_TENANT, "Default", signup.retention_days).await?;
            (SHARED_TENANT, created)
        }
    };
    let role = if admin { "admin" } else { "member" };
    tx.execute(
        "INSERT INTO memberships (tenant_id, user_id, role) VALUES ($1, $2, $3)",
        &[&tenant, &user, &role],
    )
    .await?;
    Ok(tenant)
}
