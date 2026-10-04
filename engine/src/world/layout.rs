//! Laying waves out ahead of the spawn line.
//!
//! The original places each wave at a spawn line 4000 units out when its wave timer runs out. As
//! freezes stop the walls and the timer alike, and speed only changes as waves are placed, each
//! wave lands a predictable distance behind the previous one. So the director can run as soon as
//! the stream needs more track, placing waves further out, off-screen, while everything else a
//! wave does (speed, rotation, pulses and so on) waits until it reaches the spawn line, which is
//! when the original made it. The director sees the state its wave will arrive into: the time,
//! the shape after the shape changes already laid out, and the speed earlier waves leave behind.
//!
//! The level's own progression is a timeline, so its freezes and level switches are known ahead
//! too: a freeze delays everything after it, and at a level switch the next level's director takes
//! over. Only the player can change what happens (by being hit), which drops what was laid out.

use std::collections::VecDeque;

use super::{
    Burst, Finale, MAX_SIDES, MORPH_RATE, MORPH_SETTLE, Morph, Placed, Player, Run, Scene, View, Wall, WallKind, World, place,
    wave_delay,
};
use crate::ids::{Id, LevelId};
use crate::pack::{EventKind, Pack};
use rand::rngs::SmallRng;

use crate::script::{self, Counters, Effect, Flow, Host, Val, Var};

/// How far ahead of the spawn line the stream is laid out.
const LAYOUT_LEAD: f64 = 8000.0;
/// Tolerance for laid-out distances and times, which accumulate rounding over many waves.
const TOLERANCE: f64 = 1e-6;
/// How long a shape change holds the walls: the morph, then its settling. A grow event at the full
/// number of sides doesn't morph, but holds them for the settling.
const MORPH_HOLD: f64 = 1.0 / MORPH_RATE + MORPH_SETTLE;
const NO_MORPH_HOLD: f64 = MORPH_SETTLE;

/// Where the director is up to.
#[derive(Clone, Copy)]
struct Director {
    level: LevelId,
    time_shift: i64,
    counters: Counters,
    wave: i64,
    speed: f64,
    /// How far the stream moves before the next wave reaches the spawn line.
    cursor: f64,
}

/// A wave laid out ahead, not yet at the spawn line.
struct Pending {
    /// How far the stream moves before it reaches the spawn line.
    lead: f64,
    /// Its walls and events, at their distances from the centre as of then.
    walls: Vec<Wall>,
    /// What else it does then.
    effects: Vec<&'static Effect>,
    speed: Option<f64>,
    speed_ramp: Option<f64>,
    /// The director's state before it was laid out.
    before: Director,
}

/// The wave the director is laying out, and the state it will arrive into.
struct Current {
    pending: Pending,
    /// Ticks from it to the next wave, as the original's wave timer.
    timer: f64,
    /// Extra distance before the next wave: a pattern held until its shape change.
    hold: f64,
    at: Forecast,
}

/// Something in the level's timeline that changes the stream, in ticks from now.
#[derive(Clone, Copy)]
enum Planned {
    Freeze(f64),
    Switch { level: LevelId, time_offset: i64 },
}

/// Something on its way in that changes the stream when it gets there.
#[derive(Clone, Copy)]
enum Mark {
    Speed(f64),
    Shape(EventKind),
}

#[derive(Clone, Copy)]
struct MarkAt {
    /// How far the stream moves before it gets there.
    at: f64,
    /// The lead of the wave it belongs to (0 for walls already placed).
    wave: f64,
    mark: Mark,
}

/// How far to forecast.
#[derive(Clone, Copy)]
enum Until {
    /// Until the stream has moved this far.
    Moved(f64),
    /// Until this many ticks from now.
    Ticks(f64),
}

/// Where the stream will be: when and how far it will have moved, and the shape then.
#[derive(Clone, Copy)]
struct Forecast {
    ticks: f64,
    pos: f64,
    sides: i32,
    sides_after_morph: i32,
    morphing: bool,
}

