//! Small on-disk memory: where each video was stopped, plus a few settings.
//!
//! Files live in the per-user config folder (`%APPDATA%\VPlayer`,
//! `~/Library/Application Support/VPlayer`, `~/.config/vplayer`, or the app's
//! private folder on Android):
//! `positions.tsv` holds `milliseconds<TAB>location` lines, newest last, and
//! `settings.txt` holds `key=value` lines.

use std::path::PathBuf;

/// How many videos to remember positions for.
const MAX_POSITIONS: usize = 500;
/// Positions this close to the start are not worth resuming.
const MIN_RESUME_MS: i64 = 5_000;
/// Positions this close to the end count as "watched to the end"
/// (or the last tenth, for short clips).
const END_MARGIN_MS: i64 = 15_000;

/// The app's private data folder, set by the Android entry point.
#[cfg(target_os = "android")]
pub static ANDROID_DATA_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    return ANDROID_DATA_DIR.get().cloned();
    #[allow(unreachable_code)]
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("APPDATA").map(|d| d.join("VPlayer"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/VPlayer"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config"))).map(|d| d.join("vplayer"))
    }
}

#[derive(Default)]
pub struct Store {
    dir: Option<PathBuf>,
    positions: Vec<(String, i64)>,
    pub volume: Option<i32>,
    pub audio_device: Option<String>,
    dirty: bool,
}

impl Store {
    pub fn load() -> Self {
        Self::load_from(config_dir())
    }

    pub fn load_from(dir: Option<PathBuf>) -> Self {
        let mut store = Self { dir, ..Default::default() };
        let Some(dir) = store.dir.clone() else { return store };
        if let Ok(text) = std::fs::read_to_string(dir.join("positions.tsv")) {
            for line in text.lines() {
                if let Some((ms, loc)) = line.split_once('\t') {
                    if let Ok(ms) = ms.parse() {
                        store.positions.push((loc.to_owned(), ms));
                    }
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("settings.txt")) {
            for line in text.lines() {
                match line.split_once('=') {
                    Some(("volume", v)) => store.volume = v.trim().parse().ok(),
                    Some(("audio_device", v)) if !v.is_empty() => store.audio_device = Some(v.to_owned()),
                    _ => {}
                }
            }
        }
        store
    }

    /// Saved position for `location`, if it is worth resuming from.
    pub fn position(&self, location: &str) -> Option<i64> {
        self.positions.iter().rev().find(|(l, _)| l == location).map(|&(_, ms)| ms)
    }

    /// Remembers where `location` was stopped. Positions near the start or the
    /// end (`length` known) clear the entry instead.
    pub fn set_position(&mut self, location: &str, ms: i64, length: Option<i64>) {
        let finished = length.is_some_and(|len| ms >= len - END_MARGIN_MS.min(len / 10));
        if ms < MIN_RESUME_MS || finished {
            self.forget(location);
            return;
        }
        if self.positions.last().is_some_and(|(l, old)| l == location && (old - ms).abs() < 1000) {
            return;
        }
        self.positions.retain(|(l, _)| l != location);
        self.positions.push((location.to_owned(), ms));
        if self.positions.len() > MAX_POSITIONS {
            self.positions.remove(0);
        }
        self.dirty = true;
    }

    pub fn forget(&mut self, location: &str) {
        let before = self.positions.len();
        self.positions.retain(|(l, _)| l != location);
        self.dirty |= self.positions.len() != before;
    }

    pub fn set_volume(&mut self, volume: i32) {
        if self.volume != Some(volume) {
            self.volume = Some(volume);
            self.dirty = true;
        }
    }

    pub fn set_audio_device(&mut self, device: Option<String>) {
        if self.audio_device != device {
            self.audio_device = device;
            self.dirty = true;
        }
    }

    /// Writes both files if anything changed since the last save.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(dir) = &self.dir else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let mut positions = String::new();
        for (loc, ms) in &self.positions {
            // A location with a newline could not be read back; skip it.
            if !loc.contains(['\n', '\r']) {
                positions.push_str(&format!("{ms}\t{loc}\n"));
            }
        }
        let mut settings = String::new();
        if let Some(v) = self.volume {
            settings.push_str(&format!("volume={v}\n"));
        }
        if let Some(d) = &self.audio_device {
            settings.push_str(&format!("audio_device={d}\n"));
        }
        let ok = write_atomic(&dir.join("positions.tsv"), &positions)
            && write_atomic(&dir.join("settings.txt"), &settings);
        if ok {
            self.dirty = false;
        }
    }
}

fn write_atomic(path: &std::path::Path, text: &str) -> bool {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_and_rules() {
        let dir = std::env::temp_dir().join(format!("vplayer-store-{}", std::process::id()));
        let mut s = Store::load_from(Some(dir.clone()));
        s.set_position("/v/a.mkv", 60_000, Some(600_000));
        s.set_position("/v/b.mkv", 2_000, Some(600_000)); // too early: not stored
        s.set_position("/v/c.mkv", 595_000, Some(600_000)); // near the end: not stored
        s.set_volume(65);
        s.set_audio_device(Some("hw:1,0".into()));
        s.save();

        let mut t = Store::load_from(Some(dir.clone()));
        assert_eq!(t.position("/v/a.mkv"), Some(60_000));
        assert_eq!(t.position("/v/b.mkv"), None);
        assert_eq!(t.position("/v/c.mkv"), None);
        assert_eq!(t.volume, Some(65));
        assert_eq!(t.audio_device.as_deref(), Some("hw:1,0"));

        // Watching to the end forgets the saved position.
        t.set_position("/v/a.mkv", 599_000, Some(600_000));
        assert_eq!(t.position("/v/a.mkv"), None);
        std::fs::remove_dir_all(dir).ok();
    }
}
