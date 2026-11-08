//! Connection settings shared by the services, as flags or environment
//! variables. The defaults match `docker-compose.yml`.

/// Which service is connecting. Each has its own ClickHouse user with only
/// the privileges it needs: ingest can only insert, api and the worker can
/// only read, and only one tenant's rows at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Ingest,
    Api,
    Worker,
}

impl Service {
    fn clickhouse_user(self) -> &'static str {
        match self {
            Self::Ingest => "gs_ingest",
            Self::Api => "gs_api",
            Self::Worker => "gs_worker",
        }
    }
}

#[derive(clap::Args, Debug, Clone)]
pub struct ClickhouseArgs {
    /// ClickHouse HTTP interface.
    #[arg(
        long = "clickhouse-url",
        env = "GROUNDSTATION_CLICKHOUSE_URL",
        default_value = "http://127.0.0.1:8123"
    )]
    pub clickhouse_url: String,
    /// [default: the service's own user: gs_ingest, gs_api or gs_worker]
    #[arg(long = "clickhouse-user", env = "GROUNDSTATION_CLICKHOUSE_USER")]
    pub clickhouse_user: Option<String>,
    #[arg(
        long = "clickhouse-password",
        env = "GROUNDSTATION_CLICKHOUSE_PASSWORD",
        default_value = "groundstation",
        hide_env_values = true
    )]
    pub clickhouse_password: String,
    #[arg(
        long = "clickhouse-database",
        env = "GROUNDSTATION_CLICKHOUSE_DATABASE",
        default_value = "groundstation"
    )]
    pub clickhouse_database: String,
}

impl ClickhouseArgs {
    pub fn user_for(&self, service: Service) -> &str {
        self.clickhouse_user
            .as_deref()
            .unwrap_or_else(|| service.clickhouse_user())
    }
}

/// The ClickHouse user that creates tables, roles and row policies. Only the
/// service that migrates (api) needs it, and only at startup.
#[derive(clap::Args, Debug, Clone)]
pub struct ClickhouseAdminArgs {
    #[arg(
        long = "clickhouse-admin-user",
        env = "GROUNDSTATION_CLICKHOUSE_ADMIN_USER",
        default_value = "groundstation"
    )]
    pub clickhouse_admin_user: String,
    #[arg(
        long = "clickhouse-admin-password",
        env = "GROUNDSTATION_CLICKHOUSE_ADMIN_PASSWORD",
        default_value = "groundstation",
        hide_env_values = true
    )]
    pub clickhouse_admin_password: String,
}

#[derive(clap::Args, Debug, Clone)]
pub struct PostgresArgs {
    /// Postgres connection URL. `sslmode=require` connects over TLS.
    #[arg(
        long = "postgres-url",
        env = "GROUNDSTATION_POSTGRES_URL",
        default_value = "postgres://groundstation:groundstation@127.0.0.1:5432/groundstation",
        hide_env_values = true
    )]
    pub postgres_url: String,
    #[arg(
        long = "postgres-max-connections",
        env = "GROUNDSTATION_POSTGRES_MAX_CONNECTIONS",
        default_value_t = 10
    )]
    pub postgres_max_connections: usize,
}
