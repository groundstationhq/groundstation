//! Resolving `authorization: Bearer <ingest token>` to a tenant. Answers are
//! cached for a minute, so a revoked token stops working within one.
//!
//! Requests without a valid token can't be rate limited by token, so what
//! they can cost is bounded here: a token of the wrong shape is refused
//! without a lookup, an unknown one is remembered like a known one, and
//! lookups in Postgres are limited across all clients.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, header};
use groundstation_backend::pg::{self, Pool, tokens::Grant};
use groundstation_backend::secret;
use groundstation_wire::is_ingest_token;
use uuid::Uuid;

use crate::ratelimit::{Rate, RateLimiter};

const MAX_ENTRIES: usize = 10_000;
/// How long a token that was real stays known after it was last checked.
const GRANT_KEPT: Duration = Duration::from_secs(24 * 60 * 60);

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
    cache: Mutex<Cache>,
    lookups: RateLimiter,
}

/// Answers by token hash, so the cache never holds a usable token.
struct Cache {
    answers: HashMap<Vec<u8>, Answer>,
    /// How long an answer is used without asking Postgres again.
    ttl: Duration,
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
            cache: Mutex::new(Cache {
                answers: HashMap::new(),
                ttl,
            }),
            lookups: RateLimiter::new(lookups),
        }
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Grant, AuthError> {
        let token = bearer(headers).ok_or(AuthError::Missing)?;
        if !is_ingest_token(token) {
            return Err(AuthError::Unknown);
        }
        let key = secret::hash(token);
        // Looked up on its own line: the lock must be let go before the wait below.
        let cached = self.cache().cached(&key);
        let grant = match cached {
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
                self.cache().remember(key, found.clone());
                found
            }
        }
        .ok_or(AuthError::Unknown)?;
        if grant.suspended {
            return Err(AuthError::Suspended);
        }
        Ok(grant)
    }

    fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Cache {
    fn cached(&self, key: &[u8]) -> Cached {
        match self.answers.get(key) {
            Some((grant, at)) if at.elapsed() < self.ttl => Cached::Fresh(grant.clone()),
            Some((Some(_), _)) => Cached::Granted,
            _ => Cached::Unknown,
        }
    }

    fn remember(&mut self, key: Vec<u8>, grant: Option<Grant>) {
        if self.answers.len() >= MAX_ENTRIES {
            // Full, which is what a flood of made-up tokens does: its old
            // answers go. An old grant stays, because it is what lets a real
            // token be checked again without waiting behind the flood; there
            // are only as many as real tokens used in the last day.
            let ttl = self.ttl;
            self.answers.retain(|_, (grant, at)| {
                at.elapsed() < ttl || (grant.is_some() && at.elapsed() < GRANT_KEPT)
            });
        }
        // Nor may the flood keep the real ones out.
        if grant.is_some() || self.answers.len() < MAX_ENTRIES {
            self.answers.insert(key, (grant, Instant::now()));
        }
    }
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
    fn a_flood_of_made_up_tokens_never_pushes_out_a_real_one() {
        // Every answer is old at once.
        let mut cache = Cache {
            answers: HashMap::new(),
            ttl: Duration::ZERO,
        };
        let grant = Grant {
            token_id: Uuid::now_v7(),
            tenant_id: Uuid::now_v7(),
            retention_days: 30,
            suspended: false,
        };
        let real = secret::hash("real");
        cache.remember(real.clone(), Some(grant));
        for i in 0..2 * MAX_ENTRIES {
            cache.remember(secret::hash(&format!("made up {i}")), None);
        }
        assert!(cache.answers.len() <= MAX_ENTRIES);
        // Still known to have been real: its next check skips the limiter.
        assert!(matches!(cache.cached(&real), Cached::Granted));
        assert!(matches!(
            cache.cached(&secret::hash("made up 0")),
            Cached::Unknown
        ));
    }

    #[test]
    fn only_tokens_of_the_issued_shape_are_looked_up() {
        assert!(is_ingest_token(&secret::generate(
            secret::INGEST_TOKEN_PREFIX
        )));
        assert!(!is_ingest_token("gss_a_session_id"));
        assert!(!is_ingest_token("gsi_made_up"));
        assert!(!is_ingest_token(&format!("gsi_{}", "x".repeat(256))));
    }
}
