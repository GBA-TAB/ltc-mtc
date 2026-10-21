//! Shows: named snapshots of the whole device state (settings, MIDI and OSC routing, cues),
//! stored as JSON files in `shows/` next to the active config. Also the runtime state that
//! survives a restart: which show is loaded, and whether cues are active.

use crate::config::Config;
use crate::{App, EngineMsg};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Cues fire (true) or not (false). MTC runs either way.
    pub armed: bool,
    /// The show loaded or last saved; Save writes to it.
    pub show: Option<String>,
}

impl Default for State {
    fn default() -> State {
        // Before 0.4 there was no switch: cues were always active.
        State { armed: true, show: None }
    }
}

pub fn state_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("state.json")
}

pub fn shows_dir(config_path: &Path) -> PathBuf {
    config_path.with_file_name("shows")
}

pub fn load_state(config_path: &Path) -> State {
    std::fs::read_to_string(state_path(config_path))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Writes through a temporary file and a rename, like the config.
fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn save_state(app: &App) -> Result<(), String> {
    let st = State { armed: app.armed.load(Ordering::SeqCst), show: app.show.lock().unwrap().clone() };
    write_atomic(&state_path(&app.config_path), &serde_json::to_string_pretty(&st).unwrap())
}

/// Show names become file names: 1-64 characters, letters (any script), digits, space and
/// - _ . ( ) + &, not starting with a dot or a space.
pub fn valid_name(name: &str) -> bool {
    let n = name.chars().count();
    (1..=64).contains(&n)
        && !name.starts_with(['.', ' '])
        && !name.ends_with(' ')
        && name.chars().all(|c| c.is_alphanumeric() || " -_.()+&".contains(c))
}

fn show_path(app: &App, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("'{name}' is not a valid show name (letters, digits, space - _ . ( ) + &; up to 64)"));
    }
    Ok(shows_dir(&app.config_path).join(format!("{name}.json")))
}

#[derive(Serialize)]
pub struct ShowInfo {
    pub name: String,
    /// Milliseconds since 1970, for the browser's Date.
    pub modified: u64,
    pub events: usize,
}

pub fn list(app: &App) -> Vec<ShowInfo> {
    let mut v = Vec::new();
    let Ok(dir) = std::fs::read_dir(shows_dir(&app.config_path)) else { return v };
    for e in dir.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
        if !valid_name(name) {
            continue;
        }
        let modified = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let events = read(&p).map(|c| c.events.len()).unwrap_or(0);
        v.push(ShowInfo { name: name.to_string(), modified, events });
    }
    v.sort_by_key(|s| s.name.to_lowercase());
    v
}

fn read(path: &Path) -> Result<Config, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut c: Config = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    c.validate()?;
    Ok(c)
}

/// The show file's content, for download.
pub fn file(app: &App, name: &str) -> Result<String, String> {
    let p = show_path(app, name)?;
    std::fs::read_to_string(&p).map_err(|_| format!("no show '{name}'"))
}

/// Writes a config as a show file.
pub fn write(app: &App, name: &str, c: &Config) -> Result<(), String> {
    write_atomic(&show_path(app, name)?, &serde_json::to_string_pretty(c).unwrap())
}

/// Saves the active config as show `name` (the current show when None), which becomes the
/// current show.
pub fn save(app: &App, name: Option<&str>) -> Result<String, String> {
    let name = match name {
        Some(n) => n.trim().to_string(),
        None => app.show.lock().unwrap().clone().ok_or("no show loaded yet: use Save as")?,
    };
    let c = app.config.lock().unwrap().clone();
    write(app, &name, &c)?;
    *app.show.lock().unwrap() = Some(name.clone());
    save_state(app)?;
    Ok(name)
}

/// Makes show `name` the active config.
pub fn load(app: &App, name: &str) -> Result<Config, String> {
    let c = read(&show_path(app, name)?)?;
    let c = apply(app, c)?;
    *app.show.lock().unwrap() = Some(name.to_string());
    save_state(app)?;
    Ok(c)
}

pub fn delete(app: &App, name: &str) -> Result<(), String> {
    let p = show_path(app, name)?;
    std::fs::remove_file(&p).map_err(|_| format!("no show '{name}'"))?;
    let mut cur = app.show.lock().unwrap();
    if cur.as_deref() == Some(name) {
        *cur = None;
        drop(cur);
        save_state(app)?;
    }
    Ok(())
}

/// Validates a config, stores it as the active one and hands it to the audio and engine
/// threads.
pub fn apply(app: &App, mut c: Config) -> Result<Config, String> {
    c.validate()?;
    c.save(&app.config_path)?;
    let audio_changed = {
        let mut cur = app.config.lock().unwrap();
        let changed = cur.audio_device != c.audio_device
            || cur.audio_channel != c.audio_channel
            || cur.sample_rate != c.sample_rate;
        *cur = c.clone();
        changed
    };
    if audio_changed {
        app.audio_gen.fetch_add(1, Ordering::SeqCst);
    }
    let _ = app.engine_tx.lock().unwrap().send(EngineMsg::ConfigChanged);
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["Tour 2026", "Act 1 (rehearsal)", "Zürich-Halle_B", "a", "Hamlet + Ophelia"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", ".hidden", " lead", "trail ", "a/b", "..", "a\\b", "x:y", &"n".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
