//! Installed packs: checked copies kept as packs/PACK_ID.zip in SDL's per-user preferences
//! directory, so the game can find one without being told where it is.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::pack::Pack;
use crate::save;

const INSTALL_HINT: &str = "run duperhex --install PACK.zip";

fn dir() -> PathBuf {
    save::pref_dir().join("packs")
}

/// Whether an id can name a file in the packs directory as it is.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// Checks the pack at `path` and installs a copy, replacing any installed pack with the same id,
/// and makes it the default. Returns the pack's id and where it went.
pub fn install(path: &Path) -> Result<(String, PathBuf), String> {
    let (pack, _) = Pack::read(path).map_err(|e| e.to_string())?;
    if !valid_id(&pack.id) {
        return Err(format!("{}: can't install a pack with id {:?}", path.display(), pack.id));
    }
    let dir = dir();
    let dest = dir.join(format!("{}.zip", pack.id));
    // copied under a name nothing loads, then renamed into place, so a failed copy can't leave a
    // broken pack installed
    let tmp = dir.join(format!(".{}.zip.tmp", pack.id));
    let r = std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(path, &tmp)).and_then(|_| std::fs::rename(&tmp, &dest));
    if let Err(e) = r {
        std::fs::remove_file(&tmp).ok();
        return Err(format!("installing to {}: {e}", dest.display()));
    }
    save::set_last_pack(&pack.id);
    Ok((pack.id.clone(), dest))
}

/// Installed packs' ids, each with when it was installed.
fn list() -> Vec<(String, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut packs: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".zip").filter(|id| valid_id(id))?;
            Some((id.to_string(), e.metadata().ok()?.modified().ok()?))
        })
        .collect();
    packs.sort();
    packs
}

/// The installed pack with this id, or with none given, the last installed or run by id (or if
/// that's gone, the latest installed).
pub fn find(id: Option<&str>) -> Result<PathBuf, String> {
    let packs = list();
    let found = match id {
        Some(id) => packs.iter().find(|(p, _)| p == id),
        None => {
            let last = save::last_pack();
            packs.iter().find(|(p, _)| Some(p) == last.as_ref()).or_else(|| packs.iter().max_by_key(|(_, t)| *t))
        }
    };
    if let Some((id, _)) = found {
        return Ok(dir().join(format!("{id}.zip")));
    }
    if packs.is_empty() {
        return Err(format!("no pack installed; {INSTALL_HINT}"));
    }
    let ids: Vec<_> = packs.iter().map(|(p, _)| p.as_str()).collect();
    Err(format!("pack {:?} is not installed (installed: {})", id.unwrap_or_default(), ids.join(", ")))
}
