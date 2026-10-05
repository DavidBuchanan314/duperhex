//! The game around the world: screens and menus, the HUD's state, saves, and sound.
//!
//! The game drives the world tick by tick: menus react at ticks, before the world's own logic,
//! and what the world reports (`Event`s) is turned into sound, saves and HUD flashes.

use std::ops::{BitOr, Range};

use crate::audio::Audio;
use crate::ids::{LevelId, SoundId};
use crate::pack::{Announce, Pack};
use crate::save::{MAX_VOLUME, SPEEDS, Save, VSYNCS, Vsync};
use crate::world::{self, Event, RunSetup, Scene, Turn, World};

pub const OPTIONS: usize = 5;
pub const EXTRAS: usize = 6;
const MAIN_MENU_ITEMS: usize = 4;
/// Ticks before the title's voice.
const TITLE_VOICE: u32 = 45;
/// Ticks a held menu direction waits before moving again.
const MENU_REPEAT: u32 = 8;
/// After a run, the game-over screen shows once the camera pulls back past this zoom; a run that
/// leads to the ending starts it when the camera passes ENDING_ZOOM.
pub const GAME_OVER_ZOOM: f64 = 300.0;
const ENDING_ZOOM: f64 = 200.0;
/// Ticks the game-over screen takes to slide in, and the announcement of an unlock waits for
/// before it can be dismissed.
const MENU_SLIDE: f64 = 10.0;
const UNLOCK_WAIT: u32 = 60;
/// The HUD's flashes, in ticks (the rank-up nudge goes three times as fast).
const NEW_RECORD_FLASH: f64 = 45.0;
const RANK_UP_FLASH: f64 = 30.0;
const LEVEL_UP_FLASH: f64 = 90.0;
/// The flash as the ending starts.
const ENDING_FLASH: f64 = 2.0 * world::FLASH;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Menu {
    Main,
    Options,
    Extras,
    Credits,
    Delete,
}

/// What a completed level leads to once its run is over (the original's unlockevent).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Unlock {
    None,
    /// The game-over screen announces it, then the stage select points at what it unlocked.
    Level,
    /// The game-over screen announces it, then the credits roll.
    GameComplete,
    /// The ending plays as the camera pulls back.
    Ending,
}

/// A control.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Select,
    Quit,
    Clear,
}

/// A mouse button.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// The mouse buttons, as held.
#[derive(Clone, Copy, Default)]
pub struct Buttons {
    pub left: bool,
    pub middle: bool,
    pub right: bool,
}

/// The controls, as held.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Held {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub select: bool,
    pub quit: bool,
    pub clear: bool,
}

impl BitOr for Held {
    type Output = Held;

    fn bitor(self, o: Held) -> Held {
        Held {
            left: self.left || o.left,
            right: self.right || o.right,
            up: self.up || o.up,
            down: self.down || o.down,
            select: self.select || o.select,
            quit: self.quit || o.quit,
            clear: self.clear || o.clear,
        }
    }
}

/// The HUD's view of the run.
pub struct Hud {
    /// The highest rank ever reached on the level.
    pub levelreached: usize,
    /// Where the rank bar runs from and to, in ticks.
    pub rankbar: Range<f64>,
    pub levelupflash: f64,
    pub newbestflash: f64,
    pub rankupflash: f64,
    /// The game-over screen sliding in.
    pub menuslide: f64,
}

pub struct Game {
    pack: &'static Pack,
    world: World,
    audio: Audio,
    save: Save,
    /// Set when the player asks to quit.
    pub quit: bool,
    /// A display setting changed (fullscreen or vsync), and the window should follow.
    display_changed: bool,
    /// The antialiasing sample counts the GPU supports, ascending from 1.
    sample_counts: Vec<u32>,
    /// The vsync modes the window supports, in the menu's order.
    vsyncs: Vec<Vsync>,
    /// A game speed from the command line, in place of the saved one.
    speed_override: Option<f64>,
    /// The tutorial was finished this session, at a speed that doesn't save it.
    tutorial_seen: bool,

