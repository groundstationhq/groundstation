//! A limit on wrong admin passwords. There is one password, so the limit is
//! one bucket for everyone: ten attempts at once, then one every ten seconds.
//! That makes guessing a 12-character password hopeless. It also means a
//! flood of wrong guesses locks password sign-in until it stops; signing in
//! with a provider is unaffected.

use std::sync::Mutex;
use std::time::Instant;

const BURST: f64 = 10.0;
const PER_SECOND: f64 = 0.1;

pub struct Throttle {
    /// Attempts left, and when that was last brought up to date.
    state: Mutex<(f64, Instant)>,
}

impl Default for Throttle {
    fn default() -> Self {
        Self {
            state: Mutex::new((BURST, Instant::now())),
        }
    }
}

impl Throttle {
    /// Takes one attempt; false when none is left. Every attempt takes one
    /// before the password is compared, so guesses sent at once can't all
    /// pass before the first failure counts.
    pub fn take(&self, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (left, at) = *state;
        let elapsed = now.saturating_duration_since(at).as_secs_f64();
        let left = (left + elapsed * PER_SECOND).min(BURST);
        let allowed = left >= 1.0;
        *state = (if allowed { left - 1.0 } else { left }, now);
        allowed
    }

    /// Gives an attempt back: the password was right.
    pub fn give_back(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = (state.0 + 1.0).min(BURST);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn ten_wrong_passwords_then_one_every_ten_seconds() {
        let throttle = Throttle::default();
        let t0 = Instant::now();
        assert!((0..10).all(|_| throttle.take(t0)));
        assert!(!throttle.take(t0));
        assert!(!throttle.take(t0 + Duration::from_secs(9)));
        assert!(throttle.take(t0 + Duration::from_secs(10)));
        assert!(!throttle.take(t0 + Duration::from_secs(10)));
    }

    #[test]
    fn right_passwords_cost_nothing() {
        let throttle = Throttle::default();
        let t0 = Instant::now();
        for _ in 0..50 {
            assert!(throttle.take(t0));
            throttle.give_back();
        }
    }
}
