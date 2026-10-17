//! Recently played files (Android): the last few, newest first, kept in
//! `history.txt` in the app's private folder as `location<TAB>title` lines.

use std::path::PathBuf;

use crate::store;

/// How many files to remember.
pub const MAX_ENTRIES: usize = 5;

pub struct Entry {
    pub location: String,
    pub title: String,
}

pub struct History {
    dir: Option<PathBuf>,
    pub entries: Vec<Entry>,
}

impl History {
    pub fn load() -> Self {
        Self::load_from(store::config_dir())
    }

    pub fn load_from(dir: Option<PathBuf>) -> Self {
        let mut entries = Vec::new();
        if let Some(text) = dir.as_ref().and_then(|d| std::fs::read_to_string(d.join("history.txt")).ok()) {
            for line in text.lines() {
                if let Some((location, title)) = line.split_once('\t') {
                    let seen = entries.iter().any(|e: &Entry| same_file(&e.location, location));
                    if !location.is_empty() && !seen && entries.len() < MAX_ENTRIES {
                        entries.push(Entry { location: location.to_owned(), title: title.to_owned() });
                    }
                }
            }
        }
        Self { dir, entries }
    }

    /// Puts `location` first (moving it up if it is already listed) and saves.
    pub fn add(&mut self, location: &str, title: &str) {
        // A location with a line break or tab could not be read back.
        if location.contains(['\n', '\r', '\t']) {
            return;
        }
        let title = title.replace(['\n', '\r', '\t'], " ");
        if self.entries.first().is_some_and(|e| e.location == location && e.title == title) {
            return;
        }
        self.entries.retain(|e| !same_file(&e.location, location));
        self.entries.insert(0, Entry { location: location.to_owned(), title });
        self.entries.truncate(MAX_ENTRIES);
        self.save();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    fn save(&self) {
        let Some(dir) = &self.dir else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let text: String = self.entries.iter().map(|e| format!("{}\t{}\n", e.location, e.title)).collect();
        let path = dir.join("history.txt");
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            std::fs::rename(&tmp, &path).ok();
        }
    }
}

/// Whether two locations are the same file: `/sdcard/…` is a link to
/// `/storage/emulated/0/…`, and other apps hand files over as `file://`.
fn same_file(a: &str, b: &str) -> bool {
    a == b || file_key(a) == file_key(b)
}

fn file_key(location: &str) -> String {
    let path = location.strip_prefix("file://").unwrap_or(location);
    if path.contains("://") {
        return location.to_owned();
    }
    std::fs::canonicalize(path).map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_newest_five() {
        let dir = std::env::temp_dir().join(format!("vplayer-history-{}", std::process::id()));
        let mut h = History::load_from(Some(dir.clone()));
        for i in 0..7 {
            h.add(&format!("smb://nas/Видео/{i}.mkv"), &format!("Фильм {i}"));
        }
        h.add("smb://nas/Видео/4.mkv", "Фильм 4"); // played again: back to the top
        h.add("bad\tlocation", "x");

        let h = History::load_from(Some(dir.clone()));
        let titles: Vec<&str> = h.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Фильм 4", "Фильм 6", "Фильм 5", "Фильм 3", "Фильм 2"]);
        assert_eq!(h.entries[0].location, "smb://nas/Видео/4.mkv");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn one_entry_per_file() {
        let dir = std::env::temp_dir().join(format!("vplayer-history-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mkv");
        std::fs::write(&file, b"").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let mut h = History::load_from(Some(dir.clone()));
        h.add(&format!("file://{}", file.display()), "a");
        h.add(&link.join("a.mkv").to_string_lossy(), "a");
        assert_eq!(h.entries.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
