//! The 2D interface, laid out as the original's desktop version in a 480-unit-high space. Things
//! the original anchored to the right edge of its 768-wide screen are anchored to the window's.
//! Its triangles and text are built into buffers kept from frame to frame.

use std::fmt::{self, Write};

use crate::game::{EXTRAS, GAME_OVER_ZOOM, Game, Held, Menu, OPTIONS, Unlock};
use crate::pack::{Announce, Colour, MenuColour, Rgb};
use crate::save::{MAX_VOLUME, SPEEDS};
use crate::text::{Font, Size, TextItem};
use crate::world::{self, Colours, Scene, slot};

/// The slant of the original's panels: sin(0.5), horizontal run per unit of height.
fn slant() -> f64 {
    0.5f64.sin()
}
pub const GUI_H: f64 = 480.0;

const PROMPT_BACK: &str = "ESC - RETURN TO MENU";
const PROMPT_START: &str = "PRESS SPACE TO START";
const PROMPT_STAGE_SELECT: &str = "ESC - STAGE SELECT";
const PROMPT_MOVE: [&str; 2] = ["PRESS LEFT AND RIGHT KEYS", "TO MOVE AROUND THE HEXAGON"];
const PROMPT_CONTINUE: &str = "PRESS SPACE TO CONTINUE";
const PROMPT_RETRY: &str = "PRESS SPACE TO RETRY";
const PROMPT_QUIT: &str = "ESC - QUIT";
const PROMPT_CANCEL: &str = "ESC - CANCEL";
const PROMPT_CONFIRM: &str = "PRESS SPACE TO CONFIRM";
const PROMPT_SELECT: &str = "PRESS SPACE TO SELECT";

/// Times are shown as seconds and ticks.
const TPS: i64 = world::TICK_RATE as i64;

const WHITE: Rgb = [255.0; 3];
const GREY: Rgb = [164.0; 3];

fn grey(v: f64) -> Rgb {
    [v; 3]
}

/// Number of decimal digits.
fn digits(n: i64) -> usize {
    n.unsigned_abs().checked_ilog10().map_or(1, |d| d as usize + 1)
}

#[derive(Clone, Copy)]
enum Col {
    Black,
    Slot(usize),
}

/// Where text goes: starting or ending at x, or centred on the screen offset by x.
#[derive(Clone, Copy)]
enum At {
    Left(f64),
    Right(f64),
    Centred(f64),
}

use At::{Centred, Left, Right};

/// A triangle in GUI units, with its colour.
pub struct Tri {
    pub pts: [(f64, f64); 3],
    pub color: Rgb,
}

type Quad = [(f64, f64); 4];

pub struct Gui<'f> {
    font: &'f Font,
    /// Width of the window in GUI units.
    w: f64,
    pal: Colours,
    /// Panels drawn with a border (the original's skewflip).
    flip: bool,
    pub tris: Vec<Tri>,
    pub texts: Vec<TextItem>,
    /// The text of `texts`.
    pub arena: String,
}

impl<'f> Gui<'f> {
    pub fn new(font: &'f Font) -> Gui<'f> {
        Gui { font, w: 0.0, pal: [[0.0; 3]; 9], flip: false, tris: Vec::with_capacity(256), texts: Vec::with_capacity(64), arena: String::with_capacity(1024) }
    }

    fn cx(&self) -> f64 {
        self.w / 2.0
    }

    fn color(&self, c: Col) -> Rgb {
        match c {
            Col::Black => [0.0; 3],
            Col::Slot(s) => self.pal[s],
        }
    }

    // --- shapes ---

    /// A quadrilateral, its corners in order around it.
    fn quad(&mut self, [a, b, c, d]: Quad, col: Col) {
        let color = self.color(col);
        self.tris.push(Tri { pts: [a, b, d], color });
        self.tris.push(Tri { pts: [b, c, d], color });
    }

    /// A black panel.
    fn poly(&mut self, q: Quad) {
        self.quad(q, Col::Black);
    }

    fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, c: Col) {
        self.quad([(x1 - 1.0, y1 - 1.0), (x1 - 1.0, y2 + 1.0), (x2 + 1.0, y2 + 1.0), (x2 + 1.0, y2 - 1.0)], c);
    }

