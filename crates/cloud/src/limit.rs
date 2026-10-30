use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Sliding-window limit on requests per minute, applied by the client before
/// anything is sent.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    per_minute: AtomicU32,
    sent: Mutex<VecDeque<Instant>>,
}

const WINDOW: Duration = Duration::from_secs(60);

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        RateLimiter {
            per_minute: AtomicU32::new(per_minute.max(1)),
            sent: Mutex::new(VecDeque::new()),
        }
    }

    pub fn set_per_minute(&self, per_minute: u32) {
        self.per_minute.store(per_minute.max(1), Ordering::Relaxed);
    }

    pub fn per_minute(&self) -> u32 {
        self.per_minute.load(Ordering::Relaxed)
    }

    /// Takes a slot now, or says how long until one frees up.
    pub fn acquire(&self, now: Instant) -> Result<(), Duration> {
        let limit = self.per_minute() as usize;
        let mut sent = self.sent.lock().unwrap_or_else(|p| p.into_inner());
        while sent
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= WINDOW)
        {
            sent.pop_front();
        }
        if sent.len() >= limit {
            // The slot that frees first is the one `limit` requests back.
            let oldest = sent[sent.len() - limit];
            return Err((oldest + WINDOW).saturating_duration_since(now));
        }
        sent.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_past_the_limit_and_reports_the_wait() {
        let limiter = RateLimiter::new(2);
        let t0 = Instant::now();
        assert_eq!(limiter.acquire(t0), Ok(()));
        assert_eq!(limiter.acquire(t0 + Duration::from_secs(10)), Ok(()));
        assert_eq!(
            limiter.acquire(t0 + Duration::from_secs(20)),
            Err(Duration::from_secs(40))
        );
    }

    #[test]
    fn slots_free_as_the_window_slides() {
        let limiter = RateLimiter::new(1);
        let t0 = Instant::now();
        limiter.acquire(t0).unwrap();
        assert!(limiter.acquire(t0 + Duration::from_secs(59)).is_err());
        assert_eq!(limiter.acquire(t0 + Duration::from_secs(60)), Ok(()));
    }

    #[test]
    fn lowering_the_limit_applies_to_requests_already_counted() {
        let limiter = RateLimiter::new(5);
        let t0 = Instant::now();
        for _ in 0..3 {
            limiter.acquire(t0).unwrap();
        }
        limiter.set_per_minute(2);
        assert_eq!(limiter.acquire(t0), Err(Duration::from_secs(60)));
    }
}
