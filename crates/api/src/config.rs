//! Settings, from flags or environment variables. Database defaults match
//! `docker-compose.yml`; sign-in has no default and must be configured.

use std::net::SocketAddr;

use groundstation_backend::config::{ClickhouseAdminArgs, ClickhouseArgs, PostgresArgs};

#[derive(clap::Parser, Debug, Clone)]
#[command(version, about = "Ground Station hosted API")]
pub struct Config {
    /// Address to listen on.
    #[arg(
        long,
        env = "GROUNDSTATION_API_LISTEN",
        default_value = "127.0.0.1:4321"
    )]
    pub listen: SocketAddr,
    /// The origin people and gsd use, behind the edge. Sign-in redirects,
    /// cookies and same-origin checks are based on it.
    #[arg(
        long,
        env = "GROUNDSTATION_PUBLIC_URL",
        default_value = "http://127.0.0.1:4321"
    )]
    pub public_url: String,
    /// Where gsd uploads, when uploads have an origin of their own: the one
    /// that sends `POST /v1/events` to ingest. `groundstation cloud login`
    /// asks for it. [default: the public URL, behind an edge that routes
    /// uploads to ingest]
    #[arg(long, env = "GROUNDSTATION_INGEST_URL")]
    pub ingest_url: Option<String>,
    #[command(flatten)]
    pub clickhouse: ClickhouseArgs,
    #[command(flatten)]
    pub clickhouse_admin: ClickhouseAdminArgs,
    #[command(flatten)]
    pub postgres: PostgresArgs,
    #[command(flatten)]
    pub sign_in: SignInArgs,
    /// How long new tenants keep events, in days.
    #[arg(long, env = "GROUNDSTATION_RETENTION_DAYS", default_value_t = 90)]
    pub retention_days: i32,
    /// How long a browser session lasts, in days.
    #[arg(long, env = "GROUNDSTATION_SESSION_DAYS", default_value_t = 30)]
    pub session_days: i64,
}

#[derive(clap::Args, Debug, Clone)]
pub struct SignInArgs {
    /// Sign in with this password as the admin user, for a single-person
    /// instance without an identity provider.
    #[arg(long, env = "GROUNDSTATION_ADMIN_PASSWORD", hide_env_values = true)]
    pub admin_password: Option<String>,
    /// OpenID Connect issuer URL, e.g. `https://accounts.google.com`.
    #[arg(long, env = "GROUNDSTATION_OIDC_ISSUER", requires = "oidc_client_id")]
    pub oidc_issuer: Option<String>,
    #[arg(long, env = "GROUNDSTATION_OIDC_CLIENT_ID", requires = "oidc_issuer")]
    pub oidc_client_id: Option<String>,
    #[arg(long, env = "GROUNDSTATION_OIDC_CLIENT_SECRET", hide_env_values = true)]
    pub oidc_client_secret: Option<String>,
    /// What the sign-in button says the provider is called.
    #[arg(
        long,
        env = "GROUNDSTATION_OIDC_NAME",
        default_value = "single sign-on"
    )]
    pub oidc_name: String,
    /// Only let people whose verified email is at one of these domains sign
    /// in (comma-separated). Leave empty only if the provider admits just
    /// your organization.
    #[arg(
        long,
        env = "GROUNDSTATION_OIDC_ALLOWED_DOMAINS",
        value_delimiter = ',',
        num_args = 0..
    )]
    pub oidc_allowed_domains: Vec<String>,
    /// Where a person signing in for the first time works: `personal` gives
    /// each their own tenant; `shared` adds everyone to one tenant, whose
    /// first member administers it.
    #[arg(long, env = "GROUNDSTATION_NEW_USERS", value_enum, default_value_t = NewUsers::Personal)]
    pub new_users: NewUsers,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewUsers {
    Personal,
    Shared,
}

