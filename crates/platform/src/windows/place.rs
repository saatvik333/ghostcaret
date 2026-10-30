//! Where the overlay goes, in physical screen pixels, and how far the caret
//! has moved since UI Automation last reported it.

/// A rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Px {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Px {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// A caret is about this many ems tall in common UI fonts.
const CARET_EMS: f32 = 1.3;

/// The logical font size that matches a caret `caret_h` physical pixels tall
/// at `scale` physical pixels per logical pixel.
pub fn font_px(caret_h: i32, scale: f32) -> f32 {
    (caret_h as f32 / scale / CARET_EMS).clamp(9.0, 40.0)
}

/// Top-left of a suggestion image of `size` so that its first character
/// starts at the caret and its first line is centred on it (`anchor` is that
/// point within the image). An image that would run off the right edge of
/// `work` moves to the line below, right-aligned; one that would run off the
/// bottom moves above the caret.
pub fn suggestion_at(caret: Px, anchor: (f32, f32), size: (i32, i32), work: Px) -> (i32, i32) {
    let (w, h) = size;
    let mut x = caret.x - anchor.0.round() as i32;
    let mut y = caret.y + caret.h / 2 - anchor.1.round() as i32;
    if x + w > work.right() {
        x = work.right() - w;
        y += caret.h;
    }
    if y + h > work.bottom() {
        y = caret.y - h;
    }
    (x.max(work.x), y.max(work.y))
}

/// Top-left of a card of `size`: below the caret line, or above it when it
/// does not fit below, starting at the caret and kept inside `work`.
pub fn card_at(caret: Px, size: (i32, i32), work: Px, gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let below = caret.bottom() + gap;
    let y = if below + h <= work.bottom() {
        below
    } else {
        caret.y - gap - h
    };
    let x = caret.x.min(work.right() - w);
    (x.max(work.x), y.max(work.y))
}

/// The caret position the overlay follows: the last reported rectangle plus
/// the width of text consumed since, which slides the suggestion along while
/// the app catches up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaretGuess {
    reported: Option<Px>,
    advance: f32,
}

impl CaretGuess {
    /// A new reading. An unchanged rectangle keeps the guess, since the app
    /// has not yet moved its caret past the consumed text.
    pub fn reported(&mut self, caret: Option<Px>) {
        if caret != self.reported {
            self.reported = caret;
            self.advance = 0.0;
        }
    }

    /// Text `width` physical pixels wide was typed or taken at the caret.
    pub fn advanced(&mut self, width: f32) {
        self.advance += width;
    }

    pub fn caret(&self) -> Option<Px> {
        self.reported.map(|c| Px {
            x: c.x + self.advance.round() as i32,
            ..c
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const WORK: Px = Px {
        x: 0,
        y: 0,
        w: 1920,
        h: 1040,
    };

    #[test]
    fn suggestions_start_at_the_caret_on_its_line() {
        let caret = Px {
            x: 500,
            y: 300,
            w: 1,
            h: 20,
        };
        assert_eq!(
            suggestion_at(caret, (9.0, 12.0), (200, 24), WORK),
            (491, 298)
        );
    }

    #[test]
    fn suggestions_wrap_below_at_the_right_edge_and_flip_above_at_the_bottom() {
        let caret = Px {
            x: 1900,
            y: 300,
            w: 1,
            h: 20,
        };
        assert_eq!(
            suggestion_at(caret, (9.0, 12.0), (200, 24), WORK),
            (1720, 318)
        );
        let caret = Px {
            x: 500,
            y: 1030,
            w: 1,
            h: 20,
        };
        assert_eq!(suggestion_at(caret, (9.0, 12.0), (200, 24), WORK).1, 1006);
    }

    #[test]
    fn displays_left_of_and_above_the_main_one_work() {
        let work = Px {
            x: -2560,
            y: -1440,
            w: 2560,
            h: 1400,
        };
        let caret = Px {
            x: -1000,
            y: -700,
            w: 2,
            h: 30,
        };
        assert_eq!(
            suggestion_at(caret, (10.0, 15.0), (300, 40), work),
            (-1010, -700)
        );
    }

    #[test]
    fn cards_go_below_unless_there_is_no_room() {
        let caret = Px {
            x: 100,
            y: 100,
            w: 1,
            h: 20,
        };
        assert_eq!(card_at(caret, (400, 300), WORK, 6), (100, 126));
        let caret = Px {
            x: 1800,
            y: 900,
            w: 1,
            h: 20,
        };
        assert_eq!(card_at(caret, (400, 300), WORK, 6), (1520, 594));
    }

    #[test]
    fn the_guess_slides_until_the_caret_really_moves() {
        let caret = Px {
            x: 100,
            y: 10,
            w: 1,
            h: 20,
        };
        let mut g = CaretGuess::default();
        g.reported(Some(caret));
        g.advanced(30.4);
        assert_eq!(g.caret().map(|c| c.x), Some(130));
        g.reported(Some(caret));
        assert_eq!(
            g.caret().map(|c| c.x),
            Some(130),
            "a stale reading keeps the guess"
        );
        g.reported(Some(Px { x: 131, ..caret }));
        assert_eq!(g.caret().map(|c| c.x), Some(131));
    }

    #[test]
    fn font_follows_the_caret_height() {
        assert!((font_px(26, 1.0) - 20.0).abs() < 0.01);
        assert!((font_px(39, 1.5) - 20.0).abs() < 0.01);
        assert_eq!(font_px(2, 1.0), 9.0);
    }

    proptest! {
        #[test]
        fn overlays_stay_on_their_display(cx in -3000i32..3000, cy in -2000i32..2000, w in 1i32..800, h in 1i32..400) {
            let work = Px { x: -2560, y: -1440, w: 5120, h: 2880 };
            let caret = Px { x: cx, y: cy, w: 1, h: 20 };
            for (x, y) in [suggestion_at(caret, (8.0, 10.0), (w, h), work), card_at(caret, (w, h), work, 6)] {
                prop_assert!(x >= work.x && y >= work.y);
                prop_assert!(x + w <= work.right() || x == work.x);
            }
        }
    }
}
