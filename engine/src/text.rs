//! The pack's font: measurement in GUI units, and glyphs rasterised on demand into an atlas.

use std::collections::HashMap;
use std::ops::Range;

use crate::colour::Linear;
use crate::gpu::Vertex;
use crate::pack::Rgb;

/// The original's font sizes (those it uses) at its 768x480 layout, plus Small for our own
/// overlays.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Size {
    Small,
    Normal,
    Big,
    Heading,
    Title,
}

impl Size {
    /// In pixels per em at the 480-high GUI scale: the original's points, set at 96 dpi.
    fn em(self) -> f32 {
        let pt = match self {
            Size::Small => 7.0,
            Size::Normal => 12.0,
            Size::Big => 25.0,
            Size::Heading => 38.0,
            Size::Title => 58.0,
        };
        pt * 96.0 / 72.0
    }
}
const LETTER_SPACING: f32 = 0.99;
const SPACE_SIZE: f32 = 0.4;
pub const ATLAS: u32 = 2048;

pub struct Font {
    font: fontdue::Font,
}

/// Text to draw, in GUI units: a range of the GUI's text arena.
pub struct TextItem {
    pub text: Range<usize>,
    pub size: Size,
    pub x: f64,
    pub baseline: f64,
    pub color: Rgb,
}

impl Font {
    pub fn new(bytes: &[u8]) -> Result<Font, &'static str> {
        Ok(Font { font: fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())? })
    }

    /// Glyph positions along a line, in pixels at `px` per em: (char, pen x).
    fn layout<'a>(&'a self, s: &'a str, px: f32) -> impl Iterator<Item = (char, f32)> + 'a {
        let space = self.font.metrics('p', px).advance_width * LETTER_SPACING * SPACE_SIZE;
        s.chars()
            .scan(0.0, move |x, c| {
                let pen = *x;
                *x += if c == ' ' { space } else { self.font.metrics(c, px).advance_width * LETTER_SPACING };
                Some((c, pen))
            })
            .filter(|&(c, _)| c != ' ')
    }

    /// Width of the text's bounding box, in GUI units.
    pub fn width(&self, s: &str, size: Size) -> f64 {
        let px = size.em();
        let space = self.font.metrics('p', px).advance_width * LETTER_SPACING * SPACE_SIZE;
        let (mut minx, mut maxx) = (f32::INFINITY, f32::NEG_INFINITY);
        let mut x = 0.0;
        for c in s.chars() {
            if c == ' ' {
                maxx = maxx.max(x);
                x += space;
            } else {
                let m = self.font.metrics(c, px);
                minx = minx.min(x + m.xmin as f32);
                maxx = maxx.max(x + m.xmin as f32 + m.width as f32 * LETTER_SPACING);
                x += m.advance_width * LETTER_SPACING;
            }
        }
        if minx.is_finite() { (maxx - minx) as f64 } else { 0.0 }
    }

    /// Height of the text's bounding box, in GUI units.
    pub fn height(&self, s: &str, size: Size) -> f64 {
        let px = size.em();
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for c in s.chars().filter(|&c| c != ' ') {
            let m = self.font.metrics(c, px);
            lo = lo.min(m.ymin as f32);
            hi = hi.max((m.ymin + m.height as i32) as f32);
        }
        if lo.is_finite() { (hi - lo) as f64 } else { 0.0 }
    }
}

struct Glyph {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    xmin: f32,
    ymin: f32,
}

/// A glyph's coverage, to be copied into the atlas texture at (x, y).
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub coverage: Vec<u8>,
}

/// Where glyphs are in the atlas texture, and the ones still to be copied into it.
pub struct Atlas {
    glyphs: HashMap<(char, u32), Option<Glyph>>,
    x: u32,
    y: u32,
    row_h: u32,
    /// Bumped each time the atlas is cleared.
    generation: u32,
    pub uploads: Vec<GlyphUpload>,
}

impl Atlas {
    pub fn new() -> Atlas {
        Atlas { glyphs: HashMap::new(), x: 0, y: 0, row_h: 0, generation: 0, uploads: Vec::new() }
    }

    fn glyph(&mut self, font: &Font, c: char, px: f32) -> Option<&Glyph> {
        let key = (c, (px * 16.0) as u32);
        if !self.glyphs.contains_key(&key) {
            let (m, cov) = font.font.rasterize(c, px);
            let g = if m.width == 0 || m.height == 0 {
                None
            } else {
                let (w, h) = (m.width as u32, m.height as u32);
                if self.x + w + 1 > ATLAS {
                    self.x = 0;
                    self.y += self.row_h + 1;
                    self.row_h = 0;
                }
                if self.y + h > ATLAS {
                    // full: start again (after many window resizes, or a huge window)
                    self.glyphs.clear();
                    self.generation += 1;
                    self.x = 0;
                    self.y = 0;
                    self.row_h = 0;
                }
                let (x, y) = (self.x, self.y);
                self.uploads.push(GlyphUpload { x, y, w, h, coverage: cov });
                self.x += w + 1;
                self.row_h = self.row_h.max(h);
                Some(Glyph { x, y, w, h, xmin: m.xmin as f32, ymin: m.ymin as f32 })
            };
            self.glyphs.insert(key, g);
        }
        self.glyphs[&key].as_ref()
    }

    /// Appends the text's triangles to `out`, `k` pixels per GUI unit.
    pub fn build(&mut self, font: &Font, arena: &str, items: &[TextItem], k: f64, out: &mut Vec<Vertex>) {
        let start = out.len();
        let generation = self.generation;
        self.build_once(font, arena, items, k, out);
        if self.generation != generation {
            // the atlas filled and was cleared part way through, so glyphs already emitted may
            // have been overwritten: lay it all out again into the fresh atlas
            out.truncate(start);
            self.build_once(font, arena, items, k, out);
        }
    }

    fn build_once(&mut self, font: &Font, arena: &str, items: &[TextItem], k: f64, out: &mut Vec<Vertex>) {
        let n = ATLAS as f32;
        for it in items {
            let px = it.size.em() * k as f32;
            let color = Linear::from_srgb(it.color, 1.0);
            let (x0, base) = ((it.x * k) as f32, (it.baseline * k) as f32);
            for (c, pen) in font.layout(&arena[it.text.clone()], px) {
                let Some(g) = self.glyph(font, c, px) else { continue };
                let (w, h) = (g.w as f32, g.h as f32);
                let left = (x0 + pen + g.xmin).round();
                let top = (base - g.ymin - h).round();
                let (u0, v0) = (g.x as f32 / n, g.y as f32 / n);
                let (u1, v1) = (u0 + w / n, v0 + h / n);
                let v = |x: f32, y: f32, u: f32, vv: f32| Vertex { pos: [x, y], uv: [u, vv], color };
                let (a, b, c, d) = (v(left, top, u0, v0), v(left + w, top, u1, v0), v(left + w, top + h, u1, v1), v(left, top + h, u0, v1));
                out.extend([a, b, d, b, c, d]);
            }
        }
    }
}
