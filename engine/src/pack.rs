//! Asset pack loading and checking. The files are read with serde into their
//! own shapes, then every name in them is resolved to a typed id, so the game never looks anything
//! up by name. Scripts stay JSON until `script::Names` parses them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;

use crate::ids::{DirectorId, Id, IdVec, LevelId, PatternId, RotationId, SoundId, TrackId};
use crate::script::{Action, CounterId, Counters, Names, Val};
use crate::world::SLOTS;
use crate::weighted::Weighted;

pub const FORMAT: u32 = 3;
/// Walls' sides are below this, outside a `rotate`.
const PATTERN_SIDES: u32 = 6;

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}

trait Context<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T, Error>;
}

impl<T> Context<T> for Result<T, Error> {
    fn ctx(self, what: impl fmt::Display) -> Result<T, Error> {
        self.map_err(|e| Error(format!("{what}: {}", e.0)))
    }
}

/// A colour, each channel 0 to 255.
pub type Rgb = [f64; 3];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(transparent)]
pub struct PaletteId(pub i32);

#[derive(Deserialize)]
pub struct RotationMode {
    #[serde(flatten)]
    pub motion: Motion,
    pub sway: f64,
    pub burst: f64,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Motion {
    Spin { spin: f64 },
    Settle { settle: f64, settle_rate: f64 },
}

pub struct Rank {
    pub name: String,
    pub at: i64,
    pub voice: Option<SoundId>,
    pub completes_level: bool,
}

#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Announce {
    NewHyper,
    SidesComplete,
    GameComplete,
    Congratulations,
}

#[derive(Deserialize)]
pub struct Completion {
    pub announce: Announce,
    #[serde(default)]
    pub finale: bool,
    /// Completing the level leads to the ending.
    #[serde(default, rename = "ending")]
    pub leads_to_ending: bool,
}

/// A colour channel that may pulse with the GUI glow: base + factor * glow.
#[derive(Clone, Copy, Deserialize)]
#[serde(from = "RawChannel")]
pub struct Channel {
    pub base: f64,
    pub glow: f64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawChannel {
    Plain(f64),
    Glow(f64, f64),
}

impl From<RawChannel> for Channel {
    fn from(c: RawChannel) -> Channel {
        match c {
            RawChannel::Plain(base) => Channel { base, glow: 0.0 },
            RawChannel::Glow(base, glow) => Channel { base, glow },
        }
    }
}

/// A colour that may pulse with the GUI glow, or follow the palette.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Colour {
    Slot { slot: usize },
    Rgb([Channel; 3]),
}

