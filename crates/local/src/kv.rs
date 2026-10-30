//! Reuse of the model's memory between requests. A lane is one sequence in
//! the context together with the tokens it holds. A request keeps the longest
//! common prefix, cuts the memory back at the first changed token and decodes
//! only the new tokens.
//!
//! Recurrent and hybrid models (Qwen3.5's Gated DeltaNet layers) cannot cut
//! their state back to an arbitrary position. For them the lane keeps
//! checkpoints of that state (llama.cpp's partial-only sequence state): at
//! the end of the stable segments, every [`INTERVAL`] tokens, and at the end
//! of the most recent requests. A cut restores the nearest checkpoint at or
//! before the change and decodes forward from there.

use crate::llama::{DecodeError, Seq, Token};

/// Spacing of interval checkpoints, in tokens.
pub const INTERVAL: usize = 512;
const MAX_INTERVAL: usize = 3;
const MAX_RECENT: usize = 2;

/// What a lane needs from the model's memory.
pub trait Memory {
    /// Cuts `seq` back to its first `pos` tokens. False when the memory
    /// cannot remove a partial range; the memory is then unchanged.
    fn truncate(&mut self, seq: Seq, pos: usize) -> bool;
    fn clear_seq(&mut self, seq: Seq);
    /// Saves the part of `seq`'s state that cannot be cut back.
    fn save(&mut self, seq: Seq, out: &mut Vec<u8>) -> bool;
    fn restore(&mut self, seq: Seq, data: &[u8]) -> bool;
    /// Decodes `tokens` into `seq` at positions `start..`. With `logits`, the
    /// last token's logits are copied into `logits_out`.
    fn decode(
        &mut self,
        seq: Seq,
        start: usize,
        tokens: &[Token],
        logits: bool,
        logits_out: &mut Vec<f32>,
    ) -> Result<(), DecodeError>;
    /// Number of tokens `seq` holds, after an aborted decode.
    fn len(&self, seq: Seq) -> usize;
}

pub fn common_prefix(a: &[Token], b: &[Token]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Recent,
    Interval,
    Anchor,
}

struct Checkpoint {
    pos: usize,
    kind: Kind,
    data: Vec<u8>,
}

/// Checkpoints of one lane, ordered by position.
#[derive(Default)]
pub struct Checkpoints {
    list: Vec<Checkpoint>,
    /// A buffer from an evicted checkpoint, reused for the next save.
    spare: Vec<u8>,
}

impl Checkpoints {
    /// Position of the latest checkpoint at or before `pos`.
    fn best(&self, pos: usize) -> Option<usize> {
        self.list.iter().rposition(|c| c.pos <= pos)
    }

    fn truncate(&mut self, pos: usize) {
        while self.list.last().is_some_and(|c| c.pos > pos) {
            self.spare = self.list.pop().map(|c| c.data).unwrap_or_default();
        }
    }

    fn clear(&mut self) {
        self.truncate(0);
        self.list.clear();
    }

    fn positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.list.iter().map(|c| c.pos)
    }

    fn insert(&mut self, pos: usize, kind: Kind, data: Vec<u8>) {
        let at = self.list.partition_point(|c| c.pos < pos);
        if self.list.get(at).is_some_and(|c| c.pos == pos) {
            let c = &mut self.list[at];
            c.kind = c.kind.max(kind);
            self.spare = std::mem::replace(&mut c.data, data);
        } else {
            self.list.insert(at, Checkpoint { pos, kind, data });
        }
        self.evict();
    }

    /// Keeps the newest anchor, the [`MAX_INTERVAL`] latest interval
    /// checkpoints and the [`MAX_RECENT`] latest recent ones.
    fn evict(&mut self) {
        for (kind, keep) in [
            (Kind::Anchor, 1),
            (Kind::Interval, MAX_INTERVAL),
            (Kind::Recent, MAX_RECENT),
        ] {
            while self.list.iter().filter(|c| c.kind == kind).count() > keep {
                let oldest = self
                    .list
                    .iter()
                    .position(|c| c.kind == kind)
                    .expect("counted above");
                self.spare = self.list.remove(oldest).data;
            }
        }
    }

    fn take_spare(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.spare)
    }
}

