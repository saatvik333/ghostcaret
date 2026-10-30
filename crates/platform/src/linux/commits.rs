//! What became of the text we commit.
//!
//! An insert counts as done once the field's surrounding text shows it before
//! the caret. Most clients send that text in reply to the commit. GTK 3 apps
//! whose text lives elsewhere, Firefox and Thunderbird above all, reply with
//! the text from before the commit. Their new text is ready a few
//! milliseconds later, but they hold it back until something else makes them
//! commit their state, usually the next key. So when a reply is unchanged, we
//! ask again with an empty commit, backing off, until a reply shows new text.
//!
//! An empty commit changes nothing in any toolkit as long as the field's text
//! has not changed, which is what an unchanged reply shows: GTK and Qt
//! insert nothing, and Firefox and Chromium ignore it. After a key press we
//! stop asking. The key gets the app to send its new text anyway, and a dead
//! key may have begun a composition that an empty commit would cancel.

use std::time::{Duration, Instant};

/// How long an insert waits for the field to show it before its outcome is
/// reported as unknown.
pub(crate) const INSERT_CONFIRM: Duration = Duration::from_millis(400);
/// Wait after the first unchanged reply before asking again; doubles each
/// time.
const ASK_FIRST: Duration = Duration::from_millis(8);
/// Times to ask again after one commit: 8 + 16 + 32 + 64 + 128 ms, which
/// ends within [`INSERT_CONFIRM`].
const ASKS: u8 = 5;

struct PendingInsert {
    id: u64,
    text: String,
    deadline: Instant,
}

/// Asking again after our last commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Asking {
    /// When to send the empty commit; `None` until an unchanged reply.
    at: Option<Instant>,
    delay: Duration,
    left: u8,
}

#[derive(Default)]
pub(crate) struct Commits {
    inserts: Vec<PendingInsert>,
    /// From our commit until a reply with new text, a key press, or the
    /// asks run out.
    asking: Option<Asking>,
}

impl Commits {
    /// We committed text to the field.
    pub fn committed(&mut self) {
        self.asking = Some(Asking { at: None, delay: ASK_FIRST, left: ASKS });
    }

    /// Waits for the field to show `text`, an insert we just committed.
    pub fn track(&mut self, id: u64, text: String, now: Instant) {
        self.inserts.push(PendingInsert { id, text, deadline: now + INSERT_CONFIRM });
    }

    pub fn has_inserts(&self) -> bool {
        !self.inserts.is_empty()
    }

    /// A key was pressed.
    pub fn key_pressed(&mut self) {
        self.asking = None;
    }

    /// The client sent its surrounding text unchanged.
    pub fn unchanged(&mut self, now: Instant) {
        if let Some(a) = &mut self.asking
            && a.at.is_none()
        {
            if a.left == 0 {
                self.asking = None;
            } else {
                a.at = Some(now + a.delay);
            }
        }
    }

    /// The client sent new surrounding text; `before` is all of it before the
    /// selection. Reports each insert it shows.
    pub fn changed(&mut self, before: &str, mut inserted: impl FnMut(u64)) {
        self.asking = None;
        self.inserts.retain(|i| {
            // Only the tail of a long insert fits in the surrounding text.
            let start = i.text.len().saturating_sub(before.len());
            let start = (start..=i.text.len()).find(|&s| i.text.is_char_boundary(s)).unwrap_or(i.text.len());
            let landed = before.ends_with(&i.text[start..]);
            if landed {
                inserted(i.id);
            }
            !landed
        });
    }

    /// Runs what is due at `now`: reports each insert that timed out and
    /// returns whether to send an empty commit.
    pub fn due(&mut self, now: Instant, mut timed_out: impl FnMut(u64)) -> bool {
        self.inserts.retain(|i| {
            let keep = i.deadline > now;
            if !keep {
                timed_out(i.id);
            }
            keep
        });
        match &mut self.asking {
            Some(a) if a.at.is_some_and(|at| at <= now) => {
                a.at = None;
                a.left -= 1;
                a.delay *= 2;
                true
            }
            _ => false,
        }
    }

