//! How the pack's scripts act on the world, as it is now. (Directors laying waves out ahead act
//! on the layout instead: see layout.rs.)

use super::{Burst, Event, Morph, Wobble, World, ZoomPulse};
use crate::pack::PaletteId;
use rand::RngExt;
use rand::rngs::SmallRng;
use rand::seq::IndexedRandom;

use crate::script::{self, Effect, Flow, Host, Tilt, Val, Var};

impl Host for World {
    fn get(&self, var: Var) -> Val {
        let r = &self.run;
        match var {
            Var::Wave => Val::Int(r.wave),
            Var::Time => Val::Int(r.survival + r.time_shift),
            Var::Elapsed => Val::Int(r.elapsed),
            Var::Speed => Val::Float(self.stream.speed),
            Var::Sides => Val::Int(self.shape.sides as i64),
            Var::SidesAfterMorph => Val::Int(self.shape.sides as i64 - (self.shape.morph == Morph::Shrink) as i64),
            Var::Morphing => Val::from_bool(self.shape.morph != Morph::None),
            Var::Palette => Val::Int(self.pal.id().0 as i64),
            Var::PaletteFading => Val::from_bool(self.pal.fading()),
            Var::SpinBurstActive => Val::from_bool(self.view.burst != Burst::None),
            Var::Left => Val::from_bool(self.player.left),
            Var::Right => Val::from_bool(self.player.right),
            Var::Counter(i) => {
                if r.tutorial {
                    r.tcounters[i]
                } else {
                    r.counters[i]
                }
            }
        }
    }

    fn set(&mut self, var: Var, v: Val) {
        match var {
            Var::Speed => {
                if v.f() != self.stream.speed {
                    // only the finale's logic sets it, as it starts: changes how far apart waves are
                    self.unlay();
                }
                self.stream.speed = v.f();
            }
            Var::Counter(i) => {
                if self.run.tutorial {
                    self.run.tcounters[i] = v;
                } else {
                    self.run.counters[i] = v;
                }
            }
            // the pack is checked for other assignments
            _ => {}
        }
    }

    fn rng(&mut self) -> &mut SmallRng {
        &mut self.rng
    }

    fn act(&mut self, e: &'static Effect) -> Flow {
        match e {
            Effect::Pattern(id) => self.place_now(&self.pack.patterns[*id]),
            Effect::Delay(n) => self.stream.wavetimer = *n,
            Effect::RerollRotation(m) => self.reroll(m),
            Effect::RandomRotation(m) => self.view.rotation = m.choose(&mut self.rng).copied(),
            Effect::Rotation(m) => self.view.rotation = Some(*m),
            Effect::Pulse(v) => self.view.pulse = *v,
            Effect::SpinBurst => {
                self.view.burst_dir = self.view.rotation.map_or(1.0, |r| self.pack.rotation_modes[r].burst);
                self.view.burst_vel = 0.0;
                self.view.burst = Burst::Accel;
            }
            Effect::Tilt(t) => {
                self.view.wobble = match t {
                    Tilt::Random => Wobble::Out(if self.rng.random() { 1.0 } else { -1.0 }),
                    Tilt::Right => Wobble::Out(1.0),
                    Tilt::Left => Wobble::Out(-1.0),
                    Tilt::Swing => Wobble::Swing1,
                }
            }
            Effect::ZoomPulse => self.view.zoompulse = ZoomPulse::In,
            Effect::Shrink => self.shape.morph = Morph::Shrink,
            Effect::Grow => self.shape.morph = Morph::GrowStart,
            Effect::Palette(e) => {
                let id = PaletteId(script::eval(e, self).f() as i32);
                self.pal.change(id);
            }
            Effect::Music(Some(t)) => self.play_music(*t),
            Effect::Music(None) => self.stop_music(),
            Effect::Sfx(id) => self.events.push(Event::Sound(*id)),
            Effect::Flash(n) => self.view.flash = *n,
            Effect::ClearWalls => {
                self.walls.clear();
                self.player.blocked = None;
            }
            Effect::FreezeWalls(n) => self.stream.freeze = *n,
            Effect::Camera { lean, sway } => {
                if let Some(l) = lean {
                    self.view.lean = Some(*l);
                }
                if let Some(s) = sway {
                    self.view.sway = *s;
                }
            }
            Effect::SwitchLevel { level, time_offset } => {
                self.switch_level(*level, *time_offset);
                return Flow::Break(());
            }
            Effect::AdvanceWaves => {
                if !self.lay.active() {
                    return self.advance_waves();
                }
            }
            // takes effect once this tick's logic is done, as its actions still use its counters
            Effect::EndTutorial => self.run.tutorial_over = true,
            Effect::EndSequence => {
                self.events.push(Event::EndingDone);
                self.run.hit_pending = true;
            }
        }
        Flow::Continue(())
    }
}
