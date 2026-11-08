//! Resolving `authorization: Bearer <ingest token>` to a tenant. Answers are
//! cached for a minute, so a revoked token stops working within one.
//!
//! Requests without a valid token can't be rate limited by token, so what
//! they can cost is bounded here: a token of the wrong shape is refused
//! without a lookup, an unknown one is remembered like a known one, and
//! lookups in Postgres are limited across all clients.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, header};
use groundstation_backend::pg::{self, Pool, tokens::Grant};
use groundstation_backend::secret;
use uuid::Uuid;

use crate::ratelimit::{Rate, RateLimiter};

const MAX_ENTRIES: usize = 10_000;
/// Longer than any token this backend has issued.
const MAX_TOKEN: usize = 256;

#[derive(Debug)]
pub enum AuthError {
    Missing,
    Unknown,
    Suspended,
    /// Too many lookups at once; says how long until the next is allowed.
    Busy(Duration),
    Unavailable(groundstation_backend::Error),
}

/// What Postgres said about a token, and when: `None` is a token it doesn't know.
type Answer = (Option<Grant>, Instant);

pub struct Authenticator {
    pg: Pool,
    /// Keyed by the token's hash, so the cache never holds a usable token.
    cache: Mutex<HashMap<Vec<u8>, Answer>>,
    ttl: Duration,
    lookups: RateLimiter,
}

/// What the cache knows about a token.
enum Cached {
    /// An answer younger than the TTL.
    Fresh(Option<Grant>),
    /// An older answer that was a grant: the token was real a minute ago.
    Granted,
    Unknown,
}

impl Authenticator {
    /// `lookups` limits how often tokens are looked up in Postgres; answers
    /// are reused for `ttl`.
    pub fn new(pg: Pool, lookups: Rate, ttl: Duration) -> Self {
        Self {
            pg,
            cache: Mutex::new(HashMap::new()),
            ttl,
            lookups: RateLimiter::new(lookups),
        }
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Grant, AuthError> {
        let token = bearer(headers).ok_or(AuthError::Missing)?;
        if !well_formed(token) {
            return Err(AuthError::Unknown);
        }
        let key = secret::hash(token);
        let grant = match self.cached(&key) {
            Cached::Fresh(answer) => answer,
            cached => {
                // Checking again a token that was real a minute ago doesn't
                // wait behind a flood of made-up ones. There are only as many
                // such checks as real tokens in use, once a minute each.
                if !matches!(cached, Cached::Granted) {
                    self.lookups
                        .take(Uuid::nil(), Instant::now())
                        .map_err(AuthError::Busy)?;
                }
                let found = pg::tokens::authenticate(&self.pg, token)
                    .await
                    .map_err(AuthError::Unavailable)?;
                self.remember(key, found.clone());
                found
            }
        }
        .ok_or(AuthError::Unknown)?;
        if grant.suspended {
            return Err(AuthError::Suspended);
        }
        Ok(grant)
    }

    fn cached(&self, key: &[u8]) -> Cached {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(key) {
            Some((grant, at)) if at.elapsed() < self.ttl => Cached::Fresh(grant.clone()),
            Some((Some(_), _)) => Cached::Granted,
            _ => Cached::Unknown,
        }
    }

    fn remember(&self, key: Vec<u8>, grant: Option<Grant>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= MAX_ENTRIES {
            cache.retain(|_, (_, at)| at.elapsed() < self.ttl);
        }
        // A flood of made-up tokens must not push out the real ones.
        if grant.is_some() || cache.len() < MAX_ENTRIES {
            cache.insert(key, (grant, Instant::now()));
        }
    }
}

/// Whether `token` could be one this backend issued.
fn well_formed(token: &str) -> bool {
    token.starts_with(secret::INGEST_TOKEN_PREFIX) && token.len() <= MAX_TOKEN
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bearer_tokens_only() {
        let with = |value: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::AUTHORIZATION, value.parse().unwrap());
            bearer(&h).map(str::to_string)
        };
        assert_eq!(with("Bearer gsi_abc").as_deref(), Some("gsi_abc"));
        assert_eq!(with("bearer  gsi_abc ").as_deref(), Some("gsi_abc"));
        assert_eq!(with("Basic dXNlcg=="), None);
        assert_eq!(with("Bearer "), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn only_tokens_of_the_issued_shape_are_looked_up() {
        assert!(well_formed(&secret::generate(secret::INGEST_TOKEN_PREFIX)));
        assert!(!well_formed("gss_a_session_id"));
        assert!(!well_formed(&format!("gsi_{}", "x".repeat(MAX_TOKEN))));
    }
}