#[derive(Deserialize)]
#[serde(from = "RawMenuColour")]
pub struct MenuColour {
    pub frames: Vec<Colour>,
    /// Ticks per frame.
    pub ticks: f64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawMenuColour {
    Cycle { cycle: Vec<Colour>, ticks: f64 },
    Single(Colour),
}

impl From<RawMenuColour> for MenuColour {
    fn from(c: RawMenuColour) -> MenuColour {
        match c {
            RawMenuColour::Cycle { cycle, ticks } => MenuColour { frames: cycle, ticks },
            RawMenuColour::Single(c) => MenuColour { frames: vec![c], ticks: 1.0 },
        }
    }
}

#[derive(Deserialize)]
pub struct Menu {
    pub slot: usize,
    pub name: String,
    pub difficulty: String,
    pub badge: Option<String>,
    pub colour: MenuColour,
    pub border: bool,
    pub button_text: Rgb,
    pub player_slot: usize,
}

#[derive(Deserialize)]
pub struct GameOver {
    pub border: bool,
    pub button_text: Rgb,
    pub continue_text: Rgb,
}

pub struct Level {
    pub id: String,
    pub menu: Option<Menu>,
    pub game_over: GameOver,
    /// The level whose completion unlocks this one.
    pub unlock: Option<LevelId>,
    pub kind: LevelKind,
    pub music: Option<TrackId>,
    pub palette: PaletteId,
    pub turn_speed: f64,
    pub centre_flip: bool,
    pub rotation: Vec<RotationId>,
    pub start_wave: i64,
    pub start_speed: Option<f64>,
    pub time_offset: i64,
    pub beat_divisor: i32,
    pub frozen_pulse: Option<f64>,
    pub director: DirectorId,
    pub counters: Counters,
    /// The ending's per-tick script.
    pub on_tick: Vec<Action>,
    /// Other levels' progression: what happens at which `time`.
    pub timeline: Vec<(i64, Vec<Action>)>,
    pub camera_lean: Option<f64>,
    pub camera_sway: bool,
    pub completion: Option<Completion>,
}

pub struct Tutorial {
    pub counters: Counters,
    /// The counters that pace the prompts the engine shows.
    pub step: Option<CounterId>,
    pub slide: Option<CounterId>,
    pub on_tick: Vec<Action>,
    pub on_wave: Vec<Action>,
    pub on_death: Vec<Action>,
}

#[derive(Deserialize)]
pub struct CreditEntry {
    pub role: String,
    pub name: String,
    pub site: String,
}

#[derive(Deserialize)]
pub struct CompletionText {
    pub heading: String,
    pub level_complete: String,
    pub new_hyper: String,
    pub sides_complete: String,
    pub game_complete: String,
}

#[derive(Deserialize)]
pub struct CreditsText {
    pub title: String,
    pub thanks: String,
    pub main: Vec<CreditEntry>,
    pub testers_heading: String,
    pub testers: Vec<String>,
    pub rewatch_ending: String,
}

#[derive(Deserialize)]
pub struct Text {
    pub completion: CompletionText,
    pub credits: CreditsText,
}

pub struct Finale {
    pub on_hit: Vec<Action>,
    pub on_tick: Vec<Action>,
    pub on_wave: Vec<Action>,
}

pub enum WallSpec {
    Wall { side: i32, dist: f64, len: f64 },
    Event { kind: EventKind, at: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Shrink,
    Grow,
    ZoomPulse,
}

pub enum Choice {
    Rotate(u32, Box<Body>),
    Variants(Weighted<Body>),
}

pub struct Body {
    pub walls: Vec<WallSpec>,
    pub choice: Option<Choice>,
    pub delay_distance: Option<f64>,
    pub hold_until_morphed: Option<bool>,
    pub speed_ramp: Option<f64>,
}

#[derive(Deserialize)]
pub struct Palette {
    pub start: Vec<Option<Rgb>>,
    pub end: Vec<Option<Rgb>>,
}

pub struct Track {
    pub file: String,
    pub length_ms: f64,
    pub beats: Vec<i32>,
    pub restart_points_ms: Weighted<f64>,
    pub looping: bool,
}

pub struct Sound {
    pub file: String,
}

/// The sounds the engine itself plays.
pub struct Roles {
    pub rank_up: Vec<SoundId>,
    pub level_start: Vec<SoundId>,
    pub switch_level: Vec<SoundId>,
    pub ending_start: Vec<SoundId>,
    pub new_record: Vec<SoundId>,
    pub die: Vec<SoundId>,
    pub game_over: Vec<SoundId>,
    pub unlock: Vec<SoundId>,
    pub title: Vec<SoundId>,
    pub menu_move: Vec<SoundId>,
    pub menu_select: Vec<SoundId>,
}

pub struct Pack {
    pub id: String,
    pub title: Vec<String>,
    pub font: Vec<u8>,
    pub text: Text,
    pub menu_palette: PaletteId,
    pub warning_palette: PaletteId,
    pub tutorial: Tutorial,
    pub rotation_modes: IdVec<RotationId, RotationMode>,
    /// The turning of the menus and the game over: the original's mode 1.
    pub idle_rotation: Option<RotationId>,
    pub ranks: Vec<Rank>,
    /// Index of the rank that completes a level.
    pub completing_rank: usize,
    pub levels: IdVec<LevelId, Level>,
    pub ending: Option<LevelId>,
    pub finale: Finale,
    pub directors: IdVec<DirectorId, Vec<Action>>,
    pub patterns: IdVec<PatternId, Body>,
    pub palettes: HashMap<PaletteId, Palette>,
    pub tracks: IdVec<TrackId, Track>,
    pub sounds: IdVec<SoundId, Sound>,
    pub roles: Roles,
}

/// The pack's media files (its music and sounds), for whatever plays them to take.
pub struct Files(HashMap<String, Vec<u8>>);

impl Files {
    /// A file's bytes, handed over: each file is taken once.
    pub fn take(&mut self, name: &str) -> Vec<u8> {
        self.0.remove(name).unwrap_or_else(|| {
            eprintln!("{name}: taken twice");
            Vec::new()
        })
    }
}

// --- the files' own shapes -------------------------------------------------------------------

#[derive(Deserialize)]
struct RawManifest {
    id: String,
    title: Vec<String>,
    files: RawFiles,
    font: String,
    rotation_modes: BTreeMap<i64, RotationMode>,
    ranks: Vec<RawRank>,
}

#[derive(Deserialize)]
struct RawFiles {
    levels: String,
    directors: String,
    patterns: String,
    palettes: String,
    audio: String,
    text: String,
}

#[derive(Deserialize)]
struct RawRank {
    name: String,
    at: i64,
    voice: Option<String>,
    #[serde(default)]
    completes_level: bool,
}

#[derive(Deserialize)]
struct RawAudio {
    music: BTreeMap<String, RawTrack>,
    sounds: BTreeMap<String, String>,
    roles: BTreeMap<String, OneOrMany>,
}

#[derive(Deserialize)]
struct RawTrack {
    file: String,
    length_ms: f64,
    beats: String,
    restart_points_ms: Vec<(u32, f64)>,
    #[serde(rename = "loop")]
    looping: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct RawPatterns {
    patterns: BTreeMap<String, RawBody>,
}

#[derive(Deserialize)]
struct RawBody {
    #[serde(default)]
    walls: Vec<RawWall>,
    rotate: Option<u32>,
    then: Option<Box<RawBody>>,
    variants: Option<Vec<(u32, RawBody)>>,
    delay_distance: Option<f64>,
    hold_until_morphed: Option<bool>,
    speed_ramp: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawWall {
    Wall(i64, f64, f64),
    Event { event: EventKind, at: f64 },
}

#[derive(Deserialize)]
struct RawDirectors {
    directors: BTreeMap<String, RawDirector>,
}

#[derive(Deserialize)]
struct RawDirector {
    on_wave: Json,
}

#[derive(Deserialize)]
struct RawLevels {
    levels: Vec<RawLevel>,
    tutorial: RawTutorial,
    finale: RawFinale,
}

#[derive(Deserialize)]
struct RawLevel {
    id: String,
    menu: Option<Menu>,
    game_over: GameOver,
    unlock: Option<RawUnlock>,
    #[serde(default)]
    kind: LevelKind,
    music: Option<String>,
    palette: PaletteId,
    turn_speed: f64,
    centre_flip: bool,
    rotation: Vec<i64>,
    start: RawStart,
    time_offset: i64,
    beat_divisor: i32,
    frozen_pulse: Option<f64>,
    director: String,
    counters: BTreeMap<String, Val>,
    #[serde(default)]
    on_tick: Json,
    #[serde(default)]
    timeline: Vec<RawEntry>,
    #[serde(default)]
    camera: RawCamera,
    completion: Option<Completion>,
}

#[derive(Clone, Copy, Deserialize, Default, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LevelKind {
    /// A level with a timeline and a director.
    #[default]
    Normal,
    /// The ending: its own per-tick script, no time or ranks.
    Ending,
}

#[derive(Deserialize)]
struct RawUnlock {
    completed: String,
}

#[derive(Deserialize)]
struct RawStart {
    wave: i64,
    speed: Option<f64>,
}

#[derive(Deserialize)]
struct RawEntry {
    at: i64,
    #[serde(rename = "do")]
    actions: Json,
}

#[derive(Deserialize, Default)]
struct RawCamera {
    lean: Option<f64>,
    #[serde(default)]
    sway: bool,
}

#[derive(Deserialize)]
struct RawTutorial {
    counters: BTreeMap<String, Val>,
    on_tick: Json,
    on_wave: Json,
    on_death: Json,
}

#[derive(Deserialize)]
struct RawFinale {
    on_hit: Json,
    on_tick: Json,
    on_wave: Json,
}

#[derive(Deserialize)]
struct RawPalettes {
    palettes: HashMap<PaletteId, Palette>,
    roles: RawPaletteRoles,
}

#[derive(Deserialize)]
struct RawPaletteRoles {
    menu: PaletteId,
    warning: PaletteId,
}

/// Numbers named entries in order, for looking them up by name.
fn numbered<I: Id>(names: impl IntoIterator<Item = String>) -> HashMap<String, I> {
    names.into_iter().enumerate().map(|(i, k)| (k, I::new(i))).collect()
}

impl Pack {
    /// The level in a stage select slot.
    pub fn slot(&self, slot: usize) -> Option<LevelId> {
        self.levels.position(|l| l.menu.as_ref().is_some_and(|m| m.slot == slot))
    }

