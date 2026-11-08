pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("clickhouse: {0}")]
    Clickhouse(#[from] clickhouse::error::Error),
    #[error("postgres: {}", postgres(.0))]
    Postgres(#[from] tokio_postgres::Error),
    #[error("postgres pool: {0}")]
    Pool(#[from] deadpool_postgres::PoolError),
    #[error("postgres configuration: {0}")]
    PostgresConfig(String),
    #[error("encoding attributes: {0}")]
    Json(#[from] serde_json::Error),
    /// A ClickHouse read that doesn't filter by tenant: a bug, refused
    /// before it reaches the database.
    #[error("query does not filter by `tenant_id = {{tenant:UUID}}`")]
    Unscoped,
}

/// What went wrong, which the driver's own message ("db error") leaves out:
/// the server's message and code, or the connection error's cause. Not the
/// server's detail, which can quote the values of the row.
fn postgres(e: &tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        return format!("{} ({})", db.message(), db.code().code());
    }
    match std::error::Error::source(e) {
        Some(cause) => format!("{e}: {cause}"),
        None => e.to_string(),
    }
}