/// Laying out ahead.
pub(super) struct Lay {
    active: bool,
    dir: Director,
    pending: VecDeque<Pending>,
    /// Laid-out waves' storage, for reuse.
    spare: Vec<Pending>,
    /// Scratch space for planning ahead.
    plan: Vec<(f64, Planned)>,
    marks: Vec<MarkAt>,
}

impl Default for Lay {
    fn default() -> Lay {
        let dir = Director { level: LevelId::new(0), time_shift: 0, counters: Counters::ZERO, wave: 0, speed: 0.0, cursor: 0.0 };
        Lay {
            active: false,
            dir,
            pending: VecDeque::with_capacity(16),
            spare: Vec::with_capacity(16),
            plan: Vec::with_capacity(8),
            marks: Vec::with_capacity(64),
        }
    }
}

impl Lay {
    pub(super) fn active(&self) -> bool {
        self.active
    }

    /// Drops everything laid out.
    pub(super) fn stop(&mut self) {
        self.active = false;
        self.spare.extend(self.pending.drain(..));
    }

    /// Whether the waves laid out ahead are already the given level's.
    pub(super) fn planned_for(&self, level: LevelId) -> bool {
        self.active && self.dir.level == level
    }

    pub(super) fn next_lead(&self) -> Option<f64> {
        if self.active { self.pending.front().map(|p| p.lead) } else { None }
    }

    /// The stream moves.
    pub(super) fn travel(&mut self, d: f64) {
        if self.active {
            for p in &mut self.pending {
                p.lead -= d;
            }
            self.dir.cursor -= d;
        }
    }

    fn new_pending(&mut self) -> Pending {
        let mut p = self.spare.pop().unwrap_or_else(|| Pending {
            lead: 0.0,
            walls: Vec::with_capacity(32),
            effects: Vec::with_capacity(8),
            speed: None,
            speed_ramp: None,
            before: self.dir,
        });
        p.walls.clear();
        p.effects.clear();
        p.lead = self.dir.cursor;
        p.speed = None;
        p.speed_ramp = None;
        p.before = self.dir;
        p
    }
}

/// The director, laying out a wave ahead: it sees the state the wave will arrive into, and what it
/// does waits for the spawn line.
struct Laying<'a> {
    pack: &'static Pack,
    rng: &'a mut SmallRng,
    dir: &'a mut Director,
    cur: &'a mut Current,
    run: &'a Run,
    view: &'a View,
    player: &'a Player,
    palette: Val,
    palette_fading: bool,
}