    pub fn level(&self, id: &str) -> Option<LevelId> {
        self.levels.position(|l| l.id == id)
    }

    /// Loads a pack for the rest of the program's life, and its media files.
    pub fn load(path: &Path) -> Result<(&'static Pack, Files), Error> {
        let (pack, files) = Self::read(path)?;
        Ok((Box::leak(Box::new(pack)), files))
    }

    /// Reads and checks a pack.
    pub fn read(path: &Path) -> Result<(Pack, Files), Error> {
        Self::read_unnamed(path).ctx(path.display())
    }

    fn read_unnamed(path: &Path) -> Result<(Pack, Files), Error> {
        let mut zip = std::fs::File::open(path)
            .map_err(|e| Error(e.to_string()))
            .and_then(|f| zip::ZipArchive::new(f).map_err(|e| Error(e.to_string())))?;
        let mut files = HashMap::new();
        for i in 0..zip.len() {
            let mut e = zip.by_index(i).map_err(|e| Error(e.to_string()))?;
            if e.is_file() {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).map_err(|e| Error(e.to_string()))?;
                files.insert(e.name().to_string(), buf);
            }
        }
        fn json<T: DeserializeOwned>(files: &HashMap<String, Vec<u8>>, name: &str) -> Result<T, Error> {
            let bytes = files.get(name).ok_or_else(|| Error(format!("missing {name}")))?;
            serde_json::from_slice(bytes).map_err(|e| Error(format!("{name}: {e}")))
        }

