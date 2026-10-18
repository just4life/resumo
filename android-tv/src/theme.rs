//! Dark theme and the small custom widgets the player is drawn with.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, Ui};

pub const BG: Color32 = Color32::from_rgb(0x0E, 0x10, 0x14);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x19, 0x20);
pub const SURFACE_HI: Color32 = Color32::from_rgb(0x22, 0x26, 0x30);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2A, 0x34);
pub const TRACK: Color32 = Color32::from_rgb(0x34, 0x39, 0x46);
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEA, 0xF0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8C, 0x93, 0xA3);
pub const ACCENT: Color32 = Color32::from_rgb(0x3D, 0x8B, 0xFF);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x66, 0xA3, 0xFF);
/// Hover background for buttons drawn over video or panels.
pub const HOVER: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
pub const CLOSE_RED: Color32 = Color32::from_rgb(0xE8, 0x11, 0x23);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = Color32::from_rgb(0x0B, 0x0C, 0x10);
        v.faint_bg_color = SURFACE_HI;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.slider_trailing_fill = true;

        let w = &mut v.widgets;
        for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            wv.corner_radius = CornerRadius::same(8);
        }
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.inactive.weak_bg_fill = SURFACE_HI;
        w.inactive.bg_fill = TRACK;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.bg_stroke = Stroke::NONE;
        w.hovered.weak_bg_fill = Color32::from_rgb(0x2C, 0x31, 0x3D);
        w.hovered.bg_fill = Color32::from_rgb(0x2C, 0x31, 0x3D);
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        w.active.weak_bg_fill = ACCENT.gamma_multiply(0.5);
        w.active.bg_fill = ACCENT;
        w.active.bg_stroke = Stroke::NONE;
        w.open.weak_bg_fill = SURFACE_HI;

        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.slider_rail_height = 4.0;
        style.spacing.menu_margin = egui::Margin::same(8);
    });
}

/// Vector icons, drawn with the painter so they look the same on every OS
/// and need no icon font.
#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Volume(u8),
    Mute,
    Fullscreen,
    ExitFullscreen,
    Audio,
    Subtitles,
    Crop,
    Playlist,
    Repeat,
    RepeatOne,
    Shuffle,
    More,
    Folder,
    Plus,
    Close,
    Minimize,
    Maximize,
    Restore,
    /// Computers on the local network (TV home screen).
    Network,
    /// A link to a stream.
    Link,
    Speed,
    /// One folder up.
    Up,
}

