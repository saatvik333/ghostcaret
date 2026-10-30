//! Latency of the key path, recorded from the key callback and readable from
//! any thread. Recording is one relaxed atomic add: no lock, no allocation,
//! no system call.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const STEP_NS: u64 = 250;
/// 250 ns buckets up to 50 µs, then one bucket for everything slower.
const BUCKETS: usize = 201;

#[derive(Debug)]
pub struct Histogram {
    buckets: [AtomicU32; BUCKETS],
    max_ns: AtomicU64,
}

/// Percentiles are bucket upper bounds, so they err on the slow side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub count: u64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram::new()
    }
}

impl Histogram {
    pub const fn new() -> Histogram {
        Histogram {
            buckets: [const { AtomicU32::new(0) }; BUCKETS],
            max_ns: AtomicU64::new(0),
        }
    }

    pub fn record(&self, ns: u64) {
        let i = ((ns / STEP_NS) as usize).min(BUCKETS - 1);
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    pub fn summary(&self) -> Summary {
        let counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| u64::from(b.load(Ordering::Relaxed)))
            .collect();
        let count: u64 = counts.iter().sum();
        let max_ns = self.max_ns.load(Ordering::Relaxed);
        let percentile = |p: u64| {
            let rank = (count * p).div_ceil(100).max(1);
            let mut seen = 0;
            for (i, &c) in counts.iter().enumerate() {
                seen += c;
                if seen >= rank {
                    return if i == BUCKETS - 1 {
                        max_ns
                    } else {
                        (i as u64 + 1) * STEP_NS
                    };
                }
            }
            0
        };
        if count == 0 {
            return Summary::default();
        }
        Summary {
            count,
            p50_ns: percentile(50),
            p99_ns: percentile(99),
            max_ns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_bucket_upper_bounds() {
        let h = Histogram::new();
        assert_eq!(h.summary(), Summary::default());
        for _ in 0..98 {
            h.record(100);
        }
        h.record(3_000);
        h.record(80_000);
        let s = h.summary();
        assert_eq!(
            (s.count, s.p50_ns, s.p99_ns, s.max_ns),
            (100, 250, 3_250, 80_000)
        );
        h.record(90_000);
        h.record(90_000);
        assert_eq!(
            h.summary().p99_ns,
            90_000,
            "slow outliers report the true maximum"
        );
    }
}
