//! Software renderer for the suggestion overlay and the Reply & rewrite card,
//! shared by the Linux and Windows backends.
//!
//! Output is premultiplied ARGB8888 in native-endian `u32`s, which has the
//! same byte layout as `WL_SHM_FORMAT_ARGB8888` on Wayland and a 32-bit
//! premultiplied BGRA DIB for `UpdateLayeredWindow` on Windows.
//!
//! Fonts are the system's sans-serif face (fontconfig on Linux, Segoe UI on
//! Windows), loaded on the first draw, with per-character fallback to any
//! installed face that has the character.

mod fonts;
mod paint;
mod system;
mod text;

use std::fmt;

use oft_engine::io::{Bindings, CardView, Chord, Key, SuggestionView};
use oft_engine::settings::GhostStyle;

use paint::{Color, Rect};
use text::{Line, Shaped, Text, TextStyle};

/// How to draw: output pixels per logical pixel, the body font size in
/// logical pixels, the suggestion style and the colour scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub scale: f32,
    pub font_px: f32,
    pub style: GhostStyle,
    pub dark: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            scale: 1.0,
            font_px: 15.0,
            style: GhostStyle::Faint,
            dark: false,
        }
    }
}

/// A premultiplied ARGB8888 image in output pixels, rows top to bottom.
/// Zero-sized when there is nothing to show.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

/// Draws overlays. Keeps loaded fonts and rasterised glyphs between calls;
/// owned by one thread.
pub struct Renderer {
    text: Text,
    /// Label of the key that takes the next word, shown on the suggestion.
    key_hint: String,
}

impl fmt::Debug for Renderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renderer")
            .field("key_hint", &self.key_hint)
            .finish_non_exhaustive()
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Renderer::new()
    }
}

/// Most lines of a suggestion before it is cut with an ellipsis.
const SUGGESTION_LINES: usize = 6;
/// Suggestion width before wrapping, in ems.
const SUGGESTION_EMS: f32 = 34.0;
/// Lines of the streaming draft kept visible; earlier lines scroll away.
const PREVIEW_LINES: usize = 8;
const ITEM_LINES: usize = 2;
const STATUS_LINES: usize = 3;
const ELLIPSIS: &str = "…";

struct Palette {
    bg: Color,
    border: Color,
    text: Color,
    ghost: Color,
    muted: Color,
    chip: Color,
    accent: Color,
    selected: Color,
    field: Color,
    border_px: f32,
}

fn palette(theme: &Theme) -> Palette {
    let contrast = theme.style == GhostStyle::HighContrast;
    let px = theme.scale.max(1.0).round();
    match (theme.dark, contrast) {
        (true, false) => Palette {
            bg: Color::rgb(0x1c1d21).with_alpha(246),
            border: Color::rgb(0xffffff).with_alpha(30),
            text: Color::rgb(0xe8e9ed),
            ghost: Color::rgb(0x9da1aa),
            muted: Color::rgb(0x8d919a),
            chip: Color::rgb(0xffffff).with_alpha(70),
            accent: Color::rgb(0x6b9bf2),
            selected: Color::rgb(0x6b9bf2).with_alpha(60),
            field: Color::rgb(0xffffff).with_alpha(14),
            border_px: px,
        },
        (false, false) => Palette {
            bg: Color::rgb(0xfbfbfc).with_alpha(248),
            border: Color::rgb(0x000000).with_alpha(34),
            text: Color::rgb(0x1d1f24),
            ghost: Color::rgb(0x6b6f78),
            muted: Color::rgb(0x6b6f78),
            chip: Color::rgb(0x000000).with_alpha(64),
            accent: Color::rgb(0x2f6fde),
            selected: Color::rgb(0x2f6fde).with_alpha(38),
            field: Color::rgb(0x000000).with_alpha(10),
            border_px: px,
        },
        (true, true) => Palette {
            bg: Color::rgb(0x000000),
            border: Color::rgb(0xffffff),
            text: Color::rgb(0xffffff),
            ghost: Color::rgb(0xffffff),
            muted: Color::rgb(0xd6d6d6),
            chip: Color::rgb(0xffffff),
            accent: Color::rgb(0x9cc2ff),
            selected: Color::rgb(0x9cc2ff).with_alpha(110),
            field: Color::rgb(0xffffff).with_alpha(36),
            border_px: (px * 1.5).round(),
        },
        (false, true) => Palette {
            bg: Color::rgb(0xffffff),
            border: Color::rgb(0x000000),
            text: Color::rgb(0x000000),
            ghost: Color::rgb(0x000000),
            muted: Color::rgb(0x303030),
            chip: Color::rgb(0x000000),
            accent: Color::rgb(0x0040b0),
            selected: Color::rgb(0x0040b0).with_alpha(64),
            field: Color::rgb(0x000000).with_alpha(24),
            border_px: (px * 1.5).round(),
        },
    }
}