    pub menu: Menu,
    pub cursor: usize,
    pub page: usize,
    menu_cooldown: u32,
    /// After an action, menu input is ignored until everything is released.
    inputlock: bool,
    /// The controls held, from the keyboard and the mouse together.
    keys: Held,
    keyboard: Held,
    buttons: Buttons,
    /// Which way to turn: the most recently pressed of left and right, while held.
    turning: Option<Turn>,
    title_voice: u32,

    pub hud: Hud,
    pub unlock: Unlock,
    unlocktimer: u32,
    pub announce: Option<Announce>,
    gave_up: bool,
    events: Vec<Event>,
}

impl Game {
    pub fn new(pack: &'static Pack, audio: Audio, mut save: Save,sample_counts: Vec<u32>, vsyncs: Vec<Vsync>, speed_override: Option<f64>, seed: u64) -> Game {
        // a mode saved on another display may not be supported on this one
        if !vsyncs.contains(&save.settings.vsync_mode) {
            save.settings.vsync_mode = Vsync::On;
        }
        let mut g = Game {
            pack,
            world: World::new(pack, seed),
            audio,
            save,
            quit: false,
            display_changed: true,
            sample_counts,
            vsyncs,
            speed_override,
            tutorial_seen: false,
            menu: Menu::Main,
            cursor: 0,
            page: 0,
            menu_cooldown: 0,
            inputlock: false,
            keys: Held::default(),
            keyboard: Held::default(),
            buttons: Buttons::default(),
            turning: None,
            title_voice: TITLE_VOICE,
            hud: Hud { levelreached: 0, rankbar: 0.0..1.0, levelupflash: 0.0, newbestflash: 0.0, rankupflash: 0.0, menuslide: 0.0 },
            unlock: Unlock::None,
            unlocktimer: 0,
            announce: None,
            gave_up: false,
            events: Vec::with_capacity(64),
        };
        g.audio.set_volumes(g.save.settings.music_volume, g.save.settings.sound_volume);
        g.audio.set_speed(g.speed());
        g
    }

    fn sounds(&self, ids: &[SoundId]) {
        for &id in ids {
            self.audio.sfx(id);
        }
    }

    // ---------------------------------------------------------------------------------------
    // what the screens show

    pub fn pack(&self) -> &'static Pack {
        self.pack
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn save(&self) -> &Save {
        &self.save
    }

    /// How fast the game runs.
    pub fn speed(&self) -> f64 {
        self.speed_override.unwrap_or(self.save.settings.speed)
    }

    /// Whether the speed is set from the command line.
    pub fn speed_overridden(&self) -> bool {
        self.speed_override.is_some()
    }

    /// Runs below 1X don't count towards records or unlocks.
    fn keeps_progress(&self) -> bool {
        self.speed() >= 1.0
    }

    /// Debugging: no collisions.
    pub fn set_god(&mut self, on: bool) {
        self.world.set_god(on);
    }

    pub fn best(&self, li: LevelId) -> i64 {
        self.save.progress.best.get(&self.pack.levels[li].id).copied().unwrap_or(0)
    }

    pub fn completed(&self, li: LevelId) -> bool {
        self.save.progress.completed.contains(&self.pack.levels[li].id)
    }

    pub fn unlocked(&self, li: LevelId) -> bool {
        self.pack.levels[li].unlock.is_none_or(|u| self.completed(u))
    }

    /// Whether a level that leads to the ending has been completed.
    pub fn ending_seen(&self) -> bool {
        self.pack.levels.iter_enumerated().any(|(i, l)| l.completion.as_ref().is_some_and(|c| c.leads_to_ending) && self.completed(i))
    }

    /// Whether the announcement of an unlock can be dismissed.
    pub fn unlock_ready(&self) -> bool {
        self.unlocktimer >= UNLOCK_WAIT
    }

    /// Whether a display setting has changed since the last call (true at first).
    pub fn take_display_changed(&mut self) -> bool {
        std::mem::take(&mut self.display_changed)
    }

    pub fn credits_pages(&self) -> usize {
        1 + self.pack.text.credits.testers.len().div_ceil(12) + self.ending_seen() as usize
    }

    /// The controls as shown on screen.
    pub fn keys(&self) -> Held {
        Held { left: self.turning == Some(Turn::Left), right: self.turning == Some(Turn::Right), ..self.keys }
    }

