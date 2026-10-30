//! Token choice from logits: greedy picks with probabilities (softmax via
//! max and log-sum-exp), top-k for alternatives, and temperature sampling for
//! chat generation. Full-vocabulary loops are written so they vectorize.

use crate::llama::Token;

/// `e^x` for `x <= 0`, relative error below 1e-6; vectorizes.
#[inline]
fn exp_neg(x: f32) -> f32 {
    // e^x = 2^i · 2^f with i = round(x·log2 e) and f in [-0.5, 0.5]; 2^f
    // from its Taylor series. Adding 1.5·2^23 rounds to an integer held in
    // the low mantissa bits, which avoids a float-to-int conversion.
    const SHIFT: f32 = 12_582_912.0;
    let t = x.max(-87.0) * std::f32::consts::LOG2_E;
    let r = t + SHIFT;
    let i = r.to_bits() as i32 - SHIFT.to_bits() as i32;
    let f = t - (r - SHIFT);
    let p = 1.0
        + f * (std::f32::consts::LN_2
            + f * (0.240_226_5
                + f * (0.055_504_11
                    + f * (0.009_618_129 + f * (0.001_333_356 + f * 0.000_154_035_3)))));
    f32::from_bits(((i + 127) << 23) as u32) * p
}

fn max(logits: &[f32]) -> f32 {
    let mut lanes = [f32::NEG_INFINITY; 8];
    let (chunks, rest) = logits.as_chunks::<8>();
    for c in chunks {
        for (m, &x) in lanes.iter_mut().zip(c) {
            *m = if x > *m { x } else { *m };
        }
    }
    rest.iter()
        .chain(&lanes)
        .fold(f32::NEG_INFINITY, |m, &x| if x > m { x } else { m })
}

/// `ln Σ e^l` over all logits, given their maximum.
fn log_sum_exp(logits: &[f32], max: f32) -> f32 {
    let mut lanes = [0.0f32; 8];
    let (chunks, rest) = logits.as_chunks::<8>();
    for c in chunks {
        for (s, &x) in lanes.iter_mut().zip(c) {
            *s += exp_neg(x - max);
        }
    }
    let sum: f32 = lanes.iter().sum::<f32>() + rest.iter().map(|&x| exp_neg(x - max)).sum::<f32>();
    max + sum.ln()
}

/// The vocabulary a pick ranges over: all tokens, or a constrained subset.
#[derive(Clone, Copy)]
pub enum Over<'a> {
    All,
    Only(&'a [Token]),
}

/// What probabilities from a constrained pick are relative to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Norm {
    /// The tokens picked from: the probability given the constraint.
    Within,
    /// The whole vocabulary: comparable across differently constrained
    /// picks.
    Vocabulary,
}

/// The `k` best tokens by logit, best first, with their probabilities,
/// skipping tokens `skip` rejects. Appends to `out`.
pub fn top(
    logits: &[f32],
    over: Over<'_>,
    norm: Norm,
    k: usize,
    skip: impl Fn(Token) -> bool,
    out: &mut Vec<(Token, f32)>,
) {
    let start = out.len();
    let mut best = Best {
        start,
        k,
        worst: f32::NEG_INFINITY,
    };
    let lse = match over {
        Over::All => {
            let m = max(logits);
            // Usually the best token is simply the first at the maximum.
            match logits.iter().position(|&l| l == m) {
                Some(t) if k == 1 && !skip(t as Token) => out.push((t as Token, m)),
                _ => {
                    for (t, &l) in logits.iter().enumerate() {
                        if l > best.worst && !skip(t as Token) {
                            best.insert(out, t as Token, l);
                        }
                    }
                }
            }
            log_sum_exp(logits, m)
        }
        Over::Only(tokens) => {
            let mut m = f32::NEG_INFINITY;
            for &t in tokens {
                let l = logits[t as usize];
                m = m.max(l);
                if l > best.worst && !skip(t) {
                    best.insert(out, t, l);
                }
            }
            match norm {
                Norm::Within => {
                    m + tokens
                        .iter()
                        .map(|&t| exp_neg(logits[t as usize] - m))
                        .sum::<f32>()
                        .ln()
                }
                Norm::Vocabulary => log_sum_exp(logits, max(logits)),
            }
        }
    };
    for (_, l) in &mut out[start..] {
        *l = exp_neg(*l - lse);
    }
}

/// The `k` best `(token, logit)` pairs held in `out[start..]`, best first.
struct Best {
    start: usize,
    k: usize,
    /// The k-th best logit once `k` are held; candidates must beat it.
    worst: f32,
}

impl Best {
    fn insert(&mut self, out: &mut Vec<(Token, f32)>, t: Token, l: f32) {
        let at = self.start + out[self.start..].partition_point(|&(_, x)| x >= l);
        if out.len() - self.start == self.k {
            out.pop();
        }
        out.insert(at, (t, l));
        if out.len() - self.start == self.k {
            self.worst = out[out.len() - 1].1;
        }
    }
}