/// A draw operation, laid out before the image size is known.
enum Op {
    Panel {
        rect: Rect,
        radius: f32,
        fill: Color,
        border: f32,
        stroke: Color,
    },
    Text {
        shaped: usize,
        line: Line,
        x: f32,
        baseline: f32,
        color: Color,
    },
    Fade {
        x0: i32,
        x1: i32,
        y0: i32,
        y1: i32,
        under: Color,
    },
}

/// Collects shaped texts and operations for one image.
#[derive(Default)]
struct Scene {
    shaped: Vec<Shaped>,
    ops: Vec<Op>,
}

impl Scene {
    fn add(&mut self, shaped: Shaped) -> usize {
        self.shaped.push(shaped);
        self.shaped.len() - 1
    }

    fn text(&mut self, shaped: usize, line: Line, x: f32, baseline: f32, color: Color) {
        self.ops.push(Op::Text {
            shaped,
            line,
            x,
            baseline,
            color,
        });
    }

    fn paint(self, text: &mut Text, width: f32, height: f32) -> Image {
        let mut img = Image::new(width.ceil() as u32, height.ceil() as u32);
        for op in self.ops {
            match op {
                Op::Panel {
                    rect,
                    radius,
                    fill,
                    border,
                    stroke,
                } => img.rounded_rect(rect, radius, fill, border, stroke),
                Op::Text {
                    shaped,
                    line,
                    x,
                    baseline,
                    color,
                } => text.draw(
                    &mut img,
                    &self.shaped[shaped],
                    &line,
                    x,
                    baseline,
                    color.premultiplied(),
                ),
                Op::Fade {
                    x0,
                    x1,
                    y0,
                    y1,
                    under,
                } => img.fade_rows(x0, x1, y0, y1, under.premultiplied()),
            }
        }
        img
    }
}

/// Sizes derived from the theme, in output pixels.
struct Metrics {
    em: f32,
    body: TextStyle,
    small: TextStyle,
}

impl Metrics {
    fn new(theme: &Theme) -> Metrics {
        let scale = if theme.scale.is_finite() && theme.scale > 0.0 {
            theme.scale
        } else {
            1.0
        };
        let font = if theme.font_px.is_finite() && theme.font_px > 0.0 {
            theme.font_px
        } else {
            15.0
        };
        let em = (font * scale).round().max(1.0);
        Metrics {
            em,
            body: TextStyle {
                size: em,
                bold: false,
            },
            small: TextStyle {
                size: (em * 0.78).round().max(1.0),
                bold: false,
            },
        }
    }

    fn px(&self, ems: f32) -> f32 {
        (self.em * ems).round()
    }

    /// Suggestion padding: left of the text, above the first line.
    fn suggestion_pad(&self) -> (f32, f32) {
        (self.px(0.6), self.px(0.3))
    }
}

impl Renderer {
    /// Creates a renderer; fonts load on the first draw.
    pub fn new() -> Renderer {
        Renderer {
            text: Text::default(),
            key_hint: key_hint(&Bindings::default()),
        }
    }

