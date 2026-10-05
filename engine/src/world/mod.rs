//! The simulation: the playfield and everything on it, and the logic of a run.
//!
//! Time is measured in ticks (1/60 s) as an f64. Business logic runs at integer tick instants
//! (`tick`); everything that moves is advanced continuously between them (`integrate_to`), along
//! curves whose values at the tick instants match the original's per-tick updates. Events that
//! change how other things move (walls starting or stopping, the shape losing a side, the player
//! touching a wall) cut the integration step, so they happen at their exact time.
//!
//! The world makes no sound and saves nothing: what it does that the game should know about comes
//! out as `Event`s.

mod host;
mod layout;
mod pal;

use crate::ids::{Id, LevelId, RotationId, SoundId, TrackId};
use crate::pack::{Body, Choice, EventKind, LevelKind, Pack, Motion, PaletteId, WallSpec};
use rand::rngs::SmallRng;
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};

use crate::script::{self, CounterId, Counters, Flow};

pub use pal::{Colours, FULL as PAL_FULL, Pal, SLOTS, slot};

/// Simulation ticks per second.
pub const TICK_RATE: f64 = 60.0;
/// Tolerance for continuous quantities reaching their targets.
const EPS: f64 = 1e-9;
pub const MAX_SIDES: i32 = 6;

/// Where walls hit the player, and the margin inside which a wall the player moves into kills
/// instead of blocking (minus one tick's travel).
const HIT_DIST: f64 = 150.0;
const BLOCK_DIST: f64 = 145.0;
/// Walls at least this long block sideways movement; shorter ones can be slid into, even when their
/// tail still covers the player (as in the original: a wall draining into the centre shrinks below
/// this at once, so its tail is passable).
const BLOCK_LEN: f64 = 200.0;
/// How far the player is kept from a side boundary when stopped against it, and how long after a
/// blocking wall's tail passes the player is released.
const NUDGE: f64 = 1e-7;
const UNBLOCK_MARGIN: f64 = 1e-6;
const MAX_TILT: f64 = 10.0;

/// The zoom while playing, when pulled back to the menus, and when pulled back to retry the
/// tutorial; and how fast it changes, per tick.
pub const ZOOM_IN: f64 = 40.0;
pub const ZOOM_OUT: f64 = 320.0;
const TUTORIAL_ZOOM_OUT: f64 = 180.0;
/// The title appears zoomed in this far, and pulls back.
const TITLE_ZOOM: f64 = 300.0;
const ZOOM_RATE: f64 = 20.0;

/// After a death, in ticks: the game over is announced, the camera levels out, then pulls back.
const GAME_OVER_AT: i64 = 10;
const LEVEL_CAMERA_AT: i64 = 40;
pub const PULL_BACK_AT: i64 = 60;
const OVER_MAX: i64 = 100;
/// The flash of a death, a level switch or a menu change, in ticks.
pub const FLASH: f64 = 5.0;

/// The shape gains or loses a side at this rate per tick, then holds the walls this many ticks.
const MORPH_RATE: f64 = 0.1;
const MORPH_SETTLE: f64 = 20.0;

/// The original's speedramp: walls move at `speed` once it's above 1, else drift at this times
/// it. It climbs by 1 a tick to its cap; pulling back sets it negative so the walls fly out.
const DRIFT: f64 = 5.0;
const SPEEDRAMP_CAP: f64 = 10.0;
const SPEEDRAMP_PULL_BACK: f64 = -40.0;
const SPEEDRAMP_PLAYING: f64 = 100.0;
/// The wall speed a pattern's delay distance is given at: the original's wave timer is the delay
/// in ticks at this speed, rescaled to the actual speed.
const REFERENCE_SPEED: f64 = 20.0;

/// Track time per tick: the beat table has an entry per tick of the track.
const MS_PER_TICK: f64 = 1000.0 / TICK_RATE;
/// How far behind the music clock the audio may report the music before the clock steps back to
/// it (it never steps back for less, so beats aren't crossed twice).
const MUSIC_RESYNC_MS: f64 = 100.0;

/// The colour pulse of the menus: a triangle wave from 0 to this, at 2 a tick.
const GLOW_MAX: f64 = 62.0;
const GLOW_RATE: f64 = 2.0;
const PULSE_DECAY: f64 = 2.0;

/// The stage select's slots, and the ticks it takes to turn to the next one; its camera's lean
/// and how fast it gets there; and its centre's steady pulse.
const SELECT_SLOTS: usize = 6;
const SELECT_TURN: f64 = 10.0;
const SELECT_LEAN: f64 = 45.0;
const SELECT_LEAN_RATE: f64 = 2.0;
const SELECT_PULSE: f64 = 60.0;

/// A spin burst's rate ramps up from 0 at this much per tick per tick for ACCEL ticks, holds until
/// tick CRUISE_END, then ramps down from DECEL ticks out.
const BURST_JERK: f64 = 0.5;
const BURST_ACCEL: f64 = 14.0;
const BURST_CRUISE_END: f64 = 45.0;
const BURST_DECEL: f64 = 10.0;

/// A tilt leans this far (times its direction) at a quarter degree per tick, holds, and comes
/// back; a swing goes one way and the other at 2 degrees per tick.
const TILT_LEAN: f64 = 24.0;
const TILT_RATE: f64 = 0.25;
const TILT_HOLD: f64 = 60.0;
const SWING: f64 = 32.0;
const SWING_RATE: f64 = 2.0;

/// A zoom pulse pushes the camera back this far and returns, at 20 units a tick.
const ZOOM_PULSE_DEPTH: f64 = 400.0;
const ZOOM_PULSE_RATE: f64 = 20.0;

/// What the world tells the game.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Sound(SoundId),
    Music { track: TrackId, offset_ms: f64 },
    MusicStop,
    MusicFadeOut,
    /// The run passed the level's best time.
    NewRecord,
    RankUp(usize),
    /// The run completed its level, for the first time.
    Completed,
    /// The game-over screen is coming: time to save, and maybe to say so.
    GameOver,
    /// The ending's script is over.
    EndingDone,
    TutorialDone,
}

/// A direction to turn: left is anticlockwise on screen, increasing the player's angle.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Turn {
    Left,
    Right,
}

