//! Small on-disk memory: where each video was stopped, plus a few settings.
//!
//! Files live in the per-user config folder (`%APPDATA%\VPlayer`,
//! `~/Library/Application Support/VPlayer`, `~/.config/vplayer`, or the app's
//! private folder on Android):
//! `positions.tsv` holds `milliseconds<TAB>location` lines (`ms/length` once
//! the length is known), newest last, `watched.txt` the locations watched to
//! the end or nearly, one per line, newest last, and `settings.txt` holds
//! `key=value` lines.

use std::path::PathBuf;

/// How many videos to remember positions for.
const MAX_POSITIONS: usize = 500;
/// Positions this close to the start are not worth resuming.
const MIN_RESUME_MS: i64 = 5_000;
/// Positions this close to the end count as "watched to the end"
/// (or the last tenth, for short clips).
const END_MARGIN_MS: i64 = 15_000;
/// How many videos to remember as watched.
const MAX_WATCHED: usize = 3000;
/// Past this share a video counts as watched: what is left is mostly the
/// end credits. Its position is still kept, to finish it if wanted.
const WATCHED_SHARE: f64 = 0.9;

/// How much of a video was seen.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub enum Seen {
    /// Stopped at `at_ms`: `share` of it seen, when the length is known.
    Started { at_ms: i64, share: Option<f32> },
    /// Watched to the end, or past [`WATCHED_SHARE`] (`share`, the position
    /// kept for the rest).
    Finished { share: Option<f32> },
}

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
    /// Location, milliseconds, length.
    positions: Vec<(String, i64, Option<i64>)>,
    watched: Vec<String>,
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
                if let Some((at, loc)) = line.split_once('\t') {
                    let (ms, len) = match at.split_once('/') {
                        Some((ms, len)) => (ms, len.parse().ok()),
                        None => (at, None),
                    };
                    if let Ok(ms) = ms.parse() {
                        store.positions.push((loc.to_owned(), ms, len));
                    }
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("watched.txt")) {
            store.watched = text.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
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
        self.positions.iter().rev().find(|(l, ..)| l == location).map(|&(_, ms, _)| ms)
    }

    /// How much of `location` was seen, if any.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn seen(&self, location: &str) -> Option<Seen> {
        if let Some(&(_, ms, len)) = self.positions.iter().rev().find(|(l, ..)| l == location) {
            let share = len.filter(|&len| len > 0).map(|len| ms as f64 / len as f64);
            if share.is_some_and(|s| s >= WATCHED_SHARE) {
                return Some(Seen::Finished { share: share.map(|s| s as f32) });
            }
            return Some(Seen::Started { at_ms: ms, share: share.map(|s| s as f32) });
        }
        self.watched.iter().any(|l| l == location).then_some(Seen::Finished { share: None })
    }

    /// Remembers where `location` was stopped. Positions near the start or the
    /// end (`length` known) clear the entry instead; near the end, or past
    /// [`WATCHED_SHARE`], it is also marked watched.
    pub fn set_position(&mut self, location: &str, ms: i64, length: Option<i64>) {
        if length.is_some_and(|len| ms >= len - END_MARGIN_MS.min(len / 10)) {
            self.finish(location);
            return;
        }
        if ms < MIN_RESUME_MS {
            self.forget(location);
            return;
        }
        if length.is_some_and(|len| ms as f64 >= len as f64 * WATCHED_SHARE) {
            self.mark_watched(location);
        }
        if self.positions.last().is_some_and(|(l, old, len)| l == location && (old - ms).abs() < 1000 && *len == length) {
            return;
        }
        self.positions.retain(|(l, ..)| l != location);
        self.positions.push((location.to_owned(), ms, length));
        if self.positions.len() > MAX_POSITIONS {
            self.positions.remove(0);
        }
        self.dirty = true;
    }

    /// `location` was watched to the end: marked watched, and the next time
    /// it starts from the beginning.
    pub fn finish(&mut self, location: &str) {
        self.forget(location);
        self.mark_watched(location);
    }

    fn mark_watched(&mut self, location: &str) {
        if self.watched.last().is_some_and(|l| l == location) {
            return;
        }
        self.watched.retain(|l| l != location);
        self.watched.push(location.to_owned());
        if self.watched.len() > MAX_WATCHED {
            self.watched.remove(0);
        }
        self.dirty = true;
    }

    pub fn forget(&mut self, location: &str) {
        let before = self.positions.len();
        self.positions.retain(|(l, ..)| l != location);
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
        // A location with a newline could not be read back; skip it.
        for (loc, ms, len) in self.positions.iter().filter(|(l, ..)| !l.contains(['\n', '\r'])) {
            match len {
                Some(len) => positions.push_str(&format!("{ms}/{len}\t{loc}\n")),
                None => positions.push_str(&format!("{ms}\t{loc}\n")),
            }
        }
        let watched: String =
            self.watched.iter().filter(|l| !l.contains(['\n', '\r'])).map(|l| format!("{l}\n")).collect();
        let mut settings = String::new();
        if let Some(v) = self.volume {
            settings.push_str(&format!("volume={v}\n"));
        }
        if let Some(d) = &self.audio_device {
            settings.push_str(&format!("audio_device={d}\n"));
        }
        let ok = write_atomic(&dir.join("positions.tsv"), &positions)
            && write_atomic(&dir.join("watched.txt"), &watched)
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

    #[test]
    fn watched_marks() {
        let dir = std::env::temp_dir().join(format!("vplayer-watched-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A position saved by an older version, without the length.
        std::fs::write(dir.join("positions.tsv"), "70000\t/v/old.mkv\n").unwrap();
        let mut s = Store::load_from(Some(dir.clone()));
        assert_eq!(s.seen("/v/old.mkv"), Some(Seen::Started { at_ms: 70_000, share: None }));
        assert_eq!(s.seen("/v/new.mkv"), None);

        s.set_position("/v/a.mkv", 60_000, Some(600_000));
        s.set_position("/v/credits.mkv", 560_000, Some(600_000)); // past 90 %: watched, still resumable
        s.set_position("/v/end.mkv", 599_000, Some(600_000));
        s.finish("/v/played.mkv");
        s.save();

        let mut t = Store::load_from(Some(dir.clone()));
        assert_eq!(t.seen("/v/a.mkv"), Some(Seen::Started { at_ms: 60_000, share: Some(0.1) }));
        assert_eq!(t.seen("/v/credits.mkv"), Some(Seen::Finished { share: Some(560.0 / 600.0) }));
        assert_eq!(t.position("/v/credits.mkv"), Some(560_000));
        assert_eq!(t.seen("/v/end.mkv"), Some(Seen::Finished { share: None }));
        assert_eq!(t.position("/v/end.mkv"), None);
        assert_eq!(t.seen("/v/played.mkv"), Some(Seen::Finished { share: None }));

        // Started again: shown as started until it is watched again.
        t.set_position("/v/played.mkv", 30_000, Some(600_000));
        assert_eq!(t.seen("/v/played.mkv"), Some(Seen::Started { at_ms: 30_000, share: Some(0.05) }));

        // No length (a file without one in its header): still resumable.
        t.set_position("/v/nolength.mkv", 90_000, None);
        assert_eq!(t.seen("/v/nolength.mkv"), Some(Seen::Started { at_ms: 90_000, share: None }));
        std::fs::remove_dir_all(dir).ok();
    }
}
