//! Shaping, line breaking and glyph drawing.

use std::collections::HashMap;
use std::ops::Range;

use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::shape::partition::{self, SimpleShapeOptions};
use swash::shape::{Direction, ShapeContext};
use swash::text::cluster::{Boundary, CharInfo, Token, Whitespace};
use swash::text::{BidiClass, Codepoint, Script, analyze};
use swash::zeno::{Format, Vector};

use crate::Image;
use crate::fonts::{FaceId, FaceSelector, Fonts};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextStyle {
    /// Pixels per em, in output pixels.
    pub size: f32,
    pub bold: bool,
}

#[derive(Clone, Copy, Debug)]
struct Glyph {
    face: FaceId,
    id: u16,
    /// Offset from the cluster's pen position.
    x: f32,
    y: f32,
}

#[derive(Clone, Debug)]
struct Cluster {
    glyphs: Range<u32>,
    advance: f32,
    /// Break opportunity before this cluster.
    boundary: Boundary,
    space: bool,
}

/// Shaped text in logical order.
#[derive(Debug)]
pub(crate) struct Shaped {
    glyphs: Vec<Glyph>,
    clusters: Vec<Cluster>,
    pub style: TextStyle,
}

/// One line of a [`Shaped`] text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    clusters: Range<usize>,
    /// Width without trailing white space.
    pub width: f32,
}

/// Vertical metrics of the primary face at a size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LineMetrics {
    pub ascent: f32,
    pub height: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: FaceId,
    id: u16,
    size: u32,
    /// Horizontal subpixel position in quarters.
    subpx: u8,
}

struct Raster {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    color: bool,
    data: Vec<u8>,
}

/// Glyphs kept before the cache is cleared; a few hundred kilobytes.
const GLYPH_CACHE_LIMIT: usize = 2048;

#[derive(Default)]
pub(crate) struct Text {
    pub fonts: Fonts,
    shape: ShapeContext,
    scale: ScaleContext,
    glyphs: HashMap<GlyphKey, Raster>,
}

impl Text {
    pub fn metrics(&mut self, style: TextStyle) -> LineMetrics {
        self.fonts.ensure_loaded();
        match self.fonts.primary(style.bold) {
            Some(id) => {
                let m = self.fonts.face(id).font().metrics(&[]).scale(style.size);
                LineMetrics { ascent: m.ascent, height: (m.ascent + m.descent + m.leading).ceil() }
            }
            None => LineMetrics { ascent: style.size * 0.8, height: (style.size * 1.25).ceil() },
        }
    }

    pub fn shape(&mut self, text: &str, style: TextStyle) -> Shaped {
        self.fonts.ensure_loaded();
        let mut out = Shaped { glyphs: Vec::new(), clusters: Vec::new(), style };
        if text.is_empty() || self.fonts.primary(style.bold).is_none() {
            return out;
        }
        let tokens: Vec<Token> = text
            .char_indices()
            .zip(analyze(text.chars()))
            .map(|((offset, ch), (props, boundary))| Token {
                ch,
                offset: offset as u32,
                len: ch.len_utf8() as u8,
                info: CharInfo::new(props, boundary),
                data: 0,
            })
            .collect();
        for run in script_runs(&tokens) {
            let options = SimpleShapeOptions {
                script: run.script,
                direction: run.direction,
                size: style.size,
                ..SimpleShapeOptions::default()
            };
            let mut selector = FaceSelector { fonts: &mut self.fonts, bold: style.bold };
            let tokens = tokens[run.tokens].iter().copied();
            partition::shape(&mut self.shape, &mut selector, &options, tokens, |chosen, shaper| {
                let face = chosen.id;
                shaper.shape_with(|cluster| {
                    let start = out.glyphs.len() as u32;
                    let mut pen = 0.0;
                    for g in cluster.glyphs {
                        out.glyphs.push(Glyph { face, id: g.id, x: pen + g.x, y: g.y });
                        pen += g.advance;
                    }
                    let space = cluster.info.whitespace();
                    out.clusters.push(Cluster {
                        glyphs: start..out.glyphs.len() as u32,
                        advance: pen,
                        boundary: cluster.info.boundary(),
                        space: space != Whitespace::None && space != Whitespace::NoBreakSpace,
                    });
                });
            });
        }
        out
    }

