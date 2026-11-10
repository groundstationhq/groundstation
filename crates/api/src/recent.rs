//! Answers kept for a moment, so that many requests for the same thing cost
//! one computation: every open tab asks for the same list every few seconds.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

/// The last answer per key, reused until it is `ttl` old.
pub struct Recent<K, V> {
    ttl: Duration,
    /// How long a request waits for another's answer before giving up.
    wait: Duration,
    slots: Mutex<Slots<K, V>>,
}

struct Slots<K, V> {
    all: HashMap<K, Slot<V>>,
    /// When answers too old to reuse were last forgotten.
    swept: Instant,
}

/// An answer and when it was computed, behind a lock that requests missing it
/// at once wait on, so they compute it only once.
type Slot<V> = Arc<tokio::sync::Mutex<Option<(V, Instant)>>>;

/// Another request was still computing the answer when the wait ran out.
#[derive(Debug, PartialEq)]
pub struct Busy;

impl<K: Eq + Hash, V: Clone> Recent<K, V> {
    pub fn new(ttl: Duration, wait: Duration) -> Self {
        Self {
            ttl,
            wait,
            slots: Mutex::new(Slots {
                all: HashMap::new(),
                swept: Instant::now(),
            }),
        }
    }

    /// The answer for `key`: the one kept if it is less than `ttl` old,
    /// otherwise `compute`'s, which is then kept. An error is kept for no one:
    /// the next request computes again. A request waits at most `wait` for
    /// those ahead of it, however many of them fail one after another.
    pub async fn get<E, F>(&self, key: K, compute: impl FnOnce() -> F) -> Result<V, E>
    where
        E: From<Busy>,
        F: Future<Output = Result<V, E>>,
    {
        let slot = self.slot(key);
        let mut kept = tokio::time::timeout(self.wait, slot.lock())
            .await
            .map_err(|_| Busy)?;
        if let Some((answer, at)) = &*kept
            && at.elapsed() < self.ttl
        {
            return Ok(answer.clone());
        }
        let answer = compute().await?;
        *kept = Some((answer.clone(), Instant::now()));
        Ok(answer)
    }

    fn slot(&self, key: K) -> Slot<V> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if slots.swept.elapsed() >= self.ttl {
            // What's left is what was asked for within the last `ttl` or two.
            // A slot a request holds stays: forgetting it would let a second
            // request compute the same answer beside the first.
            let ttl = self.ttl;
            slots.all.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || slot
                        .try_lock()
                        .is_ok_and(|kept| kept.as_ref().is_some_and(|(_, at)| at.elapsed() < ttl))
            });
            slots.swept = Instant::now();
        }
        slots.all.entry(key).or_default().clone()
    }

    #[cfg(test)]
    fn kept(&self) -> usize {
        self.slots.lock().unwrap().all.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const TTL: Duration = Duration::from_secs(5);
    const WAIT: Duration = Duration::from_secs(10);

    #[derive(Debug, PartialEq)]
    enum Failed {
        Busy,
        Down,
    }

    impl From<Busy> for Failed {
        fn from(_: Busy) -> Self {
            Self::Busy
        }
    }

    /// Counts how many times it was computed, taking `secs` each time.
    async fn slowly(computed: &AtomicU32, secs: u64) -> Result<u32, Failed> {
        tokio::time::sleep(Duration::from_secs(secs)).await;
        Ok(computed.fetch_add(1, Ordering::SeqCst) + 1)
    }

    #[tokio::test(start_paused = true)]
    async fn requests_at_once_share_one_answer_until_it_is_old() {
        let recent = Recent::new(TTL, WAIT);
        let computed = AtomicU32::new(0);
        let get = |key| recent.get(key, || slowly(&computed, 1));

        let (a, b) = tokio::join!(get("a"), get("a"));
        assert_eq!((a, b), (Ok(1), Ok(1)));
        // Another key is another answer.
        assert_eq!(get("b").await, Ok(2));

        // "a" was answered a second ago: "b" took that long.
        tokio::time::advance(TTL - Duration::from_secs(2)).await;
        assert_eq!(get("a").await, Ok(1));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(get("a").await, Ok(3));
    }

    #[tokio::test(start_paused = true)]
    async fn an_error_is_not_kept() {
        let recent = Recent::new(TTL, WAIT);
        let down = recent.get("a", || async { Err::<u32, _>(Failed::Down) });
        assert_eq!(down.await, Err(Failed::Down));
        assert_eq!(
            recent.get("a", || async { Ok::<_, Failed>(7) }).await,
            Ok(7)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_gives_up_on_an_answer_that_takes_too_long() {
        let recent = Recent::new(TTL, WAIT);
        let computed = AtomicU32::new(0);
        let started = Instant::now();
        let (first, second) = tokio::join!(
            recent.get("a", || slowly(&computed, 25)),
            recent.get("a", || slowly(&computed, 25)),
        );
        assert_eq!((first, second), (Ok(1), Err(Failed::Busy)));
        // The second stopped waiting; it never computed the answer again.
        assert_eq!(started.elapsed(), Duration::from_secs(25));
        assert_eq!(computed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn answers_nobody_asks_for_again_are_forgotten() {
        let recent = Recent::new(TTL, WAIT);
        for key in 0..100 {
            let got = recent.get(key, || async { Ok::<_, Failed>(key) }).await;
            assert_eq!(got, Ok(key));
        }
        assert_eq!(recent.kept(), 100);
        tokio::time::advance(TTL).await;
        assert_eq!(recent.get(0, || async { Ok::<_, Failed>(0) }).await, Ok(0));
        assert_eq!(recent.kept(), 1);
    }
}
