//! Saving the current video frame as a PNG in the user's Pictures folder.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use egui::Rect;

/// An RGBA picture, rows top to bottom.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    /// The part of the picture inside `uv` (0..1 coordinates), for cropping.
    pub fn crop(self, uv: Rect) -> Image {
        let x0 = ((uv.left() * self.width as f32).round() as usize).min(self.width);
        let y0 = ((uv.top() * self.height as f32).round() as usize).min(self.height);
        let x1 = ((uv.right() * self.width as f32).round() as usize).clamp(x0, self.width);
        let y1 = ((uv.bottom() * self.height as f32).round() as usize).clamp(y0, self.height);
        if (x0, y0, x1, y1) == (0, 0, self.width, self.height) {
            return self;
        }
        let mut rgba = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
        for y in y0..y1 {
            let row = y * self.width * 4;
            rgba.extend_from_slice(&self.rgba[row + x0 * 4..row + x1 * 4]);
        }
        Image { width: x1 - x0, height: y1 - y0, rgba }
    }

    /// Writes an opaque PNG (the alpha channel of video frames means nothing).
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let file = File::create(path).map_err(|e| e.to_string())?;
        let mut enc = png::Encoder::new(BufWriter::new(file), self.width as u32, self.height as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let rgb: Vec<u8> = self.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())
    }
}

/// `<Pictures>/Resumo`, where saved frames go.
pub fn folder() -> PathBuf {
    pictures_dir().join("Resumo")
}

/// "Movie 01-23-45.png" for a frame of "Movie" at 1:23:45, safe on every OS.
pub fn file_name(title: &str, ms: i64) -> String {
    let mut name: String = title
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .take(120)
        .collect();
    // Windows drops trailing dots and spaces from names.
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    if name.is_empty() {
        name.push_str("Resumo");
    }
    let s = ms.max(0) / 1000;
    format!("{name} {:02}-{:02}-{:02}.png", s / 3600, s / 60 % 60, s % 60)
}

/// `dir/name`, or `dir/name (2)`, ... if that file already exists.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let stem = name.strip_suffix(".png").unwrap_or(name);
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}).png")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Shows `path` in the file manager (selected, on Windows).
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    let result = if path.is_dir() {
        std::process::Command::new("explorer").arg(path).spawn()
    } else {
        let mut select = std::ffi::OsString::from("/select,");
        select.push(path);
        std::process::Command::new("explorer").arg(select).spawn()
    };
    #[cfg(not(windows))]
    let result = {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(opener).arg(dir).spawn()
    };
    let _ = result;
}

#[cfg(windows)]
fn pictures_dir() -> PathBuf {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);
    const FOLDERID_PICTURES: Guid =
        Guid(0x33E2_8130, 0x4E1E, 0x4676, [0x83, 0x5A, 0x98, 0x39, 0x5C, 0x3B, 0xC3, 0xBB]);
    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(p: *mut c_void);
    }

    // The real Pictures folder, which may have been moved (e.g. to OneDrive).
    unsafe {
        let mut raw = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_PICTURES, 0, std::ptr::null_mut(), &mut raw);
        let found = (hr >= 0 && !raw.is_null()).then(|| {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(raw, len)))
        });
        CoTaskMemFree(raw as *mut c_void);
        if let Some(dir) = found {
            return dir;
        }
    }
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    home.join("Pictures")
}

#[cfg(not(windows))]
fn pictures_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    // Localised desktops name it in ~/.config/user-dirs.dirs, e.g. "$HOME/Изображения".
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    if let Ok(text) = std::fs::read_to_string(config.join("user-dirs.dirs")) {
        for line in text.lines() {
            if let Some(v) = line.trim().strip_prefix("XDG_PICTURES_DIR=") {
                let v = v.trim_matches('"');
                let dir = match v.strip_prefix("$HOME") {
                    Some(rest) => home.join(rest.trim_start_matches('/')),
                    None => PathBuf::from(v),
                };
                if dir != home {
                    return dir;
                }
            }
        }
    }
    home.join("Pictures")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(file_name("Kung Fu: Soccer?", 3_723_000), "Kung Fu_ Soccer_ 01-02-03.png");
        assert_eq!(file_name("dots...", 5_000), "dots 00-00-05.png");
        assert_eq!(file_name("", 0), "Resumo 00-00-00.png");

        let dir = std::env::temp_dir().join(format!("resumo-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "a.png");
        assert_eq!(first, dir.join("a.png"));
        let img = Image { width: 2, height: 1, rgba: vec![255, 0, 0, 0, 0, 255, 0, 0] };
        img.save_png(&first).unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a (2).png"));
        let bytes = std::fs::read(&first).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn crop_takes_the_visible_part() {
        // 4x2 picture, pixel value = its index.
        let rgba = (0..8u8).flat_map(|i| [i, i, i, 255]).collect();
        let img = Image { width: 4, height: 2, rgba };
        let c = img.crop(Rect::from_min_max(pos2(0.25, 0.5), pos2(0.75, 1.0)));
        assert_eq!((c.width, c.height), (2, 1));
        assert_eq!(c.rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), vec![5, 6]);
    }
}