        // the format first, as other formats may not read as this one
        #[derive(Deserialize)]
        struct RawFormat {
            format: u32,
        }
        let format: RawFormat = json(&files, "pack.json")?;
        if format.format < FORMAT {
            return err(format!("pack format {} is too old, expected {FORMAT}. Re-run the extractor.", format.format));
        }
        if format.format > FORMAT {
            return err(format!("pack format {} is too new, expected {FORMAT}. Update duperhex.", format.format));
        }
        let manifest: RawManifest = json(&files, "pack.json")?;
        let audio: RawAudio = json(&files, &manifest.files.audio)?;
        let patterns: RawPatterns = json(&files, &manifest.files.patterns)?;
        let directors: RawDirectors = json(&files, &manifest.files.directors)?;
        let levels: RawLevels = json(&files, &manifest.files.levels)?;
        let palettes: RawPalettes = json(&files, &manifest.files.palettes)?;
        let text: Text = json(&files, &manifest.files.text)?;

        // names first: scripts refer to patterns, tracks, sounds, levels, counters and rotations
        let counter_names = levels.levels.iter().map(|l| &l.counters).chain([&levels.tutorial.counters]).flat_map(|c| c.keys());
        let names = Names {
            patterns: numbered(patterns.patterns.keys().cloned()),
            tracks: numbered(audio.music.keys().cloned()),
            sounds: numbered(audio.sounds.keys().cloned()),
            levels: numbered(levels.levels.iter().map(|l| l.id.clone())),
            counters: Names::number_counters(counter_names.map(String::as_str))?,
            rotations: manifest.rotation_modes.keys().enumerate().map(|(i, &m)| (m, RotationId::new(i))).collect(),
        };
        if names.levels.len() != levels.levels.len() {
            return err("two levels with the same id");
        }
        let sound = |s: &str| Names::get(&names.sounds, "sound", s);