fn arc(c: Pos2, r: f32, from: f32, to: f32) -> Vec<Pos2> {
    let n = 12;
    (0..=n)
        .map(|i| {
            let a = from + (to - from) * i as f32 / n as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect()
}

/// Paints `icon` centred in `rect`.
pub fn paint_icon(p: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let u = s / 20.0; // design grid: icons are drawn on a 20x20 canvas
    let pt = |x: f32, y: f32| c + vec2(x * u, y * u);
    let stroke = Stroke::new(1.8 * u.max(0.8), color);
    let line = |pts: Vec<Pos2>| p.add(egui::Shape::line(pts, stroke));
    let fill = |pts: Vec<Pos2>| p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    match icon {
        Icon::Play => {
            fill(vec![pt(-5.0, -7.5), pt(7.5, 0.0), pt(-5.0, 7.5)]);
        }
        Icon::Pause => {
            for x in [-3.5, 3.5] {
                p.rect_filled(Rect::from_center_size(pt(x, 0.0), vec2(3.6 * u, 14.0 * u)), 1.2 * u, color);
            }
        }
        Icon::Prev | Icon::Next => {
            let k = if icon == Icon::Prev { 1.0 } else { -1.0 };
            p.rect_filled(Rect::from_center_size(pt(-6.0 * k, 0.0), vec2(2.4 * u, 12.0 * u)), 1.0 * u, color);
            fill(if k > 0.0 {
                vec![pt(6.5, -6.5), pt(6.5, 6.5), pt(-4.0, 0.0)]
            } else {
                vec![pt(-6.5, -6.5), pt(4.0, 0.0), pt(-6.5, 6.5)]
            });
        }
        Icon::Volume(_) | Icon::Mute => {
            fill(vec![pt(-8.0, -3.0), pt(-4.5, -3.0), pt(0.5, -7.5), pt(0.5, 7.5), pt(-4.5, 3.0), pt(-8.0, 3.0)]);
            match icon {
                Icon::Mute => {
                    line(vec![pt(3.5, -3.5), pt(8.5, 3.5)]);
                    line(vec![pt(8.5, -3.5), pt(3.5, 3.5)]);
                }
                Icon::Volume(level) => {
                    if level >= 1 {
                        line(arc(pt(0.5, 0.0), 4.0 * u, -0.9, 0.9));
                    }
                    if level >= 2 {
                        line(arc(pt(0.5, 0.0), 7.5 * u, -0.95, 0.95));
                    }
                }
                _ => {}
            }
        }
        Icon::Fullscreen | Icon::ExitFullscreen => {
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                if icon == Icon::Fullscreen {
                    line(vec![pt(sx * 8.0, sy * 3.5), pt(sx * 8.0, sy * 8.0), pt(sx * 3.5, sy * 8.0)]);
                } else {
                    line(vec![pt(sx * 8.0, sy * 3.0), pt(sx * 3.0, sy * 3.0), pt(sx * 3.0, sy * 8.0)]);
                }
            }
        }
        Icon::Audio => {
            p.circle_filled(pt(-4.0, 5.0), 3.0 * u, color);
            p.circle_filled(pt(5.0, 3.0), 3.0 * u, color);
            line(vec![pt(-1.2, 5.0), pt(-1.2, -6.5), pt(7.8, -8.5), pt(7.8, 3.0)]);
            line(vec![pt(-1.2, -3.0), pt(7.8, -5.0)]);
        }
        Icon::Subtitles => {
            p.rect_stroke(
                Rect::from_center_size(c, vec2(17.0 * u, 13.0 * u)),
                2.5 * u,
                stroke,
                egui::StrokeKind::Middle,
            );
            line(vec![pt(-5.0, 1.0), pt(0.0, 1.0)]);
            line(vec![pt(2.5, 1.0), pt(5.0, 1.0)]);
            line(vec![pt(-5.0, 4.0), pt(5.0, 4.0)]);
        }
        Icon::Crop => {
            line(vec![pt(-5.0, -9.0), pt(-5.0, 5.0), pt(9.0, 5.0)]);
            line(vec![pt(-9.0, -5.0), pt(5.0, -5.0), pt(5.0, 9.0)]);
        }
        Icon::Playlist => {
            for y in [-6.0, -1.5] {
                line(vec![pt(-8.0, y), pt(7.0, y)]);
            }
            line(vec![pt(-8.0, 3.0), pt(0.0, 3.0)]);
            line(vec![pt(-8.0, 7.5), pt(0.0, 7.5)]);
            fill(vec![pt(3.0, 1.5), pt(9.0, 5.25), pt(3.0, 9.0)]);
        }
        Icon::Repeat | Icon::RepeatOne => {
            line(vec![pt(-8.0, 2.0), pt(-8.0, -4.0), pt(6.0, -4.0)]);
            fill(vec![pt(5.0, -7.5), pt(9.0, -4.0), pt(5.0, -0.5)]);
            line(vec![pt(8.0, -2.0), pt(8.0, 4.0), pt(-6.0, 4.0)]);
            fill(vec![pt(-5.0, 0.5), pt(-9.0, 4.0), pt(-5.0, 7.5)]);
            if icon == Icon::RepeatOne {
                p.text(pt(0.0, 0.0), Align2::CENTER_CENTER, "1", FontId::proportional(8.5 * u), color);
            }
        }
        Icon::Shuffle => {
            line(vec![pt(-9.0, -5.0), pt(-4.0, -5.0), pt(3.0, 5.0), pt(6.0, 5.0)]);
            line(vec![pt(-9.0, 5.0), pt(-4.0, 5.0), pt(3.0, -5.0), pt(6.0, -5.0)]);
            fill(vec![pt(5.5, -8.5), pt(9.5, -5.0), pt(5.5, -1.5)]);
            fill(vec![pt(5.5, 1.5), pt(9.5, 5.0), pt(5.5, 8.5)]);
        }
        Icon::More => {
            for x in [-6.0, 0.0, 6.0] {
                p.circle_filled(pt(x, 0.0), 1.7 * u, color);
            }
        }
        Icon::Folder => {
            line(vec![
                pt(-8.5, 6.5),
                pt(-8.5, -6.5),
                pt(-3.0, -6.5),
                pt(-1.0, -4.0),
                pt(8.5, -4.0),
                pt(8.5, 6.5),
                pt(-8.5, 6.5),
            ]);
        }
        Icon::Plus => {
            line(vec![pt(-7.0, 0.0), pt(7.0, 0.0)]);
            line(vec![pt(0.0, -7.0), pt(0.0, 7.0)]);
        }
        Icon::Close => {
            line(vec![pt(-5.5, -5.5), pt(5.5, 5.5)]);
            line(vec![pt(5.5, -5.5), pt(-5.5, 5.5)]);
        }
        Icon::Minimize => {
            line(vec![pt(-5.5, 0.0), pt(5.5, 0.0)]);
        }
        Icon::Maximize => {
            let r = Rect::from_center_size(c, vec2(11.0 * u, 11.0 * u));
            p.rect_stroke(r, 1.0 * u, stroke, egui::StrokeKind::Middle);
        }
        Icon::Restore => {
            let r = Rect::from_center_size(pt(-1.5, 1.5), vec2(9.0 * u, 9.0 * u));
            p.rect_stroke(r, 1.0 * u, stroke, egui::StrokeKind::Middle);
            line(vec![pt(-1.5, -5.5), pt(5.5, -5.5), pt(5.5, 1.5)]);
        }
        Icon::Network => {
            // A monitor with a stand, joined to a second screen behind it.
            let screen = Rect::from_center_size(pt(-1.5, -2.0), vec2(14.0 * u, 9.5 * u));
            p.rect_stroke(screen, 1.5 * u, stroke, egui::StrokeKind::Middle);
            line(vec![pt(-1.5, 2.75), pt(-1.5, 6.5)]);
            line(vec![pt(-5.5, 6.5), pt(2.5, 6.5)]);
            line(vec![pt(8.5, -9.0), pt(8.5, 1.0)]);
            line(vec![pt(2.0, -9.0), pt(8.5, -9.0)]);
        }
        Icon::Link => {
            // Two chain links on the diagonal.
            let dir = vec2(1.0, -1.0).normalized();
            let side = vec2(dir.y, -dir.x);
            for center in [pt(-3.2, 3.2), pt(3.2, -3.2)] {
                let (half, r) = (3.0 * u, 3.2 * u);
                let mut pts = Vec::new();
                for (end, from) in [(half, -std::f32::consts::FRAC_PI_2), (-half, std::f32::consts::FRAC_PI_2)] {
                    for i in 0..=8 {
                        let a = from + std::f32::consts::PI * i as f32 / 8.0;
                        pts.push(center + dir * (end + a.cos() * r) + side * (a.sin() * r));
                    }
                }
                pts.push(pts[0]);
                line(pts);
            }
            line(vec![pt(-2.0, 2.0), pt(2.0, -2.0)]);
        }
        Icon::Speed => {
            line(arc(pt(0.0, 3.0), 8.5 * u, std::f32::consts::PI, 2.0 * std::f32::consts::PI));
            line(vec![pt(0.0, 3.0), pt(5.0, -3.0)]);
            p.circle_filled(pt(0.0, 3.0), 1.8 * u, color);
        }
        Icon::Up => {
            line(vec![pt(-6.0, -1.0), pt(0.0, -7.0), pt(6.0, -1.0)]);
            line(vec![pt(0.0, -7.0), pt(0.0, 7.0)]);
        }
    }
}

/// Flat icon button: transparent until hovered, accent-coloured while `on`.
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str, on: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 6.0, HOVER);
    }
    let color = if on {
        ACCENT
    } else if hovered {
        Color32::WHITE
    } else {
        TEXT
    };
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(17.0, 17.0)), icon, color);
    resp.on_hover_text(tip)
}