    /// The earliest time [`Commits::due`] has work.
    pub fn next_deadline(&self) -> Option<Instant> {
        let ask = self.asking.and_then(|a| a.at);
        self.inserts.iter().map(|i| i.deadline).chain(ask).min()
    }

    /// The field lost focus: reports every pending insert as unconfirmed.
    pub fn clear(&mut self, mut unknown: impl FnMut(u64)) {
        self.asking = None;
        for i in self.inserts.drain(..) {
            unknown(i.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    /// Regression: Firefox answers an insert with its old text and sends the
    /// new text only on the next key, so the card insert came back Unknown
    /// although the text had landed.
    #[test]
    fn an_unchanged_reply_is_asked_again_and_the_new_text_confirms() {
        let t = Instant::now();
        let mut c = Commits::default();
        c.track(1, " world".into(), t);
        c.committed();
        c.unchanged(ms(t, 1));
        assert_eq!(c.next_deadline(), Some(ms(t, 9)));
        assert!(!c.due(ms(t, 8), |_| panic!("nothing timed out")));
        assert!(c.due(ms(t, 9), |_| panic!("nothing timed out")));
        let mut confirmed = Vec::new();
        c.changed("hello world", |id| confirmed.push(id));
        assert_eq!(confirmed, [1]);
        assert_eq!(c.next_deadline(), None);
    }

    #[test]
    fn asking_backs_off_and_stops() {
        let t = Instant::now();
        let mut c = Commits::default();
        c.committed();
        let mut now = t;
        let mut waits = Vec::new();
        loop {
            c.unchanged(now);
            let Some(at) = c.next_deadline() else { break };
            waits.push(at.duration_since(now).as_millis());
            now = at;
            assert!(c.due(now, |_| {}));
        }
        assert_eq!(waits, [8, 16, 32, 64, 128]);
        assert!(waits.iter().sum::<u128>() < INSERT_CONFIRM.as_millis());
    }

    #[test]
    fn nothing_is_asked_without_a_commit_or_after_a_key() {
        let t = Instant::now();
        let mut c = Commits::default();
        c.unchanged(t);
        assert_eq!(c.next_deadline(), None);
        c.committed();
        c.key_pressed();
        c.unchanged(t);
        assert_eq!(c.next_deadline(), None);
        assert!(!c.due(ms(t, 100), |_| {}));
    }

    #[test]
    fn new_text_stops_asking_even_without_the_insert() {
        let t = Instant::now();
        let mut c = Commits::default();
        c.track(1, "draft".into(), t);
        c.committed();
        c.changed("something else", |_| panic!("not landed"));
        c.unchanged(ms(t, 1));
        assert_eq!(c.next_deadline(), Some(ms(t, 400)));
        let mut late = Vec::new();
        assert!(!c.due(ms(t, 400), |id| late.push(id)));
        assert_eq!(late, [1]);
        assert!(!c.has_inserts());
    }

    #[test]
    fn only_the_tail_of_a_long_insert_must_show() {
        let t = Instant::now();
        let mut c = Commits::default();
        c.track(7, "é".repeat(3000), t);
        let mut confirmed = Vec::new();
        // The client sends a window of the text, here 3999 bytes, so the
        // matching tail starts inside a character of the insert.
        c.changed(&format!("x{}", "é".repeat(1999)), |id| confirmed.push(id));
        assert_eq!(confirmed, [7]);
    }

    #[test]
    fn losing_focus_reports_pending_inserts() {
        let t = Instant::now();
        let mut c = Commits::default();
        c.track(1, "a".into(), t);
        c.track(2, "b".into(), t);
        c.committed();
        let mut unknown = Vec::new();
        c.clear(|id| unknown.push(id));
        assert_eq!(unknown, [1, 2]);
        assert_eq!(c.next_deadline(), None);
    }
}