    /// Updates the key hint shown on suggestions to the current Take Word
    /// key (or Take All when Take Word is unbound).
    pub fn set_bindings(&mut self, bindings: &Bindings) {
        self.key_hint = key_hint(bindings);
    }

    /// Where the suggestion's text starts inside its image, in output
    /// pixels: `x` is the left edge of the first character, `y` the vertical
    /// centre of the first line. Backends align this point with the caret.
    pub fn suggestion_anchor(&mut self, theme: &Theme) -> (f32, f32) {
        let m = Metrics::new(theme);
        let (pad_x, pad_y) = m.suggestion_pad();
        (pad_x, pad_y + self.text.metrics(m.body).height / 2.0)
    }

    /// The advance of `text` in the suggestion's body style, in output
    /// pixels.
    pub fn text_width(&mut self, text: &str, theme: &Theme) -> f32 {
        self.text.shape(text, Metrics::new(theme).body).width()
    }

    /// The suggestion panel: the visible text in the ghost style, the key
    /// hint and the alternative indicator. Zero-sized when nothing is visible.
    pub fn suggestion(&mut self, view: &SuggestionView, theme: &Theme) -> Image {
        let visible = view.visible();
        if visible.is_empty() {
            return Image::default();
        }
        let m = Metrics::new(theme);
        let pal = palette(theme);
        let mut scene = Scene::default();
        let (pad_x, pad_y) = m.suggestion_pad();
        let gap = m.px(0.7);
        let body = self.text.metrics(m.body);
        let small = self.text.metrics(m.small);

        let max_text = m.em * SUGGESTION_EMS;
        let shaped = self.text.shape(visible, m.body);
        let mut lines = shaped.wrap(max_text);
        let ellipsis = (lines.len() > SUGGESTION_LINES).then(|| self.text.shape(ELLIPSIS, m.body));
        if let Some(ellipsis) = &ellipsis {
            lines.truncate(SUGGESTION_LINES);
            let last = lines.pop().expect("at least one line");
            lines.push(shaped.truncate(&last, ellipsis.width(), max_text));
        }
        let text_w = lines.iter().map(|l| l.width).fold(0.0, f32::max);
        let shaped = scene.add(shaped);

        let hint = (!self.key_hint.is_empty()).then(|| self.text.shape(&self.key_hint, m.small));
        let alt = view
            .alternative
            .map(|(i, n)| self.text.shape(&format!("{i}/{n}"), m.small));
        let chip_pad = m.px(0.35);
        let chip_w = hint
            .as_ref()
            .map_or(0.0, |h| h.width().ceil() + 2.0 * chip_pad);
        let alt_w = alt.as_ref().map_or(0.0, |a| a.width().ceil());

        let mut width = pad_x + text_w.ceil();
        let chip_x = width + gap;
        if hint.is_some() {
            width = chip_x + chip_w;
        }
        let alt_x = width + if hint.is_some() { m.px(0.45) } else { gap };
        if alt.is_some() {
            width = alt_x + alt_w;
        }
        width += pad_x;
        let height = 2.0 * pad_y + body.height * lines.len() as f32;

        scene.ops.push(Op::Panel {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: height,
            },
            radius: m.em * 0.42,
            fill: pal.bg,
            border: pal.border_px,
            stroke: pal.border,
        });
        let first_baseline = (pad_y + body.ascent).round();
        let last_line = lines.len() - 1;
        let ellipsis = ellipsis.map(|e| scene.add(e));
        for (i, line) in lines.into_iter().enumerate() {
            let baseline = first_baseline + body.height * i as f32;
            let end = pad_x + line.width;
            scene.text(shaped, line, pad_x, baseline, pal.ghost);
            if i == last_line
                && let Some(e) = ellipsis
            {
                let line = scene.shaped[e].wrap(f32::INFINITY).remove(0);
                scene.text(e, line, end, baseline, pal.ghost);
            }
        }
        // Small text sits centred on the first line.
        let mid = pad_y + body.height / 2.0;
        let small_baseline = (mid - small.height / 2.0 + small.ascent).round();
        if let Some(hint) = hint {
            let chip_h = small.height + m.px(0.16);
            scene.ops.push(Op::Panel {
                rect: Rect {
                    x: chip_x,
                    y: (mid - chip_h / 2.0).round(),
                    w: chip_w,
                    h: chip_h,
                },
                radius: m.em * 0.22,
                fill: Color::rgb(0).with_alpha(0),
                border: pal.border_px,
                stroke: pal.chip,
            });
            let h = scene.add(hint);
            let line = scene.shaped[h].wrap(f32::INFINITY).remove(0);
            scene.text(h, line, chip_x + chip_pad, small_baseline, pal.muted);
        }
        if let Some(alt) = alt {
            let a = scene.add(alt);
            let line = scene.shaped[a].wrap(f32::INFINITY).remove(0);
            scene.text(a, line, alt_x, small_baseline, pal.muted);
        }
        scene.paint(&mut self.text, width, height)
    }

    /// The Reply & rewrite card, at most `max_width_px` output pixels wide: title,
    /// numbered items with the selection highlighted, the custom direction
    /// with a caret, the draft preview (wrapped, scrolled to its end), the
    /// status line and the key hint.
    pub fn card(&mut self, card: &CardView, theme: &Theme, max_width_px: u32) -> Image {
        let m = Metrics::new(theme);
        let pal = palette(theme);
        let max_w = (max_width_px as f32).max(m.em * 8.0);
        let pad = m.px(0.9);
        let gap = m.px(0.55);
        let (row_x, row_y) = (m.px(0.45), m.px(0.28));
        let title_style = TextStyle {
            bold: true,
            ..m.body
        };
        let body = self.text.metrics(m.body);
        let small = self.text.metrics(m.small);
        let title = self.text.metrics(title_style);

        let mut scene = Scene::default();
        let title_text =
            (!card.title.is_empty()).then(|| scene.add(self.text.shape(&card.title, title_style)));
        let numbers: Vec<usize> = (1..=card.items.len())
            .map(|n| scene.add(self.text.shape(&n.to_string(), m.small)))
            .collect();
        let items: Vec<usize> = card
            .items
            .iter()
            .map(|t| scene.add(self.text.shape(t, m.body)))
            .collect();
        let input = card
            .input
            .as_deref()
            .map(|t| scene.add(self.text.shape(t, m.body)));
        let preview = card
            .preview
            .as_deref()
            .map(|t| scene.add(self.text.shape(t, m.body)));
        let status = card
            .status
            .as_deref()
            .map(|t| scene.add(self.text.shape(t, m.small)));
        let hint = (!card.hint.is_empty()).then(|| scene.add(self.text.shape(&card.hint, m.small)));
        let ellipsis = scene.add(self.text.shape(ELLIPSIS, m.body));

        let num_w = numbers
            .iter()
            .map(|&n| scene.shaped[n].width())
            .fold(0.0, f32::max)
            .ceil()
            + m.px(0.6);
        let caret_w = m.scale_px(theme, 1.5);
        let natural_line = |s: &Shaped| {
            s.wrap(f32::INFINITY)
                .iter()
                .map(|l| l.width)
                .fold(0.0, f32::max)
        };
        let mut natural = m.em * 18.0;
        for (&text, extra) in title_text
            .iter()
            .map(|t| (t, 0.0))
            .chain(items.iter().map(|t| (t, 2.0 * row_x + num_w)))
            .chain(input.iter().map(|t| (t, 2.0 * row_x + caret_w)))
            .chain(preview.iter().chain(&status).chain(&hint).map(|t| (t, 0.0)))
        {
            natural = natural.max(natural_line(&scene.shaped[text]) + extra);
        }
        let width = (natural.ceil() + 2.0 * pad).min(max_w).floor();
        let inner = width - 2.0 * pad;

        let mut y = pad;
        let section = |y: &mut f32, first: &mut bool| {
            if !*first {
                *y += gap;
            }
            *first = false;
        };
        let mut first = true;
        let panel_at = scene.ops.len();

        if let Some(t) = title_text {
            section(&mut y, &mut first);
            let lines = limit(&scene, t, inner, 2, ellipsis);
            y = place_lines(&mut scene, t, lines, pad, y, title, pal.text, ellipsis);
        }
        if !items.is_empty() {
            section(&mut y, &mut first);
            for (i, &item) in items.iter().enumerate() {
                let text_x = pad + row_x + num_w;
                let lines = limit(
                    &scene,
                    item,
                    inner - 2.0 * row_x - num_w,
                    ITEM_LINES,
                    ellipsis,
                );
                let row_h = body.height * lines.0.len() as f32 + 2.0 * row_y;
                if card.selected == Some(i) {
                    scene.ops.push(Op::Panel {
                        rect: Rect {
                            x: pad,
                            y,
                            w: inner,
                            h: row_h,
                        },
                        radius: m.em * 0.35,
                        fill: pal.selected,
                        border: 0.0,
                        stroke: pal.selected,
                    });
                }
                let baseline = (y + row_y + body.ascent).round();
                let n = numbers[i];
                let line = scene.shaped[n].wrap(f32::INFINITY).remove(0);
                scene.text(n, line, pad + row_x, baseline, pal.muted);
                place_lines(
                    &mut scene,
                    item,
                    lines,
                    text_x,
                    y + row_y,
                    body,
                    pal.text,
                    ellipsis,
                );
                y += row_h;
            }
        }
        if let Some(t) = input {
            section(&mut y, &mut first);
            let box_w = inner - 2.0 * row_x - caret_w;
            let all = scene.shaped[t].wrap(box_w);
            let shown: Vec<Line> = all[all.len().saturating_sub(ITEM_LINES)..].to_vec();
            let h = body.height * shown.len() as f32 + 2.0 * row_y;
            scene.ops.push(Op::Panel {
                rect: Rect {
                    x: pad,
                    y,
                    w: inner,
                    h,
                },
                radius: m.em * 0.35,
                fill: pal.field,
                border: pal.border_px,
                stroke: pal.accent,
            });
            let caret_x = pad + row_x + shown.last().map_or(0.0, |l| l.width);
            let lines = shown.len();
            place_lines(
                &mut scene,
                t,
                (shown, false),
                pad + row_x,
                y + row_y,
                body,
                pal.text,
                ellipsis,
            );
            let caret_top = y + row_y + body.height * (lines.max(1) - 1) as f32;
            scene.ops.push(Op::Panel {
                rect: Rect {
                    x: caret_x.round(),
                    y: caret_top + m.px(0.12),
                    w: caret_w,
                    h: body.height - m.px(0.24),
                },
                radius: 0.0,
                fill: pal.accent,
                border: 0.0,
                stroke: pal.accent,
            });
            y += h;
        }
        if let Some(t) = preview {
            section(&mut y, &mut first);
            scene.ops.push(Op::Panel {
                rect: Rect {
                    x: pad,
                    y,
                    w: inner,
                    h: pal.border_px,
                },
                radius: 0.0,
                fill: pal.border,
                border: 0.0,
                stroke: pal.border,
            });
            y += pal.border_px + gap;
            let all = scene.shaped[t].wrap(inner);
            let scrolled = all.len() > PREVIEW_LINES;
            let shown = all[all.len().saturating_sub(PREVIEW_LINES)..].to_vec();
            let top = y;
            y = place_lines(
                &mut scene,
                t,
                (shown, false),
                pad,
                y,
                body,
                pal.text,
                ellipsis,
            );
            if scrolled {
                scene.ops.push(Op::Fade {
                    x0: pad as i32,
                    x1: (pad + inner) as i32,
                    y0: top as i32,
                    y1: (top + body.height) as i32,
                    under: pal.bg,
                });
            }
        }
        if let Some(t) = status {
            section(&mut y, &mut first);
            let lines = limit(&scene, t, inner, STATUS_LINES, ellipsis);
            y = place_lines(&mut scene, t, lines, pad, y, small, pal.muted, ellipsis);
        }
        if let Some(t) = hint {
            section(&mut y, &mut first);
            let lines = limit(&scene, t, inner, 2, ellipsis);
            y = place_lines(&mut scene, t, lines, pad, y, small, pal.muted, ellipsis);
        }
        let height = (y + pad).ceil();
        scene.ops.insert(
            panel_at,
            Op::Panel {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: width,
                    h: height,
                },
                radius: m.em * 0.75,
                fill: pal.bg,
                border: pal.border_px,
                stroke: pal.border,
            },
        );
        scene.paint(&mut self.text, width, height)
    }
}