/// Caption button for the custom title bar (minimise, maximise, close).
pub fn caption_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(44.0, ui.available_height()), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        let fill = if icon == Icon::Close { CLOSE_RED } else { HOVER };
        ui.painter().rect_filled(rect, 0.0, fill);
    }
    let color = if hovered { Color32::WHITE } else { TEXT_DIM };
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(14.0, 14.0)), icon, color);
    resp.on_hover_text(tip)
}

/// Rounded button with an icon and a label, for primary actions.
pub fn pill(ui: &mut Ui, icon: Option<Icon>, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(14.0), TEXT);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 28.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() { ACCENT_HOVER } else { ACCENT };
    ui.painter().rect_filled(rect, 15.0, fill);
    let mut x = rect.min.x + 14.0;
    if let Some(i) = icon {
        let r = Rect::from_center_size(pos2(x + 8.0, rect.center().y), vec2(16.0, 16.0));
        paint_icon(ui.painter(), r, i, Color32::WHITE);
        x += icon_w;
    }
    ui.painter().galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    resp
}

/// Play / pause: a large bare icon with a soft round hover.
pub fn play_button(ui: &mut Ui, playing: bool) -> Response {
    let size = 30.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    if resp.hovered() {
        ui.painter().circle_filled(rect.center(), size / 2.0, HOVER);
    }
    let (icon, dx) = if playing { (Icon::Pause, 0.0) } else { (Icon::Play, 1.0) };
    paint_icon(
        ui.painter(),
        Rect::from_center_size(rect.center() + vec2(dx, 0.0), vec2(20.0, 20.0)),
        icon,
        Color32::WHITE,
    );
    resp
}