impl Turn {
    fn sign(self) -> f64 {
        match self {
            Turn::Left => 1.0,
            Turn::Right => -1.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scene {
    Title,
    StageSelect,
    /// A run, including the tutorial and the ending.
    Run,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WallKind {
    Wall,
    Event(EventKind),
}

#[derive(Clone, Copy, Debug)]
pub struct Wall {
    pub kind: WallKind,
    pub side: i32,
    pub dist: f64,
    pub len: f64,
    pub active: bool,
}

/// How a run starts, from what the game knows.
pub struct RunSetup {
    pub level: LevelId,
    /// The level's best time.
    pub best: i64,
    /// Whether completing the level now would be its first completion.
    pub first_completion: bool,
    pub tutorial: bool,
}

/// The shape in the centre changing its number of sides.
#[derive(Clone, Copy, PartialEq)]
enum Morph {
    None,
    /// Losing its last side.
    Shrink,
    /// About to gain a side.
    GrowStart,
    /// Gaining it.
    Grow,
    /// Regaining sides after a game over, without holding the walls.
    RegrowStart,
    Regrow,
}

#[derive(Clone, Copy, PartialEq)]
enum Burst {
    None,
    Accel,
    Cruise,
    Decel,
}

#[derive(Clone, Copy, PartialEq)]
enum Wobble {
    None,
    Out(f64),
    Hold(f64),
    Back(f64),
    Swing1,
    Swing2,
    Swing3,
}

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Idle,
    In,
    Out,
}

#[derive(Clone, Copy, PartialEq)]
enum ZoomPulse {
    None,
    In,
    Out,
}

#[derive(Clone, Copy, PartialEq)]
enum Finale {
    Off,
    /// The level is complete: the next hit starts the finale.
    Armed,
    On,
}

#[derive(Clone, Copy, Debug)]
enum Ev {
    /// The next wave laid out ahead reaches the spawn line.
    Fire,
    ZoomIn,
    ZoomOut,
    Freeze,
    Morph,
    Marker(usize),
    Front(usize),
    Cross,
    Unblock,
    /// The music moves on to the beat table's next entry.
    Beat,
}

/// Where the music is between the ticks that report it: a tick of track time per tick.
#[derive(Clone, Copy)]
struct MusicClock {
    track: TrackId,
    /// Track time at `t`.
    ms: f64,
    t: f64,
}

impl MusicClock {
    fn at(&self, t: f64) -> f64 {
        self.ms + (t - self.t) * MS_PER_TICK
    }
}

/// The beat table entry for a track time (with a margin, so an entry's own start is in it).
fn beat_index(ms: f64) -> usize {
    (ms / MS_PER_TICK + 1e-6).max(0.0) as usize
}

/// The camera and everything that only changes how things look.
pub struct View {
    pub spin: f64,
    rotation: Option<RotationId>,
    burst: Burst,
    burst_dir: f64,
    burst_vel: f64,
    /// The centre's pulse to the beat: it decays at PULSE_DECAY, but not below its target.
    pub pulse: f64,
    /// The last beat value seen, for when there's no music to read it from.
    beat: i32,
    pub zoom: f64,
    zoom_phase: Zoom,
    zoom_target: f64,
    pub camangle: f64,
    lean: Option<f64>,
    lean_rate: f64,
    pub pitch: f64,
    sway: bool,
    wobble: Wobble,
    pub wobbleangle: f64,
    wobbletimer: f64,
    zoompulse: ZoomPulse,
    pub zoompulsedepth: f64,
    pub flash: f64,
    /// A slow triangle wave, 0 to GLOW_MAX, that pulses the colours of the menus and game over.
    pub glow: f64,
    glow_down: bool,
    pub centre_flip: bool,
    /// Ticks since the run ended, up to OVER_MAX, for the player's scattering.
    pub gameover: f64,
}

pub struct Shape {
    pub sides: i32,
    morph: Morph,
    /// How far through a morph, 0 to 1.
    pub morph_amount: f64,
}

pub struct Player {
    pub angle: f64,
    turning: Option<Turn>,
    left: bool,
    right: bool,
    blocked: Option<i32>,
    pub tilt: f64,
    turn_speed: f64,
}

/// The run's logic.
struct Run {
    /// The level the run started on, and the one whose logic runs (after level switches).
    level: LevelId,
    logic: LevelId,
    counters: Counters,
    tutorial: bool,
    tutorial_over: bool,
    tcounters: Counters,
    finale: Finale,
    first_completion: bool,
    survival: i64,
    time_shift: i64,
    elapsed: i64,
    wave: i64,
    best: i64,
    rank: usize,
    /// The original's gameovertimer: 0 while alive, then ticks since dying, up to OVER_MAX.
    over: i64,
    hit_pending: bool,
    next_entry: usize,
    music: Option<TrackId>,
    music_plays: u32,
}

/// The wall stream.
struct Stream {
    speed: f64,
    /// The original's speedramp (see SPEEDRAMP_CAP).
    speedramp: f64,
    freeze: f64,
    wavetimer: f64,
    pausewaves: bool,
}

/// What placing a pattern sets, besides its walls.
#[derive(Default)]
struct Placed {
    delay_distance: Option<f64>,
    hold_until_morphed: bool,
    speed_ramp: Option<f64>,
}

pub struct World {
    pack: &'static Pack,
    rng: SmallRng,
    /// Simulation time in ticks.
    t: f64,
    next_tick: f64,
    /// Debugging: no collisions.
    god: bool,
    scene: Scene,
    /// The stage select's slot, and ticks left of its turn to the next one (positive turns left).
    slot: usize,
    turn: f64,
    view: View,
    shape: Shape,
    player: Player,
    walls: Vec<Wall>,
    pal: Pal,
    run: Run,
    stream: Stream,
    lay: layout::Lay,
    music: Option<MusicClock>,
    events: Vec<Event>,
}

/// The middle of a stage select slot.
fn slot_angle(slot: usize) -> f64 {
    (slot as f64 + 0.5) * 360.0 / SELECT_SLOTS as f64
}

fn approach(v: f64, target: f64, step: f64) -> f64 {
    if v < target { (v + step).min(target) } else { (v - step).max(target) }
}

/// The wave timer for a delay distance at a wall speed.
fn wave_delay(distance: f64, speed: f64) -> f64 {
    distance / if speed != 0.0 { speed } else { REFERENCE_SPEED }
}

/// Puts a wall in the first free slot (slots set the walls' alternating colours).
fn add_wall(walls: &mut Vec<Wall>, w: Wall) {
    match walls.iter().position(|w| !w.active) {
        Some(i) => walls[i] = w,
        None => walls.push(w),
    }
}

/// Places a pattern's walls through `add`, under a rotation (r of n) if any.
fn place(b: &Body, rot: Option<(u32, u32)>, rng: &mut SmallRng, out: &mut Placed, add: &mut impl FnMut(Wall)) {
    for w in &b.walls {
        add(match *w {
            WallSpec::Wall { side, dist, len } => {
                let side = rot.map_or(side, |(r, n)| (r as i32 + side).rem_euclid(n as i32));
                Wall { kind: WallKind::Wall, side, dist, len, active: true }
            }
            // events aren't on a side
            WallSpec::Event { kind, at } => Wall { kind: WallKind::Event(kind), side: -1, dist: at, len: 1.0, active: true },
        });
    }
    if b.delay_distance.is_some() {
        out.delay_distance = b.delay_distance;
    }
    if let Some(h) = b.hold_until_morphed {
        out.hold_until_morphed = h;
    }
    if b.speed_ramp.is_some() {
        out.speed_ramp = b.speed_ramp;
    }
    match &b.choice {
        Some(Choice::Rotate(n, then)) => {
            let r = rng.random_range(0..*n);
            place(then, Some((r, *n)), rng, out, add);
        }
        Some(Choice::Variants(vs)) => {
            let body = vs.pick(rng);
            place(body, None, rng, out, add);
        }
        None => {}
    }
}

impl World {
    pub fn new(pack: &'static Pack, seed: u64) -> World {
        World {
            pack,
            rng: SmallRng::seed_from_u64(seed),
            t: 0.0,
            next_tick: 1.0,
            god: false,
            scene: Scene::Title,
            slot: 0,
            turn: 0.0,
            view: View {
                spin: 0.0,
                rotation: pack.idle_rotation,
                burst: Burst::None,
                burst_dir: 1.0,
                burst_vel: 0.0,
                pulse: 0.0,
                beat: 0,
                zoom: ZOOM_OUT,
                zoom_phase: Zoom::Idle,
                zoom_target: ZOOM_OUT,
                camangle: 0.0,
                lean: None,
                lean_rate: 1.0,
                pitch: 0.0,
                sway: false,
                wobble: Wobble::None,
                wobbleangle: 0.0,
                wobbletimer: 0.0,
                zoompulse: ZoomPulse::None,
                zoompulsedepth: 0.0,
                flash: 0.0,
                glow: 0.0,
                glow_down: false,
                centre_flip: false,
                gameover: PULL_BACK_AT as f64,
            },
            shape: Shape { sides: MAX_SIDES, morph: Morph::None, morph_amount: 0.0 },
            player: Player { angle: 30.0, turning: None, left: false, right: false, blocked: None, tilt: 0.0, turn_speed: 0.0 },
            walls: Vec::with_capacity(512),
            pal: Pal::new(pack, pack.menu_palette),
            run: Run {
                level: LevelId::new(0),
                logic: LevelId::new(0),
                counters: Counters::ZERO,
                tutorial: false,
                tutorial_over: false,
                tcounters: Counters::ZERO,
                finale: Finale::Off,
                first_completion: false,
                survival: 0,
                time_shift: 0,
                elapsed: 0,
                wave: 0,
                best: 0,
                rank: 0,
                over: PULL_BACK_AT,
                hit_pending: false,
                next_entry: 0,
                music: None,
                music_plays: 0,
            },
            stream: Stream { speed: 0.0, speedramp: 0.0, freeze: 0.0, wavetimer: 0.0, pausewaves: false },
            lay: layout::Lay::default(),
            music: None,
            events: Vec::with_capacity(64),
        }
    }

    fn sounds(&mut self, ids: &'static [SoundId]) {
        self.events.extend(ids.iter().map(|&id| Event::Sound(id)));
    }

    /// Hands what happened since the last call to `out`, keeping both buffers' storage.
    pub fn take_events(&mut self, out: &mut Vec<Event>) {
        out.clear();
        std::mem::swap(out, &mut self.events);
    }

    // ---------------------------------------------------------------------------------------
    // what the game sees

    pub fn t(&self) -> f64 {
        self.t
    }

    pub fn scene(&self) -> Scene {
        self.scene
    }

    /// The stage select's slot.
    pub fn slot(&self) -> usize {
        self.slot
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn shape(&self) -> &Shape {
        &self.shape
    }

    pub fn player(&self) -> &Player {
        &self.player
    }

    pub fn walls(&self) -> &[Wall] {
        &self.walls
    }

    pub fn pal(&self) -> &Pal {
        &self.pal
    }

    pub fn alive(&self) -> bool {
        self.run.over == 0
    }

    /// Ticks since the run ended (0 while alive).
    pub fn over(&self) -> i64 {
        self.run.over
    }

    pub fn ending(&self) -> bool {
        self.scene == Scene::Run && self.pack.levels[self.run.logic].kind == LevelKind::Ending
    }

    pub fn tutorial(&self) -> bool {
        self.scene == Scene::Run && self.run.tutorial
    }

    pub fn survival(&self) -> i64 {
        self.run.survival
    }

    pub fn best(&self) -> i64 {
        self.run.best
    }

    pub fn rank(&self) -> usize {
        self.run.rank
    }

    pub fn run_level(&self) -> LevelId {
        self.run.level
    }

    /// Whether the stage select is turning to another slot.
    pub fn turning(&self) -> bool {
        self.turn != 0.0
    }

    /// A tutorial counter's value, for its prompts.
    pub fn tutorial_counter(&self, c: CounterId) -> f64 {
        self.run.tcounters[c].f()
    }

    fn side_width(&self) -> f64 {
        (360 / self.shape.sides) as f64
    }

    pub fn player_side(&self) -> i32 {
        (self.player.angle / self.side_width()).floor() as i32
    }

    /// The palette slot the player is drawn in.
    pub fn player_slot(&self) -> usize {
        match self.scene {
            Scene::StageSelect => self.pack.slot(self.slot).and_then(|li| self.pack.levels[li].menu.as_ref()).map_or(slot::PLAYER, |m| m.player_slot),
            _ => slot::PLAYER,
        }
    }

    // ---------------------------------------------------------------------------------------
    // what the game does

    pub fn set_god(&mut self, on: bool) {
        self.god = on;
    }

    /// The held controls: the way to turn, and left and right as held (for the tutorial).
    pub fn set_input(&mut self, turning: Option<Turn>, left: bool, right: bool) {
        self.player.left = left;
        self.player.right = right;
        if turning != self.player.turning {
            self.player.turning = turning;
            self.player.blocked = None;
        }
    }

    pub fn show_palette(&mut self, id: PaletteId) {
        if self.pal.id() != id {
            self.pal.set(self.pack, id);
        }
    }

    pub fn flash(&mut self, ticks: f64) {
        self.view.flash = ticks;
    }

    /// The next track starts from its beginning again.
    pub fn reset_music(&mut self) {
        self.run.music_plays = 0;
    }

    pub fn enter_title(&mut self) {
        self.scene = Scene::Title;
        self.pal.set(self.pack, self.pack.menu_palette);
        self.view.camangle = 0.0;
        self.view.lean = None;
        self.run.over = PULL_BACK_AT;
        self.view.gameover = PULL_BACK_AT as f64;
        self.view.zoom = TITLE_ZOOM;
        self.run.tutorial = false;
    }

    pub fn enter_stage_select(&mut self, slot: usize) {
        self.scene = Scene::StageSelect;
        self.restart();
        self.slot = slot;
        self.turn = 0.0;
        self.run.tutorial = false;
        self.player.angle = slot_angle(slot);
    }

    /// Starts the stage select turning to the next slot; returns whether it did.
    pub fn turn_select(&mut self, way: Turn) -> bool {
        if self.turn != 0.0 {
            return false;
        }
        self.turn = SELECT_TURN * way.sign();
        true
    }

    pub fn start_run(&mut self, setup: RunSetup) {
        let pack = self.pack;
        let l = &pack.levels[setup.level];
        if self.scene == Scene::StageSelect {
            self.view.zoom = ZOOM_OUT;
        }
        self.scene = Scene::Run;
        self.restart();
        self.run.finale = Finale::Off;
        self.run.rank = 0;
        self.run.level = setup.level;
        self.run.best = setup.best;
        self.run.first_completion = setup.first_completion;
        self.view.pitch = 0.0;
        self.view.sway = false;
        self.view.lean = None;
        self.view.lean_rate = 1.0;
        self.run.time_shift = l.time_offset;
        self.setup_level(setup.level);
        self.sounds(if l.kind == LevelKind::Ending { &pack.roles.ending_start } else { &pack.roles.level_start });
        self.run.tutorial = setup.tutorial && l.kind != LevelKind::Ending;
        if self.run.tutorial {
            self.run.tcounters = pack.tutorial.counters;
        }
    }

    pub fn give_up(&mut self) {
        if self.scene == Scene::Run && self.alive() && !self.ending() {
            self.hit();
        }
    }

    // ---------------------------------------------------------------------------------------
    // run setup

    fn play_music(&mut self, track: TrackId) {
        if self.run.music == Some(track) {
            return;
        }
        let tr = &self.pack.tracks[track];
        let offset_ms = if self.run.music_plays > 0 { *tr.restart_points_ms.pick(&mut self.rng) } else { 0.0 };
        self.run.music_plays += 1;
        self.run.music = Some(track);
        self.events.push(Event::Music { track, offset_ms });
    }

    fn stop_music(&mut self) {
        self.run.music = None;
        self.events.push(Event::MusicStop);
    }

    /// Switches to one of `modes`, other than the current one if there's a choice.
    fn reroll(&mut self, modes: &[RotationId]) {
        if let [only] = modes {
            self.view.rotation = Some(*only);
            return;
        }
        loop {
            let m = *modes.choose(&mut self.rng).expect("the pack has modes in each list");
            if Some(m) != self.view.rotation {
                self.view.rotation = Some(m);
                return;
            }
        }
    }

    /// The level-specific part of starting a run or switching level.
    fn setup_level(&mut self, li: LevelId) {
        let l = &self.pack.levels[li];
        if let Some(m) = l.music {
            self.play_music(m);
        }
        self.view.centre_flip = l.centre_flip;
        self.player.turn_speed = l.turn_speed;
        self.show_palette(l.palette);
        self.reroll(&l.rotation);
        self.run.wave = l.start_wave;
        if let Some(s) = l.start_speed {
            self.stream.speed = s;
        }
        self.run.counters = l.counters;
        self.run.logic = li;
        let now = self.run.survival + self.run.time_shift;
        self.run.next_entry = l.timeline.iter().position(|(at, _)| *at > now).unwrap_or(l.timeline.len());
    }

    fn reset_walls(&mut self) {
        self.lay.stop();
        self.walls.clear();
        self.run.wave = 0;
        self.stream.wavetimer = 0.0;
        self.view.pulse = 0.0;
        self.shape.sides = MAX_SIDES;
        self.shape.morph_amount = 0.0;
        self.shape.morph = Morph::None;
        self.stream.pausewaves = false;
        self.player.blocked = None;
    }

    /// The original's restart(): back to the start of a run, alive.
    fn restart(&mut self) {
        self.reset_walls();
        self.run.survival = 0;
        self.run.elapsed = 0;
        self.run.over = 0;
        self.view.gameover = 0.0;
        self.run.hit_pending = false;
    }

    fn switch_level(&mut self, li: LevelId, time_offset: i64) {
        self.run.time_shift += time_offset;
        if self.lay.planned_for(li) {
            // laid out for already: only what's on screen and around it changes
            let lay = std::mem::take(&mut self.lay);
            self.reset_walls();
            self.setup_level(li);
            self.lay = lay;
        } else {
            self.reset_walls();
            self.setup_level(li);
        }
        self.view.pitch = 0.0;
        self.sounds(&self.pack.roles.switch_level);
        self.view.flash = FLASH;
    }

    fn hit(&mut self) {
        self.unlay();
        self.run.hit_pending = true;
        self.player.blocked = None;
    }

    fn die(&mut self) {
        if !self.run.tutorial {
            self.run.music = None;
            self.events.push(Event::MusicFadeOut);
        }
        self.sounds(&self.pack.roles.die);
        self.run.over = 1;
        self.view.gameover = 1.0;
        self.stream.speedramp = 0.0;
        self.view.flash = FLASH;
        self.run.finale = Finale::Off;
        self.view.lean = None;
    }

    fn complete_level(&mut self) {
        if !std::mem::take(&mut self.run.first_completion) {
            return;
        }
        if self.pack.levels[self.run.level].completion.as_ref().is_some_and(|c| c.finale) {
            self.run.finale = Finale::Armed;
        }
        self.events.push(Event::Completed);
    }

    // ---------------------------------------------------------------------------------------
    // time

    /// Moves continuously up to `t`, or to the next tick if that comes first.
    pub fn integrate_to(&mut self, t: f64) {
        let end = t.min(self.next_tick);
        let mut guard = 0;
        while self.t < end {
            let (te, ev) = self.next_event();
            let dt = (end - self.t).min(te);
            self.step(dt);
            self.t += dt;
            if dt == te {
                self.handle(ev.expect("an event at its time"));
                self.raise_pulse();
            }
            guard += 1;
            if guard > 10000 {
                panic!("event storm at t={} ({ev:?})", self.t);
            }
        }
        self.t = end;
    }

    /// Whether `tick` is due.
    pub fn tick_due(&self) -> bool {
        self.t >= self.next_tick
    }

    fn wall_velocity(&self) -> f64 {
        if self.run.hit_pending {
            0.0
        } else if self.stream.speedramp > 1.0 {
            if self.stream.freeze > 0.0 || self.morph_holds() { 0.0 } else { self.stream.speed }
        } else {
            self.stream.speedramp * DRIFT
        }
    }

    fn player_velocity(&self) -> f64 {
        if self.scene == Scene::Run && self.alive() && !self.run.hit_pending && self.player.blocked.is_none() {
            self.player.turning.map_or(0.0, |t| t.sign() * self.player.turn_speed)
        } else {
            0.0
        }
    }

    fn collisions(&self) -> bool {
        self.scene == Scene::Run && self.alive() && !self.run.hit_pending && !self.ending() && !self.god
    }

    /// Whether the shape's morph holds the walls.
    fn morph_holds(&self) -> bool {
        matches!(self.shape.morph, Morph::Shrink | Morph::GrowStart | Morph::Grow)
    }

    /// Walls on `side` that a player moving onto it would run into: (kills, blocks).
    fn entering(&self, side: i32) -> (bool, bool) {
        let mut kill = false;
        let mut block = false;
        for w in self.walls.iter().filter(|w| w.active && w.kind == WallKind::Wall && w.side == side && w.dist <= HIT_DIST) {
            if w.dist >= BLOCK_DIST - self.stream.speed {
                kill = true;
            } else if w.len >= BLOCK_LEN {
                block = true;
            }
        }
        (kill, block)
    }

    fn unblock_time(&self, side: i32) -> f64 {
        let v = self.wall_velocity();
        let mut t: f64 = 0.0;
        for w in &self.walls {
            if w.active
                && w.kind == WallKind::Wall
                && w.side == side
                && w.dist <= HIT_DIST
                && w.dist < BLOCK_DIST - self.stream.speed
                && w.len >= BLOCK_LEN
            {
                if v <= 0.0 {
                    return f64::INFINITY;
                }
                t = t.max((w.dist + w.len - BLOCK_LEN) / v + UNBLOCK_MARGIN);
            }
        }
        t
    }

    fn next_event(&self) -> (f64, Option<Ev>) {
        let mut best = (f64::INFINITY, None);
        let mut consider = |t: f64, ev: Ev| {
            if t < best.0 {
                best = (t.max(0.0), Some(ev));
            }
        };
        match self.view.zoom_phase {
            Zoom::In => consider((self.view.zoom - ZOOM_IN) / ZOOM_RATE, Ev::ZoomIn),
            Zoom::Out => consider((self.view.zoom_target - self.view.zoom) / ZOOM_RATE, Ev::ZoomOut),
            Zoom::Idle => {}
        }
        if self.stream.freeze > 0.0 && !self.morph_holds() {
            consider(self.stream.freeze, Ev::Freeze);
        }
        match self.shape.morph {
            Morph::Shrink => consider((1.0 - self.shape.morph_amount) / MORPH_RATE, Ev::Morph),
            Morph::GrowStart | Morph::RegrowStart => consider(0.0, Ev::Morph),
            Morph::Grow | Morph::Regrow => consider(self.shape.morph_amount / MORPH_RATE, Ev::Morph),
            Morph::None => {}
        }
        if let Some(t) = self.next_beat() {
            consider(t, Ev::Beat);
        }
        let v = self.wall_velocity();
        if v > 0.0
            && let Some(lead) = self.lay.next_lead()
        {
            consider(lead / v, Ev::Fire);
        }
        if v > 0.0 && self.stream.speedramp > 1.0 {
            for (i, w) in self.walls.iter().enumerate() {
                if w.active && matches!(w.kind, WallKind::Event(_)) {
                    consider(w.dist / v, Ev::Marker(i));
                }
            }
        }
        if self.collisions() {
            let side = self.player_side();
            if v > 0.0 {
                for (i, w) in self.walls.iter().enumerate() {
                    if w.active && w.kind == WallKind::Wall && w.side == side && w.dist >= HIT_DIST - EPS {
                        consider((w.dist - HIT_DIST) / v, Ev::Front(i));
                    }
                }
            }
            if let Some(target) = self.player.blocked {
                consider(self.unblock_time(target), Ev::Unblock);
            } else {
                let om = self.player_velocity();
                let w = self.side_width();
                if om > 0.0 {
                    consider(((side + 1) as f64 * w - self.player.angle) / om, Ev::Cross);
                } else if om < 0.0 {
                    consider((self.player.angle - side as f64 * w) / -om, Ev::Cross);
                }
            }
        }
        best
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Fire => self.fire(),
            Ev::ZoomIn => {
                self.view.zoom = ZOOM_IN;
                self.view.zoom_phase = Zoom::Idle;
                self.stream.speedramp = SPEEDRAMP_PLAYING;
                self.view.rotation = self.pack.idle_rotation;
            }
            Ev::ZoomOut => {
                self.view.zoom = self.view.zoom_target;
                self.view.zoom_phase = Zoom::Idle;
                if self.scene == Scene::Run && self.run.tutorial {
                    // died in the tutorial: try again
                    self.restart();
                    let _ = script::run(&self.pack.tutorial.on_death, self);
                } else {
                    self.view.rotation = self.pack.idle_rotation;
                    self.stop_music();
                }
            }
            Ev::Freeze => self.stream.freeze = 0.0,
            Ev::Morph => {
                let s = &mut self.shape;
                match s.morph {
                    Morph::Shrink => {
                        s.morph_amount = 0.0;
                        s.sides -= 1;
                        s.morph = Morph::None;
                    }
                    Morph::GrowStart | Morph::RegrowStart => {
                        if s.morph == Morph::GrowStart {
                            // the original holds the walls even when there's no side to add
                            self.stream.freeze = MORPH_SETTLE;
                        }
                        if s.sides < MAX_SIDES {
                            s.morph = if s.morph == Morph::GrowStart { Morph::Grow } else { Morph::Regrow };
                            s.sides += 1;
                            s.morph_amount = 1.0;
                        } else {
                            s.morph = Morph::None;
                        }
                    }
                    _ => {
                        s.morph_amount = 0.0;
                        s.morph = Morph::None;
                    }
                }
                self.player.blocked = None;
            }
            Ev::Marker(i) => {
                let w = &mut self.walls[i];
                w.active = false;
                w.dist = 0.0;
                match w.kind {
                    WallKind::Event(EventKind::Shrink) => {
                        self.shape.morph = Morph::Shrink;
                        self.stream.pausewaves = false;
                    }
                    WallKind::Event(EventKind::Grow) => {
                        self.shape.morph = Morph::GrowStart;
                        self.stream.pausewaves = false;
                    }
                    WallKind::Event(EventKind::ZoomPulse) => self.view.zoompulse = ZoomPulse::In,
                    WallKind::Wall => unreachable!("markers are events"),
                }
            }
            Ev::Front(i) => {
                self.walls[i].dist = HIT_DIST;
                self.hit();
            }
            Ev::Cross => self.cross(),
            Ev::Unblock => self.player.blocked = None,
            // the pulse takes it from here
            Ev::Beat => {}
        }
    }

    /// The player reaches the edge of their side.
    fn cross(&mut self) {
        let w = self.side_width();
        let up = self.player.turning == Some(Turn::Left);
        // the step stopped exactly on the boundary, which belongs to the upper side
        let side = if up { (self.player.angle / w).ceil() as i32 - 1 } else { self.player_side() };
        let (boundary, target) = if up {
            ((side + 1) as f64 * w, (side + 1).rem_euclid(self.shape.sides))
        } else {
            (side as f64 * w, (side - 1).rem_euclid(self.shape.sides))
        };
        let (kill, block) = self.entering(target);
        let inside_target = if up { boundary } else { boundary - NUDGE };
        if kill {
            self.player.angle = inside_target.rem_euclid(360.0);
            self.hit();
        } else if block {
            self.player.angle = if up { boundary - NUDGE } else { boundary };
            self.player.blocked = Some(target);
        } else {
            self.player.angle = inside_target.rem_euclid(360.0);
        }
    }

    /// Advances everything continuous by `dt`, which never spans an event from `next_event`.
    fn step(&mut self, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        // constant through the step: what changes it ends a step
        let pulse_target = self.pulse_target();
        self.step_walls(dt);
        self.step_player(dt);
        self.step_spin(dt);
        self.step_camera(dt);

        // shape morphs, and the wall freeze they hold
        if self.morph_holds() {
            self.stream.freeze = MORPH_SETTLE;
        } else {
            self.stream.freeze = (self.stream.freeze - dt).max(0.0);
        }
        match self.shape.morph {
            Morph::Shrink => self.shape.morph_amount = (self.shape.morph_amount + MORPH_RATE * dt).min(1.0),
            Morph::Grow | Morph::Regrow => self.shape.morph_amount = (self.shape.morph_amount - MORPH_RATE * dt).max(0.0),
            _ => {}
        }

        let v = &mut self.view;
        v.pulse = (v.pulse - PULSE_DECAY * dt).max(pulse_target);
        v.flash = (v.flash - dt).max(0.0);
        if self.run.over != 0 {
            v.gameover = (v.gameover + dt).min(OVER_MAX as f64);
        }
        let mut rem = dt;
        while rem > 0.0 {
            if v.glow_down {
                let tt = rem.min(v.glow / GLOW_RATE);
                v.glow -= GLOW_RATE * tt;
                rem -= tt;
                if v.glow <= EPS {
                    v.glow = 0.0;
                    v.glow_down = false;
                }
            } else {
                let tt = rem.min((GLOW_MAX - v.glow) / GLOW_RATE);
                v.glow += GLOW_RATE * tt;
                rem -= tt;
                if v.glow >= GLOW_MAX - EPS {
                    v.glow = GLOW_MAX;
                    v.glow_down = true;
                }
            }
        }
        self.pal.step(self.pack, dt);
    }

    fn step_walls(&mut self, dt: f64) {
        let v = self.wall_velocity();
        if v > 0.0 {
            self.lay.travel(v * dt);
        }
        let moving = self.stream.speedramp > 1.0;
        for w in self.walls.iter_mut().filter(|w| w.active) {
            if v > 0.0 {
                let mut travel = v * dt;
                if w.dist > 0.0 {
                    let d = w.dist.min(travel);
                    w.dist -= d;
                    travel -= d;
                }
                if w.dist <= EPS && (w.kind == WallKind::Wall || !moving) {
                    w.dist = 0.0;
                    w.len -= travel;
                    if w.len <= 0.0 {
                        w.active = false;
                    }
                }
            } else if v < 0.0 {
                w.dist -= v * dt;
            }
        }
    }

    fn step_player(&mut self, dt: f64) {
        let om = self.player_velocity();
        let p = &mut self.player;
        if om != 0.0 {
            p.angle = (p.angle + om * dt).rem_euclid(360.0);
            if p.angle >= 360.0 {
                // rem_euclid of a tiny negative rounds up to 360
                p.angle = 0.0;
            }
        }
        if self.scene == Scene::StageSelect {
            // turning to the next slot
            if self.turn != 0.0 {
                let d = self.turn.abs().min(dt);
                // the way it turns, as the player's angle
                let dir = self.turn.signum();
                self.turn -= dir * d;
                let slot_width = 360.0 / SELECT_SLOTS as f64;
                p.angle = (p.angle + dir * slot_width / SELECT_TURN * d).rem_euclid(360.0);
                p.tilt = (p.tilt - dir * dt).clamp(-MAX_TILT, MAX_TILT);
                if self.turn.abs() <= EPS {
                    self.turn = 0.0;
                    self.slot = (self.slot + if dir > 0.0 { 1 } else { SELECT_SLOTS - 1 }) % SELECT_SLOTS;
                    p.angle = slot_angle(self.slot);
                }
            } else {
                p.tilt = approach(p.tilt, 0.0, dt);
            }
        } else if self.scene == Scene::Run && self.run.over == 0 {
            p.tilt = match p.turning {
                Some(t) => p.tilt - t.sign() * dt,
                None => approach(p.tilt, 0.0, dt),
            }
            .clamp(-MAX_TILT, MAX_TILT);
        }
    }

    fn step_spin(&mut self, dt: f64) {
        let mode = self.view.rotation.map(|r| &self.pack.rotation_modes[r]);
        let v = &mut self.view;
        let mut rem = dt;
        while rem > 0.0 {
            match v.burst {
                Burst::None => {
                    if let Some(m) = mode {
                        match m.motion {
                            Motion::Spin { spin } => v.spin += spin * rem,
                            Motion::Settle { settle, settle_rate } => v.spin = approach(v.spin, settle, settle_rate * rem),
                        }
                    }
                    rem = 0.0;
                }
                Burst::Accel => {
                    let v0 = v.burst_vel;
                    let tt = rem.min(BURST_ACCEL - v0).max(0.0);
                    v.spin += v.burst_dir * BURST_JERK * (v0 * tt + tt * tt / 2.0);
                    v.burst_vel += tt;
                    rem -= tt;
                    if v.burst_vel >= BURST_ACCEL - EPS {
                        v.burst = Burst::Cruise;
                    }
                }
                Burst::Cruise => {
                    let tt = rem.min(BURST_CRUISE_END - v.burst_vel).max(0.0);
                    v.spin += v.burst_dir * BURST_JERK * BURST_ACCEL * tt;
                    v.burst_vel += tt;
                    rem -= tt;
                    if v.burst_vel >= BURST_CRUISE_END - EPS {
                        v.burst_vel = BURST_DECEL;
                        v.burst = Burst::Decel;
                    }
                }
                Burst::Decel => {
                    let v0 = v.burst_vel;
                    let tt = rem.min(v0).max(0.0);
                    v.spin += v.burst_dir * BURST_JERK * (v0 * tt - tt * tt / 2.0);
                    v.burst_vel -= tt;
                    rem -= tt;
                    if v.burst_vel <= EPS {
                        v.burst_vel = 0.0;
                        v.burst = Burst::None;
                    }
                }
            }
        }
        v.spin = v.spin.rem_euclid(360.0);
        if self.scene == Scene::StageSelect {
            // the selected slot faces the camera
            v.spin = (180.0 - self.player.angle).rem_euclid(360.0);
        }
    }

    fn step_camera(&mut self, dt: f64) {
        let sway = match self.view.rotation {
            Some(r) if self.view.sway => self.pack.rotation_modes[r].sway,
            _ => 0.0,
        };
        let v = &mut self.view;
        match v.zoom_phase {
            Zoom::In => v.zoom = (v.zoom - ZOOM_RATE * dt).max(ZOOM_IN),
            Zoom::Out => v.zoom = (v.zoom + ZOOM_RATE * dt).min(v.zoom_target),
            Zoom::Idle => {}
        }
        if let Some(l) = v.lean {
            v.camangle = approach(v.camangle, l, v.lean_rate * dt);
        }
        v.pitch = approach(v.pitch, sway, dt);

        let mut rem = dt;
        while rem > 0.0 {
            let (rate, target): (f64, f64) = match v.wobble {
                Wobble::None => break,
                Wobble::Out(d) => (TILT_RATE * d, TILT_LEAN * d),
                Wobble::Back(d) => (-TILT_RATE * d, 0.0),
                Wobble::Swing1 => (SWING_RATE, SWING),
                Wobble::Swing2 => (-SWING_RATE, -SWING),
                Wobble::Swing3 => (SWING_RATE, 0.0),
                Wobble::Hold(d) => {
                    let tt = rem.min(TILT_HOLD - v.wobbletimer);
                    v.wobbletimer += tt;
                    rem -= tt;
                    if v.wobbletimer >= TILT_HOLD - EPS {
                        v.wobble = Wobble::Back(d);
                    }
                    continue;
                }
            };
            let tt = rem.min(((target - v.wobbleangle) / rate).max(0.0));
            v.wobbleangle += rate * tt;
            rem -= tt;
            if (target - v.wobbleangle) * rate <= EPS {
                v.wobbleangle = target;
                v.wobble = match v.wobble {
                    Wobble::Out(d) => Wobble::Hold(d),
                    Wobble::Swing1 => Wobble::Swing2,
                    Wobble::Swing2 => Wobble::Swing3,
                    _ => {
                        v.wobbletimer = 0.0;
                        Wobble::None
                    }
                };
            }
        }

        match v.zoompulse {
            ZoomPulse::In => {
                v.zoompulsedepth += ZOOM_PULSE_RATE * dt;
                if v.zoompulsedepth >= ZOOM_PULSE_DEPTH {
                    // turn around at the top
                    v.zoompulsedepth = 2.0 * ZOOM_PULSE_DEPTH - v.zoompulsedepth;
                    v.zoompulse = ZoomPulse::Out;
                }
            }
            ZoomPulse::Out => {
                v.zoompulsedepth -= ZOOM_PULSE_RATE * dt;
                if v.zoompulsedepth <= 0.0 {
                    v.zoompulsedepth = 0.0;
                    v.zoompulse = ZoomPulse::None;
                }
            }
            ZoomPulse::None => {}
        }
    }

    // ---------------------------------------------------------------------------------------
    // business logic

    /// The tick: the run's logic. `music` is the playing track and how far into it.
    pub fn tick(&mut self, music: Option<(TrackId, f64)>) {
        self.next_tick += 1.0;
        let pack = self.pack;
        while self.walls.last().is_some_and(|w| !w.active) {
            self.walls.pop();
        }
        if self.scene == Scene::StageSelect {
            self.view.rotation = None;
            self.view.lean = Some(SELECT_LEAN);
            self.view.lean_rate = SELECT_LEAN_RATE;
        }

        let in_run = self.scene == Scene::Run;
        if self.alive() {
            if self.view.zoom > ZOOM_IN {
                self.view.zoom_phase = Zoom::In;
            }
            if in_run && !self.run.tutorial && self.run.finale != Finale::On && !self.ending() {
                self.count_time();
            }
        } else {
            if self.run.over < OVER_MAX {
                self.run.over += 1;
            }
            if in_run {
                if self.run.over == GAME_OVER_AT && !self.run.tutorial {
                    self.events.push(Event::GameOver);
                }
                if self.run.over >= LEVEL_CAMERA_AT && !self.run.tutorial {
                    self.view.lean = Some(0.0);
                    self.view.lean_rate = 1.0;
                }
                if self.run.over >= PULL_BACK_AT {
                    self.stream.speedramp = SPEEDRAMP_PULL_BACK;
                    // in the tutorial, pull back a little and try again
                    self.zoom_out(if self.run.tutorial { TUTORIAL_ZOOM_OUT } else { ZOOM_OUT });
                }
                if self.view.zoom >= ZOOM_OUT && self.shape.sides < MAX_SIDES && self.shape.morph == Morph::None {
                    self.shape.morph = Morph::RegrowStart;
                }
            } else if self.run.over >= PULL_BACK_AT {
                self.stream.speedramp = SPEEDRAMP_PULL_BACK;
                self.zoom_out(ZOOM_OUT);
            }
        }

        if self.run.hit_pending && in_run {
            self.run.hit_pending = false;
            if self.run.finale == Finale::Armed {
                self.run.finale = Finale::On;
                let _ = script::run(&pack.finale.on_hit, self);
            } else {
                self.die();
            }
        }

        if self.alive() && self.stream.speedramp < SPEEDRAMP_CAP {
            self.stream.speedramp += 1.0;
        }

        self.sync_music(music);
        self.raise_pulse();
        let l = &pack.levels[self.run.logic];

        if self.alive() && in_run {
            self.run.elapsed += 1;
            if self.run.tutorial {
                let _ = script::run(&pack.tutorial.on_tick, self);
                if std::mem::take(&mut self.run.tutorial_over) {
                    self.run.tutorial = false;
                    self.events.push(Event::TutorialDone);
                }
            } else if self.run.finale == Finale::On {
                let _ = script::run(&pack.finale.on_tick, self);
            } else if l.kind == LevelKind::Ending {
                let _ = script::run(&l.on_tick, self);
            } else {
                self.run_timeline();
            }
        }
        self.sync_layout();
    }

    /// A level's tick: its camera, what its timeline has for now, and the wave timer (unless the
    /// waves are laid out ahead).
    fn run_timeline(&mut self) {
        let l = &self.pack.levels[self.run.logic];
        if let Some(lean) = l.camera_lean {
            self.view.lean = Some(lean);
            self.view.lean_rate = 1.0;
        }
        self.view.sway = l.camera_sway;
        let now = self.run.survival + self.run.time_shift;
        while let Some((at, acts)) = l.timeline.get(self.run.next_entry) {
            if *at > now {
                break;
            }
            self.run.next_entry += 1;
            if script::run(acts, self).is_break() {
                // switched level
                return;
            }
        }
        if !self.lay.active() {
            let _ = self.advance_waves();
        }
    }

    fn zoom_out(&mut self, target: f64) {
        if self.view.zoom < target {
            self.view.zoom_phase = Zoom::Out;
            self.view.zoom_target = target;
        }
    }

    /// Takes the music's position as the audio reports it, at a tick.
    fn sync_music(&mut self, music: Option<(TrackId, f64)>) {
        self.music = music.map(|(track, ms)| {
            let ms = match self.music {
                // keep to the clock while the audio is only a little behind it
                Some(c) if c.track == track && (0.0..MUSIC_RESYNC_MS).contains(&(c.at(self.t) - ms)) => c.at(self.t),
                _ => ms,
            };
            MusicClock { track, ms, t: self.t }
        });
    }

    /// The beat value now, from the music if it's playing.
    fn beat(&self) -> i32 {
        match self.music {
            Some(c) => {
                let beats = &self.pack.tracks[c.track].beats;
                beats.get(beat_index(c.at(self.t)).min(beats.len().saturating_sub(1))).copied().unwrap_or(self.view.beat)
            }
            None => self.view.beat,
        }
    }

    /// What the pulse can't fall below now.
    fn pulse_target(&self) -> f64 {
        let l = &self.pack.levels[self.run.logic];
        let held = self.stream.freeze > 0.0 || self.morph_holds();
        match (self.scene, l.frozen_pulse) {
            // the stage select's centre holds a steady pulse
            (Scene::StageSelect, _) => SELECT_PULSE,
            (Scene::Run, Some(p)) if held => p,
            _ => (self.beat().abs() / l.beat_divisor) as f64,
        }
    }

    /// Brings the pulse up to its target, as that changes.
    fn raise_pulse(&mut self) {
        self.view.beat = self.beat();
        self.view.pulse = self.view.pulse.max(self.pulse_target());
    }

    /// Ticks until the music reaches the beat table's next entry, if that's before the next tick.
    fn next_beat(&self) -> Option<f64> {
        let c = self.music?;
        let ms = c.at(self.t);
        let next = beat_index(ms) + 1;
        if next >= self.pack.tracks[c.track].beats.len() {
            return None;
        }
        let t = (next as f64 * MS_PER_TICK - ms) / MS_PER_TICK;
        (self.t + t < self.next_tick).then_some(t)
    }

    /// Survival time, records and ranks.
    fn count_time(&mut self) {
        let pack = self.pack;
        self.run.survival += 1;
        if self.run.survival >= self.run.best {
            if self.run.survival - 1 < self.run.best {
                self.events.push(Event::NewRecord);
                self.sounds(&pack.roles.new_record);
            }
            self.run.best = self.run.survival;
        }
        for (i, r) in pack.ranks.iter().enumerate().skip(1) {
            if self.run.survival > r.at && self.run.rank == i - 1 {
                self.run.rank = i;
                self.events.push(Event::RankUp(i));
                self.sounds(&pack.roles.rank_up);
                if let Some(v) = r.voice {
                    self.events.push(Event::Sound(v));
                }
                if r.completes_level {
                    self.complete_level();
                }
                break;
            }
        }
    }

    /// Places a pattern now (not laid out ahead).
    fn place_now(&mut self, b: &Body) {
        let mut placed = Placed::default();
        let walls = &mut self.walls;
        place(b, None, &mut self.rng, &mut placed, &mut |w| add_wall(walls, w));
        if let Some(d) = placed.delay_distance {
            self.stream.wavetimer = wave_delay(d, self.stream.speed);
        }
        if placed.hold_until_morphed {
            self.stream.pausewaves = true;
        }
        if let Some(r) = placed.speed_ramp {
            self.stream.speedramp = r;
        }
    }

    /// The original's wave timer: counts down, and runs the director when it's out.
    fn advance_waves(&mut self) -> Flow {
        let held = self.stream.pausewaves || (self.stream.freeze > 0.0 && self.stream.speedramp > 1.0);
        if !held {
            self.stream.wavetimer -= 1.0;
        }
        if self.stream.wavetimer > 0.0 {
            return Flow::Continue(());
        }
        self.stream.wavetimer = 0.0;
        let pack = self.pack;
        if self.run.tutorial {
            script::run(&pack.tutorial.on_wave, self)
        } else if self.run.finale == Finale::On {
            script::run(&pack.finale.on_wave, self)
        } else {
            let l = &pack.levels[self.run.logic];
            let flow = script::run(&pack.directors[l.director], self);
            if l.kind != LevelKind::Ending {
                self.run.wave += 1;
            }
            flow
        }
    }
}
