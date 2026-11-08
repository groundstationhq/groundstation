//! Signing in with an OpenID Connect provider: authorization code flow with
//! PKCE and a nonce. The provider's metadata and keys are fetched on first use
//! and refreshed hourly, so the api starts while the provider is unreachable
//! and picks up rotated keys.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, HttpRequest, HttpResponse, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use tokio::sync::Mutex;

use crate::accounts::Identity;
use crate::config::SignInArgs;
use crate::sessions::Flow;

const REFRESH: Duration = Duration::from_secs(3600);
/// The soonest the provider is asked again after it couldn't be reached.
const RETRY: Duration = Duration::from_secs(30);
/// The soonest the provider's keys are fetched again because a token was
/// signed with one the api doesn't have yet.
const ROTATED: Duration = Duration::from_secs(60);

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("the identity provider can't be reached: {0}")]
    Discovery(String),
    #[error("the identity provider refused the sign-in: {0}")]
    Exchange(String),
    #[error("the identity provider's answer didn't verify: {0}")]
    Verification(String),
    #[error("this account isn't allowed to sign in here")]
    NotAllowed,
}

pub struct Oidc {
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: Option<ClientSecret>,
    redirect: RedirectUrl,
    allowed_domains: Vec<String>,
    pub name: String,
    http: Http,
    client: Mutex<Discovered>,
}

/// What the api knows of the provider's metadata and keys.
enum Discovered {
    Nothing,
    Client(Arc<Client>, Instant),
    /// It couldn't be reached at that time, for that reason.
    Failed(String, Instant),
}

/// Where to send the browser, and what to remember until it comes back.
pub struct Start {
    pub url: String,
    pub state: String,
    pub pkce_verifier: String,
    pub nonce: String,
}

impl Oidc {
    pub fn new(args: &SignInArgs, public_url: &str) -> anyhow::Result<Option<Self>> {
        let (Some(issuer), Some(client_id)) = (&args.oidc_issuer, &args.oidc_client_id) else {
            return Ok(None);
        };
        let redirect = format!("{}/auth/callback", public_url.trim_end_matches('/'));
        let http = reqwest::Client::builder()
            // Following redirects would let a provider point us anywhere.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Some(Self {
            issuer: IssuerUrl::new(issuer.clone())
                .map_err(|e| anyhow::anyhow!("GROUNDSTATION_OIDC_ISSUER {issuer:?}: {e}"))?,
            client_id: ClientId::new(client_id.clone()),
            client_secret: args.oidc_client_secret.clone().map(ClientSecret::new),
            redirect: RedirectUrl::new(redirect)
                .map_err(|e| anyhow::anyhow!("GROUNDSTATION_PUBLIC_URL {public_url:?}: {e}"))?,
            allowed_domains: args
                .oidc_allowed_domains
                .iter()
                .map(|d| d.trim().to_ascii_lowercase())
                .filter(|d| !d.is_empty())
                .collect(),
            name: args.oidc_name.clone(),
            http: Http(http),
            client: Mutex::new(Discovered::Nothing),
        }))
    }

    /// The provider's metadata and keys, fetched again hourly, or sooner when
    /// `older_than` says the ones held are too old. A provider that can't be
    /// reached is asked again only after [`RETRY`]: anyone can start a
    /// sign-in, and each would otherwise wait on it.
    async fn client(&self, older_than: Duration) -> Result<Arc<Client>, OidcError> {
        let mut cached = self.client.lock().await;
        match &*cached {
            Discovered::Client(client, at) if at.elapsed() < older_than => {
                return Ok(client.clone());
            }
            Discovered::Failed(why, at) if at.elapsed() < RETRY => {
                return Err(OidcError::Discovery(why.clone()));
            }
            _ => {}
        }
        let metadata =
            match CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http).await {
                Ok(metadata) => metadata,
                Err(e) => {
                    *cached = Discovered::Failed(e.to_string(), Instant::now());
                    return Err(OidcError::Discovery(e.to_string()));
                }
            };
        let client = Arc::new(
            CoreClient::from_provider_metadata(
                metadata,
                self.client_id.clone(),
                self.client_secret.clone(),
            )
            .set_redirect_uri(self.redirect.clone()),
        );
        *cached = Discovered::Client(client.clone(), Instant::now());
        Ok(client)
    }

    pub async fn start(&self) -> Result<Start, OidcError> {
        let client = self.client(REFRESH).await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".into()))
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .url();
        Ok(Start {
            url: url.to_string(),
            state: state.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
            nonce: nonce.secret().clone(),
        })
    }

    /// Trades the code the provider sent back for a verified identity.
    pub async fn finish(&self, code: String, flow: &Flow) -> Result<Identity, OidcError> {
        let client = self.client(REFRESH).await?;
        let tokens = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|e| OidcError::Exchange(e.to_string()))?
            .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier.clone()))
            .request_async(&self.http)
            .await
            .map_err(|e| OidcError::Exchange(e.to_string()))?;
        let id_token = tokens
            .id_token()
            .ok_or_else(|| OidcError::Verification("no ID token".into()))?;
        let nonce = Nonce::new(flow.nonce.clone());
        let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
            Ok(claims) => claims.clone(),
            // Signed with a key published since the keys were fetched: the
            // provider rotated them. Fetch them again and check once more.
            Err(first) => {
                let client = self.client(ROTATED).await?;
                id_token
                    .claims(&client.id_token_verifier(), &nonce)
                    .map_err(|_| OidcError::Verification(first.to_string()))?
                    .clone()
            }
        };
        let email = claims.email().map(|e| e.as_str().to_string());
        if !self.allowed(email.as_deref(), claims.email_verified()) {
            return Err(OidcError::NotAllowed);
        }
        Ok(Identity {
            issuer: self.issuer.as_str().to_string(),
            subject: claims.subject().as_str().to_string(),
            email,
            name: claims
                .name()
                .and_then(|n| n.get(None))
                .map(|n| n.as_str().to_string()),
        })
    }

    fn allowed(&self, email: Option<&str>, verified: Option<bool>) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }
        let Some((_, domain)) = email.and_then(|e| e.rsplit_once('@')) else {
            return false;
        };
        verified == Some(true)
            && self
                .allowed_domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(domain))
    }
}

/// The OAuth library's HTTP client, on the reqwest the rest of the workspace uses.
#[derive(Clone)]
struct Http(reqwest::Client);

#[derive(Debug, thiserror::Error)]
enum HttpError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Http(#[from] openidconnect::http::Error),
}

impl<'c> AsyncHttpClient<'c> for Http {
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let response = self.0.execute(request.try_into()?).await?;
            let mut builder = openidconnect::http::Response::builder().status(response.status());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            let body = response.bytes().await?.to_vec();
            Ok(builder.body(body)?)
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::config::Config;

    fn oidc(domains: &str) -> Oidc {
        let config = Config::parse_from([
            "api",
            "--oidc-issuer",
            "https://idp.example",
            "--oidc-client-id",
            "gs",
            "--oidc-allowed-domains",
            domains,
        ]);
        Oidc::new(&config.sign_in, &config.public_url)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn allowed_domains_need_a_verified_email_at_one_of_them() {
        let o = oidc("acme.com,Example.org");
        assert!(o.allowed(Some("ana@acme.com"), Some(true)));
        assert!(o.allowed(Some("bo@EXAMPLE.org"), Some(true)));
        assert!(!o.allowed(Some("ana@acme.com"), None));
        assert!(!o.allowed(Some("ana@acme.com.evil.io"), Some(true)));
        assert!(!o.allowed(None, Some(true)));
        assert!(oidc("").allowed(None, None));
    }
}
