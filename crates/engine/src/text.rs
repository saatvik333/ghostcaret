//! Take Word units and suggestion boundaries, found together in one O(n)
//! pass over a continuation.
//!
//! **Units** ([`word_units`]) are what Take Word inserts: a word plus the
//! whitespace after it. Compounds split after hyphens (`well-` `known`), paths
//! after slashes and before extensions (`src/` `main` `.rs`), a unit stops
//! flush before trailing punctuation (`hello` `, ` `world`), and scripts
//! written without spaces between words (Han, Kana, Hangul, Yi, Thai, Lao,
//! Khmer, Myanmar) take one grapheme per unit. Leading whitespace and opening
//! punctuation join the word after them (` (see`).
//!
//! **Boundaries** ([`boundaries`]) are where a suggestion may end, each with a
//! [`Level`]:
//!
//! - Word: after a complete word, or after a grapheme of a script without
//!   spaces, flush before whatever follows. Never after a hyphen or slash
//!   inside a compound, inside `src/main.rs`, or right after a dash.
//! - Phrase: after `,` `;` `:` and their CJK, Arabic and Ethiopic forms;
//!   before a dash; before an English conjunction (`and`, `but`, `because`,
//!   …); after an ellipsis followed by a lowercase word; at a space after Thai,
//!   Lao, Khmer or Myanmar text.
//! - Sentence: after terminal punctuation (`.` `!` `?` `…` `。` `！` `？` `؟`
//!   `।` and others) and any closing quotes or brackets, when whitespace or the
//!   end follows; CJK terminals need nothing after them. Not after a title
//!   abbreviation (`Mr.`, `e.g.`) or an initial (`J.`), and after a soft
//!   abbreviation (`etc.`, `Inc.`, `p.m.`) only when the next word is
//!   capitalised. Decimals (`3.14`) and host names (`example.com`) hold no
//!   boundary because their dot sits inside a word.
//! - Paragraph: before a line break. Nothing after the first line break is
//!   scanned, so a suggestion never spans lines.
//!
//! The end of the text counts as a boundary only after whitespace,
//! punctuation or a grapheme of a script without spaces: a trailing word may
//! have been cut off by a token limit.
//!
//! The scanner also runs incrementally over text that grows token by token
//! (see [`crate::stop::Stopper`]): it never decides on the last grapheme until
//! more text or the end arrives, so every boundary it reports is final.

use unicode_segmentation::UnicodeSegmentation;

/// How strong a boundary is. A boundary of one level also ends every lower
/// level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// The end of a word.
    Word,
    /// A natural pause: a comma, a dash, before a conjunction.
    Phrase,
    /// The end of a sentence.
    Sentence,
    /// The end of a line.
    Paragraph,
}

/// A place where a suggestion may end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boundary {
    /// Byte offset just after the last character a suggestion ending here
    /// shows.
    pub end: usize,
    /// The strongest level that ends here.
    pub level: Level,
    /// Words before `end`, counted in half-words: a word of a spaced script
    /// counts 2, a grapheme of a script written without spaces counts 1 (an
    /// average Chinese or Japanese word is about two characters).
    pub half_words: u32,
}

/// Byte offsets where each Take Word unit of `text` ends, up to its first
/// line break.
pub fn word_units(text: &str) -> Vec<usize> {
    struct Units(Vec<usize>);
    impl Sink for Units {
        fn unit(&mut self, end: usize) {
            self.0.push(end);
        }
    }
    let mut sink = Units(Vec::new());
    Scanner::new("").feed(text, true, &mut sink);
    sink.0
}

/// Boundaries of `text`, a continuation of `before`, in ascending order, up
/// to its first line break. `before` only supplies the word the continuation
/// follows, so `Mr` + `. Smith` is recognised as an abbreviation.
pub fn boundaries(before: &str, text: &str) -> Vec<Boundary> {
    struct Bounds(Vec<Boundary>);
    impl Sink for Bounds {
        fn boundary(&mut self, b: Boundary) {
            self.0.push(b);
        }
    }
    let mut sink = Bounds(Vec::new());
    Scanner::new(before).feed(text, true, &mut sink);
    sink.0
}

/// Receives what the scanner finds, in text order.
pub(crate) trait Sink {
    fn unit(&mut self, _end: usize) {}
    fn boundary(&mut self, _b: Boundary) {}
    /// A word, or a grapheme of a script without spaces, completed at `start`.
    /// Called after any boundary that completing it decided.
    fn word(&mut self, _start: usize) {}
    /// True when nothing further is wanted.
    fn done(&self) -> bool {
        false
    }
}

pub(crate) fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