    // ---------------------------------------------------------------------------------------
    // time

    /// A control pressed or released.
    pub fn key(&mut self, key: Key, down: bool) {
        let k = &mut self.keyboard;
        match key {
            Key::Left => k.left = down,
            Key::Right => k.right = down,
            Key::Up => k.up = down,
            Key::Down => k.down = down,
            Key::Select => k.select = down,
            Key::Quit => k.quit = down,
            Key::Clear => k.clear = down,
        }
        self.update_keys(match (key, down) {
            (Key::Left, true) => Some(Turn::Left),
            (Key::Right, true) => Some(Turn::Right),
            _ => None,
        });
    }

    /// A mouse button pressed or released.
    pub fn button(&mut self, button: Button, down: bool) {
        let before = self.mouse_keys();
        let b = &mut self.buttons;
        match button {
            Button::Left => b.left = down,
            Button::Middle => b.middle = down,
            Button::Right => b.right = down,
        }
        let after = self.mouse_keys();
        self.update_keys(if after.left && !before.left {
            Some(Turn::Left)
        } else if after.right && !before.right {
            Some(Turn::Right)
        } else {
            None
        });
    }

    /// The controls the mouse buttons hold, as the original: in a run the left and right buttons
    /// turn (and select), elsewhere the left one selects and the right one moves right; the
    /// middle one quits.
    fn mouse_keys(&self) -> Held {
        let b = self.buttons;
        if self.world.scene() == Scene::Run {
            Held { left: b.left, right: b.right, select: b.left || b.right, quit: b.middle, ..Held::default() }
        } else {
            Held { right: b.right, select: b.left, quit: b.middle, ..Held::default() }
        }
    }

    /// Merges the keyboard and the mouse. Of left and right, the most recently pressed one wins:
    /// `pressed`, if one just was.
    fn update_keys(&mut self, pressed: Option<Turn>) {
        let k = self.keyboard | self.mouse_keys();
        self.keys = k;
        let held = |t| if t == Turn::Left { k.left } else { k.right };
        let any_held = if k.left { Some(Turn::Left) } else if k.right { Some(Turn::Right) } else { None };
        self.turning = pressed.or(self.turning.filter(|&t| held(t))).or(any_held);
        self.world.set_input(self.turning, k.left, k.right);
    }

    pub fn advance_to(&mut self, t: f64) {
        while self.world.t() < t {
            let before = self.world.t();
            self.world.integrate_to(t);
            self.animate(self.world.t() - before);
            self.handle_events();
            if self.world.tick_due() {
                self.tick();
            }
        }
    }

    fn animate(&mut self, dt: f64) {
        let h = &mut self.hud;
        h.menuslide = (h.menuslide - dt).max(0.0);
        h.levelupflash = (h.levelupflash - dt).max(0.0);
        h.newbestflash = (h.newbestflash - dt).max(0.0);
        h.rankupflash = (h.rankupflash - 3.0 * dt).max(0.0);
    }

    fn tick(&mut self) {
        // what the mouse buttons do depends on the scene, which may have changed
        self.update_keys(None);
        let keys = self.menu_keys();
        match self.world.scene() {
            Scene::Title => self.title_input(keys),
            Scene::StageSelect => self.stage_select_input(keys),
            Scene::Run => self.run_input(keys),
        }
        if self.title_voice > 0 {
            self.title_voice -= 1;
            if self.title_voice == 0 {
                self.sounds(&self.pack.roles.title);
            }
        }
        if self.world.scene() == Scene::StageSelect {
            let want = match self.pack.slot(self.world.slot()) {
                Some(li) if self.unlocked(li) => self.pack.levels[li].palette,
                _ => self.pack.menu_palette,
            };
            self.world.show_palette(want);
        }

        self.audio.tick();
        self.world.tick(self.audio.music_position());
        self.handle_events();

        // the game over's own flow
        let w = &self.world;
        if w.scene() == Scene::Run && !w.alive() && !w.tutorial() && w.over() >= world::PULL_BACK_AT {
            if self.unlock == Unlock::Ending && w.view().zoom >= ENDING_ZOOM {
                self.start_ending();
            } else if w.view().zoom < GAME_OVER_ZOOM {
                self.hud.menuslide = MENU_SLIDE;
            }
        }
    }

