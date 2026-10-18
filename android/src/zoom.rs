//! How the picture fills the video area (Android): whole, filling the
//! screen, by width or by height, plus cutting off black bars that are part
//! of the video itself (a film letterboxed into a 16:9 or 4:3 frame), found
//! by looking at the frames; and how the video gets on screen. The choices
//! are kept in `zoom.txt` in the app's private folder.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Rect};

use crate::store;
use crate::video::VideoSink;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zoom {
    /// The whole picture, with bars where its shape differs from the screen's.
    Fit,
    /// Fills the area; whatever sticks out is cut off.
    Fill,
    Width,
    Height,
}

impl Zoom {
    pub const ALL: [(Zoom, &'static str); 4] =
        [(Zoom::Fit, "Целиком"), (Zoom::Fill, "Под экран"), (Zoom::Width, "По ширине"), (Zoom::Height, "По высоте")];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(z, _)| *z == self).map_or("", |(_, n)| n)
    }

    fn key(self) -> &'static str {
        match self {
            Zoom::Fit => "fit",
            Zoom::Fill => "fill",
            Zoom::Width => "width",
            Zoom::Height => "height",
        }
    }

    /// Where a picture with `aspect` (w/h) goes, centred on `area`. It can be
    /// larger than `area`; the caller clips to it.
    pub fn place(self, area: Rect, aspect: f32) -> Rect {
        let by_width = vec2(area.width(), area.width() / aspect);
        let by_height = vec2(area.height() * aspect, area.height());
        let wider = aspect > area.width() / area.height();
        let size = match self {
            Zoom::Fit => if wider { by_width } else { by_height },
            Zoom::Fill => if wider { by_height } else { by_width },
            Zoom::Width => by_width,
            Zoom::Height => by_height,
        };
        Rect::from_center_size(area.center(), size)
    }
}

/// How the video gets on screen (a change takes effect on the next start).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Output {
    /// The decoder's own video layer under the interface, as in VLC: HDR
    /// video shows as HDR.
    #[default]
    Layer,
    /// The decoder's frames drawn by the app through the GPU (HDR turned
    /// into SDR).
    Texture,
    /// Frames copied through the CPU.
    Cpu,
}

impl Output {
    pub const ALL: [(Output, &'static str); 3] =
        [(Output::Layer, "Видеослой (HDR)"), (Output::Texture, "Видеокарта"), (Output::Cpu, "Процессор")];

    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(o, _)| *o == self).map_or("", |(_, n)| n)
    }

    fn key(self) -> &'static str {
        match self {
            Output::Layer => "layer",
            Output::Texture => "texture",
            Output::Cpu => "cpu",
        }
    }
}

pub struct ViewSettings {
    pub zoom: Zoom,
    /// Cut off black bars found in the video.
    pub cut_bars: bool,
    /// Turn HDR video into SDR (drawn through the GPU only): `None` decides
    /// from the video.
    pub hdr_to_sdr: Option<bool>,
    pub output: Output,
    dir: Option<PathBuf>,
}