impl Host for Laying<'_> {
    fn get(&self, var: Var) -> Val {
        let at = &self.cur.at;
        let ticks = at.ticks.round() as i64;
        match var {
            Var::Wave => Val::Int(self.dir.wave),
            // the finale's director keeps to its own clock
            Var::Time if self.run.finale == Finale::On => Val::Int(self.run.survival + self.dir.time_shift),
            Var::Time => Val::Int(self.run.survival + ticks + self.dir.time_shift),
            Var::Elapsed => Val::Int(self.run.elapsed + ticks),
            Var::Speed => Val::Float(self.dir.speed),
            Var::Sides => Val::Int(at.sides as i64),
            Var::SidesAfterMorph => Val::Int(at.sides_after_morph as i64),
            Var::Morphing => Val::from_bool(at.morphing),
            Var::Counter(i) => self.dir.counters[i],
            // as they are now
            Var::Palette => self.palette,
            Var::PaletteFading => Val::from_bool(self.palette_fading),
            Var::SpinBurstActive => Val::from_bool(self.view.burst != Burst::None),
            Var::Left => Val::from_bool(self.player.left),
            Var::Right => Val::from_bool(self.player.right),
        }
    }

    fn set(&mut self, var: Var, v: Val) {
        match var {
            Var::Speed => {
                self.dir.speed = v.f();
                self.cur.pending.speed = Some(v.f());
            }
            Var::Counter(i) => self.dir.counters[i] = v,
            // the pack is checked for other assignments
            _ => {}
        }
    }

    fn rng(&mut self) -> &mut SmallRng {
        self.rng
    }

    fn act(&mut self, e: &'static Effect) -> Flow {
        let cur = &mut *self.cur;
        match e {
            Effect::Pattern(id) => {
                let first = cur.pending.walls.len();
                let mut placed = Placed::default();
                let walls = &mut cur.pending.walls;
                place(&self.pack.patterns[*id], None, self.rng, &mut placed, &mut |w| walls.push(w));
                if let Some(d) = placed.delay_distance {
                    cur.timer = wave_delay(d, self.dir.speed);
                }
                if placed.hold_until_morphed {
                    // the next wave waits until this pattern's shape change reaches the centre
                    cur.hold = cur.pending.walls[first..]
                        .iter()
                        .find(|w| matches!(w.kind, WallKind::Event(EventKind::Shrink | EventKind::Grow)))
                        .map_or(0.0, |w| w.dist);
                }
                if placed.speed_ramp.is_some() {
                    cur.pending.speed_ramp = placed.speed_ramp;
                }
            }
            Effect::Delay(n) => cur.timer = *n,
            _ => cur.pending.effects.push(e),
        }
        Flow::Continue(())
    }
}

impl World {
    fn can_lay_out(&self) -> bool {
        self.scene == Scene::Run
            && self.alive()
            && !self.run.hit_pending
            && !self.run.tutorial
            && !self.ending()
            && self.stream.speedramp > 1.0
            && self.stream.speed > 0.0
            && !self.stream.pausewaves
    }

    /// Run at the end of each tick: start, continue or stop laying out ahead.
    pub(super) fn sync_layout(&mut self) {
        if !self.can_lay_out() {
            self.unlay();
            return;
        }
        if !self.lay.active {
            // the wave timer, as stream distance
            self.lay.active = true;
            self.lay.dir = Director {
                level: self.run.logic,
                time_shift: self.run.time_shift,
                counters: self.run.counters,
                wave: self.run.wave,
                speed: self.stream.speed,
                cursor: (self.stream.wavetimer - TOLERANCE).ceil().max(1.0) * self.stream.speed,
            };
        }
        let pack = self.pack;
        self.plan();
        while self.lay.dir.cursor < LAYOUT_LEAD {
            let at = self.forecast(Until::Moved(self.lay.dir.cursor));
            // a level switch first: the next level's director takes over the tick after it
            let switch = self.lay.plan.iter().find_map(|&(t, p)| match p {
                Planned::Switch { level, time_offset } if t <= at.ticks + TOLERANCE => Some((t, level, time_offset)),
                _ => None,
            });
            if let Some((t, level, time_offset)) = switch
                && self.lay.dir.level != level
            {
                let l = &pack.levels[level];
                let pos = self.forecast(Until::Ticks(t)).pos;
                let d = &mut self.lay.dir;
                d.level = level;
                d.time_shift += time_offset;
                d.counters = l.counters;
                d.wave = l.start_wave;
                if let Some(s) = l.start_speed {
                    d.speed = s;
                }
                // its first wave is due the tick after
                d.cursor = pos + d.speed;
                continue;
            }
            let mut cur = Current { pending: self.lay.new_pending(), timer: 0.0, hold: 0.0, at };
            let finale = self.run.finale == Finale::On;
            let director = if finale { &pack.finale.on_wave } else { &pack.directors[pack.levels[self.lay.dir.level].director] };
            let mut laying = Laying {
                pack,
                rng: &mut self.rng,
                dir: &mut self.lay.dir,
                cur: &mut cur,
                run: &self.run,
                view: &self.view,
                player: &self.player,
                palette: Val::Int(self.pal.id().0 as i64),
                palette_fading: self.pal.fading(),
            };
            let _ = script::run(director, &mut laying);
            if !finale {
                self.lay.dir.wave += 1;
            }
            let ticks = (cur.timer - TOLERANCE).ceil().max(1.0);
            self.lay.dir.cursor = cur.pending.lead + cur.hold + ticks * self.lay.dir.speed;
            self.lay.pending.push_back(cur.pending);
        }
    }