    fn handle_events(&mut self) {
        let mut events = std::mem::take(&mut self.events);
        self.world.take_events(&mut events);
        for &e in &events {
            match e {
                Event::Sound(id) => self.audio.sfx(id),
                Event::Music { track, offset_ms } => self.audio.play_music(track, offset_ms),
                Event::MusicStop => self.audio.stop_music(),
                Event::MusicFadeOut => self.audio.fade_out_music(),
                Event::NewRecord => self.hud.newbestflash = NEW_RECORD_FLASH,
                Event::RankUp(rank) => self.rank_up(rank),
                Event::Completed => self.completed_level(),
                Event::GameOver => {
                    match self.unlock {
                        _ if self.gave_up => {}
                        Unlock::None => self.sounds(&self.pack.roles.game_over),
                        Unlock::Level | Unlock::GameComplete => self.sounds(&self.pack.roles.unlock),
                        Unlock::Ending => {}
                    }
                    self.gave_up = false;
                    if self.keeps_progress() {
                        self.record_best();
                        self.save.write();
                    }
                }
                Event::EndingDone => {
                    self.unlock = Unlock::GameComplete;
                    self.announce = Some(Announce::GameComplete);
                    self.sounds(&self.pack.roles.unlock);
                }
                Event::TutorialDone => {
                    self.tutorial_seen = true;
                    if self.keeps_progress() {
                        self.save.progress.tutorial_done = true;
                        self.save.write();
                    }
                }
            }
        }
        self.events = events;
    }

    fn rank_up(&mut self, rank: usize) {
        let pack = self.pack;
        let h = &mut self.hud;
        h.rankupflash = RANK_UP_FLASH;
        if rank <= pack.completing_rank && h.levelreached < rank {
            h.levelreached = rank;
            h.levelupflash = LEVEL_UP_FLASH;
            let at = pack.ranks[rank].at;
            h.rankbar = at as f64..pack.ranks.get(rank + 1).map_or(at, |n| n.at) as f64;
        }
    }

    fn record_best(&mut self) {
        let id = &self.pack.levels[self.world.run_level()].id;
        let best = self.world.best();
        if self.save.progress.best.get(id).is_none_or(|&b| b < best) {
            self.save.progress.best.insert(id.clone(), best);
        }
    }

    fn completed_level(&mut self) {
        let l = &self.pack.levels[self.world.run_level()];
        self.save.progress.completed.insert(l.id.clone());
        let c = l.completion.as_ref();
        self.announce = c.map(|c| c.announce);
        self.unlock = match c {
            Some(c) if c.leads_to_ending => Unlock::Ending,
            Some(c) if c.announce == Announce::GameComplete => Unlock::GameComplete,
            _ => Unlock::Level,
        };
        self.record_best();
        self.save.write();
    }

    // ---------------------------------------------------------------------------------------
    // runs

    pub fn start_run(&mut self, li: LevelId) {
        let pack = self.pack;
        let best = self.best(li);
        // below 1X, a level can't be completed (as if it already had been)
        let first_completion = !self.completed(li) && self.keeps_progress();
        let tutorial = !self.save.progress.tutorial_done && !self.tutorial_seen;
        self.world.start_run(RunSetup { level: li, best, first_completion, tutorial });
        self.unlock = Unlock::None;
        self.unlocktimer = 0;
        self.announce = None;
        self.gave_up = false;

        // ranks reached before, for the HUD
        let c = pack.completing_rank;
        let reached = (1..pack.ranks.len()).take_while(|&k| best > pack.ranks[k].at).count();
        let to = if reached < c { pack.ranks[reached + 1].at } else { best };
        self.hud = Hud { levelreached: reached, rankbar: 0.0..to as f64, levelupflash: 0.0, newbestflash: 0.0, rankupflash: 0.0, menuslide: 0.0 };
    }

    fn start_ending(&mut self) {
        if let Some(li) = self.pack.ending {
            self.start_run(li);
            self.world.flash(ENDING_FLASH);
        }
    }