impl Metrics {
    /// Logical pixels to output pixels, at least one.
    fn scale_px(&self, theme: &Theme, logical: f32) -> f32 {
        (logical * theme.scale.max(1.0)).round().max(1.0)
    }
}

/// Wraps `shaped` at `width` and keeps at most `max` lines, cutting the last
/// kept line for an ellipsis. Returns the lines and whether they were cut.
fn limit(
    scene: &Scene,
    shaped: usize,
    width: f32,
    max: usize,
    ellipsis: usize,
) -> (Vec<Line>, bool) {
    let s = &scene.shaped[shaped];
    let mut lines = s.wrap(width);
    if lines.len() <= max {
        return (lines, false);
    }
    lines.truncate(max);
    let last = lines.pop().expect("max is at least one");
    lines.push(s.truncate(&last, scene.shaped[ellipsis].width(), width));
    (lines, true)
}

/// Queues `lines` top-down from `top`; returns the y below the last line.
fn place_lines(
    scene: &mut Scene,
    shaped: usize,
    (lines, cut): (Vec<Line>, bool),
    x: f32,
    top: f32,
    metrics: text::LineMetrics,
    color: Color,
    ellipsis: usize,
) -> f32 {
    let count = lines.len();
    let mut y = top;
    for (i, line) in lines.into_iter().enumerate() {
        let baseline = (y + metrics.ascent).round();
        let end = x + line.width;
        scene.text(shaped, line, x, baseline, color);
        if cut && i + 1 == count {
            let e = scene.shaped[ellipsis].wrap(f32::INFINITY).remove(0);
            scene.text(ellipsis, e, end, baseline, color);
        }
        y += metrics.height;
    }
    y
}