    fn skew_left(&mut self, x1: f64, y1: f64, x2: f64, y2: f64) {
        if self.flip {
            let t = (y2 - y1 + 4.0) * slant();
            self.quad([(x1 - 2.0 - t, y1 - 2.0), (x1 - 2.0, y2 + 2.0), (x2 + 2.0 + t, y2 + 2.0), (x2 + 2.0, y1 - 2.0)], Col::Slot(slot::HIGHLIGHT));
        }
        let t = (y2 - y1) * slant();
        self.poly([(x1 - t, y1), (x1, y2), (x2 + t, y2), (x2, y1)]);
    }

    fn skew_center(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, skew: f64) {
        if self.flip {
            let t = (y2 - y1 + 4.0) * slant() * skew;
            self.quad([(x1 - 2.0, y1 - 2.0), (x1 - 2.0 - t, y2 + 2.0), (x2 + 2.0 + t, y2 + 2.0), (x2 + 2.0, y1 - 2.0)], Col::Slot(slot::HIGHLIGHT));
        }
        let t = (y2 - y1) * slant() * skew;
        self.poly([(x1, y1), (x1 - t, y2), (x2 + t, y2), (x2, y1)]);
    }

    fn button(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, skew: f64) {
        let t = (y2 - y1 + 8.0) * slant() * skew;
        self.quad([(x1 - 6.0, y1 - 4.0), (x1 - 6.0 - t, y2 + 4.0), (x2 + 6.0 + t, y2 + 4.0), (x2 + 6.0, y1 - 4.0)], Col::Slot(slot::HIGHLIGHT));
        let t = (y2 - y1) * slant() * skew;
        self.quad([(x1, y1), (x1 - t, y2), (x2 + t, y2), (x2, y1)], Col::Slot(slot::BUTTON));
    }

    /// A button in the top right corner, around `prompt`.
    fn corner_button(&mut self, prompt: &str, slide: f64) {
        let x1 = self.w - self.width(prompt, Size::Normal) - 20.0 - 10.0;
        self.button(x1, -slide, self.w + 30.0, 30.0 - slide, -1.0);
    }

    fn arrow_button(&mut self, x: f64, y: f64, selected: bool, right: bool) {
        // drawn as the left button, mirrored about the button's centre for the right one
        let m = |px: f64| if right { 2.0 * x + 80.0 - px } else { px };
        let q = |g: &mut Self, p: Quad, c: Col| {
            g.quad(p.map(|(x, y)| (m(x), y)), c);
        };
        let (mut x, mut y) = (x, y);
        let mut temp = x - 5.0 + 80.0;
        let mut sl = y + 8.0 + 80.0;
        let t = (sl - (y + 8.0)) * slant();
        q(self, [(x - 5.0 - t, y + 8.0), (x - 5.0, sl), (temp + t, sl), (temp, y + 8.0)], Col::Black);
        if selected {
            x -= 3.0;
            y += 6.0;
            sl -= 2.0;
            temp += 2.0;
        } else {
            sl -= 8.0;
            temp += 5.0;
        }
        let t = (sl - y) * slant();
        q(self, [(x - t, y), (x, sl), (temp + t, sl), (temp, y)], Col::Slot(slot::HIGHLIGHT));
        x += 4.0;
        y += 4.0;
        sl -= 4.0;
        temp -= 4.0;
        let t = (sl - y) * slant();
        q(self, [(x - t, y), (x, sl), (temp + t, sl), (temp, y)], Col::Slot(slot::BUTTON));
        x += 24.0;
        temp -= 8.0;
        y += 20.0;
        sl -= 20.0;
        let t = (sl - y) * slant();
        q(self, [(x - t, y), (x, sl), (temp + t, sl), (temp, y)], Col::Black);
        x += 8.0;
        y -= 12.0;
        sl += 12.0;
        let t = (sl - y) * slant();
        q(self, [(x - t, y), (x, sl), (x - 32.0, (y + sl) / 2.0), (x - t, y)], Col::Black);
    }

