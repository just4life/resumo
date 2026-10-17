//! Cropping: which part of the decoded picture is shown.
//!
//! The crop is applied when the frame is drawn, as a texture sub-rectangle,
//! so switching modes is instant and never restarts the video output.

use egui::{pos2, Rect};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Crop {
    /// Whole picture.
    None,
    /// Largest centred window with this width/height ratio.
    Ratio(f32, f32),
    /// Percent cut from each edge: left, top, right, bottom.
    Custom { left: f32, top: f32, right: f32, bottom: f32 },
}

pub const PRESETS: &[(&str, Crop)] = &[
    ("16:9", Crop::Ratio(16.0, 9.0)),
    ("16:10", Crop::Ratio(16.0, 10.0)),
    ("4:3", Crop::Ratio(4.0, 3.0)),
    ("5:4", Crop::Ratio(5.0, 4.0)),
    ("1:1", Crop::Ratio(1.0, 1.0)),
    ("1.85:1", Crop::Ratio(1.85, 1.0)),
    ("2.35:1", Crop::Ratio(2.35, 1.0)),
    ("2.39:1", Crop::Ratio(2.39, 1.0)),
    ("9:16", Crop::Ratio(9.0, 16.0)),
];

impl Crop {
    pub fn label(&self) -> String {
        match *self {
            Crop::None => "Нет".to_owned(),
            Crop::Ratio(..) => PRESETS
                .iter()
                .find(|(_, c)| c == self)
                .map_or_else(|| "Своё".to_owned(), |(n, _)| (*n).to_owned()),
            Crop::Custom { .. } => "Вручную".to_owned(),
        }
    }

    /// Visible part of a `width` x `height` picture in normalised texture
    /// coordinates (0..1 on both axes).
    pub fn uv_rect(&self, width: f32, height: f32) -> Rect {
        match *self {
            Crop::None => Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Crop::Ratio(w, h) => {
                let src = width / height;
                let dst = w / h;
                if src > dst {
                    let keep = dst / src;
                    let off = (1.0 - keep) / 2.0;
                    Rect::from_min_max(pos2(off, 0.0), pos2(off + keep, 1.0))
                } else {
                    let keep = src / dst;
                    let off = (1.0 - keep) / 2.0;
                    Rect::from_min_max(pos2(0.0, off), pos2(1.0, off + keep))
                }
            }
            Crop::Custom { left, top, right, bottom } => {
                let l = (left / 100.0).clamp(0.0, 0.45);
                let t = (top / 100.0).clamp(0.0, 0.45);
                let r = (right / 100.0).clamp(0.0, 0.45);
                let b = (bottom / 100.0).clamp(0.0, 0.45);
                Rect::from_min_max(pos2(l, t), pos2(1.0 - r, 1.0 - b))
            }
        }
    }
}

/// Largest rectangle with `aspect` (w/h) that fits centred in `area`.
pub fn fit(area: Rect, aspect: f32) -> Rect {
    let (w, h) = if area.width() / area.height() > aspect {
        (area.height() * aspect, area.height())
    } else {
        (area.width(), area.width() / aspect)
    };
    Rect::from_center_size(area.center(), egui::vec2(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn ratio_crops_the_long_side() {
        // 16:9 picture cropped to 4:3 keeps 75% of the width.
        let uv = Crop::Ratio(4.0, 3.0).uv_rect(1920.0, 1080.0);
        assert!(approx(uv.width(), 0.75) && approx(uv.height(), 1.0));
        assert!(approx(uv.min.x, 0.125));
        // 4:3 picture cropped to 2.35:1 keeps the middle band.
        let uv = Crop::Ratio(2.35, 1.0).uv_rect(640.0, 480.0);
        assert!(approx(uv.width(), 1.0));
        assert!(approx(uv.height() * 480.0 * 2.35, 640.0));
    }

    #[test]
    fn custom_is_clamped() {
        let uv = Crop::Custom { left: 10.0, top: 0.0, right: 90.0, bottom: 5.0 }.uv_rect(100.0, 100.0);
        assert!(approx(uv.min.x, 0.1) && approx(uv.max.x, 0.55) && approx(uv.max.y, 0.95));
    }
}