        let tracks = audio
            .music
            .into_iter()
            .map(|(k, t)| {
                let beats = json(&files, &t.beats).ctx(format!("track {k}"))?;
                let restart_points_ms = Weighted::new(t.restart_points_ms).ctx(format!("track {k}"))?;
                Ok(Track { file: t.file, length_ms: t.length_ms, beats, restart_points_ms, looping: t.looping })
            })
            .collect::<Result<IdVec<_, _>, Error>>()?;
        let sounds: IdVec<SoundId, Sound> = audio.sounds.into_values().map(|file| Sound { file }).collect();
        for f in tracks.iter().map(|t| &t.file).chain(sounds.iter().map(|s| &s.file)) {
            if !files.contains_key(f) {
                return err(format!("missing {f}"));
            }
        }
        let role = |k: &str| -> Result<Vec<SoundId>, Error> {
            match audio.roles.get(k) {
                None => Ok(Vec::new()),
                Some(OneOrMany::One(s)) => Ok(vec![sound(s)?]),
                Some(OneOrMany::Many(v)) => v.iter().map(|s| sound(s)).collect(),
            }
            .ctx(format!("sound role {k}"))
        };
        let roles = Roles {
            rank_up: role("rank_up")?,
            level_start: role("level_start")?,
            switch_level: role("switch_level")?,
            ending_start: role("ending_start")?,
            new_record: role("new_record")?,
            die: role("die")?,
            game_over: role("game_over")?,
            unlock: role("unlock")?,
            title: role("title")?,
            menu_move: role("menu_move")?,
            menu_select: role("menu_select")?,
        };

        let patterns =
            patterns.patterns.into_iter().map(|(k, b)| body(b, PATTERN_SIDES).ctx(format!("pattern {k}"))).collect::<Result<IdVec<_, _>, _>>()?;
        let director_names: HashMap<String, DirectorId> = numbered(directors.directors.keys().cloned());
        let directors = directors
            .directors
            .into_iter()
            .map(|(k, d)| names.actions(&d.on_wave).ctx(format!("director {k}")))
            .collect::<Result<IdVec<_, _>, _>>()?;

        let tu = &levels.tutorial;
        let tutorial = Tutorial {
            counters: names.counters(&tu.counters)?,
            step: names.counters.get("step").copied(),
            slide: names.counters.get("slide").copied(),
            on_tick: names.actions(&tu.on_tick).ctx("tutorial")?,
            on_wave: names.actions(&tu.on_wave).ctx("tutorial")?,
            on_death: names.actions(&tu.on_death).ctx("tutorial")?,
        };
        let fin = &levels.finale;
        let finale = Finale {
            on_hit: names.actions(&fin.on_hit).ctx("finale")?,
            on_tick: names.actions(&fin.on_tick).ctx("finale")?,
            on_wave: names.actions(&fin.on_wave).ctx("finale")?,
        };
        let levels = levels
            .levels
            .into_iter()
            .map(|l| {
                let id = l.id.clone();
                level(l, &names, &director_names).ctx(format!("level {id}"))
            })
            .collect::<Result<IdVec<_, _>, _>>()?;

        let ranks = manifest
            .ranks
            .into_iter()
            .map(|r| Ok(Rank { name: r.name, at: r.at, voice: r.voice.as_deref().map(sound).transpose()?, completes_level: r.completes_level }))
            .collect::<Result<Vec<_>, Error>>()
            .ctx("ranks")?;
        if ranks.is_empty() {
            return err("no ranks");
        }
        let completing_rank = ranks.iter().position(|r| r.completes_level).unwrap_or(ranks.len() - 1);
        let font = files.remove(&manifest.font).ok_or_else(|| Error(format!("missing {}", manifest.font)))?;
        // what's left is media: only what the pack plays
        let media: HashSet<&String> = tracks.iter().map(|t| &t.file).chain(sounds.iter().map(|s| &s.file)).collect();
        files.retain(|name, _| media.contains(name));

        let pack = Pack {
            id: manifest.id,
            title: manifest.title,
            font,
            text,
            menu_palette: palettes.roles.menu,
            warning_palette: palettes.roles.warning,
            tutorial,
            idle_rotation: names.rotations.get(&1).copied(),
            rotation_modes: manifest.rotation_modes.into_values().collect(),
            ranks,
            completing_rank,
            ending: levels.position(|l| l.kind == LevelKind::Ending),
            levels,
            finale,
            directors,
            patterns,
            palettes: palettes.palettes,
            tracks,
            sounds,
            roles,
        };
        pack.check()?;
        Ok((pack, Files(files)))
    }