    fn arrows(&mut self, keys: Held) {
        self.arrow_button(80.0, GUI_H - 150.0, keys.left, false);
        self.arrow_button(self.w - 160.0, GUI_H - 150.0, keys.right, true);
    }

    /// A rank's icon: a polygon with rank + 1 sides (a dot, then a line, then a triangle...).
    fn rank_icon(&mut self, x: f64, y: f64, rank: usize, ang: f64, size: f64) {
        let p = |a: f64, r: f64| {
            let (s, c) = a.to_radians().sin_cos();
            (s * r + x, c * r + y)
        };
        let t = rank + 1;
        let edge = |g: &mut Self, a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)| {
            g.quad([a, c, d, b], Col::Slot(slot::HIGHLIGHT));
        };
        match t {
            1 => {
                let q = [0.0, 90.0, 180.0, 270.0].map(|d| p(ang + d, size / 4.0));
                self.quad(q, Col::Slot(slot::HIGHLIGHT));
            }
            2 => {
                for i in 0..2 {
                    let a = ang + 180.0 * i as f64;
                    edge(self, p(a, size), p(a + 180.0, size), p(a + 5.0, size), p(a + 185.0, size));
                }
            }
            _ => {
                let step = 360.0 / t as f64;
                for i in 0..t {
                    let a = ang + step * i as f64;
                    edge(self, p(a, size), p(a + step, size), p(a, size + 5.0), p(a + step, size + 5.0));
                }
            }
        }
    }

    // --- text ---

    fn width(&self, s: &str, size: Size) -> f64 {
        self.font.width(s, size)
    }

    fn text(&mut self, at: At, y: f64, s: impl fmt::Display, rgb: Rgb, size: Size) {
        let start = self.arena.len();
        let _ = write!(self.arena, "{s}");
        let t = &self.arena[start..];
        let width = self.font.width(t, size);
        let x = match at {
            At::Left(x) => x,
            At::Right(x) => x - width,
            At::Centred(dx) => self.cx() - width / 2.0 + dx,
        };
        // the original's baselines: small text by an X's height, big text by its own
        let (pad, height) = match size {
            Size::Small => (2.0, self.font.height("X", size)),
            Size::Normal => (4.0, self.font.height("X", size)),
            Size::Big => (5.0, self.font.height(t, size)),
            Size::Heading | Size::Title => (4.0, self.font.height(t, size)),
        };
        let color = rgb.map(|v| v.clamp(0.0, 255.0));
        self.texts.push(TextItem { text: start..self.arena.len(), size, x, baseline: y + pad + height, color });
    }

    fn print(&mut self, at: At, y: f64, s: impl fmt::Display, rgb: Rgb) {
        self.text(at, y, s, rgb, Size::Normal);
    }

    /// An FPS counter in the bottom left corner, on a black panel.
    pub fn fps(&mut self, fps: f64) {
        let s = format!("{fps:.0} FPS");
        let r = self.width(&s, Size::Small) + 6.0;
        // Small text's baseline is 2 below y plus an X's height; leave 2 more under it
        let top = GUI_H - 6.0 - self.font.height("X", Size::Small);
        self.poly([(0.0, top), (0.0, GUI_H), (r + (GUI_H - top) * slant(), GUI_H), (r, top)]);
        self.text(Left(3.0), top + 2.0, s, WHITE, Size::Small);
    }

    /// Lays out the interface for this frame, `w` GUI units wide.
    pub fn build(&mut self, g: &Game, w: f64, pal: Colours) {
        self.w = w;
        self.pal = pal;
        self.flip = false;
        self.tris.clear();
        self.texts.clear();
        self.arena.clear();
        let world = g.world();
        if world.view().flash > 0.0 {
            return;
        }
        let pack = g.pack();
        // the level whose panels these are
        let styled = match world.scene() {
            Scene::StageSelect => pack.slot(world.slot()).filter(|&li| g.unlocked(li)),
            Scene::Run => Some(world.run_level()),
            Scene::Title => None,
        }
        .map(|li| &pack.levels[li]);
        let (on_button, border) = match world.scene() {
            Scene::StageSelect => styled.and_then(|l| l.menu.as_ref()).map_or((WHITE, false), |m| (m.button_text, m.border)),
            Scene::Run => styled.map_or((WHITE, false), |l| (l.game_over.button_text, l.game_over.border)),
            Scene::Title => (WHITE, false),
        };
        self.flip = border;

        let in_play = world.over() <= 1 || world.view().zoom < GAME_OVER_ZOOM;
        match world.scene() {
            Scene::Run if in_play => {
                self.flip = false;
                if world.ending() {
                    // no interface while the ending plays
                } else if world.tutorial() {
                    self.tutorial(g);
                } else {
                    self.hud(g);
                }
            }
            Scene::Run => self.game_over(g, on_button),
            Scene::StageSelect => self.stage_select(g, on_button),
            Scene::Title => {
                self.flip = false;
                match g.menu {
                    Menu::Main => self.title(g),
                    Menu::Options => self.options(g),
                    Menu::Extras => self.extras(g),
                    Menu::Credits => self.credits(g),
                    Menu::Delete => self.delete(g),
                }
            }
        }
    }

    fn tutorial(&mut self, g: &Game) {
        let t = &g.pack().tutorial;
        let counter = |c: Option<_>| c.map(|c| g.world().tutorial_counter(c));
        if counter(t.step) == Some(1.0) {
            let slide = counter(t.slide).unwrap_or(0.0) * 8.0;
            let temp = self.width(PROMPT_MOVE[1], Size::Normal) / 2.0 + 20.0;
            let cx = self.cx();
            self.skew_center(cx - temp, 25.0 - slide, cx + temp, 86.0 - slide, 1.0);
            self.print(Centred(0.0), 30.0 - slide, PROMPT_MOVE[0], WHITE);
            self.print(Centred(0.0), 55.0 - slide, PROMPT_MOVE[1], WHITE);
        }
        self.arrows(g.keys());
    }

    fn hud(&mut self, g: &Game) {
        let pack = g.pack();
        let (world, hud) = (g.world(), &g.hud);
        let c = pack.completing_rank;
        let (time, best) = (world.survival(), world.best());
        let w = self.w;

        // top left: rank, or a level up / new record flash
        if hud.levelupflash > 0.0 {
            let name = &pack.ranks[hud.levelreached].name;
            let temp = self.width(name, Size::Big) + 30.0;
            self.poly([(23.0 + temp, 0.0), (temp, 72.0), (0.0, 72.0), (0.0, 0.0)]);
            let col = if (hud.levelupflash / 8.0) as i32 % 2 == 0 { WHITE } else { GREY };
            self.print(Left(15.0), 4.0, "LEVEL UP", col);
            self.text(Left(15.0), 24.0, name, col, Size::Big);
        } else if hud.newbestflash > 0.0 {
            let sl = 30.0 + self.width("NEW RECORD", Size::Big);
            self.poly([(23.0 + sl, 0.0), (sl, 50.0), (0.0, 50.0), (0.0, 0.0)]);
            let col = if (hud.newbestflash / 8.0) as i32 % 2 == 0 { WHITE } else { GREY };
            self.text(Left(15.0), 4.0, "NEW RECORD", col, Size::Big);
        } else {
            let mut bar = hud.rankbar.clone();
            let f = hud.rankupflash;
            let start = self.texts.len();
            if hud.levelreached < c {
                self.print(Left(15.0 + f), 4.0, &pack.ranks[world.rank()].name, WHITE);
            } else if best == time {
                bar = 0.0..time as f64;
                self.print(Left(15.0 + f), 4.0, "NEW RECORD", WHITE);
            } else {
                self.print(Left(15.0 + f), 4.0, format_args!("BEST: {:02}:{:02}", best / TPS, best % TPS), WHITE);
            }
            let sl = self.font.width(&self.arena[self.texts[start].text.clone()], Size::Normal);
            self.poly([(30.0 + sl + 23.0 + f, 0.0), (30.0 + sl + f, 32.0), (0.0, 32.0), (0.0, 0.0)]);
            self.line(18.0, 27.0, sl + 16.0, 27.0, Col::Slot(slot::WEDGE));
            let p = ((time as f64 - bar.start) / (bar.end - bar.start)).clamp(0.0, 1.0);
            let p = if p.is_finite() { p } else { 1.0 };
            self.line(18.0, 27.0, p * (sl - 2.0) + 18.0, 27.0, Col::Slot(slot::HIGHLIGHT));
            // the panel goes under its text
            let label = self.texts.remove(start);
            self.texts.push(label);
        }

        // top right: the time
        let temp = (digits(time / TPS) as f64 - 1.0) * 35.0;
        let badge = pack.levels[world.run_level()].menu.as_ref().and_then(|m| m.badge.as_deref());
        let (a, b) = if badge.is_some() { (334.0, 311.0) } else { (234.0, 211.0) };
        self.poly([(w - a - temp, 0.0), (w - b - temp, 32.0), (w, 32.0), (w, 0.0)]);
        self.poly([(w - 129.0 - temp, 0.0), (w - 101.0 - temp, 52.0), (w, 52.0), (w, 0.0)]);
        self.print(Right(w - 123.0 - temp), 4.0, badge.unwrap_or("TIME"), WHITE);
        self.text(Right(w - 59.0), 4.0, time / TPS, grey(215.0), Size::Big);
        self.print(Left(w - 55.0), 22.0, format_args!(":{:02}", time % TPS), grey(215.0));
    }

    fn menu_colour(&self, mc: &MenuColour, g: &Game) -> Rgb {
        match &mc.frames[(g.world().t() / mc.ticks) as usize % mc.frames.len()] {
            Colour::Rgb(ch) => ch.map(|c| c.base + c.glow * g.world().view().glow),
            Colour::Slot { slot } => self.pal[*slot],
        }
    }

    fn stage_select(&mut self, g: &Game, on_button: Rgb) {
        let pack = g.pack();
        let w = self.w;
        let cx = self.cx();
        self.corner_button(PROMPT_BACK, 0.0);
        let turning = g.world().turning();
        let k = g.keys();
        self.arrows(Held { left: k.left && !turning, right: k.right && !turning, ..k });
        self.skew_center(0.0, 75.0, w, 200.0, 1.0);

        let level = pack.slot(g.world().slot());
        match level.filter(|&li| g.unlocked(li)).and_then(|li| Some((li, pack.levels[li].menu.as_ref()?))) {
            Some((li, l)) => {
                if let Some(badge) = &l.badge {
                    let x = w - self.width(badge, Size::Normal) - 35.0;
                    let flip = std::mem::replace(&mut self.flip, false);
                    self.skew_left(x, 60.0, w, 105.0);
                    self.flip = flip;
                }
                let temp = self.width(PROMPT_START, Size::Normal) / 2.0 + 40.0;
                self.button(cx - temp, 215.0, cx + temp, 250.0, -1.0);

                let name_col = self.menu_colour(&l.colour, g);
                let best = g.best(li);
                self.text(Left(180.0), 85.0, &l.name, name_col, Size::Big);
                self.print(Left(200.0), 135.0, "DIFFICULTY:", GREY);
                self.print(Left(400.0), 135.0, &l.difficulty, WHITE);
                self.print(Left(200.0), 160.0, "BEST TIME:", GREY);
                self.print(Left(400.0), 160.0, format_args!("{:02}:{:02}", best / TPS, best % TPS), WHITE);
                if let Some(badge) = &l.badge {
                    self.print(Right(w - 10.0), 65.0, badge, name_col);
                }
                self.print(Centred(0.0), 220.0, PROMPT_START, on_button);
                self.print(Right(w - 10.0), 1.0, PROMPT_BACK, on_button);
            }
            None => {
                self.text(Centred(0.0), 85.0, "LOCKED", self.pal[7], Size::Big);
                if let Some(n) = level.and_then(|li| pack.levels[li].unlock)
                    && let Some(m) = &pack.levels[n].menu
                {
                    self.print(Centred(0.0), 150.0, format_args!("COMPLETE {} TO UNLOCK", m.name), WHITE);
                }
                self.print(Right(w - 10.0), 1.0, PROMPT_BACK, WHITE);
            }
        }
    }

    fn title(&mut self, g: &Game) {
        let cx = self.cx();
        let temp = self.width("STAGE SELECT XX", Size::Normal) / 2.0 + 20.0;
        self.button(cx - temp, 350.0, cx + temp, 390.0, -1.0);
        self.arrows(g.keys());
        self.corner_button(PROMPT_QUIT, 0.0);

        let t = &g.pack().title;
        if let Some(l) = t.first() {
            self.text(Centred(0.0), 124.0, format_args!("{l} "), WHITE, Size::Title);
        }
        if let Some(l) = t.get(1) {
            self.text(Centred(-10.0), 210.0, format_args!("   {l}"), WHITE, Size::Heading);
        }
        let item = ["START GAME", "OPTIONS", "EXTRAS", "CREDITS"][g.cursor.min(3)];
        self.print(Centred(0.0), 355.0, item, grey(225.0 - g.world().view().glow));
        self.print(Centred(0.0), GUI_H - 30.0, PROMPT_SELECT, WHITE);
        let w = self.w;
        self.print(Right(w - 10.0), 1.0, PROMPT_QUIT, WHITE);
    }

    fn options(&mut self, g: &Game) {
        let w = self.w;
        let cx = self.cx();
        self.corner_button(PROMPT_BACK, 0.0);
        let temp = self.width("CHANGE TO FULLSCREEN", Size::Normal) / 2.0 + 20.0;
        let y = 177.0 + g.cursor as f64 * 40.0;
        self.button(cx - temp, y, cx + temp, y + 30.0, 0.0);

        self.print(Right(w - 10.0), 1.0, PROMPT_BACK, WHITE);
        self.text(Centred(0.0), 90.0, "OPTIONS", WHITE, Size::Heading);
        let s = &g.save().settings;
        for i in 0..OPTIONS {
            let y = 180.0 + 40.0 * i as f64;
            match i {
                0 => self.print(Centred(0.0), y, if s.fullscreen { "CHANGE TO WINDOW" } else { "CHANGE TO FULLSCREEN" }, WHITE),
                1 => self.print(Centred(0.0), y, if s.vsync { "DISABLE VSYNC" } else { "ENABLE VSYNC" }, WHITE),
                2 => self.print(Centred(0.0), y, format_args!("MUSIC VOLUME: {} / {MAX_VOLUME}", s.music_volume), WHITE),
                3 => self.print(Centred(0.0), y, format_args!("SOUND VOLUME: {} / {MAX_VOLUME}", s.sound_volume), WHITE),
                _ => self.print(Centred(0.0), y, "DELETE RECORDS", WHITE),
            }
        }
    }

    fn extras(&mut self, g: &Game) {
        let w = self.w;
        let cx = self.cx();
        let s = &g.save().settings;
        let on = |b| if b { "ON" } else { "OFF" };
        let lines: [String; EXTRAS] = [
            format!("BLACK BARS: {}", on(s.black_bars)),
            format!("CHROMATIC ABERRATION: {}", on(s.aberration)),
            format!("BLOOM: {}", on(s.bloom)),
            match s.antialiasing {
                1 => "ANTIALIASING: OFF".into(),
                n => format!("ANTIALIASING: {n}X MSAA"),
            },
            format!("SHOW FPS: {}", on(s.show_fps)),
            match SPEEDS.iter().find(|&&(_, x)| x == g.speed()) {
                _ if g.speed_overridden() => format!("SPEED: {}X (COMMAND LINE)", g.speed()),
                Some((name, x)) => format!("SPEED: {name} ({x}X)"),
                None => format!("SPEED: {}X", g.speed()),
            },
        ];
        self.corner_button(PROMPT_BACK, 0.0);
        let temp = lines.iter().map(|l| self.width(l, Size::Normal)).fold(0.0, f64::max) / 2.0 + 20.0;
        let y = 177.0 + g.cursor as f64 * 40.0;
        self.button(cx - temp, y, cx + temp, y + 30.0, 0.0);

        self.print(Right(w - 10.0), 1.0, PROMPT_BACK, WHITE);
        self.text(Centred(0.0), 90.0, "EXTRAS", WHITE, Size::Heading);
        for (i, line) in lines.iter().enumerate() {
            self.print(Centred(0.0), 180.0 + 40.0 * i as f64, line, WHITE);
        }
    }

    fn credits(&mut self, g: &Game) {
        let tx = &g.pack().text.credits;
        let w = self.w;
        let cx = self.cx();
        let pages = g.credits_pages();
        self.arrows(g.keys());
        self.corner_button(PROMPT_BACK, 0.0);
        let rewatch = g.ending_seen() && g.page == pages - 1;

        self.text(Centred(0.0), GUI_H - 150.0, &tx.title, WHITE, Size::Big);
        self.print(Right(w - 10.0), 1.0, PROMPT_BACK, WHITE);
        self.print(Centred(0.0), GUI_H - 30.0, &tx.thanks, WHITE);
        if g.page == 0 {
            for (i, e) in tx.main.iter().enumerate() {
                let y = 40.0 + 70.0 * i as f64;
                self.print(Right(180.0), y + 18.0, &e.role, grey(196.0));
                self.text(Left(220.0), y, &e.name, WHITE, Size::Big);
                self.print(Left(250.0), y + 45.0, &e.site, grey(128.0));
            }
        } else if rewatch {
            self.print(Centred(0.0), 160.0, &tx.rewatch_ending, grey(196.0));
            self.print(Centred(0.0), 220.0, PROMPT_CONFIRM, WHITE);
        } else {
            self.print(Centred(0.0), 60.0, &tx.testers_heading, grey(196.0));
            let first = (g.page - 1) * 12;
            let count = tx.testers.len().saturating_sub(first).min(12);
            let half = count.div_ceil(2);
            // a short page sits half a row lower, as the original's second page does
            let y0 = 110.0 + if half < 6 { 17.5 } else { 0.0 };
            for (i, n) in tx.testers[first..first + count].iter().enumerate() {
                let y = y0 + 35.0 * (i % half) as f64;
                if i < half {
                    self.print(Right(cx - 50.0), y, n, WHITE);
                } else {
                    self.print(Left(cx - 10.0), y, n, WHITE);
                }
            }
        }
        self.print(Centred(0.0), GUI_H - 100.0, format_args!("PAGE {}/{}", g.page + 1, pages), GREY);
    }

    fn delete(&mut self, g: &Game) {
        let w = self.w;
        let cx = self.cx();
        self.corner_button(PROMPT_CANCEL, 0.0);
        let temp = self.width(PROMPT_CONFIRM, Size::Normal) / 2.0 + 20.0;
        self.button(cx - temp, 315.0, cx + temp, 350.0, -1.0);

        self.print(Right(w - 10.0), 1.0, PROMPT_CANCEL, WHITE);
        let v = if (g.world().view().glow / 16.0) as i32 % 2 == 0 { 255.0 } else { 200.0 };
        let col = [v, v, 0.0];
        self.text(Centred(0.0), 135.0, "WARNING", col, Size::Heading);
        self.print(Centred(0.0), 225.0, "THIS WILL DELETE YOUR PROGRESS", col);
        self.print(Centred(0.0), 275.0, "ARE YOU SURE?", col);
        self.print(Centred(0.0), 320.0, PROMPT_CONFIRM, WHITE);
    }

    fn game_over(&mut self, g: &Game, on_button: Rgb) {
        let pack = g.pack();
        let world = g.world();
        let w = self.w;
        let cx = self.cx();

        if g.unlock != Unlock::None {
            self.flip = false;
            self.skew_left(0.0, 175.0, w, 300.0);
            if g.unlock_ready() {
                let temp = self.width(PROMPT_CONTINUE, Size::Normal) / 2.0 + 20.0;
                self.button(cx - temp, 315.0, cx + temp, 350.0, -1.0);
            }
            let v = if (world.view().glow / 16.0) as i32 % 2 == 0 { 255.0 } else { 200.0 };
            let col = grey(v);
            let tx = &pack.text.completion;
            self.text(Centred(0.0), 180.0, &tx.heading, col, Size::Big);
            match g.announce {
                Some(Announce::NewHyper) => {
                    self.print(Centred(0.0), 225.0, &tx.level_complete, col);
                    self.print(Centred(0.0), 271.0, &tx.new_hyper, col);
                }
                Some(Announce::SidesComplete) => {
                    self.print(Centred(0.0), 225.0, &tx.level_complete, col);
                    let done = pack.levels.iter_enumerated().filter(|&(i, l)| l.completion.is_some() && g.completed(i)).count();
                    match tx.sides_complete.split_once("{completed}") {
                        Some((a, b)) => self.print(Centred(0.0), 271.0, format_args!("{a}{done}{b}"), col),
                        None => self.print(Centred(0.0), 271.0, &tx.sides_complete, col),
                    }
                }
                Some(Announce::GameComplete) => self.text(Centred(0.0), 250.0, &tx.game_complete, col, Size::Big),
                _ => {}
            }
            if g.unlock_ready() {
                let col = pack.levels[world.run_level()].game_over.continue_text;
                self.print(Centred(0.0), 320.0, PROMPT_CONTINUE, col);
            }
            return;
        }

        let s = g.hud.menuslide;
        let time = world.survival();
        let best = world.best();
        let c = pack.completing_rank;

        // panels
        self.skew_left(w - 337.0 + 60.0 * s, 157.0, w, 192.0);
        self.skew_left(60.0 * s + w - 94.0 - 35.0 * digits(time / TPS) as f64, 157.0, w, 207.0);
        self.skew_left(w - 305.0 + 60.0 * s, 247.0, w, 282.0);
        if best != time {
            self.skew_left(60.0 * s + w - 64.0 - 35.0 * digits(best / TPS) as f64, 247.0, w, 297.0);
        }
        self.skew_left(-60.0 * s, 157.0, 340.0 - 60.0 * s, 300.0);
        let shown = g.hud.levelreached.min(c);
        let label = self.texts.len();
        self.text(Left(0.0), 162.0, format_args!("LEVEL {}", shown + 1), WHITE, Size::Big);
        let sl = self.font.width(&self.arena[self.texts[label].text.clone()], Size::Big);
        let temp = 130.0 - sl / 2.0;
        self.texts[label].x = temp - 60.0 * s;
        self.rank_icon(temp + sl + 73.0 - 60.0 * s, 195.0, shown, world.view().spin, 25.0);
        let temp2 = self.width(PROMPT_RETRY, Size::Normal) / 2.0 + 20.0;
        self.skew_center(cx - temp2, GUI_H - 35.0 + s * 15.0, cx + temp2, GUI_H + s * 15.0, 1.0);
        self.corner_button(PROMPT_STAGE_SELECT, s * 15.0);

        // text
        self.print(Left(w - 332.0 + 60.0 * s), 162.0, "LAST", WHITE);
        self.text(Right(w - 89.0 + 60.0 * s), 159.0, time / TPS, grey(215.0), Size::Big);
        self.print(Left(w - 85.0 + 60.0 * s), 177.0, format_args!(":{:02}", time % TPS), grey(215.0));
        if best == time {
            self.print(Left(w - 280.0 + 60.0 * s), 252.0, "NEW RECORD", WHITE);
        } else {
            self.print(Left(w - 300.0 + 60.0 * s), 252.0, "BEST", WHITE);
            self.text(Right(w - 59.0 + 60.0 * s), 249.0, best / TPS, grey(215.0), Size::Big);
            self.print(Left(w - 55.0 + 60.0 * s), 267.0, format_args!(":{:02}", best % TPS), grey(215.0));
        }
        self.print(Right(temp + sl - 60.0 * s), 207.0, &pack.ranks[shown].name, WHITE);
        if g.hud.levelreached < c {
            self.print(Left(140.0 - 60.0 * s), 242.0, "NEXT LEVEL AT", WHITE);
            self.print(Left(200.0 - 60.0 * s), 267.0, format_args!("{} SECONDS", pack.ranks[g.hud.levelreached + 1].at / TPS), WHITE);
        } else {
            self.print(Left(135.0 - 60.0 * s), 254.0, "STAGE COMPLETE", WHITE);
        }
        self.print(Centred(0.0), GUI_H - 30.0 + s * 15.0, PROMPT_RETRY, WHITE);
        let temp3 = self.width(PROMPT_STAGE_SELECT, Size::Normal) + 10.0;
        self.print(Left(w - temp3), 1.0 - 15.0 * s, PROMPT_STAGE_SELECT, on_button);
    }
}
