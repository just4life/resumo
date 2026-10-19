//! Playlist state: items, the current position, repeat and shuffle, and M3U I/O.

use std::path::{Path, PathBuf};

pub const MEDIA_EXTENSIONS: &[&str] = &[
    "3gp", "aac", "avi", "flac", "flv", "m2ts", "m4a", "m4v", "mka", "mkv", "mov", "mp3", "mp4", "mpeg", "mpg",
    "mts", "ogg", "ogv", "opus", "ts", "vob", "wav", "webm", "wma", "wmv",
];
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "sub", "vtt", "idx", "smi"];
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8"];

pub fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| list.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[derive(Clone, Debug)]
pub struct Item {
    /// Local path or URL handed to libVLC.
    pub location: String,
    pub title: String,
    /// Length in milliseconds once known.
    pub duration: Option<i64>,
}

impl Item {
    pub fn new(location: impl Into<String>) -> Self {
        let location = location.into();
        let title = if location.contains("://") {
            location.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&location).to_owned()
        } else {
            Path::new(&location)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| location.clone())
        };
        Self { location, title, duration: None }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

pub struct Playlist {
    pub items: Vec<Item>,
    pub current: Option<usize>,
    pub repeat: Repeat,
    pub shuffle: bool,
    rng: u64,
}

impl Default for Playlist {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9, |d| d.as_nanos() as u64);
        Self { items: Vec::new(), current: None, repeat: Repeat::Off, shuffle: false, rng: seed | 1 }
    }
}

impl Playlist {
    /// Adds a file, a folder (its media files, sorted) or an M3U playlist.
    /// Returns how many items were added.
    pub fn add_path(&mut self, path: &Path) -> usize {
        if path.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(path)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            files.retain(|p| p.is_file() && has_extension(p, MEDIA_EXTENSIONS));
            files.sort();
            files.iter().map(|f| self.add_path(f)).sum()
        } else if has_extension(path, PLAYLIST_EXTENSIONS) {
            self.load_m3u(path)
        } else {
            self.items.push(Item::new(path.to_string_lossy()));
            1
        }
    }

    pub fn add_location(&mut self, location: &str) {
        self.items.push(Item::new(location.trim()));
    }

    pub fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        self.items.remove(index);
        self.current = match self.current {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };
    }

    /// Moves an item, keeping `current` pointing at the same entry.
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        if let Some(c) = self.current {
            self.current = Some(if c == from {
                to
            } else if from < c && to >= c {
                c - 1
            } else if from > c && to <= c {
                c + 1
            } else {
                c
            });
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
    }

    fn random_index(&mut self) -> usize {
        // xorshift64: plenty for picking the next track.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % self.items.len() as u64) as usize
    }

    /// Index to play after the current item. `auto` is true when the
    /// current item finished by itself (so "repeat one" applies).
    pub fn next_index(&mut self, auto: bool) -> Option<usize> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        let cur = self.current;
        if auto && self.repeat == Repeat::One {
            return cur;
        }
        if self.shuffle && n > 1 {
            let mut i = self.random_index();
            if Some(i) == cur {
                i = (i + 1) % n;
            }
            return Some(i);
        }
        match cur {
            None => Some(0),
            Some(c) if c + 1 < n => Some(c + 1),
            Some(_) if self.repeat != Repeat::Off || !auto => Some(0),
            Some(_) => None,
        }
    }

    pub fn prev_index(&self) -> Option<usize> {
        let n = self.items.len();
        match self.current {
            _ if n == 0 => None,
            None | Some(0) => Some(n - 1),
            Some(c) => Some(c - 1),
        }
    }

    pub fn load_m3u(&mut self, path: &Path) -> usize {
        let Ok(text) = std::fs::read_to_string(path) else { return 0 };
        let base = path.parent().unwrap_or(Path::new("."));
        let mut added = 0;
        let mut pending_title: Option<String> = None;
        for line in text.lines().map(str::trim) {
            if let Some(info) = line.strip_prefix("#EXTINF:") {
                pending_title = info.split_once(',').map(|(_, t)| t.trim().to_owned());
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let location = if line.contains("://") || Path::new(line).is_absolute() {
                line.to_owned()
            } else {
                base.join(line).to_string_lossy().into_owned()
            };
            let mut item = Item::new(location);
            if let Some(t) = pending_title.take().filter(|t| !t.is_empty()) {
                item.title = t;
            }
            self.items.push(item);
            added += 1;
        }
        added
    }

    pub fn save_m3u(&self, path: &Path) -> std::io::Result<()> {
        let mut out = String::from("#EXTM3U\n");
        for item in &self.items {
            let secs = item.duration.map_or(-1, |d| d / 1000);
            out.push_str(&format!("#EXTINF:{secs},{}\n{}\n", item.title, item.location));
        }
        std::fs::write(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(n: usize) -> Playlist {
        let mut p = Playlist::default();
        for i in 0..n {
            p.add_location(&format!("http://x/{i}.mp4"));
        }
        p
    }

    #[test]
    fn next_respects_repeat() {
        let mut p = list(3);
        assert_eq!(p.next_index(true), Some(0));
        p.current = Some(2);
        assert_eq!(p.next_index(true), None);
        assert_eq!(p.next_index(false), Some(0));
        p.repeat = Repeat::All;
        assert_eq!(p.next_index(true), Some(0));
        p.repeat = Repeat::One;
        assert_eq!(p.next_index(true), Some(2));
        assert_eq!(p.next_index(false), Some(0));
    }

    #[test]
    fn remove_and_move_keep_current() {
        let mut p = list(4);
        p.current = Some(2);
        p.remove(0);
        assert_eq!(p.current, Some(1));
        p.move_item(1, 0);
        assert_eq!(p.current, Some(0));
        p.move_item(2, 0);
        assert_eq!(p.current, Some(1));
        p.remove(1);
        assert_eq!(p.current, None);
    }

    #[test]
    fn m3u_round_trip() {
        let dir = std::env::temp_dir().join(format!("resumo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.m3u");
        let mut p = list(2);
        p.items[0].title = "First".into();
        p.save_m3u(&file).unwrap();
        let mut q = Playlist::default();
        assert_eq!(q.load_m3u(&file), 2);
        assert_eq!(q.items[0].title, "First");
        assert_eq!(q.items[1].location, "http://x/1.mp4");
        std::fs::remove_dir_all(dir).ok();
    }
}