    /// Draws `line` with its left edge at `x` and baseline at `baseline`.
    pub fn draw(&mut self, img: &mut Image, shaped: &Shaped, line: &Line, x: f32, baseline: f32, color: u32) {
        let mut pen = x;
        let size = shaped.style.size;
        for cluster in &shaped.clusters[line.clusters.clone()] {
            for g in &shaped.glyphs[cluster.glyphs.start as usize..cluster.glyphs.end as usize] {
                let gx = pen + g.x;
                let gy = baseline - g.y;
                let ix = gx.floor();
                let subpx = ((gx - ix) * 4.0) as u8 & 3;
                let key = GlyphKey { face: g.face, id: g.id, size: size.to_bits(), subpx };
                if !self.glyphs.contains_key(&key) {
                    if self.glyphs.len() >= GLYPH_CACHE_LIMIT {
                        self.glyphs.clear();
                    }
                    let raster = self.rasterize(key, size);
                    self.glyphs.insert(key, raster);
                }
                let r = &self.glyphs[&key];
                let (px, py) = (ix as i32 + r.left, gy.round() as i32 - r.top);
                if r.color {
                    img.rgba(px, py, r.width, r.height, &r.data, (color >> 24) as u8);
                } else {
                    img.mask(px, py, r.width, r.height, &r.data, color);
                }
            }
            pen += cluster.advance;
        }
    }

    fn rasterize(&mut self, key: GlyphKey, size: f32) -> Raster {
        let face = self.fonts.face(key.face).clone();
        let mut scaler = self.scale.builder(face.font()).size(size).hint(true).variations(face.variations()).build();
        let sources = [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
        let embolden = if face.synthetic_bold { size / 32.0 } else { 0.0 };
        let image = Render::new(&sources)
            .format(Format::Alpha)
            .offset(Vector::new(f32::from(key.subpx) / 4.0, 0.0))
            .embolden(embolden)
            .render(&mut scaler, key.id);
        match image {
            Some(image) => Raster {
                left: image.placement.left,
                top: image.placement.top,
                width: image.placement.width,
                height: image.placement.height,
                color: image.content == Content::Color,
                data: image.data,
            },
            None => Raster { left: 0, top: 0, width: 0, height: 0, color: false, data: Vec::new() },
        }
    }
}

impl Shaped {
    /// Total advance, ignoring line breaks.
    pub fn width(&self) -> f32 {
        self.clusters.iter().map(|c| c.advance).sum()
    }

    /// Breaks into lines no wider than `max_width` at Unicode line-break
    /// opportunities and at newlines. A cluster wider than the whole line is
    /// placed on a line of its own.
    pub fn wrap(&self, max_width: f32) -> Vec<Line> {
        let mut lines = Vec::new();
        let mut start = 0;
        let mut width = 0.0;
        // Index of the last break opportunity on the current line.
        let mut opportunity = None;
        let line = |clusters: Range<usize>| Line { width: self.trimmed_width(clusters.clone()), clusters };
        for (i, c) in self.clusters.iter().enumerate() {
            if i > start {
                if c.boundary == Boundary::Mandatory {
                    lines.push(line(start..i));
                    (start, width, opportunity) = (i, 0.0, None);
                } else if c.boundary == Boundary::Line {
                    opportunity = Some(i);
                }
            }
            if !c.space && width + c.advance > max_width && i > start {
                if let Some(at) = opportunity.filter(|&o| o > start) {
                    lines.push(line(start..at));
                    start = at;
                    width = self.clusters[at..i].iter().map(|c| c.advance).sum();
                }
                // No opportunity, or the word alone is wider than a line.
                if i > start && width + c.advance > max_width {
                    lines.push(line(start..i));
                    (start, width) = (i, 0.0);
                }
                opportunity = None;
            }
            width += c.advance;
        }
        if start < self.clusters.len() || lines.is_empty() {
            lines.push(line(start..self.clusters.len()));
        }
        lines
    }

    fn trimmed_width(&self, clusters: Range<usize>) -> f32 {
        let slice = &self.clusters[clusters];
        let end = slice.iter().rposition(|c| !c.space).map_or(0, |i| i + 1);
        slice[..end].iter().map(|c| c.advance).sum()
    }

