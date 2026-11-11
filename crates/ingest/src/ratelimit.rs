//! Token buckets per key (an ingest token, a tenant). In memory and per
//! instance, so limits across instances are approximate; they exist to stop
//! runaway clients, not to meter.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// Requests per second a key may sustain, and how many it may send at once.
#[derive(Debug, Clone, Copy)]
pub struct Rate {
    pub per_second: f64,
    pub burst: f64,
}

pub struct RateLimiter {
    rate: Rate,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Buckets kept before idle, full ones are forgotten.
const PRUNE_ABOVE: usize = 10_000;

impl RateLimiter {
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one request from `key`'s bucket, or says how long until it can.
    pub fn take(&self, key: Uuid, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > PRUNE_ABOVE {
            buckets.retain(|_, b| self.refill(*b, now).tokens < self.rate.burst);
        }
        let full = Bucket {
            tokens: self.rate.burst,
            at: now,
        };
        let bucket = buckets.entry(key).or_insert(full);
        *bucket = self.refill(*bucket, now);
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(());
        }
        let wait = (1.0 - bucket.tokens) / self.rate.per_second;
        // A rate of zero never refills: say "later" instead of "never".
        Err(Duration::try_from_secs_f64(wait).unwrap_or(Duration::from_secs(60)))
    }

    fn refill(&self, b: Bucket, now: Instant) -> Bucket {
        let elapsed = now.saturating_duration_since(b.at).as_secs_f64();
        Bucket {
            tokens: (b.tokens + elapsed * self.rate.per_second).min(self.rate.burst),
            at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_a_burst_then_the_sustained_rate() {
        let limiter = RateLimiter::new(Rate {
            per_second: 2.0,
            burst: 3.0,
        });
        let (key, other, t0) = (Uuid::now_v7(), Uuid::now_v7(), Instant::now());
        for _ in 0..3 {
            limiter.take(key, t0).unwrap();
        }
        let wait = limiter.take(key, t0).unwrap_err();
        assert_eq!(wait, Duration::from_millis(500));
        // Keys don't share buckets.
        limiter.take(other, t0).unwrap();
        // Half a second refills one request.
        limiter.take(key, t0 + Duration::from_millis(500)).unwrap();
        assert!(limiter.take(key, t0 + Duration::from_millis(500)).is_err());
    }
}
