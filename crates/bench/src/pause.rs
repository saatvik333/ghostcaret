//! Where the simulated typist pauses long enough to see a suggestion.
//!
//! The product asks for a suggestion once `pause_ms` pass without a keystroke
//! and shows it when the model answers. A suggestion helps only when it is on
//! screen before the next keystroke, so a caret position is a **pause point**
//! when the typist's next inter-key interval (IKI) lasts at least `pause_ms`
//! plus the time-to-suggestion target (docs/quality.md, "Responsiveness":
//! 100 ms at the median). With the default 30 ms pause that is 130 ms.
//!
//! - **Word boundaries** (the caret right after whitespace, including the
//!   start of a new line) are always pause points: typists pause longer
//!   between words than inside them, and this is where a suggestion for the
//!   next word is read.
//! - **Right after Take Word or Take All** is always a pause point: the
//!   typist has just read the suggestion to accept it.
//! - **Every other position** draws an IKI from a log-normal distribution
//!   whose mean is 238.66 ms, the mean IKI of 168,000 typists in Dhakal et
//!   al., "Observations on Typing from 136 Million Keystrokes" (CHI 2018).
//!   The spread of ln(IKI), σ = 0.5, is an assumption of this model, not a
//!   published figure. With these values about 83 % of positions inside a
//!   word are pause points.
//!
//! The draw for a position comes from a hash of (seed, sample, byte offset in
//! the sample's text), so every configuration sees the same pauses at the
//! same places (common random numbers) and every run is identical.

use serde::{Deserialize, Serialize};

/// Mean inter-key interval from Dhakal et al. (CHI 2018).
pub const DHAKAL_MEAN_IKI_MS: f64 = 238.66;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PauseModel {
    pub seed: u64,
    /// Mean inter-key interval inside words, in ms.
    pub mean_iki_ms: f64,
    /// Standard deviation of ln(IKI).
    pub sigma: f64,
    /// An interval at least this long shows a suggestion: the product's pause
    /// plus the latency target.
    pub visible_ms: f64,
}

impl PauseModel {
    /// The model with the published mean, σ = 0.5, and a suggestion visible
    /// after `pause_ms` plus the 100 ms latency target.
    pub fn new(seed: u64, pause_ms: u32) -> PauseModel {
        PauseModel {
            seed,
            mean_iki_ms: DHAKAL_MEAN_IKI_MS,
            sigma: 0.5,
            visible_ms: f64::from(pause_ms) + 100.0,
        }
    }

    /// Median of the IKI distribution.
    pub fn median_ms(&self) -> f64 {
        self.mean_iki_ms / (self.sigma * self.sigma / 2.0).exp()
    }

    /// The interval after the keystroke that put the caret at byte `pos` of
    /// sample `sample`'s text.
    pub fn iki_ms(&self, sample: u64, pos: usize) -> f64 {
        let z = normal(self.seed, sample, pos as u64);
        self.median_ms() * (self.sigma * z).exp()
    }

    /// Whether the caret at byte `pos`, with `before` in the field before it,
    /// is a pause point (see the module documentation).
    pub fn pauses(&self, sample: u64, pos: usize, before: &str) -> bool {
        before.ends_with(char::is_whitespace) || self.iki_ms(sample, pos) >= self.visible_ms
    }

    /// Probability that a position inside a word is a pause point.
    pub fn within_word_rate(&self) -> f64 {
        let z = (self.visible_ms / self.median_ms()).ln() / self.sigma;
        1.0 - normal_cdf(z)
    }
}

/// A stable 64-bit key for a sample id (FNV-1a), so draws do not depend on
/// the order samples are loaded in.
pub fn sample_key(id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// SplitMix64 finaliser.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A uniform draw in (0, 1) from 53 hash bits.
fn uniform(h: u64) -> f64 {
    ((h >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

/// A standard normal draw for one (seed, sample, position), by Box–Muller.
fn normal(seed: u64, sample: u64, pos: u64) -> f64 {
    let a = mix(seed ^ mix(sample ^ mix(pos)));
    let b = mix(a ^ 0x9e37_79b9_7f4a_7c15);
    let (u1, u2) = (uniform(a), uniform(b));
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// Standard normal CDF through erf (Abramowitz and Stegun 7.1.26, absolute
/// error below 1.5e-7).
fn normal_cdf(z: f64) -> f64 {
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-x * x).exp();
    if z >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> PauseModel {
        PauseModel::new(7, 30)
    }

    #[test]
    fn median_follows_from_the_published_mean() {
        // mean = median · exp(σ²/2), so 238.66 / exp(0.125).
        assert!((model().median_ms() - 210.62).abs() < 0.01);
    }

    #[test]
    fn visibility_is_pause_plus_latency_target() {
        assert_eq!(model().visible_ms, 130.0);
    }

    #[test]
    fn within_word_rate_matches_hand_computation() {
        // z = ln(130 / 210.62) / 0.5 = −0.965; 1 − Φ(−0.965) = 0.833.
        assert!((model().within_word_rate() - 0.8327).abs() < 0.001);
    }

    #[test]
    fn normal_cdf_known_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-4);
        assert!((normal_cdf(-1.0) - 0.158_655).abs() < 1e-5);
    }

    #[test]
    fn word_boundaries_always_pause() {
        let m = PauseModel {
            // No interval is ever long enough inside a word.
            visible_ms: f64::INFINITY,
            ..model()
        };
        assert!(m.pauses(1, 6, "Hello "));
        assert!(m.pauses(1, 6, "Hello\n"));
        assert!(!m.pauses(1, 3, "Hel"));
    }

    #[test]
    fn draws_are_deterministic_and_position_dependent() {
        let m = model();
        assert_eq!(m.iki_ms(5, 10), m.iki_ms(5, 10));
        assert_ne!(m.iki_ms(5, 10), m.iki_ms(5, 11));
        assert_ne!(m.iki_ms(5, 10), m.iki_ms(6, 10));
        assert_ne!(m.iki_ms(5, 10), PauseModel { seed: 8, ..m }.iki_ms(5, 10));
    }

    #[test]
    fn empirical_rate_and_mean_match_the_model() {
        let m = model();
        let n = 200_000;
        let draws: Vec<f64> = (0..n).map(|i| m.iki_ms(3, i)).collect();
        let rate = draws.iter().filter(|&&d| d >= m.visible_ms).count() as f64 / n as f64;
        let mean = draws.iter().sum::<f64>() / n as f64;
        assert!((rate - m.within_word_rate()).abs() < 0.005, "{rate}");
        assert!((mean - DHAKAL_MEAN_IKI_MS).abs() < 2.0, "{mean}");
    }

    #[test]
    fn sample_key_is_fnv1a() {
        // FNV-1a of the empty string is the offset basis; of "a" a known value.
        assert_eq!(sample_key(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(sample_key("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