    /// Drops the waves laid out ahead and goes back to the wave timer, as of before them.
    pub(super) fn unlay(&mut self) {
        if !self.lay.active {
            return;
        }
        if let Some(p) = self.lay.pending.front() {
            self.lay.dir = p.before;
        }
        self.lay.stop();
        self.run.counters = self.lay.dir.counters;
        self.run.wave = self.lay.dir.wave;
        self.stream.wavetimer = self.lay.dir.cursor / self.stream.speed;
    }

    /// The level timeline's freezes and level switches from now on, in ticks from now.
    fn plan(&mut self) {
        let pack = self.pack;
        self.lay.plan.clear();
        if self.run.finale == Finale::On {
            return;
        }
        let (mut level, mut shift, mut from) = (self.run.logic, self.run.time_shift, 0.0);
        // follow level switches a little way, enough for anything laid out ahead
        for _ in 0..4 {
            let now = (self.run.survival + shift) as f64;
            let mut next = None;
            for (at, acts) in &pack.levels[level].timeline {
                let t = *at as f64 - now;
                if t <= from {
                    continue;
                }
                for a in acts {
                    match a {
                        script::Action::Do(Effect::FreezeWalls(n)) => self.lay.plan.push((t, Planned::Freeze(*n))),
                        script::Action::Do(Effect::SwitchLevel { level: l, time_offset }) => {
                            self.lay.plan.push((t, Planned::Switch { level: *l, time_offset: *time_offset }));
                            next = Some((*l, shift + time_offset, t));
                        }
                        _ => {}
                    }
                }
                if next.is_some() {
                    break;
                }
            }
            match next {
                Some(n) => (level, shift, from) = n,
                None => break,
            }
        }
    }