/// Positions in `(from, to]` where a checkpoint is saved while decoding
/// `from..to`, ascending.
fn plan(from: usize, to: usize, anchor: usize, out: &mut Vec<(usize, Kind)>) {
    out.clear();
    if to <= from {
        return;
    }
    if anchor > from && anchor <= to {
        out.push((anchor, Kind::Anchor));
    }
    let mut p = (from / INTERVAL + 1) * INTERVAL;
    while p <= to {
        out.push((p, Kind::Interval));
        p += INTERVAL;
    }
    out.push((to, Kind::Recent));
    out.sort_unstable();
    out.dedup_by(|b, a| {
        if a.0 == b.0 {
            a.1 = a.1.max(b.1);
            true
        } else {
            false
        }
    });
}

#[derive(Debug, PartialEq, Eq)]
pub enum Synced {
    Done,
    Cancelled,
}

/// One sequence of the context and the tokens it holds.
pub struct Lane {
    seq: Seq,
    tokens: Vec<Token>,
    /// Present when the model's memory cannot be cut back partially.
    checkpoints: Option<Checkpoints>,
    /// Logits after the last token, valid when `logits_valid`.
    pub logits: Vec<f32>,
    logits_valid: bool,
    n_batch: usize,
    plan: Vec<(usize, Kind)>,
    /// Prompt tokens decoded over the lane's life, for timings.
    decoded: usize,
}

impl Lane {
    pub fn new(seq: Seq, needs_checkpoints: bool, n_batch: usize) -> Lane {
        Lane {
            seq,
            tokens: Vec::new(),
            checkpoints: needs_checkpoints.then(Checkpoints::default),
            logits: Vec::new(),
            logits_valid: false,
            n_batch: n_batch.max(1),
            plan: Vec::new(),
            decoded: 0,
        }
    }

    pub fn decoded(&self) -> usize {
        self.decoded
    }

    /// Forgets everything; for a recreated context, whose memory is empty.
    pub fn reset(&mut self) {
        self.tokens.clear();
        self.logits_valid = false;
        if let Some(c) = &mut self.checkpoints {
            c.clear();
        }
    }

    /// Empties the lane's sequence.
    pub fn clear(&mut self, mem: &mut impl Memory) {
        mem.clear_seq(self.seq);
        self.reset();
    }