    // ---------------------------------------------------------------------------------------
    // menus

    /// The controls as menus see them this tick.
    fn menu_keys(&mut self) -> Held {
        let k = self.keys;
        if self.inputlock {
            if k == Held::default() {
                self.inputlock = false;
            }
            return Held::default();
        }
        self.keys()
    }

    /// A menu move from the held controls, with the menus' repeat delay.
    fn menu_step(&mut self, k: Held, vertical: bool) -> i32 {
        if self.menu_cooldown > 0 {
            self.menu_cooldown -= 1;
            return 0;
        }
        let d = if k.left || (vertical && k.up) {
            -1
        } else if k.right || (vertical && k.down) {
            1
        } else {
            0
        };
        if d != 0 {
            self.menu_cooldown = MENU_REPEAT;
            self.sounds(&self.pack.roles.menu_move);
        }
        d
    }

    fn open(&mut self, menu: Menu, cursor: usize) {
        self.menu = menu;
        self.cursor = cursor;
        self.page = 0;
        self.inputlock = true;
    }

    fn go_title(&mut self, menu: Menu, cursor: usize) {
        self.world.enter_title();
        self.open(menu, cursor);
    }

    fn go_stage_select(&mut self, slot: usize) {
        self.world.enter_stage_select(slot);
        self.inputlock = true;
    }

    pub fn toggle_fullscreen(&mut self) {
        self.save.settings.fullscreen = !self.save.settings.fullscreen;
        self.save.write();
        self.display_changed = true;
    }

    fn title_input(&mut self, k: Held) {
        let pack = self.pack;
        match self.menu {
            Menu::Main => {
                let d = self.menu_step(k, false);
                self.cursor = (self.cursor as i32 + d).rem_euclid(MAIN_MENU_ITEMS as i32) as usize;
                if k.select {
                    self.sounds(&pack.roles.menu_select);
                    match self.cursor {
                        0 => self.go_stage_select(0),
                        1 => self.open(Menu::Options, 0),
                        2 => self.open(Menu::Extras, 0),
                        _ => self.open(Menu::Credits, 0),
                    }
                }
                if k.quit {
                    self.quit = true;
                }
            }
            Menu::Options => {
                let d = self.menu_step(k, true);
                self.cursor = (self.cursor as i32 + d).rem_euclid(OPTIONS as i32) as usize;
                if k.select {
                    self.sounds(&pack.roles.menu_select);
                    self.inputlock = true;
                    let s = &mut self.save.settings;
                    match self.cursor {
                        0 => s.fullscreen = !s.fullscreen,
                        1 => {
                            // the next supported mode, wrapping round to the first
                            let i = VSYNCS.iter().position(|&v| v == s.vsync_mode).unwrap();
                            s.vsync_mode = VSYNCS[i + 1..].iter().chain(&VSYNCS).copied().find(|v| self.vsyncs.contains(v)).unwrap();
                        }
                        2 => s.music_volume = (s.music_volume + 1) % (MAX_VOLUME + 1),
                        3 => s.sound_volume = (s.sound_volume + 1) % (MAX_VOLUME + 1),
                        _ => {
                            self.world.show_palette(pack.warning_palette);
                            self.open(Menu::Delete, 0);
                            return;
                        }
                    }
                    self.audio.set_volumes(self.save.settings.music_volume, self.save.settings.sound_volume);
                    self.save.write();
                    self.display_changed = true;
                }
                if k.quit {
                    self.sounds(&pack.roles.rank_up);
                    self.open(Menu::Main, 1);
                }
            }
            Menu::Extras => {
                let d = self.menu_step(k, true);
                self.cursor = (self.cursor as i32 + d).rem_euclid(EXTRAS as i32) as usize;
                if k.select {
                    self.sounds(&pack.roles.menu_select);
                    self.inputlock = true;
                    let s = &mut self.save.settings;
                    match self.cursor {
                        0 => s.black_bars = !s.black_bars,
                        1 => s.aberration = !s.aberration,
                        2 => s.bloom = !s.bloom,
                        3 => {
                            // the next supported sample count, wrapping round to the first
                            let counts = &self.sample_counts;
                            s.antialiasing = counts.iter().copied().find(|&n| n > s.antialiasing).unwrap_or(counts[0]);
                        }
                        4 => s.show_fps = !s.show_fps,
                        _ => {
                            // the next speed up, wrapping round to the slowest
                            if self.speed_override.is_none() {
                                s.speed = SPEEDS.iter().map(|&(_, x)| x).find(|&x| x > s.speed).unwrap_or(SPEEDS[0].1);
                                self.audio.set_speed(s.speed);
                            }
                        }
                    }
                    self.save.write();
                }
                if k.quit {
                    self.sounds(&pack.roles.rank_up);
                    self.open(Menu::Main, 2);
                }
            }
            Menu::Credits => {
                let d = self.menu_step(k, false);
                let n = self.credits_pages();
                self.page = (self.page as i32 + d).rem_euclid(n as i32) as usize;
                if k.select && self.ending_seen() && self.page == n - 1 {
                    self.start_ending();
                }
                if k.quit {
                    self.sounds(&pack.roles.rank_up);
                    self.open(Menu::Main, 3);
                }
                if k.clear {
                    self.sounds(&pack.roles.menu_select);
                    self.world.show_palette(pack.warning_palette);
                    self.open(Menu::Delete, 0);
                }
            }
            Menu::Delete => {
                if k.select {
                    self.sounds(&pack.roles.die);
                    self.save.clear();
                    self.save.write();
                    self.world.flash(world::FLASH);
                    self.world.show_palette(pack.menu_palette);
                    self.open(Menu::Main, 0);
                }
                if k.quit {
                    self.sounds(&pack.roles.rank_up);
                    self.world.show_palette(pack.menu_palette);
                    self.open(Menu::Main, 0);
                }
            }
        }
    }

