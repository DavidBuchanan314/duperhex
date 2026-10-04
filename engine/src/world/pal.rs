//! The palette: start and end colours per slot, oscillated between continuously, and cross-faded
//! to another palette on request.

use super::EPS;
use crate::pack::{Pack, PaletteId, Rgb};

pub const SLOTS: usize = 9;

/// What the slots colour: the engine's choice, after the original.
pub mod slot {
    pub const BACKGROUND: usize = 0;
    /// Every other background wedge.
    pub const WEDGE: usize = 1;
    /// The walls, alternating with the next slot; also the centre shape's border.
    pub const WALLS: usize = 2;
    pub const CENTRE: usize = 2;
    /// GUI buttons.
    pub const BUTTON: usize = 4;
    pub const PLAYER: usize = 5;
    /// Walls on the player's side, alternating with the next slot.
    pub const NEAR_WALLS: usize = 6;
    /// GUI highlights and borders, and the player's shadow.
    pub const HIGHLIGHT: usize = 7;
    /// A five-sided shape's odd background wedge.
    pub const ODD_WEDGE: usize = 8;
}
pub type Colours = [Rgb; SLOTS];

/// The oscillation's range (0 is the start colours, FULL the end ones), its rate per tick, how
/// long it rests at the start colours, and how fast a cross-fade goes out and comes back in.
pub const FULL: f64 = 255.0;
const OSC_RATE: f64 = 4.0;
const REST: f64 = 1.0;
const FADE_OUT_RATE: f64 = 25.0;
const FADE_IN_RATE: f64 = 4.0;

/// The continuous start/end oscillation.
#[derive(Clone, Copy, PartialEq)]
enum Osc {
    Rising,
    Falling,
    /// A tick's pause at the start colours.
    Resting(f64),
}

/// A cross-fade to another palette.
#[derive(Clone, Copy, PartialEq)]
enum Fade {
    None,
    /// Fading out to the start colours.
    Out(PaletteId),
    /// Fading in from the colours on screen to the new palette.
    In(PaletteId),
}

pub struct Pal {
    start: Colours,
    end: Colours,
    /// 0 is the start colours, FULL the end ones.
    fade: f64,
    osc: Osc,
    xfade: Fade,
    id: PaletteId,
}

impl Pal {
    pub fn new(pack: &Pack, id: PaletteId) -> Pal {
        let mut p = Pal { start: [[0.0; 3]; SLOTS], end: [[0.0; 3]; SLOTS], fade: 0.0, osc: Osc::Resting(REST), xfade: Fade::None, id };
        p.set(pack, id);
        p
    }

    pub fn colours_at(&self, fade: f64) -> Colours {
        let mut c = [[0.0; 3]; SLOTS];
        for (s, slot) in c.iter_mut().enumerate() {
            for (k, ch) in slot.iter_mut().enumerate() {
                *ch = self.start[s][k] + (self.end[s][k] - self.start[s][k]) * fade / FULL;
            }
        }
        c
    }

    pub fn colours(&self) -> Colours {
        self.colours_at(self.fade)
    }


    /// The palette's id for the logic: during a cross-fade, the new one once the old is gone.
    pub fn id(&self) -> PaletteId {
        match self.xfade {
            Fade::In(target) => target,
            _ => self.id,
        }
    }

    pub fn fading(&self) -> bool {
        self.xfade != Fade::None
    }

    /// Switches palette at once. Slots the palette leaves out keep their colours.
    pub fn set(&mut self, pack: &Pack, id: PaletteId) {
        if let Some(p) = pack.palettes.get(&id) {
            for s in 0..SLOTS {
                if let Some(Some(c)) = p.start.get(s) {
                    self.start[s] = *c;
                }
                if let Some(Some(c)) = p.end.get(s) {
                    self.end[s] = *c;
                }
            }
        }
        self.id = id;
    }

    pub fn change(&mut self, id: PaletteId) {
        self.xfade = Fade::Out(id);
    }

    pub fn step(&mut self, pack: &Pack, mut dt: f64) {
        while dt > 0.0 {
            let osc_rate = match self.osc {
                Osc::Rising => OSC_RATE,
                Osc::Falling => -OSC_RATE,
                Osc::Resting(_) => 0.0,
            };
            let fade_rate = match self.xfade {
                Fade::Out(_) => -FADE_OUT_RATE,
                Fade::In(_) => FADE_IN_RATE,
                Fade::None => 0.0,
            };
            let rate = osc_rate + fade_rate;
            let mut tt = dt;
            if let Osc::Resting(left) = self.osc {
                tt = tt.min(left);
            }
            if rate > 0.0 {
                tt = tt.min((FULL - self.fade) / rate);
            } else if rate < 0.0 {
                tt = tt.min(self.fade / -rate);
            }
            let tt = tt.max(0.0);
            self.fade += rate * tt;
            dt -= tt;
            if let Osc::Resting(left) = self.osc {
                self.osc = if left - tt <= EPS { Osc::Rising } else { Osc::Resting(left - tt) };
            }
            if rate > 0.0 && self.fade >= FULL - EPS {
                self.fade = FULL;
                if self.osc == Osc::Rising {
                    self.osc = Osc::Falling;
                }
                if let Fade::In(target) = self.xfade {
                    self.osc = Osc::Falling;
                    self.xfade = Fade::None;
                    self.set(pack, target);
                }
            }
            if rate < 0.0 && self.fade <= EPS {
                self.fade = 0.0;
                if self.osc == Osc::Falling {
                    self.osc = Osc::Resting(REST);
                }
                if let Fade::Out(target) = self.xfade {
                    // fade in from the colours on screen (all but the last slot, as the original)
                    let cur = self.colours();
                    self.set(pack, target);
                    self.start[..SLOTS - 1].copy_from_slice(&cur[..SLOTS - 1]);
                    self.xfade = Fade::In(target);
                }
            }
            if tt == 0.0 && rate == 0.0 && !matches!(self.osc, Osc::Resting(_)) {
                break;
            }
        }
    }
}