    /// Makes the lane hold `target[..end]`. `anchor` is the end of the stable
    /// segments. With `logits`, the lane ends exactly at `end` with the last
    /// token's logits in [`Self::logits`]; without, tokens past `end` that are
    /// already decoded are left in place (prefill).
    pub fn sync(
        &mut self,
        mem: &mut impl Memory,
        target: &[Token],
        end: usize,
        anchor: usize,
        logits: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Synced, DecodeError> {
        let end = end.min(target.len());
        let common = common_prefix(&self.tokens, &target[..end]);
        if logits {
            if common == end && self.tokens.len() == end && self.logits_valid {
                return Ok(Synced::Done);
            }
            // At least one token is decoded to produce logits.
            self.cut(mem, common.min(end.saturating_sub(1)));
        } else if common < end {
            self.cut(mem, common);
        } else {
            return Ok(Synced::Done);
        }
        self.decode(mem, target, end, anchor, logits, cancelled)
    }

    /// Decodes one more token at the end of the lane, leaving its logits in
    /// [`Self::logits`]. Used for tokens generated on this lane.
    pub fn push(&mut self, mem: &mut impl Memory, token: Token) -> Result<(), DecodeError> {
        self.logits_valid = false;
        mem.decode(
            self.seq,
            self.tokens.len(),
            &[token],
            true,
            &mut self.logits,
        )?;
        self.tokens.push(token);
        self.logits_valid = true;
        Ok(())
    }

    /// Cuts the lane back to at most `keep` tokens.
    fn cut(&mut self, mem: &mut impl Memory, keep: usize) {
        if self.tokens.len() <= keep {
            return;
        }
        self.logits_valid = false;
        if mem.truncate(self.seq, keep) {
            self.tokens.truncate(keep);
            if let Some(c) = &mut self.checkpoints {
                c.truncate(keep);
            }
            return;
        }
        if let Some(c) = &mut self.checkpoints
            && let Some(i) = c.best(keep)
        {
            let pos = c.list[i].pos;
            if mem.restore(self.seq, &c.list[i].data) && mem.truncate(self.seq, pos) {
                self.tokens.truncate(pos);
                c.truncate(pos);
                return;
            }
        }
        self.clear(mem);
    }

    fn decode(
        &mut self,
        mem: &mut impl Memory,
        target: &[Token],
        end: usize,
        anchor: usize,
        logits: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Synced, DecodeError> {
        let mut stops = std::mem::take(&mut self.plan);
        match &self.checkpoints {
            Some(c) => {
                plan(self.tokens.len(), end, anchor, &mut stops);
                stops.retain(|&(p, _)| !c.positions().any(|q| q == p));
            }
            None => stops.clear(),
        }
        let result = self.decode_planned(mem, target, end, logits, cancelled, &stops);
        self.plan = stops;
        result
    }

    fn decode_planned(
        &mut self,
        mem: &mut impl Memory,
        target: &[Token],
        end: usize,
        logits: bool,
        cancelled: &dyn Fn() -> bool,
        plan: &[(usize, Kind)],
    ) -> Result<Synced, DecodeError> {
        let mut stops = plan.iter().copied().peekable();
        while self.tokens.len() < end {
            if cancelled() {
                return Ok(Synced::Cancelled);
            }
            let start = self.tokens.len();
            while stops.next_if(|&(p, _)| p <= start).is_some() {}
            let stop = stops.peek().map_or(end, |&(p, _)| p);
            let chunk_end = stop.min(start + self.n_batch);
            let want = logits && chunk_end == end;
            match mem.decode(
                self.seq,
                start,
                &target[start..chunk_end],
                want,
                &mut self.logits,
            ) {
                Ok(()) => {}
                Err(DecodeError::Aborted) => {
                    let n = mem.len(self.seq).clamp(start, chunk_end);
                    self.tokens.extend_from_slice(&target[start..n]);
                    self.decoded += n - start;
                    return Ok(Synced::Cancelled);
                }
                Err(e) => return Err(e),
            }
            self.tokens.extend_from_slice(&target[start..chunk_end]);
            self.decoded += chunk_end - start;
            self.logits_valid = want;
            if let Some(&(p, kind)) = stops.peek()
                && p == chunk_end
                && let Some(c) = &mut self.checkpoints
            {
                let mut data = c.take_spare();
                if mem.save(self.seq, &mut data) {
                    c.insert(p, kind, data);
                }
            }
        }
        Ok(Synced::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A memory that records decoded tokens. `recurrent` memories refuse
    /// partial cuts, and a checkpoint stores the tokens decoded so far.
    #[derive(Default)]
    struct Fake {
        recurrent: bool,
        seq: Vec<Token>,
        decoded: usize,
        saves: usize,
        abort_after: Option<usize>,
    }

    impl Memory for Fake {
        fn truncate(&mut self, _: Seq, pos: usize) -> bool {
            if self.recurrent && pos > 0 && pos < self.seq.len() {
                return false;
            }
            self.seq.truncate(pos);
            true
        }
        fn clear_seq(&mut self, _: Seq) {
            self.seq.clear();
        }
        fn save(&mut self, _: Seq, out: &mut Vec<u8>) -> bool {
            self.saves += 1;
            out.clear();
            out.extend(self.seq.iter().flat_map(|t| t.to_le_bytes()));
            true
        }
        fn restore(&mut self, _: Seq, data: &[u8]) -> bool {
            self.seq = data
                .chunks(4)
                .map(|c| Token::from_le_bytes(c.try_into().unwrap()))
                .collect();
            true
        }
        fn decode(
            &mut self,
            _: Seq,
            start: usize,
            tokens: &[Token],
            logits: bool,
            out: &mut Vec<f32>,
        ) -> Result<(), DecodeError> {
            assert_eq!(start, self.seq.len(), "decode must continue the sequence");
            if let Some(n) = self.abort_after {
                let n = n.min(tokens.len());
                self.seq.extend_from_slice(&tokens[..n]);
                self.decoded += n;
                self.abort_after = None;
                return Err(DecodeError::Aborted);
            }
            self.seq.extend_from_slice(tokens);
            self.decoded += tokens.len();
            if logits {
                out.clear();
                out.push(*tokens.last().unwrap() as f32);
            }
            Ok(())
        }
        fn len(&self, _: Seq) -> usize {
            self.seq.len()
        }
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn appending_decodes_only_new_tokens() {
        let mut mem = Fake::default();
        let mut lane = Lane::new(0, false, 8);
        let a: Vec<Token> = (0..20).collect();
        lane.sync(&mut mem, &a, a.len(), 5, true, &never).unwrap();
        assert_eq!(mem.decoded, 20);
        assert_eq!(lane.logits, vec![19.0]);
        let b: Vec<Token> = (0..23).collect();
        lane.sync(&mut mem, &b, b.len(), 5, true, &never).unwrap();
        assert_eq!(mem.decoded, 23);
        assert_eq!(mem.seq, b);
    }

    #[test]
    fn unchanged_prompt_reuses_logits() {
        let mut mem = Fake::default();
        let mut lane = Lane::new(0, false, 8);
        let a: Vec<Token> = (0..10).collect();
        lane.sync(&mut mem, &a, a.len(), 0, true, &never).unwrap();
        lane.sync(&mut mem, &a, a.len(), 0, true, &never).unwrap();
        assert_eq!(mem.decoded, 10);
    }

    #[test]
    fn logits_after_prefill_redecode_one_token() {
        let mut mem = Fake::default();
        let mut lane = Lane::new(0, false, 8);
        let a: Vec<Token> = (0..10).collect();
        lane.sync(&mut mem, &a, a.len(), 0, false, &never).unwrap();
        lane.sync(&mut mem, &a, a.len(), 0, true, &never).unwrap();
        assert_eq!(mem.decoded, 11);
        assert_eq!(lane.logits, vec![9.0]);
    }

    #[test]
    fn prefill_keeps_tokens_decoded_past_its_end() {
        let mut mem = Fake::default();
        let mut lane = Lane::new(0, false, 8);
        let a: Vec<Token> = (0..10).collect();
        lane.sync(&mut mem, &a, 10, 0, true, &never).unwrap();
        lane.sync(&mut mem, &a, 8, 0, false, &never).unwrap();
        assert_eq!(mem.seq.len(), 10);
    }

    #[test]
    fn attention_memory_cuts_at_the_change() {
        let mut mem = Fake::default();
        let mut lane = Lane::new(0, false, 8);
        let a: Vec<Token> = (0..30).collect();
        lane.sync(&mut mem, &a, a.len(), 0, true, &never).unwrap();
        let mut b = a.clone();
        b[25] = 99;
        lane.sync(&mut mem, &b, b.len(), 0, true, &never).unwrap();
        assert_eq!(mem.decoded, 35);
        assert_eq!(mem.seq, b);
        assert_eq!(mem.saves, 0, "attention memories need no checkpoints");
    }

    #[test]
    fn recurrent_memory_restores_the_nearest_checkpoint() {
        let mut mem = Fake {
            recurrent: true,
            ..Fake::default()
        };
        let mut lane = Lane::new(0, true, 64);
        // Stable segments end at 100; a request ends at 120.
        let a: Vec<Token> = (0..120).collect();
        lane.sync(&mut mem, &a, a.len(), 100, true, &never).unwrap();
        // The user appends a word: the end of the last request is a checkpoint.
        let b: Vec<Token> = (0..124).collect();
        lane.sync(&mut mem, &b, b.len(), 100, true, &never).unwrap();
        let before = mem.decoded;
        // Backspace across the word boundary: change at 122.
        let mut c = b.clone();
        c.truncate(122);
        c.push(500);
        lane.sync(&mut mem, &c, c.len(), 100, true, &never).unwrap();
        assert_eq!(mem.seq, c);
        assert_eq!(mem.decoded - before, 3, "restored the checkpoint at 120");
        // An edit inside the text: falls back to the anchor at 100.
        let mut d = c.clone();
        d[105] = 7;
        let before = mem.decoded;
        lane.sync(&mut mem, &d, d.len(), 100, true, &never).unwrap();
        assert_eq!(mem.seq, d);
        assert_eq!(mem.decoded - before, d.len() - 100);
    }

    #[test]
    fn recurrent_memory_without_checkpoint_starts_over() {
        let mut mem = Fake {
            recurrent: true,
            ..Fake::default()
        };
        let mut lane = Lane::new(0, true, 64);
        let a: Vec<Token> = (0..50).collect();
        lane.sync(&mut mem, &a, a.len(), 40, true, &never).unwrap();
        let mut b = a.clone();
        b[10] = 77;
        let before = mem.decoded;
        lane.sync(&mut mem, &b, b.len(), 40, true, &never).unwrap();
        assert_eq!(mem.seq, b);
        assert_eq!(mem.decoded - before, 50);
    }

    #[test]
    fn cancel_stops_between_batches() {
        let mut mem = Fake::default();
        let mut lane = Lane::new(0, false, 4);
        let a: Vec<Token> = (0..20).collect();
        let calls = std::cell::Cell::new(0);
        let cancel_after_two = || {
            calls.set(calls.get() + 1);
            calls.get() > 2
        };
        let r = lane
            .sync(&mut mem, &a, a.len(), 0, false, &cancel_after_two)
            .unwrap();
        assert_eq!(r, Synced::Cancelled);
        assert_eq!(lane.tokens, &a[..8]);
        assert_eq!(mem.seq, &a[..8]);
    }

    #[test]
    fn aborted_decode_keeps_what_was_processed() {
        let mut mem = Fake {
            abort_after: Some(3),
            ..Fake::default()
        };
        let mut lane = Lane::new(0, false, 8);
        let a: Vec<Token> = (0..20).collect();
        let r = lane.sync(&mut mem, &a, a.len(), 0, false, &never).unwrap();
        assert_eq!(r, Synced::Cancelled);
        assert_eq!(lane.tokens, &a[..3]);
        lane.sync(&mut mem, &a, a.len(), 0, true, &never).unwrap();
        assert_eq!(mem.seq, a);
        assert_eq!(mem.decoded, 20);
    }

    #[test]
    fn plan_places_anchor_intervals_and_end() {
        let mut out = Vec::new();
        plan(0, 1200, 300, &mut out);
        assert_eq!(
            out,
            vec![
                (300, Kind::Anchor),
                (512, Kind::Interval),
                (1024, Kind::Interval),
                (1200, Kind::Recent)
            ]
        );
        plan(512, 512, 0, &mut out);
        assert!(out.is_empty());
        plan(100, 512, 512, &mut out);
        assert_eq!(out, vec![(512, Kind::Anchor)]);
    }

    #[test]
    fn eviction_keeps_anchor_and_latest() {
        let mut c = Checkpoints::default();
        c.insert(100, Kind::Anchor, vec![]);
        for p in [512, 1024, 1536, 2048] {
            c.insert(p, Kind::Interval, vec![]);
        }
        for p in [2050, 2060, 2070] {
            c.insert(p, Kind::Recent, vec![]);
        }
        let positions: Vec<usize> = c.positions().collect();
        assert_eq!(positions, vec![100, 1024, 1536, 2048, 2060, 2070]);
        assert_eq!(c.best(1500).map(|i| c.list[i].pos), Some(1024));
        assert_eq!(c.best(99), None);
    }

    proptest! {
        /// Whatever the edit history, a synced lane holds exactly the target
        /// and the memory agrees with it.
        #[test]
        fn sync_always_reaches_the_target(
            recurrent in any::<bool>(),
            prompts in proptest::collection::vec(proptest::collection::vec(0..4i32, 0..40), 1..12),
        ) {
            let mut mem = Fake { recurrent, ..Fake::default() };
            let mut lane = Lane::new(0, recurrent, 7);
            for p in &prompts {
                let mut target = vec![9];
                target.extend_from_slice(p);
                lane.sync(&mut mem, &target, target.len(), 1, true, &never).unwrap();
                prop_assert_eq!(&lane.tokens, &target);
                prop_assert_eq!(&mem.seq, &target);
                prop_assert_eq!(lane.logits.clone(), vec![*target.last().unwrap() as f32]);
            }
        }
    }
}
