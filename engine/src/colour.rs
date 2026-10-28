//! Colours for drawing. The pack's colours are sRGB; everything drawn is linear light, and the
//! only way to make one is from an sRGB colour, so sRGB never reaches the GPU. The window's
//! surface encodes back to sRGB at the very end.

use bytemuck::{Pod, Zeroable};

use crate::pack::Rgb;

/// A colour in linear light, with alpha.
#[repr(transparent)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Linear([f32; 4]);

fn decode(c: f64) -> f32 {
    let c = (c / 255.0).clamp(0.0, 1.0);
    (if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }) as f32
}

impl Linear {
    /// From sRGB channels, 0 to 255.
    pub fn from_srgb(c: Rgb, alpha: f32) -> Linear {
        Linear([decode(c[0]), decode(c[1]), decode(c[2]), alpha])
    }

    pub fn rgba(self) -> [f32; 4] {
        self.0
    }

    /// Brighter or darker by `k`, keeping alpha.
    pub fn scaled(self, k: f32) -> Linear {
        let [r, g, b, a] = self.0;
        Linear([r * k, g * k, b * k, a])
    }

    /// `k` of the way from `self` to `to`.
    pub fn lerp(self, to: Linear, k: f32) -> Linear {
        Linear(std::array::from_fn(|i| self.0[i] + (to.0[i] - self.0[i]) * k))
    }

    /// Perceived lightness, 0 to 1: Oklab's L.
    #[allow(clippy::excessive_precision)] // Oklab's published coefficients, as in bloom.wgsl
    pub fn lightness(self) -> f32 {
        let [r, g, b, _] = self.0;
        let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
        let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
        let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
        0.2104542553 * l.max(0.0).cbrt() + 0.7936177850 * m.max(0.0).cbrt() - 0.0040720468 * s.max(0.0).cbrt()
    }
}
