//! Postgres: tenants, ingest tokens, insights, and the migrations that create
//! them and the api's accounts.

pub mod insights;
mod migrate;
pub mod tenants;
pub mod tokens;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use deadpool_postgres::{Manager, ManagerConfig, RecyclingMethod, Runtime};
pub use deadpool_postgres::{Object as Connection, Pool};
pub use migrate::{INGEST_ROLE, WORKER_ROLE, migrate};
use percent_encoding::percent_decode_str;
use tokio_postgres::NoTls;
use tokio_postgres::config::SslMode;

use crate::config::PostgresArgs;
use crate::{Error, Result};

const TIMEOUT: Duration = Duration::from_secs(5);

/// A connection pool. Lazy: connections open on first use, so a service starts
/// while Postgres is down and its health check says so.
pub fn connect(args: &PostgresArgs) -> Result<Pool> {
    let (url, root_cert) = take_root_cert(&args.postgres_url);
    let config: tokio_postgres::Config = tls_mode(&url).parse().map_err(|e| {
        let cause = std::error::Error::source(&e).map(|c| format!(": {c}"));
        Error::PostgresConfig(format!("{e}{}", cause.unwrap_or_default()))
    })?;
    let recycling = ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    };
    let manager = match config.get_ssl_mode() {
        SslMode::Require => Manager::from_config(config, tls(root_cert.as_deref())?, recycling),
        // Whoever names a CA expects verified TLS; without it, the password
        // and every tenant's data would cross the network in the clear.
        _ if root_cert.is_some() => {
            return Err(Error::PostgresConfig(
                "sslrootcert needs sslmode=require (or verify-ca, verify-full): without it \
                 the connection isn't encrypted"
                    .into(),
            ));
        }
        _ => Manager::from_config(config, NoTls, recycling),
    };
    Pool::builder(manager)
        .max_size(args.postgres_max_connections.max(1))
        .runtime(Runtime::Tokio1)
        .wait_timeout(Some(TIMEOUT))
        .create_timeout(Some(TIMEOUT))
        .recycle_timeout(Some(TIMEOUT))
        .build()
        .map_err(|e| Error::PostgresConfig(e.to_string()))
}

/// `sslmode=verify-ca` and `verify-full`, as managed Postgres hands them out,
/// read as `require`: TLS here always verifies the certificate and the host
/// name, and the driver knows no stricter mode.
fn tls_mode(url: &str) -> String {
    url.replace("sslmode=verify-full", "sslmode=require")
        .replace("sslmode=verify-ca", "sslmode=require")
}

/// Takes `sslrootcert` out of a URL, since the driver doesn't know it: the
/// CA a managed Postgres (RDS, Cloud SQL) signs its certificates with, which
/// the system usually doesn't trust.
fn take_root_cert(url: &str) -> (String, Option<String>) {
    let Some((base, query)) = url.split_once('?') else {
        return (url.to_string(), None);
    };
    let (mut root, mut kept) = (None, Vec::new());
    for pair in query.split('&') {
        match pair.strip_prefix("sslrootcert=") {
            Some(path) => root = Some(percent_decode_str(path).decode_utf8_lossy().into_owned()),
            None => kept.push(pair),
        }
    }
    let url = match kept.is_empty() {
        true => base.to_string(),
        false => format!("{base}?{}", kept.join("&")),
    };
    (url, root)
}

/// TLS that verifies the server's certificate and host name: against the CA
/// in `root_cert` when there is one, otherwise against the system's roots.
fn tls(root_cert: Option<&str>) -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let config = match root_cert {
        Some(path) => with_root(Path::new(path))?,
        None => {
            use rustls_platform_verifier::ConfigVerifierExt;
            rustls::ClientConfig::with_platform_verifier()
                .map_err(|e| Error::PostgresConfig(format!("TLS: {e}")))?
        }
    };
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

fn with_root(path: &Path) -> Result<rustls::ClientConfig> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    let unusable = |e: &dyn std::fmt::Display| {
        Error::PostgresConfig(format!("sslrootcert {}: {e}", path.display()))
    };
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(path).map_err(|e| unusable(&e))? {
        roots
            .add(cert.map_err(|e| unusable(&e))?)
            .map_err(|e| unusable(&e))?;
    }
    if roots.is_empty() {
        return Err(unusable(&"no certificates in the file"));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| unusable(&e))?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// Checks that Postgres answers.