pub(crate) fn is_horizontal_space(c: char) -> bool {
    c.is_whitespace() && !is_newline(c)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    /// Han, Kana, Hangul, Bopomofo, Yi: a space is just a space.
    Cjk,
    /// Thai, Lao, Khmer, Myanmar: a space separates phrases.
    SouthEastAsian,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cls {
    Space,
    Newline,
    Letter,
    Digit,
    NoSpace(Script),
    Dot,
    Apostrophe,
    Comma,
    Joiner,
    Punct,
}

fn no_space(c: char) -> Option<Script> {
    match c as u32 {
        0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF | 0xA9E0..=0xA9FF | 0xAA60..=0xAA7F => {
            Some(Script::SouthEastAsian)
        }
        0x1100..=0x11FF
        | 0x2E80..=0x2FDF
        | 0x3005..=0x3007
        | 0x3021..=0x3029
        | 0x3031..=0x3035
        | 0x3038..=0x303C
        | 0x3041..=0x30FA
        | 0x30FC..=0x30FF
        | 0x3100..=0x312F
        | 0x3131..=0x318F
        | 0x31A0..=0x31BF
        | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF
        | 0xF900..=0xFAFF
        | 0xFF66..=0xFF9F
        | 0x20000..=0x3FFFF => Some(Script::Cjk),
        _ => None,
    }
}

fn classify(c: char) -> Cls {
    match c {
        c if is_newline(c) => Cls::Newline,
        '.' => Cls::Dot,
        '\'' | '\u{2019}' => Cls::Apostrophe,
        ',' => Cls::Comma,
        '-' | '/' | '\\' | '\u{2010}' | '\u{2011}' => Cls::Joiner,
        '_' => Cls::Letter,
        c if c.is_ascii_alphabetic() => Cls::Letter,
        c if c.is_ascii_digit() => Cls::Digit,
        c if c.is_ascii_whitespace() => Cls::Space,
        c if c.is_ascii() => Cls::Punct,
        c => match no_space(c) {
            Some(script) => Cls::NoSpace(script),
            None if c.is_whitespace() => Cls::Space,
            None if c.is_alphabetic() => Cls::Letter,
            None if c.is_numeric() => Cls::Digit,
            None => Cls::Punct,
        },
    }
}

/// End of the extended grapheme cluster starting at `i`. ASCII pairs other
/// than CR LF always break, which keeps Latin text off the slow path.
fn grapheme_end(text: &str, i: usize) -> usize {
    let b = text.as_bytes();
    if b[i] < 0x80
        && b.get(i + 1)
            .is_none_or(|&n| n < 0x80 && !(b[i] == b'\r' && n == b'\n'))
    {
        return i + 1;
    }
    i + text[i..].graphemes(true).next().map_or(1, str::len)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    Space,
    Newline,
    /// `glued` marks an extension part (`.rs` of `main.rs`): a new Take Word
    /// unit, but the same word for boundaries and counting.
    Word {
        glued: bool,
    },
    NoSpace(Script),
    Joiner,
    Punct(char),
}

#[derive(Clone, Copy, Debug)]
enum Lex {
    Idle,
    Space {
        start: usize,
    },
    Word {
        part: usize,
        part_chars: u32,
        last: Cls,
        glued: bool,
        /// A `.` `'` `,` after the word that the next grapheme decides about.
        held: Option<(usize, usize, Cls)>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Core {
    #[default]
    None,
    Word,
    Punct,
}

/// The Take Word unit being built.
#[derive(Clone, Copy, Debug, Default)]
struct Unit {
    open: bool,
    core: Core,
    /// Whitespace followed the core.
    spaced: bool,
    /// Opening punctuation waiting for its word.
    lead: bool,
    /// Ended by a joiner (`well-`); the next piece starts a new unit.
    closed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Abbrev {
    #[default]
    None,
    /// `Mr`, `e.g`: the period never ends a sentence.
    Title,
    /// `etc`, `Inc`: the period ends a sentence when a capital follows.
    Soft,
    /// A single capital letter, as in `J. K. Rowling`.
    Initial,
}

/// `word` lower-cased into a stack buffer, when it is ASCII and fits.
fn lower<const N: usize>(word: &str) -> Option<([u8; N], usize)> {
    if word.len() > N || !word.is_ascii() {
        return None;
    }
    let mut buf = [0u8; N];
    for (d, s) in buf.iter_mut().zip(word.bytes()) {
        *d = s.to_ascii_lowercase();
    }
    Some((buf, word.len()))
}

/// How the period after `word` behaves. Only called when a run of
/// punctuation directly follows a word.
fn abbrev(word: &str) -> Abbrev {
    let mut chars = word.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return if c.is_uppercase() {
            Abbrev::Initial
        } else {
            Abbrev::None
        };
    }
    let Some((buf, n)) = lower::<6>(word) else {
        return Abbrev::None;
    };
    match &buf[..n] {
        // Titles and abbreviations whose period never ends a sentence.
        b"mr" | b"mrs" | b"ms" | b"mx" | b"dr" | b"prof" | b"st" | b"mt" | b"rev" | b"fr"
        | b"gen" | b"col" | b"lt" | b"sgt" | b"capt" | b"gov" | b"sen" | b"rep" | b"hon"
        | b"pres" | b"vs" | b"cf" | b"approx" | b"fig" | b"figs" | b"vol" | b"vols" | b"ch"
        | b"pp" | b"ed" | b"eds" | b"est" | b"dept" | b"ext" | b"tel" | b"viz" | b"ca" | b"al"
        | b"e.g" | b"i.e" | b"eg" | b"ie" | b"a.k.a" | b"resp" | b"nr" | b"abs" | b"abt" => {
            Abbrev::Title
        }
        // Abbreviations whose period ends a sentence only before a capital.
        b"etc" | b"inc" | b"ltd" | b"co" | b"corp" | b"llc" | b"bros" | b"jr" | b"sr" | b"no"
        | b"jan" | b"feb" | b"mar" | b"apr" | b"jun" | b"jul" | b"aug" | b"sep" | b"sept"
        | b"oct" | b"nov" | b"dec" | b"mon" | b"tue" | b"tues" | b"wed" | b"thu" | b"thur"
        | b"thurs" | b"fri" | b"sat" | b"sun" | b"a.m" | b"p.m" | b"u.s" | b"u.k" | b"u.s.a"
        | b"e.u" | b"ave" | b"blvd" | b"rd" => Abbrev::Soft,
        _ => Abbrev::None,
    }
}

/// English conjunctions that open a new phrase.
fn is_conjunction(word: &str) -> bool {
    lower::<8>(word).is_some_and(|(buf, n)| {
        matches!(
            &buf[..n],
            b"and"
                | b"but"
                | b"or"
                | b"nor"
                | b"yet"
                | b"so"
                | b"because"
                | b"since"
                | b"although"
                | b"though"
                | b"while"
                | b"whereas"
                | b"unless"
                | b"until"
                | b"when"
                | b"whenever"
                | b"where"
                | b"if"
                | b"which"
        )
    })
}

fn is_dash(c: char) -> bool {
    matches!(c, '—' | '–' | '―' | '⸺' | '-' | '\u{2010}' | '\u{2011}')
}

/// A run of punctuation with no whitespace inside it.
#[derive(Clone, Copy, Debug, Default)]
struct Run {
    /// Abbreviation kind of the word the run directly follows.
    abbrev: Abbrev,
    periods: u8,
    ellipsis: bool,
    /// `!`, `?` and other terminals besides `.` and `…`.
    other_terminal: bool,
    cjk_terminal: bool,
    phrase: bool,
    cjk_phrase: bool,
    dash: bool,
    closing: bool,
    other: bool,
}

impl Run {
    fn add(&mut self, c: char) {
        match c {
            '.' => self.periods = self.periods.saturating_add(1),
            '…' | '⋯' => self.ellipsis = true,
            '。' | '！' | '？' | '｡' | '．' => {
                self.cjk_terminal = true;
                self.other_terminal = true;
            }
            '!' | '?' | '‼' | '⁇' | '⁈' | '⁉' | '؟' | '۔' | '।' | '॥' | '።' | '։' | '\u{037E}' =>
            {
                self.other_terminal = true;
            }
            '、' | '，' | '；' | '：' | '､' => {
                self.cjk_phrase = true;
                self.phrase = true;
            }
            ',' | ';' | ':' | '،' | '؛' | '፣' | '፤' => self.phrase = true,
            c if is_dash(c) => self.dash = true,
            '"' | '\'' | '’' | '”' | '»' | '›' | ')' | ']' | '}' | '」' | '』' | '）' | '】'
            | '〉' | '》' | '〕' | '〗' | '〙' | '〛' | '］' | '｝' | '〞' => {
                self.closing = true
            }
            _ => self.other = true,
        }
    }

    fn terminal(&self) -> bool {
        self.periods > 0 || self.ellipsis || self.other_terminal
    }

    fn only_period(&self) -> bool {
        self.periods == 1
            && !self.ellipsis
            && !self.other_terminal
            && !self.phrase
            && !self.dash
            && !self.other
    }

    fn dash_only(&self) -> bool {
        self.dash && !self.terminal() && !self.phrase && !self.closing && !self.other
    }
}

/// A boundary candidate waiting for what follows it.
#[derive(Clone, Copy, Debug)]
struct Pending {
    end: usize,
    half_words: u32,
    spaced: bool,
    kind: PendingKind,
}

#[derive(Clone, Copy, Debug)]
enum PendingKind {
    /// A word starting at `start`; `None` once an extension joined it
    /// (`example.com`), which is never an abbreviation.
    Word {
        start: Option<usize>,
    },
    NoSpace(Script),
    Run(Run),
}

/// What follows a pending boundary.
#[derive(Clone, Copy, Debug)]
enum Next<'t> {
    Word(&'t str),
    NoSpace,
    Mark { c: char, joiner: bool },
    Newline,
    End,
}

/// Streaming scanner behind [`word_units`], [`boundaries`],
/// [`crate::stop::cut`] and [`crate::stop::Stopper`]. State is a few words;
/// it never allocates.
#[derive(Clone, Debug)]
pub(crate) struct Scanner {
    /// Start of the first grapheme not yet processed.
    pos: usize,
    /// A line break was reached; nothing after it counts.
    line_end: bool,
    lex: Lex,
    unit: Unit,
    last_unit: usize,
    half_words: u32,
    pending: Option<Pending>,
    /// The text before the continuation ends in a word (with this
    /// abbreviation kind) that a run at offset 0 directly follows.
    seed: Option<Abbrev>,
}

impl Scanner {
    pub(crate) fn new(before: &str) -> Scanner {
        let tail = before
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '.'))
            .next()
            .unwrap_or("");
        let tail = tail.trim_matches('.');
        let seed =
            (!tail.is_empty() && before.ends_with(char::is_alphanumeric)).then(|| abbrev(tail));
        Scanner {
            pos: 0,
            line_end: false,
            lex: Lex::Idle,
            unit: Unit::default(),
            last_unit: 0,
            half_words: 0,
            pending: None,
            seed,
        }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn at_line_end(&self) -> bool {
        self.line_end
    }

    /// Scans `text` from where the last call stopped. `text` must extend the
    /// text of earlier calls. Without `finish` the last grapheme is left for
    /// later, since more text may still join it.
    pub(crate) fn feed(&mut self, text: &str, finish: bool, sink: &mut impl Sink) {
        let len = text.len();
        let mut i = self.pos;
        while i < len && !self.line_end && !sink.done() {
            let end = grapheme_end(text, i);
            if end == len && !finish {
                break;
            }
            let c = text[i..]
                .chars()
                .next()
                .expect("i < len is a char boundary");
            self.grapheme(text, i, end, c, sink);
            i = end;
        }
        self.pos = i;
        if finish && !self.line_end && !sink.done() {
            self.close(text, len, sink);
            self.resolve(Next::End, sink);
            if self.unit.open {
                self.emit_unit(len, sink);
            }
        }
    }

    fn grapheme(&mut self, text: &str, s: usize, e: usize, c: char, sink: &mut impl Sink) {
        let cls = classify(c);
        if let Lex::Word {
            part,
            part_chars,
            last,
            glued,
            held: Some((hs, _, hc)),
        } = self.lex
        {
            let internal = match hc {
                Cls::Dot => {
                    (last == Cls::Letter && cls == Cls::Letter)
                        || (last == Cls::Digit && cls == Cls::Digit)
                }
                Cls::Apostrophe => last == Cls::Letter && cls == Cls::Letter,
                _ => last == Cls::Digit && cls == Cls::Digit,
            };
            if !internal {
                self.close(text, s, sink);
            } else if hc == Cls::Dot && cls == Cls::Letter && part_chars >= 2 {
                // An extension: `main` `.rs`, `example` `.com`.
                self.piece(text, Piece::Word { glued }, part, hs, sink);
                self.lex = Lex::Word {
                    part: hs,
                    part_chars: 0,
                    last,
                    glued: true,
                    held: None,
                };
            } else {
                let part_chars = if hc == Cls::Dot { 0 } else { part_chars };
                self.lex = Lex::Word {
                    part,
                    part_chars,
                    last,
                    glued,
                    held: None,
                };
            }
        }
        match cls {
            Cls::Letter | Cls::Digit => match &mut self.lex {
                Lex::Word {
                    last, part_chars, ..
                } => {
                    *last = cls;
                    *part_chars += 1;
                }
                _ => {
                    self.close(text, s, sink);
                    self.lex = Lex::Word {
                        part: s,
                        part_chars: 1,
                        last: cls,
                        glued: false,
                        held: None,
                    };
                }
            },
            Cls::Dot | Cls::Apostrophe | Cls::Comma => match &mut self.lex {
                Lex::Word { held, .. } => *held = Some((s, e, cls)),
                _ => {
                    self.close(text, s, sink);
                    self.piece(text, Piece::Punct(c), s, e, sink);
                }
            },
            Cls::Space => {
                if !matches!(self.lex, Lex::Space { .. }) {
                    self.close(text, s, sink);
                    self.lex = Lex::Space { start: s };
                }
            }
            Cls::Newline => {
                self.close(text, s, sink);
                self.piece(text, Piece::Newline, s, e, sink);
            }
            Cls::NoSpace(script) => {
                self.close(text, s, sink);
                self.piece(text, Piece::NoSpace(script), s, e, sink);
            }
            Cls::Joiner => {
                self.close(text, s, sink);
                self.piece(text, Piece::Joiner, s, e, sink);
            }
            Cls::Punct => {
                self.close(text, s, sink);
                self.piece(text, Piece::Punct(c), s, e, sink);
            }
        }
    }

    /// Emits the space or word being lexed, which ends at `at`.
    fn close(&mut self, text: &str, at: usize, sink: &mut impl Sink) {
        match std::mem::replace(&mut self.lex, Lex::Idle) {
            Lex::Idle => {}
            Lex::Space { start } => self.piece(text, Piece::Space, start, at, sink),
            Lex::Word {
                part, glued, held, ..
            } => match held {
                Some((hs, he, _)) => {
                    self.piece(text, Piece::Word { glued }, part, hs, sink);
                    let c = text[hs..].chars().next().expect("held grapheme");
                    self.piece(text, Piece::Punct(c), hs, he, sink);
                }
                None => self.piece(text, Piece::Word { glued }, part, at, sink),
            },
        }
    }

    fn piece(&mut self, text: &str, piece: Piece, s: usize, e: usize, sink: &mut impl Sink) {
        self.unit_piece(piece, s, e, sink);
        match piece {
            Piece::Space => {
                if let Some(p) = &mut self.pending {
                    p.spaced = true;
                }
            }
            Piece::Newline => {
                self.resolve(Next::Newline, sink);
                self.line_end = true;
            }
            Piece::Word { glued: true } => {
                if let Some(p) = &mut self.pending
                    && matches!(p.kind, PendingKind::Word { .. })
                {
                    p.end = e;
                    p.kind = PendingKind::Word { start: None };
                }
            }
            Piece::Word { glued: false } => {
                let word = &text[s..e];
                self.resolve(Next::Word(word), sink);
                self.half_words += 2;
                self.pending = Some(Pending {
                    end: e,
                    half_words: self.half_words,
                    spaced: false,
                    kind: PendingKind::Word { start: Some(s) },
                });
                sink.word(s);
            }
            Piece::NoSpace(script) => {
                self.resolve(Next::NoSpace, sink);
                self.half_words += 1;
                self.pending = Some(Pending {
                    end: e,
                    half_words: self.half_words,
                    spaced: false,
                    kind: PendingKind::NoSpace(script),
                });
                sink.word(s);
            }
            Piece::Joiner | Piece::Punct(_) => {
                let c = text[s..].chars().next().expect("piece is not empty");
                if let Some(Pending {
                    kind: PendingKind::Run(run),
                    spaced: false,
                    end,
                    ..
                }) = &mut self.pending
                {
                    run.add(c);
                    *end = e;
                    return;
                }
                // Only a period after a word can be an abbreviation's.
                let abbrev = match self.pending {
                    _ if c != '.' => Abbrev::None,
                    Some(Pending {
                        kind: PendingKind::Word { start: Some(start) },
                        spaced: false,
                        end,
                        ..
                    }) => abbrev(&text[start..end]),
                    None if s == 0 => self.seed.unwrap_or_default(),
                    _ => Abbrev::None,
                };
                self.resolve(
                    Next::Mark {
                        c,
                        joiner: piece == Piece::Joiner,
                    },
                    sink,
                );
                let mut run = Run {
                    abbrev,
                    ..Run::default()
                };
                run.add(c);
                self.pending = Some(Pending {
                    end: e,
                    half_words: self.half_words,
                    spaced: false,
                    kind: PendingKind::Run(run),
                });
            }
        }
    }

    fn resolve(&mut self, next: Next<'_>, sink: &mut impl Sink) {
        let Some(p) = self.pending.take() else { return };
        let level = match p.kind {
            PendingKind::Word { .. } => word_level(p.spaced, next),
            PendingKind::NoSpace(script) => Some(match next {
                Next::Newline => Level::Paragraph,
                _ if p.spaced && script == Script::SouthEastAsian => Level::Phrase,
                _ => Level::Word,
            }),
            PendingKind::Run(run) => run_level(&run, p.spaced, next),
        };
        if let Some(level) = level
            && p.end > 0
        {
            sink.boundary(Boundary {
                end: p.end,
                level,
                half_words: p.half_words,
            });
        }
    }

    fn emit_unit(&mut self, end: usize, sink: &mut impl Sink) {
        if end > self.last_unit {
            self.last_unit = end;
            sink.unit(end);
        }
    }

    fn unit_piece(&mut self, piece: Piece, s: usize, e: usize, sink: &mut impl Sink) {
        let u = self.unit;
        let fresh = |core, lead| Unit {
            open: true,
            core,
            spaced: false,
            lead,
            closed: false,
        };
        self.unit = match piece {
            Piece::Space if u.closed => fresh(Core::None, false),
            Piece::Space if u.core != Core::None => Unit { spaced: true, ..u },
            Piece::Space if u.lead => Unit {
                core: Core::Punct,
                spaced: true,
                ..u
            },
            Piece::Space => Unit { open: true, ..u },
            Piece::Newline => {
                if u.core != Core::None {
                    self.emit_unit(s, sink);
                }
                Unit::default()
            }
            Piece::Word { glued: true } => {
                self.emit_unit(s, sink);
                fresh(Core::Word, false)
            }
            Piece::Word { glued: false } | Piece::NoSpace(_) => {
                if u.closed || u.core != Core::None {
                    self.emit_unit(s, sink);
                    fresh(Core::Word, false)
                } else {
                    Unit {
                        open: true,
                        core: Core::Word,
                        ..u
                    }
                }
            }
            Piece::Joiner if !u.closed && u.core == Core::Word && !u.spaced => {
                self.emit_unit(e, sink);
                Unit { closed: true, ..u }
            }
            Piece::Joiner | Piece::Punct(_) => {
                if u.closed {
                    fresh(Core::None, true)
                } else if u.core == Core::Word && !u.spaced {
                    self.emit_unit(s, sink);
                    fresh(Core::Punct, false)
                } else if u.core == Core::Punct && !u.spaced {
                    u
                } else if u.core != Core::None {
                    self.emit_unit(s, sink);
                    fresh(Core::None, true)
                } else {
                    Unit {
                        open: true,
                        lead: true,
                        ..u
                    }
                }
            }
        };
    }
}

fn word_level(spaced: bool, next: Next<'_>) -> Option<Level> {
    match next {
        Next::Newline => Some(Level::Paragraph),
        Next::End => spaced.then_some(Level::Word),
        Next::Mark { joiner: true, .. } if !spaced => None,
        Next::Mark { c, joiner } => Some(if joiner || is_dash(c) {
            Level::Phrase
        } else {
            Level::Word
        }),
        Next::Word(w) if spaced && is_conjunction(w) => Some(Level::Phrase),
        Next::Word(_) | Next::NoSpace => Some(Level::Word),
    }
}

fn run_level(run: &Run, spaced: bool, next: Next<'_>) -> Option<Level> {
    if run.dash_only() {
        return None;
    }
    let gap = spaced || matches!(next, Next::End | Next::Newline);
    if !gap {
        return match next {
            Next::NoSpace if run.terminal() => Some(Level::Sentence),
            Next::NoSpace if run.phrase => Some(Level::Phrase),
            Next::NoSpace => Some(Level::Word),
            _ if run.cjk_terminal => Some(Level::Sentence),
            _ if run.cjk_phrase => Some(Level::Phrase),
            _ => None,
        };
    }
    if matches!(next, Next::Newline) {
        return Some(Level::Paragraph);
    }
    let capital = match next {
        Next::Word(w) => w
            .chars()
            .next()
            .map(|c| (c.is_uppercase(), c.is_lowercase())),
        _ => None,
    };
    let level = if run.terminal() {
        let at_end = matches!(next, Next::End);
        let soft_lower =
            run.abbrev == Abbrev::Soft && !at_end && capital.is_none_or(|(upper, _)| !upper);
        let abbreviation = run.only_period()
            && (matches!(run.abbrev, Abbrev::Title | Abbrev::Initial) || soft_lower);
        if abbreviation {
            Level::Word
        } else if (run.ellipsis || run.periods >= 2)
            && !run.other_terminal
            && capital.is_some_and(|(_, lower)| lower)
        {
            Level::Phrase
        } else {
            Level::Sentence
        }
    } else if run.phrase {
        Level::Phrase
    } else {
        Level::Word
    };
    Some(level)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits `text` into its Take Word units.
    fn units(text: &str) -> Vec<&str> {
        let mut at = 0;
        word_units(text)
            .into_iter()
            .map(|e| {
                let u = &text[at..e];
                at = e;
                u
            })
            .collect()
    }

    /// The prefixes of `text` that end at a boundary of at least `level`.
    fn ends(before: &str, text: &str, level: Level) -> Vec<String> {
        boundaries(before, text)
            .into_iter()
            .filter(|b| b.level >= level)
            .map(|b| text[..b.end].to_owned())
            .collect()
    }

    #[test]
    fn units_follow_words_compounds_paths_and_punctuation() {
        let table: &[(&str, &[&str])] = &[
            ("see you tomorrow", &["see ", "you ", "tomorrow"]),
            (" see you", &[" see ", "you"]),
            ("a well-known fact", &["a ", "well-", "known ", "fact"]),
            (
                "open src/main.rs now",
                &["open ", "src/", "main", ".rs ", "now"],
            ),
            ("hello, world.", &["hello", ", ", "world", "."]),
            ("don't stop", &["don't ", "stop"]),
            (
                "it costs 3.14 or 1,000",
                &["it ", "costs ", "3.14 ", "or ", "1,000"],
            ),
            ("see (below) now", &["see ", "(below", ") ", "now"]),
            ("e.g. this", &["e.g", ". ", "this"]),
            (
                "visit example.com today",
                &["visit ", "example", ".com ", "today"],
            ),
            ("great 👍 thanks", &["great ", "👍 ", "thanks"]),
            ("wait... what?!", &["wait", "... ", "what", "?!"]),
            ("Straße und Brücke", &["Straße ", "und ", "Brücke"]),
            ("l'homme est là", &["l'homme ", "est ", "là"]),
            ("¿Qué tal?", &["¿Qué ", "tal", "?"]),
            ("Привет, мир", &["Привет", ", ", "мир"]),
        ];
        for (text, want) in table {
            assert_eq!(units(text), *want, "{text:?}");
        }
    }

    #[test]
    fn scripts_without_spaces_take_one_grapheme_per_unit() {
        let table: &[(&str, &[&str])] = &[
            ("你好，世界。", &["你", "好", "，", "世", "界", "。"]),
            (
                "今日はいい天気",
                &["今", "日", "は", "い", "い", "天", "気"],
            ),
            (
                "안녕하세요 친구",
                &["안", "녕", "하", "세", "요 ", "친", "구"],
            ),
            ("สวัสดีครับ", &["ส", "วั", "ส", "ดี", "ค", "รั", "บ"]),
            ("OK好的", &["OK", "好", "的"]),
        ];
        for (text, want) in table {
            assert_eq!(units(text), *want, "{text:?}");
        }
    }

    #[test]
    fn units_stop_at_the_first_line_break() {
        assert_eq!(units("one two\nthree"), ["one ", "two"]);
        assert_eq!(units("one\r\ntwo"), ["one"]);
    }

    #[test]
    fn word_boundaries_sit_flush_after_complete_words() {
        assert_eq!(
            ends("", "see you tomorrow at", Level::Word),
            ["see", "see you", "see you tomorrow"]
        );
        assert_eq!(
            ends("", "a well-known fact.", Level::Word),
            [
                "a",
                "a well-known",
                "a well-known fact",
                "a well-known fact."
            ]
        );
        assert_eq!(
            ends("", "open src/main.rs now", Level::Word),
            ["open", "open src/main.rs"]
        );
        assert_eq!(
            ends("", "tomorrow, then", Level::Word),
            ["tomorrow", "tomorrow,"]
        );
    }

    #[test]
    fn end_of_text_is_a_boundary_only_after_a_separator() {
        assert_eq!(ends("", "see you tomor", Level::Word), ["see", "see you"]);
        assert_eq!(ends("", "see you ", Level::Word), ["see", "see you"]);
        assert_eq!(
            ends("", "see you.", Level::Word),
            ["see", "see you", "see you."]
        );
        assert_eq!(ends("", "你好", Level::Word), ["你", "你好"]);
    }

    #[test]
    fn phrase_boundaries() {
        assert_eq!(
            ends("", "Sure, I can do that and then call you", Level::Phrase),
            ["Sure,", "Sure, I can do that"]
        );
        assert_eq!(
            ends("", "I'll be there — probably at noon", Level::Phrase),
            ["I'll be there"]
        );
        assert_eq!(
            ends("", "first; second: third", Level::Phrase),
            ["first;", "first; second:"]
        );
        assert_eq!(ends("", "wait... what now", Level::Phrase), ["wait..."]);
        assert_eq!(ends("", "好的，明天见", Level::Phrase), ["好的，"]);
        assert_eq!(ends("", "شكرا، أراك", Level::Phrase), ["شكرا،"]);
        assert_eq!(
            ends("", "android phones", Level::Phrase),
            Vec::<String>::new()
        );
    }

    #[test]
    fn sentence_boundaries_respect_abbreviations_decimals_and_urls() {
        let s = |text| ends("", text, Level::Sentence);
        assert_eq!(s("See you then. Bye now"), ["See you then."]);
        assert_eq!(s("Ask Mr. Smith. He knows"), ["Ask Mr. Smith."]);
        assert_eq!(
            s("Use tools, e.g. a hammer. Done"),
            ["Use tools, e.g. a hammer."]
        );
        assert_eq!(
            s("Books by J. K. Rowling are fine. Yes"),
            ["Books by J. K. Rowling are fine."]
        );
        assert_eq!(
            s("Apples, pears, etc. and more. Then"),
            ["Apples, pears, etc. and more."]
        );
        assert_eq!(s("Apples, pears, etc. Then more"), ["Apples, pears, etc."]);
        assert_eq!(s("It costs 3.14 today. Ok"), ["It costs 3.14 today."]);
        assert_eq!(
            s("Visit example.com/a.html now. Ok"),
            ["Visit example.com/a.html now."]
        );
        assert_eq!(s("Really?! Yes"), ["Really?!"]);
        assert_eq!(s("He said \"go.\" Then left"), ["He said \"go.\""]);
        assert_eq!(s("wait... What now"), ["wait..."]);
        assert_eq!(s("wait… what now"), Vec::<String>::new());
        assert_eq!(s("好的。明天见！"), ["好的。", "好的。明天见！"]);
        assert_eq!(s("क्या हाल है? ठीक।"), ["क्या हाल है?", "क्या हाल है? ठीक।"]);
        assert_eq!(s("Done.Next"), Vec::<String>::new());
    }

    #[test]
    fn abbreviation_context_comes_from_before() {
        assert_eq!(
            ends("Talk to Mr", ". Smith today. Ok", Level::Sentence),
            [". Smith today."]
        );
        assert_eq!(ends("See you then", ". Bye", Level::Sentence), ["."]);
    }

    #[test]
    fn paragraph_boundary_is_the_first_line_break() {
        let b = boundaries("", "Thanks a lot.\nBest,\nAnna");
        assert_eq!(
            b.last().map(|b| (b.end, b.level)),
            Some((13, Level::Paragraph))
        );
        assert!(b.iter().all(|b| b.end <= 13));
    }

    #[test]
    fn half_words_count_words_and_cjk_graphemes() {
        let b = boundaries("", "one two-three four ");
        assert_eq!(
            b.iter().map(|b| b.half_words).collect::<Vec<_>>(),
            [2, 6, 8]
        );
        let b = boundaries("", "你好吗");
        assert_eq!(
            b.iter().map(|b| b.half_words).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    /// Feeding text in pieces, as a model streams it, finds exactly what one
    /// pass over the whole text finds.
    #[test]
    fn streaming_matches_one_pass() {
        struct All(Vec<Boundary>, Vec<usize>);
        impl Sink for All {
            fn boundary(&mut self, b: Boundary) {
                self.0.push(b);
            }
            fn unit(&mut self, e: usize) {
                self.1.push(e);
            }
        }
        for text in [
            "Hi Mr. Smith, see you at 3.14 p.m. Tomorrow — maybe… or not?! 你好。ok",
            "e.g. src/main.rs etc. Fine",
        ] {
            let mut whole = All(Vec::new(), Vec::new());
            Scanner::new("").feed(text, true, &mut whole);
            let mut streamed = All(Vec::new(), Vec::new());
            let mut s = Scanner::new("");
            for (i, _) in text.char_indices().skip(1) {
                s.feed(&text[..i], false, &mut streamed);
            }
            s.feed(text, true, &mut streamed);
            assert_eq!(streamed.0, whole.0, "{text:?}");
            assert_eq!(streamed.1, whole.1, "{text:?}");
        }
    }

    mod props {
        use proptest::prelude::*;

        use super::super::*;

        fn text() -> impl Strategy<Value = String> {
            proptest::string::string_regex("([a-zA-Z]{1,6}|[ .,!?;:'’\"()/\\-—…_0-9]|[你好。，]|[éàß]|e\u{301}|👍|🇺🇸| \n){0,40}")
                .expect("valid regex")
        }

        proptest! {
            #[test]
            fn units_are_ascending_char_boundaries(t in text()) {
                let u = word_units(&t);
                prop_assert!(u.windows(2).all(|w| w[0] < w[1]));
                prop_assert!(u.iter().all(|&e| e > 0 && e <= t.len() && t.is_char_boundary(e)));
            }

            #[test]
            fn boundaries_are_ascending_and_never_split_graphemes(t in text(), before in text()) {
                let b = boundaries(&before, &t);
                prop_assert!(b.windows(2).all(|w| w[0].end < w[1].end && w[0].half_words <= w[1].half_words));
                let graphemes: Vec<usize> = t.grapheme_indices(true).map(|(i, g)| i + g.len()).collect();
                for x in &b {
                    prop_assert!(graphemes.contains(&x.end), "{x:?} in {t:?}");
                    prop_assert!(!t[..x.end].contains(is_newline));
                }
            }

            #[test]
            fn streaming_matches_one_pass_for_any_text(t in text()) {
                struct All(Vec<Boundary>, Vec<usize>);
                impl Sink for All {
                    fn boundary(&mut self, b: Boundary) { self.0.push(b); }
                    fn unit(&mut self, e: usize) { self.1.push(e); }
                }
                let mut whole = All(Vec::new(), Vec::new());
                Scanner::new("").feed(&t, true, &mut whole);
                let mut streamed = All(Vec::new(), Vec::new());
                let mut s = Scanner::new("");
                for (i, _) in t.char_indices().skip(1) {
                    s.feed(&t[..i], false, &mut streamed);
                }
                s.feed(&t, true, &mut streamed);
                prop_assert_eq!(streamed.0, whole.0);
                prop_assert_eq!(streamed.1, whole.1);
            }
        }
    }
}