/// Icon button that toggles a popup menu below/above it.
pub fn menu_icon(ui: &mut Ui, icon: Icon, tip: &str, on: bool, add: impl FnOnce(&mut Ui)) {
    let resp = icon_button(ui, icon, tip, on);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(230.0);
        add(ui);
    });
}

/// Text button that toggles a popup menu.
pub fn menu_text(ui: &mut Ui, text: &str, tip: &str, add: impl FnOnce(&mut Ui)) {
    let resp = ui
        .add(
            egui::Button::new(RichText::new(text).size(12.0).color(TEXT))
                .frame_when_inactive(false)
                .min_size(vec2(34.0, 28.0)),
        )
        .on_hover_text(tip);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(140.0);
        add(ui);
    });
}

pub struct RowResponse {
    pub row: Response,
    pub remove: bool,
}

/// One playlist entry: number, title, duration and a remove button on hover.
pub fn playlist_row(
    ui: &mut Ui,
    index: usize,
    title: &str,
    duration: Option<String>,
    current: bool,
    selected: bool,
) -> RowResponse {
    let height = 34.0;
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let hovered = row.hovered();
    let bg = if current {
        ACCENT.gamma_multiply(0.16)
    } else if selected {
        SURFACE_HI
    } else if hovered {
        Color32::from_rgb(0x1D, 0x21, 0x2A)
    } else {
        Color32::TRANSPARENT
    };
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, bg);
    if current {
        let bar = Rect::from_min_size(rect.min + vec2(0.0, 7.0), vec2(3.0, height - 14.0));
        p.rect_filled(bar, 2.0, ACCENT);
    }

    let num_at = rect.left_center() + vec2(22.0, 0.0);
    if current {
        paint_icon(&p, Rect::from_center_size(num_at, vec2(12.0, 12.0)), Icon::Play, ACCENT);
    } else {
        p.text(num_at, Align2::CENTER_CENTER, format!("{}", index + 1), FontId::proportional(12.0), TEXT_DIM);
    }

    let right_reserved = 70.0;
    let title_color = if current { Color32::WHITE } else { TEXT };
    let max_w = (rect.width() - 44.0 - right_reserved).max(20.0);
    let job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping::truncate_at_width(max_w),
        ..egui::text::LayoutJob::simple_singleline(title.to_owned(), FontId::proportional(14.0), title_color)
    };
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    p.galley(rect.left_center() + vec2(40.0, -galley.size().y / 2.0), galley, title_color);

    let mut remove = false;
    if hovered {
        let x_rect = Rect::from_center_size(rect.right_center() - vec2(18.0, 0.0), vec2(24.0, 24.0));
        let x = ui.interact(x_rect, row.id.with("remove"), Sense::click());
        let col = if x.hovered() { Color32::WHITE } else { TEXT_DIM };
        paint_icon(&p, Rect::from_center_size(x_rect.center(), vec2(14.0, 14.0)), Icon::Close, col);
        remove = x.clicked();
    } else if let Some(d) = duration {
        p.text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, d, FontId::monospace(12.0), TEXT_DIM);
    }
    RowResponse { row, remove }
}