pub async fn ping(pool: &Pool) -> Result<()> {
    pool.get().await?.simple_query("SELECT 1").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Args {
        #[command(flatten)]
        postgres: PostgresArgs,
    }

    #[tokio::test]
    async fn accepts_the_verifying_tls_modes() {
        for mode in ["require", "verify-ca", "verify-full"] {
            let url = format!("postgres://u:p@db.example:5432/gs?sslmode={mode}");
            let args = Args::parse_from(["x", "--postgres-url", &url]);
            assert!(connect(&args.postgres).is_ok(), "{mode}");
        }
        let args = Args::parse_from(["x", "--postgres-url", "postgres://u@h/d?sslmode=nope"]);
        assert!(matches!(
            connect(&args.postgres),
            Err(Error::PostgresConfig(_))
        ));
    }

    /// A self-signed certificate, made for this test (its key is not kept).
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----
MIIBlzCCAT2gAwIBAgIUAm890xNlGqJRAbWHyv8YFrjp7w4wCgYIKoZIzj0EAwIw
ITEfMB0GA1UEAwwWR3JvdW5kIFN0YXRpb24gdGVzdCBDQTAeFw0yNjEwMDMxNDM3
NThaFw0zNjA5MzAxNDM3NThaMCExHzAdBgNVBAMMFkdyb3VuZCBTdGF0aW9uIHRl
c3QgQ0EwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASE0Wv0FhIB9GxumIVKnfow
uswViy4AGr+KwrdS53kK4FI6qVfuYNNWAfbJCbBkOAcBiMSaoc6sOD3NRu8Jhj27
o1MwUTAdBgNVHQ4EFgQU2y3KUJDoPW43asAB1uGOWhjEQA0wHwYDVR0jBBgwFoAU
2y3KUJDoPW43asAB1uGOWhjEQA0wDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQD
AgNIADBFAiBaSFJnbt1xTTIv+b3czqNskHMai2AzOkileX9eCA/0+gIhAMiJJeRz
N2sJBwqdKMzLkhFBFt6URAcQnffRGb52f0MI
-----END CERTIFICATE-----\n";

    #[test]
    fn reads_the_ca_from_sslrootcert() {
        let (url, root) = take_root_cert(
            "postgres://u@db/gs?sslmode=verify-full&sslrootcert=%2Fetc%2Frds%20ca.pem",
        );
        assert_eq!(url, "postgres://u@db/gs?sslmode=verify-full");
        assert_eq!(root.as_deref(), Some("/etc/rds ca.pem"));
        assert_eq!(take_root_cert("postgres://u@db/gs").1, None);

        let dir = std::env::temp_dir().join(format!("gs-pg-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ca = dir.join("ca.pem");
        std::fs::write(&ca, TEST_CA).unwrap();
        let connect_with = |root: &std::path::Path| {
            let url = format!(
                "postgres://u:p@db.example/gs?sslmode=verify-full&sslrootcert={}",
                root.display()
            );
            connect(&Args::parse_from(["x", "--postgres-url", &url]).postgres)
        };
        assert!(connect_with(&ca).is_ok());
        let Err(Error::PostgresConfig(e)) = connect_with(&dir.join("missing.pem")) else {
            panic!("a missing CA file must be refused");
        };
        assert!(e.contains("missing.pem"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_ca_without_tls_is_refused_not_dropped() {
        for query in [
            "sslrootcert=/etc/rds-ca.pem",
            "sslmode=prefer&sslrootcert=/etc/rds-ca.pem",
            "sslrootcert=/etc/rds-ca.pem&sslmode=disable",
        ] {
            let url = format!("postgres://u:p@db.example/gs?{query}");
            let Err(Error::PostgresConfig(e)) =
                connect(&Args::parse_from(["x", "--postgres-url", &url]).postgres)
            else {
                panic!("{query}: a CA without sslmode=require would connect unencrypted");
            };
            assert!(
                e.contains("sslrootcert") && e.contains("sslmode=require"),
                "{e}"
            );
        }
    }

    #[test]
    fn says_which_setting_it_refuses() {
        let url = "postgres://u@db/gs?sslcert=/client.pem";
        let Err(Error::PostgresConfig(e)) =
            connect(&Args::parse_from(["x", "--postgres-url", url]).postgres)
        else {
            panic!("sslcert is not supported");
        };
        assert!(e.contains("sslcert"), "{e}");
    }
}