/// The label for the key that takes the next word.
fn key_hint(bindings: &Bindings) -> String {
    bindings
        .take_word
        .or(bindings.take_all)
        .map(chord_label)
        .unwrap_or_default()
}

fn chord_label(chord: Chord) -> String {
    let mut label = String::new();
    let mods = chord.mods;
    for (on, name) in [
        (mods.ctrl, "Ctrl+"),
        (mods.alt, "Alt+"),
        (mods.shift, "Shift+"),
        (mods.meta, "Super+"),
    ] {
        if on {
            label.push_str(name);
        }
    }
    match chord.key {
        Key::Tab => label.push_str("Tab"),
        Key::Escape => label.push_str("Esc"),
        Key::Enter => label.push_str("Enter"),
        Key::Backspace => label.push_str("Backspace"),
        Key::Space => label.push_str("Space"),
        Key::Left => label.push('←'),
        Key::Right => label.push('→'),
        Key::Up => label.push('↑'),
        Key::Down => label.push('↓'),
        Key::Home => label.push_str("Home"),
        Key::End => label.push_str("End"),
        Key::PageUp => label.push_str("PgUp"),
        Key::PageDown => label.push_str("PgDn"),
        Key::F(n) => label.push_str(&format!("F{n}")),
        Key::Char(c) => label.extend(c.to_uppercase()),
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use oft_engine::io::Mods;

    fn view(text: &str, shown: usize) -> SuggestionView {
        SuggestionView {
            text: text.into(),
            shown,
            ..Default::default()
        }
    }

    fn premultiplied(img: &Image) -> bool {
        img.pixels.iter().all(|&p| {
            let a = p >> 24;
            (p >> 16) & 0xff <= a && (p >> 8) & 0xff <= a && p & 0xff <= a
        })
    }

    #[test]
    fn nothing_visible_draws_nothing() {
        let mut r = Renderer::new();
        let img = r.suggestion(&view("hello", 0), &Theme::default());
        assert_eq!((img.width, img.height), (0, 0));
    }

    #[test]
    fn suggestion_scales_with_hidpi_and_grows_with_text() {
        let mut r = Renderer::new();
        let short = r.suggestion(&view(" world", 6), &Theme::default());
        let long = r.suggestion(&view(" world, how are you", 19), &Theme::default());
        let hidpi = r.suggestion(
            &view(" world", 6),
            &Theme {
                scale: 2.0,
                ..Theme::default()
            },
        );
        assert!(long.width > short.width);
        assert!(
            (hidpi.width as f32 / short.width as f32 - 2.0).abs() < 0.15,
            "{hidpi:?} vs {short:?}"
        );
        assert!((hidpi.height as f32 / short.height as f32 - 2.0).abs() < 0.15);
        for img in [&short, &long, &hidpi] {
            assert_eq!(img.pixels.len(), (img.width * img.height) as usize);
            assert!(premultiplied(img));
        }
    }

    #[test]
    fn long_suggestion_wraps_to_bounded_lines() {
        let mut r = Renderer::new();
        let theme = Theme::default();
        let one = r.suggestion(&view("word", 4), &theme);
        let text = "word ".repeat(400);
        let long = r.suggestion(&view(&text, text.len()), &theme);
        assert!(long.width as f32 <= theme.font_px * (SUGGESTION_EMS + 8.0));
        let lines = (long.height as f32 / one.height as f32).round();
        assert!(lines <= SUGGESTION_LINES as f32 + 1.0, "{long:?}");
    }

    #[test]
    fn card_respects_max_width_and_scrolls_preview() {
        let mut r = Renderer::new();
        let theme = Theme {
            dark: true,
            ..Theme::default()
        };
        let mut card = CardView {
            title: "Reply".into(),
            items: vec![
                "Accept and suggest Tuesday".into(),
                "Decline politely".into(),
                "Ask for details".into(),
            ],
            selected: Some(1),
            input: Some("make it short".into()),
            preview: Some("Thanks! ".repeat(10)),
            status: Some("Writing…".into()),
            hint: "Enter insert · Esc close".into(),
            ..Default::default()
        };
        let short = r.card(&card, &theme, 420);
        assert!(short.width <= 420 && short.height > 0);
        assert!(premultiplied(&short));
        card.preview = Some("Thanks! ".repeat(400));
        let long = r.card(&card, &theme, 420);
        assert!(long.height > short.height);
        card.preview = Some("Thanks! ".repeat(800));
        let longer = r.card(&card, &theme, 420);
        assert_eq!(
            long.height, longer.height,
            "the preview is capped and scrolled"
        );
    }

    #[test]
    fn key_hint_follows_bindings() {
        let mut bindings = Bindings::default();
        assert_eq!(key_hint(&bindings), "Tab");
        bindings.take_word = Some(Chord {
            key: Key::Right,
            mods: Mods {
                ctrl: true,
                ..Mods::default()
            },
        });
        assert_eq!(key_hint(&bindings), "Ctrl+→");
        bindings.take_word = None;
        assert_eq!(key_hint(&bindings), "`");
    }
}