    /// What can only be checked with the whole pack loaded.
    fn check(&self) -> Result<(), Error> {
        for id in [self.menu_palette, self.warning_palette] {
            if !self.palettes.contains_key(&id) {
                return err(format!("no palette {}", id.0));
            }
        }
        let mut slots = HashSet::new();
        for l in &self.levels {
            if let Some(m) = &l.menu {
                if !slots.insert(m.slot) {
                    return err(format!("two levels in menu slot {}", m.slot));
                }
                if m.player_slot >= SLOTS {
                    return err(format!("level {}: no palette slot {}", l.id, m.player_slot));
                }
            }
            if let Some(u) = l.unlock
                && self.levels[u].menu.is_none()
            {
                return err(format!("level {}: unlocked by {}, which is not in the stage select", l.id, self.levels[u].id));
            }
            if !self.palettes.contains_key(&l.palette) {
                return err(format!("level {}: no palette {}", l.id, l.palette.0));
            }
            // unlock chains end
            let mut seen = HashSet::new();
            let mut at = l.unlock;
            while let Some(u) = at {
                if !seen.insert(u) {
                    return err(format!("level {}: unlock chain loops", l.id));
                }
                at = self.levels[u].unlock;
            }
        }
        Ok(())
    }
}

fn level(
    l: RawLevel,
    names: &Names,
    directors: &HashMap<String, DirectorId>,
) -> Result<Level, Error> {
    Ok(Level {
        unlock: l.unlock.map(|u| Names::get(&names.levels, "level", &u.completed)).transpose()?,
        kind: l.kind,
        music: l.music.map(|t| Names::get(&names.tracks, "track", &t)).transpose()?,
        rotation: match &l.rotation[..] {
            [] => return err("no rotation modes"),
            modes => modes.iter().map(|&m| names.rotation(m)).collect::<Result<_, _>>()?,
        },
        director: Names::get(directors, "director", &l.director)?,
        counters: names.counters(&l.counters)?,
        on_tick: names.actions(&l.on_tick)?,
        timeline: l.timeline.into_iter().map(|e| Ok((e.at, names.actions(&e.actions)?))).collect::<Result<_, Error>>()?,
        id: l.id,
        menu: l.menu,
        game_over: l.game_over,
        palette: l.palette,
        turn_speed: l.turn_speed,
        centre_flip: l.centre_flip,
        start_wave: l.start.wave,
        start_speed: l.start.speed,
        time_offset: l.time_offset,
        beat_divisor: match l.beat_divisor {
            d if d < 1 => return err(format!("beat divisor {d}")),
            d => d,
        },
        frozen_pulse: l.frozen_pulse,
        camera_lean: l.camera.lean,
        camera_sway: l.camera.sway,
        completion: l.completion,
    })
}

/// A pattern body whose walls' sides are below `sides`.
fn body(b: RawBody, sides: u32) -> Result<Body, Error> {
    let walls = b
        .walls
        .into_iter()
        .map(|w| match w {
            RawWall::Wall(side, dist, len) if (0..sides as i64).contains(&side) => Ok(WallSpec::Wall { side: side as i32, dist, len }),
            RawWall::Wall(side, ..) => err(format!("wall side {side} out of range")),
            RawWall::Event { event, at } => Ok(WallSpec::Event { kind: event, at }),
        })
        .collect::<Result<_, _>>()?;
    let choice = match (b.rotate, b.then, b.variants) {
        (Some(n), Some(then), None) if n > 0 => Some(Choice::Rotate(n, Box::new(body(*then, n)?))),
        (None, None, Some(vs)) => {
            let vs = vs.into_iter().map(|(w, v)| Ok((w, body(v, PATTERN_SIDES)?))).collect::<Result<Vec<_>, Error>>()?;
            Some(Choice::Variants(Weighted::new(vs)?))
        }
        (None, None, None) => None,
        _ => return err("a pattern takes either rotate and then, or variants"),
    };
    Ok(Body { walls, choice, delay_distance: b.delay_distance, hold_until_morphed: b.hold_until_morphed, speed_ramp: b.speed_ramp })
}
