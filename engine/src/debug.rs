//! Debugging hooks, all driven by environment variables:
//!
//! - `DUPERHEX_SHOT=DIR:T1,T2,...` saves a screenshot at each tick time, then exits.
//! - `DUPERHEX_INPUT=T:KEYS,...` holds the keys from each tick time: L(eft) R(ight) U(p) D(own)
//!   S(elect) E(sc) C(lear), the mouse buttons 1 (left) 2 (middle) 3 (right), or - for none.
//! - `DUPERHEX_TRACE` prints the player's state and the pulse every frame.
//! - `DUPERHEX_GOD` makes the player immune to walls.
//!
//! Release builds ignore them all.

use std::env;

use crate::game::{Button, Buttons, Game, Held, Key};
use crate::gpu::Image;

pub struct Debug {
    pub trace: bool,
    pub god: bool,
    /// Pending screenshot times, last first.
    shots: Vec<f64>,
    shot_dir: String,
    /// Pending input changes, last first, and the controls and mouse buttons held now.
    input: Vec<(f64, Held, Buttons)>,
    held: Held,
    buttons: Buttons,
}

impl Debug {
    pub fn from_env() -> Debug {
        if !cfg!(debug_assertions) {
            return Debug { trace: false, god: false, shots: vec![], shot_dir: String::new(), input: vec![], held: Held::default(), buttons: Buttons::default() };
        }
        let (shot_dir, mut shots) = match env::var("DUPERHEX_SHOT") {
            Ok(v) => {
                let (d, ts) = v.split_once(':').expect("DUPERHEX_SHOT=DIR:T1,T2,...");
                (d.to_string(), ts.split(',').map(|t| t.parse().expect("DUPERHEX_SHOT time")).collect())
            }
            Err(_) => (String::new(), vec![]),
        };
        shots.reverse();
        let mut input: Vec<(f64, Held, Buttons)> = env::var("DUPERHEX_INPUT")
            .map(|v| {
                v.split(',')
                    .map(|e| {
                        let (t, k) = e.split_once(':').expect("DUPERHEX_INPUT=T:KEYS,...");
                        let h = Held {
                            left: k.contains('L'),
                            right: k.contains('R'),
                            up: k.contains('U'),
                            down: k.contains('D'),
                            select: k.contains('S'),
                            quit: k.contains('E'),
                            clear: k.contains('C'),
                        };
                        let b = Buttons { left: k.contains('1'), middle: k.contains('2'), right: k.contains('3') };
                        (t.parse().expect("DUPERHEX_INPUT time"), h, b)
                    })
                    .collect()
            })
            .unwrap_or_default();
        input.reverse();
        Debug { trace: env::var_os("DUPERHEX_TRACE").is_some(), god: env::var_os("DUPERHEX_GOD").is_some(), shots, shot_dir, input, held: Held::default(), buttons: Buttons::default() }
    }

    /// Applies scripted input up to time `target`.
    pub fn feed(&mut self, g: &mut Game, target: f64) {
        while let Some(&(t, k, b)) = self.input.last().filter(|&&(t, _, _)| t <= target) {
            self.input.pop();
            g.advance_to(t);
            // pressed together, left counts as the later
            let was = std::mem::replace(&mut self.held, k);
            let changes = [
                (Key::Right, was.right, k.right),
                (Key::Left, was.left, k.left),
                (Key::Up, was.up, k.up),
                (Key::Down, was.down, k.down),
                (Key::Select, was.select, k.select),
                (Key::Quit, was.quit, k.quit),
                (Key::Clear, was.clear, k.clear),
            ];
            for (key, before, now) in changes {
                if before != now {
                    g.key(key, now);
                }
            }
            let was = std::mem::replace(&mut self.buttons, b);
            let changes = [(Button::Right, was.right, b.right), (Button::Middle, was.middle, b.middle), (Button::Left, was.left, b.left)];
            for (button, before, now) in changes {
                if before != now {
                    g.button(button, now);
                }
            }
        }
    }

    pub fn trace(&self, g: &Game) {
        if self.trace {
            let w = g.world();
            println!("trace {:.4} {:.4} {} {} {:.3}", w.t(), w.player().angle, w.player_side(), w.alive(), w.view().pulse);
        }
    }

    /// Whether any screenshots were asked for.
    pub fn wants_shots(&self) -> bool {
        !self.shots.is_empty()
    }

    /// Whether a screenshot is due by time `t`.
    pub fn shot_due(&self, t: f64) -> bool {
        self.shots.last().is_some_and(|&s| t >= s)
    }

    /// Saves the screenshot due by time `t`; returns true once the last has been taken.
    pub fn save_shot(&mut self, image: &Image, t: f64) -> bool {
        let Some(s) = self.shots.pop_if(|&mut s| t >= s) else { return false };
        let f = std::fs::File::create(format!("{}/{s:06.0}.png", self.shot_dir)).unwrap();
        let mut enc = png::Encoder::new(f, image.width, image.height);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&image.rgba).unwrap();
        self.shots.is_empty()
    }
}
