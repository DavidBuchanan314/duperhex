//! The pack's font: measurement in GUI units, and glyphs rasterised on demand into an atlas.

use std::collections::HashMap;
use std::ops::Range;

use sdl3::pixels::PixelFormat;
use sdl3::rect::Rect;
use sdl3::render::{BlendMode, FPoint, Texture, TextureCreator, Vertex};
use sdl3::video::WindowContext;

use crate::pack::Rgb;
use crate::render::fcolor;

/// The original's font sizes (those it uses) at its 768x480 layout.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Size {
    Normal,
    Big,
    Heading,
    Title,
}

impl Size {
    /// In pixels per em at the 480-high GUI scale: the original's points, set at 96 dpi.
    fn em(self) -> f32 {
        let pt = match self {
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
const ATLAS: u32 = 2048;

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
    rect: Rect,
    xmin: f32,
    ymin: f32,
}

pub struct Atlas<'a> {
    pub tex: Texture<'a>,
    glyphs: HashMap<(char, u32), Option<Glyph>>,
    x: i32,
    y: i32,
    row_h: i32,
    rgba: Vec<u8>,
}

impl<'a> Atlas<'a> {
    pub fn new(creator: &'a TextureCreator<WindowContext>) -> Result<Atlas<'a>, sdl3::render::TextureValueError> {
        let mut tex = creator.create_texture_static(PixelFormat::RGBA32, ATLAS, ATLAS)?;
        tex.set_blend_mode(BlendMode::Blend);
        Ok(Atlas { tex, glyphs: HashMap::new(), x: 0, y: 0, row_h: 0, rgba: Vec::new() })
    }

    fn glyph(&mut self, font: &Font, c: char, px: f32) -> Option<&Glyph> {
        let key = (c, (px * 16.0) as u32);
        if !self.glyphs.contains_key(&key) {
            let (m, cov) = font.font.rasterize(c, px);
            let g = if m.width == 0 || m.height == 0 {
                None
            } else {
                let (w, h) = (m.width as i32, m.height as i32);
                if self.x + w + 1 > ATLAS as i32 {
                    self.x = 0;
                    self.y += self.row_h + 1;
                    self.row_h = 0;
                }
                if self.y + h > ATLAS as i32 {
                    // full: start again (only happens after many window resizes)
                    self.glyphs.clear();
                    self.x = 0;
                    self.y = 0;
                    self.row_h = 0;
                }
                let rect = Rect::new(self.x, self.y, w as u32, h as u32);
                self.rgba.clear();
                self.rgba.extend(cov.iter().flat_map(|&a| [255, 255, 255, a]));
                if let Err(e) = self.tex.update(rect, &self.rgba, w as usize * 4) {
                    eprintln!("glyph {c:?}: {e}");
                }
                self.x += w + 1;
                self.row_h = self.row_h.max(h);
                Some(Glyph { rect, xmin: m.xmin as f32, ymin: m.ymin as f32 })
            };
            self.glyphs.insert(key, g);
        }
        self.glyphs[&key].as_ref()
    }

    /// Appends the text's triangles to `out`, `k` pixels per GUI unit.
    pub fn build(&mut self, font: &Font, arena: &str, items: &[TextItem], k: f64, out: &mut Vec<Vertex>) {
        let n = ATLAS as f32;
        for it in items {
            let px = it.size.em() * k as f32;
            let color = fcolor(it.color);
            let (x0, base) = ((it.x * k) as f32, (it.baseline * k) as f32);
            for (c, pen) in font.layout(&arena[it.text.clone()], px) {
                let Some(g) = self.glyph(font, c, px) else { continue };
                let (w, h) = (g.rect.width() as f32, g.rect.height() as f32);
                let left = (x0 + pen + g.xmin).round();
                let top = (base - g.ymin - h).round();
                let (u0, v0) = (g.rect.x() as f32 / n, g.rect.y() as f32 / n);
                let (u1, v1) = (u0 + w / n, v0 + h / n);
                let v = |x: f32, y: f32, u: f32, vv: f32| Vertex { position: FPoint::new(x, y), color, tex_coord: FPoint::new(u, vv) };
                let (a, b, c, d) = (v(left, top, u0, v0), v(left + w, top, u1, v0), v(left + w, top + h, u1, v1), v(left, top + h, u0, v1));
                out.extend([a, b, d, b, c, d]);
            }
        }
    }
}