    /// Follows the stream forward until `until`.
    fn forecast(&mut self, until: Until) -> Forecast {
        let mut sides = self.shape.sides;
        // walls held by a shape change in progress
        let mut t = match self.shape.morph {
            Morph::Shrink => {
                sides -= 1;
                (1.0 - self.shape.morph_amount) / MORPH_RATE + MORPH_SETTLE
            }
            Morph::Grow => self.shape.morph_amount / MORPH_RATE + MORPH_SETTLE,
            _ => self.stream.freeze,
        };
        let mut marks = std::mem::take(&mut self.lay.marks);
        marks.clear();
        let shape = |w: &Wall| match w.kind {
            WallKind::Event(k @ (EventKind::Shrink | EventKind::Grow)) if w.active => Some(Mark::Shape(k)),
            _ => None,
        };
        marks.extend(self.walls.iter().filter_map(|w| Some(MarkAt { at: w.dist, wave: 0.0, mark: shape(w)? })));
        for p in &self.lay.pending {
            if let Some(s) = p.speed {
                marks.push(MarkAt { at: p.lead, wave: p.lead, mark: Mark::Speed(s) });
            }
            marks.extend(p.walls.iter().filter_map(|w| Some(MarkAt { at: p.lead + w.dist, wave: p.lead, mark: shape(w)? })));
        }
        marks.sort_by(|a, b| a.at.total_cmp(&b.at));

        let (lead, until) = match until {
            Until::Moved(d) => (d, f64::INFINITY),
            Until::Ticks(t) => (f64::INFINITY, t),
        };
        let (mut v, mut pos) = (self.stream.speed, 0.0);
        let mut cleared = f64::NEG_INFINITY;
        let (mut mi, mut pi) = (0, 0);
        loop {
            // the stream moves to the next mark, or the end, unless the timeline gets there first
            let (x, mark) = match marks.get(mi) {
                Some(m) if m.at < lead - TOLERANCE => (m.at, Some(*m)),
                _ => (lead, None),
            };
            let reach = t + (x - pos) / v;
            if let Some(&(f, p)) = self.lay.plan.get(pi)
                && f <= reach.min(until) + TOLERANCE
            {
                pos += (f - t).max(0.0) * v;
                t = t.max(f);
                pi += 1;
                match p {
                    Planned::Freeze(n) => t += n,
                    Planned::Switch { level, .. } => {
                        // a new level: all walls cleared, the full shape, its own speed
                        sides = MAX_SIDES;
                        cleared = pos;
                        if let Some(s) = self.pack.levels[level].start_speed {
                            v = s;
                        }
                    }
                }
                continue;
            }
            if let Some(m) = mark
                && m.wave <= cleared + TOLERANCE
            {
                mi += 1;
                continue;
            }
            if until < reach {
                pos += (until - t).max(0.0) * v;
                t = until;
                break;
            }
            pos = x;
            t = reach;
            let Some(m) = mark else { break };
            mi += 1;
            match m.mark {
                Mark::Speed(s) => v = s,
                Mark::Shape(EventKind::Shrink) => {
                    sides -= 1;
                    t += MORPH_HOLD;
                }
                Mark::Shape(_) if sides < MAX_SIDES => {
                    sides += 1;
                    t += MORPH_HOLD;
                }
                Mark::Shape(_) => t += NO_MORPH_HOLD,
            }
        }
        let mut out = Forecast { ticks: t, pos, sides, sides_after_morph: sides, morphing: false };
        // shape changes reaching the centre as the wave is placed are in progress
        for m in &marks[mi..] {
            if m.at > lead + TOLERANCE {
                break;
            }
            if m.wave <= cleared + TOLERANCE {
                continue;
            }
            if let Mark::Shape(k) = m.mark {
                out.morphing = true;
                if k == EventKind::Shrink {
                    out.sides_after_morph = sides - 1;
                }
            }
        }
        self.lay.marks = marks;
        out
    }

    /// The next laid-out wave reaches the spawn line.
    pub(super) fn fire(&mut self) {
        let p = self.lay.pending.pop_front().expect("a wave to fire");
        for &w in &p.walls {
            super::add_wall(&mut self.walls, w);
        }
        if let Some(r) = p.speed_ramp {
            self.stream.speedramp = r;
        }
        if let Some(s) = p.speed {
            self.stream.speed = s;
        }
        for &e in &p.effects {
            // directors don't switch level, so there's no flow to keep to
            let _ = self.act(e);
        }
        self.lay.spare.push(p);
    }

    /// The walls laid out ahead, where they are now, with the slots they will take (which set
    /// their colour): each takes the first slot free by the time it's placed. `free_at` is
    /// scratch space.
    pub fn pending_walls(&self, out: &mut Vec<(usize, Wall)>, free_at: &mut Vec<f64>) {
        out.clear();
        let gone = |w: &Wall, lead: f64| lead + if w.kind == WallKind::Wall { w.dist + w.len } else { w.dist };
        free_at.clear();
        free_at.extend(self.walls.iter().map(|w| if w.active { gone(w, 0.0) } else { f64::NEG_INFINITY }));
        for p in &self.lay.pending {
            for w in &p.walls {
                let at = gone(w, p.lead);
                let slot = match free_at.iter().position(|&f| f <= p.lead) {
                    Some(i) => {
                        free_at[i] = at;
                        i
                    }
                    None => {
                        free_at.push(at);
                        free_at.len() - 1
                    }
                };
                out.push((slot, Wall { dist: w.dist + p.lead, ..*w }));
            }
        }
    }
}