    /// Shortens `line` from the end until it plus `suffix_width` fits
    /// `max_width`, dropping trailing white space.
    pub fn truncate(&self, line: &Line, suffix_width: f32, max_width: f32) -> Line {
        let mut end = line.clusters.end;
        while end > line.clusters.start {
            let candidate = line.clusters.start..end;
            let w = self.trimmed_width(candidate.clone());
            let ends_in_space = self.clusters[end - 1].space;
            if !ends_in_space && w + suffix_width <= max_width {
                return Line { clusters: candidate, width: w };
            }
            end -= 1;
        }
        Line { clusters: line.clusters.start..line.clusters.start, width: 0.0 }
    }
}

struct Run {
    tokens: Range<usize>,
    script: Script,
    direction: Direction,
}

/// Splits tokens into runs of one script; characters common to all scripts
/// (spaces, digits, punctuation) and combining marks join the run around them.
fn script_runs(tokens: &[Token]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut start = 0;
    let mut script = None;
    let mut rtl = None;
    for (i, t) in tokens.iter().enumerate() {
        let s = t.ch.script();
        let neutral = matches!(s, Script::Common | Script::Inherited | Script::Unknown);
        if rtl.is_none() {
            rtl = match t.ch.bidi_class() {
                BidiClass::R | BidiClass::AL => Some(true),
                BidiClass::L => Some(false),
                _ => None,
            };
        }
        match script {
            _ if neutral => {}
            None => script = Some(s),
            Some(current) if current != s => {
                runs.push(run(start..i, current, rtl));
                start = i;
                script = Some(s);
                rtl = None;
            }
            Some(_) => {}
        }
    }
    if start < tokens.len() {
        runs.push(run(start..tokens.len(), script.unwrap_or(Script::Latin), rtl));
    }
    runs
}

fn run(tokens: Range<usize>, script: Script, rtl: Option<bool>) -> Run {
    let direction = if rtl == Some(true) { Direction::RightToLeft } else { Direction::LeftToRight };
    Run { tokens, script, direction }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn text() -> Text {
        let mut t = Text::default();
        t.fonts.ensure_loaded();
        assert!(t.fonts.primary(false).is_some(), "a system sans-serif font is required");
        t
    }

    const BODY: TextStyle = TextStyle { size: 16.0, bold: false };

    #[test]
    fn newline_forces_a_break() {
        let mut t = text();
        let shaped = t.shape("one\ntwo", BODY);
        let lines = shaped.wrap(1000.0);
        assert_eq!(lines.len(), 2);
        let one = t.shape("one", BODY).width();
        assert!((lines[0].width - one).abs() < 0.5, "{} vs {one}", lines[0].width);
    }

    #[test]
    fn wraps_between_words() {
        let mut t = text();
        let shaped = t.shape("alpha beta gamma", BODY);
        let one = t.shape("alpha", BODY).width();
        let lines = shaped.wrap(one * 1.5);
        assert_eq!(lines.len(), 3);
        assert!((lines[0].width - one).abs() < 0.5, "{} vs {one}", lines[0].width);
    }

    #[test]
    fn falls_back_for_characters_the_primary_face_lacks() {
        let mut t = text();
        let shaped = t.shape("a漢", BODY);
        let faces: Vec<FaceId> = shaped.glyphs.iter().map(|g| g.face).collect();
        assert_eq!(faces.len(), 2);
        assert!(shaped.glyphs.iter().all(|g| g.id != 0), "a CJK fallback font is installed");
        assert_ne!(faces[0], faces[1]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn lines_cover_every_cluster_in_order_and_fit(s in "[a-z ]{0,12}( [a-zé漢,.!-]{1,9}){0,12}", max in 20.0f32..400.0) {
            let mut t = text();
            let shaped = t.shape(&s, BODY);
            let lines = shaped.wrap(max);
            let mut next = 0;
            for line in &lines {
                prop_assert_eq!(line.clusters.start, next);
                next = line.clusters.end;
                // Only a single cluster wider than the line may overflow it.
                let visible = shaped.clusters[line.clusters.clone()].iter().filter(|c| !c.space).count();
                prop_assert!(line.width <= max + 0.01 || visible == 1, "{} > {max}", line.width);
            }
            prop_assert_eq!(next, shaped.clusters.len());
        }
    }
}
