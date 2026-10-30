use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// One request exactly as it was sent to a provider.
///
/// Only the body is recorded. API keys travel in headers, which never reach
/// this log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SentEntry {
    /// Unix time in milliseconds.
    pub at_ms: u64,
    /// `ProviderConfig::label`.
    pub provider: String,
    pub model: String,
    /// The request body, byte for byte.
    pub body: String,
    /// The body was longer than the whole log may hold and was cut.
    pub truncated: bool,
}

/// What the "What was sent" view shows: a bounded in-memory ring of recent
/// requests. Cloning gives another handle to the same log.
#[derive(Clone, Debug)]
pub struct SentLog(Arc<Mutex<Ring>>);

#[derive(Debug)]
struct Ring {
    entries: VecDeque<SentEntry>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl SentLog {
    /// A log holding at most `max_entries` requests and `max_bytes` of body
    /// text. The oldest entries are dropped first.
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        SentLog(Arc::new(Mutex::new(Ring {
            entries: VecDeque::new(),
            bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes,
        })))
    }

    /// Oldest first.
    pub fn entries(&self) -> Vec<SentEntry> {
        self.lock().entries.iter().cloned().collect()
    }

    pub fn clear(&self) {
        let mut ring = self.lock();
        ring.entries.clear();
        ring.bytes = 0;
    }

    pub(crate) fn record(&self, provider: &str, model: &str, body: &str) {
        let mut ring = self.lock();
        let (body, truncated) = fit(body, ring.max_bytes);
        ring.bytes += body.len();
        ring.entries.push_back(SentEntry {
            at_ms: unix_ms(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            body: body.to_owned(),
            truncated,
        });
        while ring.entries.len() > ring.max_entries || ring.bytes > ring.max_bytes {
            let Some(old) = ring.entries.pop_front() else {
                break;
            };
            ring.bytes -= old.body.len();
        }
    }

    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for SentLog {
    fn default() -> Self {
        SentLog::new(256, 8 << 20)
    }
}

/// `body` cut to at most `max` bytes on a char boundary.
fn fit(body: &str, max: usize) -> (&str, bool) {
    if body.len() <= max {
        return (body, false);
    }
    let mut end = max;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (&body[..end], true)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_newest_entries_within_count() {
        let log = SentLog::new(3, 1 << 20);
        for i in 0..5 {
            log.record("p", "m", &format!("body {i}"));
        }
        let bodies: Vec<_> = log.entries().into_iter().map(|e| e.body).collect();
        assert_eq!(bodies, ["body 2", "body 3", "body 4"]);
    }

    #[test]
    fn evicts_oldest_to_stay_within_bytes() {
        let log = SentLog::new(100, 10);
        log.record("p", "m", "aaaa");
        log.record("p", "m", "bbbb");
        log.record("p", "m", "cccc");
        let bodies: Vec<_> = log.entries().into_iter().map(|e| e.body).collect();
        assert_eq!(bodies, ["bbbb", "cccc"]);
    }

    #[test]
    fn oversized_body_is_cut_on_a_char_boundary_and_flagged() {
        let log = SentLog::new(10, 5);
        log.record("p", "m", "ééé");
        let entry = &log.entries()[0];
        assert_eq!(entry.body, "éé");
        assert!(entry.truncated);
    }

    #[test]
    fn clones_share_one_log_and_clear_resets_the_byte_count() {
        let log = SentLog::new(10, 6);
        let other = log.clone();
        other.record("p", "m", "abcdef");
        assert_eq!(log.entries().len(), 1);
        log.clear();
        assert!(other.entries().is_empty());
        other.record("p", "m", "abcdef");
        assert_eq!(
            log.entries().len(),
            1,
            "byte count must restart after clear"
        );
    }
}
