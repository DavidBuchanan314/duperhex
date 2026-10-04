//! Settings (settings.json) and per-pack progress (saves/PACK_ID.json), as JSON in SDL's per-user
//! preferences directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const MAX_VOLUME: u32 = 10;

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fullscreen: bool,
    pub vsync: bool,
    /// 0..=MAX_VOLUME
    pub music_volume: u32,
    pub sound_volume: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { fullscreen: false, vsync: true, music_volume: MAX_VOLUME, sound_volume: MAX_VOLUME }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Progress {
    /// Best survival time per level id, in ticks.
    pub best: BTreeMap<String, i64>,
    pub completed: BTreeSet<String>,
    pub tutorial_done: bool,
}

pub struct Save {
    settings_path: PathBuf,
    progress_path: PathBuf,
    pub settings: Settings,
    pub progress: Progress,
}

/// A file's contents, or the defaults if it's missing or unreadable.
fn read<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = std::fs::read(path) else { return T::default() };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        eprintln!("{}: {e}; starting afresh", path.display());
        T::default()
    })
}

fn write(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value)?)
}

impl Save {
    pub fn load(pack_id: &str) -> Save {
        let dir = sdl3::filesystem::get_pref_path("", "duperhex").unwrap_or_else(|_| PathBuf::from("."));
        let settings_path = dir.join("settings.json");
        let progress_path = dir.join("saves").join(format!("{pack_id}.json"));
        let mut settings: Settings = read(&settings_path);
        settings.music_volume = settings.music_volume.min(MAX_VOLUME);
        settings.sound_volume = settings.sound_volume.min(MAX_VOLUME);
        Save { progress: read(&progress_path), settings, settings_path, progress_path }
    }

    pub fn write(&self) {
        let results = [(&self.settings_path, write(&self.settings_path, &self.settings)), (&self.progress_path, write(&self.progress_path, &self.progress))];
        for (path, r) in results {
            if let Err(e) = r {
                eprintln!("saving {}: {e}", path.display());
            }
        }
    }

    /// Forgets records and unlocks (not settings, and not the tutorial).
    pub fn clear(&mut self) {
        self.progress.best.clear();
        self.progress.completed.clear();
    }
}