/// A small, fast generator for sampling (splitmix64).
#[derive(Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }
}

const TOP_K: usize = 40;
const TOP_P: f32 = 0.95;

/// Samples with temperature from the top-k tokens within top-p; greedy at
/// temperature 0. `scratch` is reused between calls.
pub fn sample(
    logits: &[f32],
    temperature: f32,
    skip: impl Fn(Token) -> bool,
    rng: &mut Rng,
    scratch: &mut Vec<(Token, f32)>,
) -> Option<Token> {
    scratch.clear();
    if temperature <= 0.0 {
        top(logits, Over::All, Norm::Vocabulary, 1, skip, scratch);
        return scratch.first().map(|&(t, _)| t);
    }
    top(logits, Over::All, Norm::Vocabulary, TOP_K, skip, scratch);
    // Re-weight the kept tokens at the temperature: p^(1/T), normalized.
    let inv_t = 1.0 / temperature;
    let mut total = 0.0;
    let mut cumulative = 0.0;
    let mut keep = scratch.len();
    for (i, (_, p)) in scratch.iter().enumerate() {
        cumulative += p;
        if cumulative >= TOP_P {
            keep = i + 1;
            break;
        }
    }
    scratch.truncate(keep);
    for (_, p) in scratch.iter_mut() {
        *p = p.powf(inv_t);
        total += *p;
    }
    let mut r = rng.next_f32() * total;
    for &(t, p) in scratch.iter() {
        if r < p {
            return Some(t);
        }
        r -= p;
    }
    scratch.last().map(|&(t, _)| t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::{prop_assert, prop_assert_eq, proptest};

    #[test]
    fn exp_is_accurate() {
        for i in 0..=8700 {
            let x = -(i as f32) / 100.0;
            let (got, want) = (exp_neg(x), x.exp());
            assert!(
                (got - want).abs() <= want * 2e-5 + 1e-37,
                "e^{x}: {got} vs {want}"
            );
        }
    }

    proptest! {
        #[test]
        fn top_matches_a_full_sort(logits in proptest::collection::vec(-30.0f32..30.0, 1..300), k in 1usize..5) {
            let mut out = Vec::new();
            top(&logits, Over::All, Norm::Vocabulary, k, |_| false, &mut out);
            let mut sorted: Vec<(usize, f32)> = logits.iter().copied().enumerate().collect();
            sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
            let lse = logits.iter().map(|&l| (l as f64).exp()).sum::<f64>().ln();
            prop_assert_eq!(out.len(), k.min(logits.len()));
            for (&(t, p), &(_, l)) in out.iter().zip(&sorted) {
                prop_assert_eq!(logits[t as usize], l);
                let want = ((l as f64) - lse).exp();
                prop_assert!((p as f64 - want).abs() <= want * 1e-4 + 1e-9, "{} vs {}", p, want);
            }
        }

        #[test]
        fn constrained_probabilities_renormalize(logits in proptest::collection::vec(-10.0f32..10.0, 4..50)) {
            let only: Vec<Token> = (0..logits.len() as Token).step_by(2).collect();
            let mut out = Vec::new();
            top(&logits, Over::Only(&only), Norm::Within, only.len(), |_| false, &mut out);
            let total: f32 = out.iter().map(|&(_, p)| p).sum();
            prop_assert!((total - 1.0).abs() < 1e-3);
            prop_assert!(out.iter().all(|&(t, _)| t % 2 == 0));
            out.clear();
            top(&logits, Over::Only(&only), Norm::Vocabulary, only.len(), |_| false, &mut out);
            let lse = logits.iter().map(|&l| (l as f64).exp()).sum::<f64>().ln();
            for &(t, p) in &out {
                let want = (logits[t as usize] as f64 - lse).exp();
                prop_assert!((p as f64 - want).abs() <= want * 1e-4 + 1e-9);
            }
        }
    }

    #[test]
    fn skipped_tokens_are_never_picked() {
        let logits = [5.0, 1.0, 4.0];
        let mut out = Vec::new();
        top(
            &logits,
            Over::All,
            Norm::Vocabulary,
            1,
            |t| t == 0,
            &mut out,
        );
        assert_eq!(out[0].0, 2);
        let mut rng = Rng::new(1);
        for _ in 0..100 {
            assert_ne!(
                sample(&logits, 1.0, |t| t == 0, &mut rng, &mut out),
                Some(0)
            );
        }
    }

    #[test]
    fn greedy_at_zero_temperature() {
        let logits = [0.0, 3.0, 1.0];
        let mut out = Vec::new();
        assert_eq!(
            sample(&logits, 0.0, |_| false, &mut Rng::new(7), &mut out),
            Some(1)
        );
    }
}
