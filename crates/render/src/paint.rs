//! Pixel operations on premultiplied ARGB8888.

use crate::Image;

/// A straight-alpha colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(rgb: u32) -> Color {
        Color { r: (rgb >> 16) as u8, g: (rgb >> 8) as u8, b: rgb as u8, a: 255 }
    }

    pub const fn with_alpha(self, a: u8) -> Color {
        Color { a, ..self }
    }

    pub fn premultiplied(self) -> u32 {
        let a = u32::from(self.a);
        let ch = |c: u8| div255(u32::from(c) * a);
        a << 24 | ch(self.r) << 16 | ch(self.g) << 8 | ch(self.b)
    }
}

/// `x / 255` rounded, exact for `x <= 255 * 255`.
#[inline]
fn div255(x: u32) -> u32 {
    let x = x + 128;
    (x + (x >> 8)) >> 8
}

/// Multiplies every channel of a premultiplied pixel by `a / 255`.
#[inline]
pub(crate) fn scale(px: u32, a: u32) -> u32 {
    let rb = (px & 0x00ff_00ff) * a + 0x0080_0080;
    let rb = ((rb + ((rb >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let ag = ((px >> 8) & 0x00ff_00ff) * a + 0x0080_0080;
    let ag = (ag + ((ag >> 8) & 0x00ff_00ff)) & 0xff00_ff00;
    rb | ag
}

/// Source-over for premultiplied pixels.
#[inline]
pub(crate) fn over(dst: u32, src: u32) -> u32 {
    src + scale(dst, 255 - (src >> 24))
}

impl Image {
    pub(crate) fn new(width: u32, height: u32) -> Image {
        Image { width, height, pixels: vec![0; width as usize * height as usize] }
    }

    /// Blends `color` scaled by `coverage / 255` over the pixel at `(x, y)`;
    /// coordinates outside the image are ignored.
    #[inline]
    pub(crate) fn blend(&mut self, x: i32, y: i32, color: u32, coverage: u32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 || coverage == 0 {
            return;
        }
        let i = y as usize * self.width as usize + x as usize;
        let src = if coverage >= 255 { color } else { scale(color, coverage) };
        self.pixels[i] = over(self.pixels[i], src);
    }

    /// Fills a rounded rectangle with `fill` and strokes `border` pixels
    /// inside its edge with `stroke`, anti-aliased.
    pub(crate) fn rounded_rect(&mut self, r: Rect, radius: f32, fill: Color, border: f32, stroke: Color) {
        let fill = fill.premultiplied();
        let stroke = stroke.premultiplied();
        let radius = radius.clamp(0.0, (r.w.min(r.h)) / 2.0);
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (hw, hh) = (r.w / 2.0 - radius, r.h / 2.0 - radius);
        let x0 = r.x.floor().max(0.0) as i32;
        let y0 = r.y.floor().max(0.0) as i32;
        let x1 = ((r.x + r.w).ceil() as i32).min(self.width as i32);
        let y1 = ((r.y + r.h).ceil() as i32).min(self.height as i32);
        for y in y0..y1 {
            let qy = (y as f32 + 0.5 - cy).abs() - hh;
            for x in x0..x1 {
                let qx = (x as f32 + 0.5 - cx).abs() - hw;
                // Signed distance to the rounded rectangle's edge.
                let outside = qx.max(0.0).hypot(qy.max(0.0));
                let d = outside + qx.max(qy).min(0.0) - radius;
                let outer = (0.5 - d).clamp(0.0, 1.0);
                if outer == 0.0 {
                    continue;
                }
                let inner = (0.5 - d - border).clamp(0.0, 1.0);
                self.blend(x, y, fill, (outer * 255.0 + 0.5) as u32);
                if border > 0.0 {
                    self.blend(x, y, stroke, ((outer - inner) * 255.0 + 0.5) as u32);
                }
            }
        }
    }

    /// Blends an 8-bit coverage mask of `w` by `h` with its top-left at
    /// `(x, y)`.
    pub(crate) fn mask(&mut self, x: i32, y: i32, w: u32, h: u32, data: &[u8], color: u32) {
        for row in 0..h {
            for col in 0..w {
                let c = data[(row * w + col) as usize];
                self.blend(x + col as i32, y + row as i32, color, u32::from(c));
            }
        }
    }

    /// Blends a straight-alpha RGBA bitmap scaled by `alpha / 255`.
    pub(crate) fn rgba(&mut self, x: i32, y: i32, w: u32, h: u32, data: &[u8], alpha: u8) {
        for row in 0..h {
            for col in 0..w {
                let i = ((row * w + col) * 4) as usize;
                let px = Color { r: data[i], g: data[i + 1], b: data[i + 2], a: data[i + 3] }.premultiplied();
                self.blend(x + col as i32, y + row as i32, px, u32::from(alpha));
            }
        }
    }

    /// Multiplies the alpha of rows `y0..y1` from fully transparent at `y0`
    /// to unchanged at `y1`, fading out content scrolled past the top.
    pub(crate) fn fade_rows(&mut self, x0: i32, x1: i32, y0: i32, y1: i32, under: u32) {
        let span = (y1 - y0).max(1) as f32;
        for y in y0.max(0)..y1.min(self.height as i32) {
            let keep = (((y - y0) as f32 + 0.5) / span * 255.0) as u32;
            for x in x0.max(0)..x1.min(self.width as i32) {
                let i = y as usize * self.width as usize + x as usize;
                self.pixels[i] = over(scale(under, 255 - keep), scale(self.pixels[i], keep));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn is_premultiplied(px: u32) -> bool {
        let a = px >> 24;
        [px >> 16, px >> 8, px].iter().all(|&c| c & 0xff <= a)
    }

    proptest! {
        #[test]
        fn over_keeps_pixels_premultiplied(dst in any::<[u8; 4]>(), src in any::<[u8; 4]>(), cov in 0u32..=255) {
            let dst = Color { r: dst[0], g: dst[1], b: dst[2], a: dst[3] }.premultiplied();
            let src = Color { r: src[0], g: src[1], b: src[2], a: src[3] }.premultiplied();
            let out = over(dst, scale(src, cov));
            prop_assert!(is_premultiplied(out), "{out:08x}");
            prop_assert!(out >> 24 >= dst >> 24);
        }
    }

    #[test]
    fn rounded_rect_is_solid_inside_and_clear_outside() {
        let mut img = Image::new(40, 20);
        let white = Color::rgb(0xffffff);
        img.rounded_rect(Rect { x: 2.0, y: 2.0, w: 36.0, h: 16.0 }, 6.0, white, 0.0, white);
        assert_eq!(img.pixels[10 * 40 + 20], 0xffff_ffff);
        // Corners are cut, edges outside the rectangle are untouched.
        assert_eq!(img.pixels[2 * 40 + 2], 0);
        assert_eq!(img.pixels[0], 0);
        assert_eq!(img.pixels[10 * 40 + 39], 0);
        assert!(img.pixels.iter().all(|&p| is_premultiplied(p)));
    }

    #[test]
    fn border_covers_only_the_edge() {
        let mut img = Image::new(30, 30);
        let clear = Color::rgb(0).with_alpha(0);
        img.rounded_rect(Rect { x: 0.0, y: 0.0, w: 30.0, h: 30.0 }, 0.0, clear, 2.0, Color::rgb(0xff0000));
        assert_eq!(img.pixels[15 * 30], 0xffff_0000);
        assert_eq!(img.pixels[15 * 30 + 1], 0xffff_0000);
        assert_eq!(img.pixels[15 * 30 + 15], 0);
    }
}