    fn stage_select_input(&mut self, k: Held) {
        let pack = self.pack;
        if self.menu_cooldown > 0 {
            self.menu_cooldown -= 1;
        } else {
            let way = if k.left { Some(Turn::Left) } else if k.right { Some(Turn::Right) } else { None };
            if way.is_some_and(|w| self.world.turn_select(w)) {
                self.menu_cooldown = MENU_REPEAT;
                self.sounds(&pack.roles.menu_move);
            }
        }
        if k.select
            && let Some(li) = pack.slot(self.world.slot()).filter(|&li| self.unlocked(li)) {
                self.world.flash(world::FLASH);
                self.start_run(li);
                return;
            }
        if k.quit {
            self.sounds(&pack.roles.rank_up);
            self.go_title(Menu::Main, 0);
        }
    }

    fn run_input(&mut self, k: Held) {
        let pack = self.pack;
        if self.world.alive() {
            if k.quit && !self.world.ending() {
                self.gave_up = true;
                self.world.give_up();
                self.inputlock = true;
            }
            return;
        }
        let run_slot = pack.levels[self.world.run_level()].menu.as_ref().map_or(0, |m| m.slot);
        if self.unlock != Unlock::None {
            if self.world.view().zoom < world::ZOOM_OUT {
                return;
            }
            self.unlocktimer += 1;
            if self.unlocktimer < UNLOCK_WAIT || !k.select {
                return;
            }
            self.sounds(&pack.roles.rank_up);
            match self.unlock {
                Unlock::GameComplete => self.go_title(Menu::Credits, 0),
                _ => {
                    // point at what this level unlocked
                    let run = self.world.run_level();
                    let slot = pack.levels.iter().find(|l| l.unlock == Some(run)).and_then(|l| l.menu.as_ref()).map_or(run_slot, |m| m.slot);
                    self.go_stage_select(slot);
                    self.world.flash(world::FLASH);
                    self.world.reset_music();
                }
            }
            return;
        }
        if k.quit {
            self.sounds(&pack.roles.rank_up);
            self.go_stage_select(run_slot);
            self.world.flash(world::FLASH);
            self.world.reset_music();
            return;
        }
        if (k.up || k.select) && self.world.view().zoom >= world::ZOOM_OUT {
            self.start_run(self.world.run_level());
        }
    }
}