impl SignInArgs {
    /// Treats empty values as unset: compose files and secret stores often
    /// pass `VAR=` for "not configured".
    fn drop_empty(&mut self) {
        for value in [
            &mut self.admin_password,
            &mut self.oidc_issuer,
            &mut self.oidc_client_id,
            &mut self.oidc_client_secret,
        ] {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                *value = None;
            }
        }
        self.oidc_allowed_domains = self
            .oidc_allowed_domains
            .iter()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
    }
}

impl Config {
    /// Whether cookies can be marked `Secure`.
    pub fn https(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    /// The public origin (scheme, host and port), as browsers send it in
    /// `Origin`. [`Config::check`] has put the public URL in this form.
    pub fn origin(&self) -> &str {
        &self.public_url
    }

    /// The origin gsd uploads to: ingest's own, or the public one.
    pub fn upload_origin(&self) -> &str {
        self.ingest_url.as_deref().unwrap_or(&self.public_url)
    }

    pub fn check(&mut self) -> anyhow::Result<()> {
        self.public_url = origin_of("GROUNDSTATION_PUBLIC_URL", &self.public_url)?;
        // Empty counts as unset, as for sign-in: `VAR=` is how compose files
        // and secret stores say "not configured".
        self.ingest_url = match self.ingest_url.take().filter(|url| !url.trim().is_empty()) {
            Some(url) => Some(origin_of("GROUNDSTATION_INGEST_URL", &url)?),
            None => None,
        };
        if !(1..=400).contains(&self.session_days) {
            anyhow::bail!(
                "GROUNDSTATION_SESSION_DAYS must be 1 to 400: browsers keep cookies 400 days at most"
            );
        }
        self.sign_in.drop_empty();
        let s = &self.sign_in;
        if s.oidc_issuer.is_some() != s.oidc_client_id.is_some() {
            anyhow::bail!(
                "set both GROUNDSTATION_OIDC_ISSUER and GROUNDSTATION_OIDC_CLIENT_ID, or neither"
            );
        }
        if s.admin_password.is_none() && s.oidc_issuer.is_none() {
            anyhow::bail!(
                "no way to sign in: set GROUNDSTATION_ADMIN_PASSWORD or GROUNDSTATION_OIDC_ISSUER"
            );
        }
        if s.admin_password
            .as_ref()
            .is_some_and(|p| p.chars().count() < 12)
        {
            anyhow::bail!("GROUNDSTATION_ADMIN_PASSWORD must be at least 12 characters");
        }
        if self.retention_days < 1 {
            anyhow::bail!("GROUNDSTATION_RETENTION_DAYS must be at least 1");
        }
        if s.oidc_issuer.is_some()
            && s.new_users == NewUsers::Shared
            && s.oidc_allowed_domains.is_empty()
        {
            tracing::warn!(
                "new users join the shared tenant and any account at the identity provider can sign in; \
                 set GROUNDSTATION_OIDC_ALLOWED_DOMAINS unless the provider admits only your organization"
            );
        }
        Ok(())
    }
}

/// `url` as an origin, the way browsers write it in `Origin`: lowercase
/// scheme and host, no default port, no trailing slash. Ground Station is
/// served at the root of its own origin, so anything after it is refused.
fn origin_of(setting: &str, url: &str) -> anyhow::Result<String> {
    let refuse = |why: &str| anyhow::anyhow!("{setting} {url:?}: {why}");
    let parsed = reqwest::Url::parse(url).map_err(|e| refuse(&e.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(refuse("must be an http:// or https:// URL"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refuse("must not carry credentials"));
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(refuse(
            "must be an origin with nothing after it: Ground Station is served at the root",
        ));
    }
    Ok(parsed.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn the_public_url_is_read_as_browsers_write_origins() {
        let with = |url: &str| {
            let mut config = Config::parse_from([
                "api",
                "--public-url",
                url,
                "--admin-password",
                "a long password",
            ]);
            config.check().map(|()| config.origin().to_string())
        };
        assert_eq!(
            with("https://gs.example.com/").unwrap(),
            "https://gs.example.com"
        );
        assert_eq!(
            with("HTTPS://GS.Example.com:443").unwrap(),
            "https://gs.example.com"
        );
        assert_eq!(
            with("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            with("https://bücher.example").unwrap(),
            "https://xn--bcher-kva.example"
        );
        for bad in [
            "http://127.0.0.1:8080/app",
            "gs.example.com",
            "ftp://gs.example.com",
            "https://user:pass@gs.example.com",
            "https://gs.example.com/?x=1",
            "https://gs.example.com/#top",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
        let mut https = Config::parse_from([
            "api",
            "--public-url",
            "https://x.example",
            "--admin-password",
            "a long password",
        ]);
        https.check().unwrap();
        assert!(https.https());
    }

    #[test]
    fn uploads_go_to_the_public_url_unless_ingest_has_its_own() {
        let with = |ingest: Option<&str>| {
            let mut args = vec![
                "api",
                "--public-url",
                "https://app.example.com",
                "--admin-password",
                "a long password",
            ];
            if let Some(url) = ingest {
                args.extend(["--ingest-url", url]);
            }
            let mut config = Config::parse_from(args);
            config.check().map(|()| config.upload_origin().to_string())
        };
        assert_eq!(with(None).unwrap(), "https://app.example.com");
        // Set to nothing, as a compose file passes a variable it has no value for.
        assert_eq!(with(Some(" ")).unwrap(), "https://app.example.com");
        assert_eq!(
            with(Some("https://Ingest.example.com/")).unwrap(),
            "https://ingest.example.com"
        );
        let refused = with(Some("https://ingest.example.com/v1/events")).unwrap_err();
        assert!(
            refused.to_string().contains("GROUNDSTATION_INGEST_URL"),
            "{refused}"
        );
    }

    #[test]
    fn sessions_last_a_day_to_400() {
        let with = |days: i64| {
            let mut config = Config::parse_from(["api", "--admin-password", "a long password"]);
            config.session_days = days;
            config.check()
        };
        with(1).unwrap();
        with(400).unwrap();
        assert!(with(0).is_err() && with(-1).is_err() && with(401).is_err());
    }

    #[test]
    fn a_provider_needs_both_issuer_and_client_and_empty_domains_are_none() {
        let mut half = Config::parse_from([
            "api",
            "--admin-password",
            "a long password",
            "--oidc-issuer",
            "https://idp.example",
            "--oidc-client-id",
            " ",
        ]);
        assert!(half.check().is_err());
        let mut empty = Config::parse_from([
            "api",
            "--oidc-issuer",
            "https://idp.example",
            "--oidc-client-id",
            "gs",
            "--oidc-allowed-domains",
            "",
        ]);
        empty.check().unwrap();
        assert!(empty.sign_in.oidc_allowed_domains.is_empty());
    }

    #[test]
    fn needs_a_way_to_sign_in() {
        assert!(Config::parse_from(["api"]).check().is_err());
        let mut short = Config::parse_from(["api", "--admin-password", "short"]);
        assert!(short.check().is_err());
        // Twelve bytes, seven characters.
        let mut accents = Config::parse_from(["api", "--admin-password", "ééééééa"]);
        assert!(accents.check().is_err());
        // Empty means unset, as compose passes it.
        let mut empty = Config::parse_from([
            "api",
            "--admin-password",
            "a long password",
            "--oidc-issuer",
            "",
            "--oidc-client-id",
            "",
        ]);
        empty.check().unwrap();
        assert!(empty.sign_in.oidc_issuer.is_none());
    }

    #[test]
    fn retention_is_at_least_a_day() {
        let with = |days: i32| {
            let mut config = Config::parse_from(["api", "--admin-password", "a long password"]);
            config.retention_days = days;
            config.check()
        };
        with(1).unwrap();
        // Longer than ClickHouse can date is fine: events stay until 2106.
        with(100_000).unwrap();
        assert!(with(0).is_err() && with(-1).is_err());
    }
}