impl ViewSettings {
    pub fn load() -> Self {
        let dir = store::config_dir();
        let mut s = Self { zoom: Zoom::Fit, cut_bars: false, hdr_to_sdr: None, output: Output::Layer, dir };
        let text = s.dir.as_ref().and_then(|d| std::fs::read_to_string(d.join("zoom.txt")).ok()).unwrap_or_default();
        for line in text.lines() {
            match line.split_once('=') {
                Some(("zoom", v)) => {
                    s.zoom = Zoom::ALL.iter().map(|(z, _)| *z).find(|z| z.key() == v).unwrap_or(Zoom::Fit)
                }
                Some(("cut_bars", v)) => s.cut_bars = v == "1",
                Some(("hdr", v)) => s.hdr_to_sdr = match v { "on" => Some(true), "off" => Some(false), _ => None },
                Some(("video_out", v)) => {
                    s.output = Output::ALL.iter().map(|(o, _)| *o).find(|o| o.key() == v).unwrap_or_default()
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(dir) = &self.dir else { return };
        let _ = std::fs::create_dir_all(dir);
        let hdr = match self.hdr_to_sdr {
            Some(true) => "on",
            Some(false) => "off",
            None => "auto",
        };
        let text = format!(
            "zoom={}\ncut_bars={}\nhdr={hdr}\nvideo_out={}\n",
            self.zoom.key(),
            self.cut_bars as u8,
            self.output.key()
        );
        let _ = std::fs::write(dir.join("zoom.txt"), text);
    }
}

/// A pixel brighter than this (in any channel) is picture, not bar. Bars are
/// 0 or 16 with some compression noise; very dark scenes can dip below it,
/// which the checks in [`find_bars`] and [`BarFinder`] guard against.
const BRIGHT: u8 = 40;
/// Bars wider than this (per side, as a fraction) are more likely a dark scene.
const MAX_BAR: f32 = 0.25;
/// Bars thinner than this are left alone.
const MIN_BAR: f32 = 0.01;
const SAMPLES: usize = 12;

/// The part of a tightly packed RGBA frame that is not black bars, in
/// texture coordinates (0..1), or `None` when the frame can't tell (too dark).
/// Bars are taken as symmetric, which they are in real videos: the smaller
/// of the two sides wins, so a dark area on one side is never cut.
pub fn find_bars(pixels: &[u8], width: usize, height: usize) -> Option<Rect> {
    if width < 32 || height < 32 || pixels.len() < width * height * 4 {
        return None;
    }
    let bright = |x: usize, y: usize| {
        let i = (y * width + x) * 4;
        pixels[i] > BRIGHT || pixels[i + 1] > BRIGHT || pixels[i + 2] > BRIGHT
    };
    // Rows are sampled near the left and right edges, so subtitles drawn in
    // the lower bar don't count as picture; columns away from the top and
    // bottom, for the same reason.
    let row_has_picture = |y: usize| {
        let mut n = 0;
        for k in 0..SAMPLES {
            let x = width * (2 + k * 18 / SAMPLES) / 100;
            n += bright(x, y) as usize + bright(width - 1 - x, y) as usize;
        }
        n >= 2
    };
    let column_has_picture = |x: usize| {
        let n: usize = (0..SAMPLES * 2).map(|k| bright(x, height * (20 + k * 60 / (SAMPLES * 2)) / 100) as usize).sum();
        n >= 2
    };
    let top = (0..height).position(row_has_picture)?;
    let bottom = (0..height).rev().position(row_has_picture)?;
    let left = (0..width).position(column_has_picture)?;
    let right = (0..width).rev().position(column_has_picture)?;

    let side = |a: usize, b: usize, len: usize| -> Option<f32> {
        let bar = a.min(b) as f32 / len as f32;
        if bar > MAX_BAR {
            None
        } else if bar < MIN_BAR {
            Some(0.0)
        } else {
            // A little extra for the soft edge between bar and picture.
            Some(bar + 0.004)
        }
    };
    let v = side(top, bottom, height)?;
    let h = side(left, right, width)?;
    Some(Rect::from_min_max(pos2(h, v), pos2(1.0 - h, 1.0 - v)))
}

/// Watches the frames for black bars and settles on a crop only once the
/// same one has been seen a few times in a row, so dark scenes and fades
/// don't make the picture jump.
pub struct BarFinder {
    found: Rect,
    candidate: Option<Rect>,
    streak: u32,
    checked: Instant,
}

const FULL: Rect = Rect { min: egui::Pos2::ZERO, max: pos2(1.0, 1.0) };
const CHECK_EVERY: Duration = Duration::from_millis(500);
const CONFIRMATIONS: u32 = 3;

impl Default for BarFinder {
    fn default() -> Self {
        Self { found: FULL, candidate: None, streak: 0, checked: Instant::now() }
    }
}

fn close(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() < 0.01 && (a.max - b.max).length() < 0.01
}

impl BarFinder {
    /// The part of the picture without bars (texture coordinates).
    pub fn content(&self) -> Rect {
        self.found
    }

    /// Starts over, for a new video.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Looks at the newest frame now and then (while paused, the same frame
    /// again, which confirms itself).
    pub fn update(&mut self, sink: &VideoSink) {
        if self.due() {
            if let Some(found) = sink.with_frame(|f| (f.width > 0).then(|| find_bars(&f.pixels, f.width, f.height))) {
                self.feed(found);
            }
        }
    }

    /// Whether it is time to look at a frame again (then [`Self::feed`] it
    /// what [`find_bars`] made of one).
    pub fn due(&mut self) -> bool {
        if self.checked.elapsed() < CHECK_EVERY {
            return false;
        }
        self.checked = Instant::now();
        true
    }

    pub fn feed(&mut self, found: Option<Rect>) {
        let Some(found) = found else {
            self.candidate = None;
            return;
        };
        if close(found, self.found) {
            self.candidate = None;
            return;
        }
        match self.candidate {
            Some(c) if close(c, found) => self.streak += 1,
            _ => {
                self.candidate = Some(found);
                self.streak = 1;
            }
        }
        if self.streak >= CONFIRMATIONS {
            self.found = found;
            self.candidate = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey `w` x `h` frame with black bars of `bar_v` rows and `bar_h` columns on each side.
    fn frame(w: usize, h: usize, bar_v: usize, bar_h: usize) -> Vec<u8> {
        let mut px = vec![0u8; w * h * 4];
        for y in bar_v..h - bar_v {
            for x in bar_h..w - bar_h {
                px[(y * w + x) * 4..][..3].copy_from_slice(&[120, 110, 100]);
            }
        }
        px
    }

    #[test]
    fn letterbox_and_pillarbox() {
        // 2.39:1 film in a 1920x1080 frame: 138 black rows top and bottom.
        let r = find_bars(&frame(1920, 1080, 138, 0), 1920, 1080).unwrap();
        assert!((r.min.y - 138.0 / 1080.0 - 0.004).abs() < 0.002 && r.min.x == 0.0);
        // 4:3 in 16:9: 240 black columns left and right.
        let r = find_bars(&frame(1920, 1080, 0, 240), 1920, 1080).unwrap();
        assert!((r.min.x - 0.125 - 0.004).abs() < 0.002 && r.min.y == 0.0);
        // No bars.
        assert_eq!(find_bars(&frame(640, 360, 0, 0), 640, 360), Some(FULL));
    }

    #[test]
    fn subtitles_in_the_bar_and_dark_frames() {
        let (w, h) = (1280, 720);
        let mut px = frame(w, h, 90, 0);
        // White subtitle text in the middle of the lower bar.
        for y in 650..690 {
            for x in 400..880 {
                px[(y * w + x) * 4..][..3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let r = find_bars(&px, w, h).unwrap();
        assert!((r.min.y - 90.0 / 720.0 - 0.004).abs() < 0.002);
        // All black: can't tell.
        assert_eq!(find_bars(&frame(w, h, h / 2, 0), w, h), None);
        // Only a small bright patch in the middle: a dark scene, not bars.
        assert_eq!(find_bars(&frame(w, h, 300, 500), w, h), None);
    }

    #[test]
    fn zoom_modes() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 500.0)); // 2:1 screen
        let r = Zoom::Fit.place(area, 4.0 / 3.0);
        assert!((r.height() - 500.0).abs() < 0.01 && r.width() < 1000.0);
        let r = Zoom::Fill.place(area, 4.0 / 3.0);
        assert!((r.width() - 1000.0).abs() < 0.01 && r.height() > 500.0);
        let r = Zoom::Height.place(area, 2.39);
        assert!((r.height() - 500.0).abs() < 0.01 && r.width() > 1000.0);
        let r = Zoom::Width.place(area, 2.39);
        assert!((r.width() - 1000.0).abs() < 0.01 && r.height() < 500.0);
    }
}
