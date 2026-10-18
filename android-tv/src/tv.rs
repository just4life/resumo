//! Android TV: everything is driven by the remote. Key presses are taken out
//! of egui's input (see `PlayerApp::raw_input_hook`) and turned into
//! [`Nav`] commands, which each screen handles itself; this file also has the
//! large, focus-highlighted widgets those screens are drawn with.

use egui::{pos2, vec2, Align2, Color32, FontId, Key, Painter, Rect, Stroke};
use winit::platform::android::activity::input::Keycode;

use crate::theme::{self, Icon};

/// One press of a remote-control button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Menu,
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    Rewind,
    Forward,
}

#[derive(Clone, Copy, Debug)]
pub struct Press {
    pub nav: Nav,
    /// The button is being held down.
    pub repeat: bool,
}

/// Takes the remote's key presses out of `raw`, so egui's own keyboard
/// focus doesn't act on them as well. With `typing` (a text field is on
/// screen) the arrows and OK stay with egui, which moves between the field
/// and its buttons (up and down work as Shift+Tab and Tab). OK on a text
/// field (`text_focused`) comes out as [`Nav::Ok`], for the on-screen
/// keyboard, instead of submitting.
pub fn take_presses(raw: &mut egui::RawInput, typing: bool, text_focused: bool) -> Vec<Press> {
    let mut out = Vec::new();
    let remote = winit::platform::android::take_remote_keys();
    if typing && text_focused && remote.iter().any(|(c, _)| *c == Keycode::DpadCenter) {
        // The remote's OK arrives as Enter too; a keyboard's own Enter doesn't
        // come from the D-pad, so it still submits.
        if let Some(i) = raw.events.iter().position(|e| matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, .. })) {
            raw.events.remove(i);
            raw.events.retain(|e| !matches!(e, egui::Event::Key { key: Key::Enter, pressed: false, .. }));
        }
        out.push(Press { nav: Nav::Ok, repeat: false });
    }
    if typing {
        // Down / up move between the text field and its buttons, the way Tab does.
        for e in &mut raw.events {
            if let egui::Event::Key { key: k @ (Key::ArrowDown | Key::ArrowUp), modifiers, .. } = e {
                modifiers.shift = *k == Key::ArrowUp;
                *k = Key::Tab;
            }
        }
    }
    raw.events.retain(|e| {
        let egui::Event::Key { key, pressed, repeat, .. } = e else { return true };
        let nav = match key {
            Key::ArrowUp if !typing => Nav::Up,
            Key::ArrowDown if !typing => Nav::Down,
            Key::ArrowLeft if !typing => Nav::Left,
            Key::ArrowRight if !typing => Nav::Right,
            Key::Enter | Key::Space if !typing => Nav::Ok,
            Key::BrowserBack => Nav::Back,
            Key::Escape if !typing => Nav::Back,
            Key::MediaPlayPause => Nav::PlayPause,
            Key::MediaStop => Nav::Stop,
            Key::MediaTrackNext => Nav::Next,
            Key::MediaTrackPrevious => Nav::Prev,
            _ => return true,
        };
        if *pressed {
            out.push(Press { nav, repeat: *repeat });
        }
        false
    });
    for (code, repeat) in remote {
        let nav = match code {
            Keycode::MediaPlay => Nav::Play,
            Keycode::MediaPause => Nav::Pause,
            Keycode::MediaRewind => Nav::Rewind,
            Keycode::MediaFastForward => Nav::Forward,
            Keycode::Menu | Keycode::Info | Keycode::ButtonStart => Nav::Menu,
            Keycode::ButtonA | Keycode::ButtonSelect if !typing => Nav::Ok,
            Keycode::ButtonB => Nav::Back,
            Keycode::ChannelUp => Nav::Prev,
            Keycode::ChannelDown => Nav::Next,
            Keycode::Captions => Nav::Menu,
            _ => continue,
        };
        out.push(Press { nav, repeat });
    }
    out
}

/// Size of the screen the TV interface is laid out for, in points: big
/// enough to read from the sofa on any TV, whatever its resolution.
pub const LAYOUT_HEIGHT: f32 = 540.0;

/// The zoom that makes the screen [`LAYOUT_HEIGHT`] points tall.
pub fn zoom_for(ctx: &egui::Context) -> Option<f32> {
    let native = ctx.native_pixels_per_point()?;
    let px_height = ctx.viewport_rect().height() * ctx.pixels_per_point();
    (px_height > 100.0).then(|| (px_height / native / LAYOUT_HEIGHT).clamp(0.5, 4.0))
}

pub const FOCUS: Color32 = Color32::WHITE;
pub const ROW_H: f32 = 46.0;

/// The ring around whatever the remote is on.
pub fn focus_ring(p: &Painter, rect: Rect, radius: f32) {
    p.rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(3.0, FOCUS), egui::StrokeKind::Outside);
}

/// A big tile on the home screen: icon over a label.
pub fn tile(p: &Painter, rect: Rect, icon: Icon, label: &str, focused: bool) {
    let fill = if focused { theme::ACCENT } else { theme::SURFACE_HI };
    p.rect_filled(rect, 14.0, fill);
    if focused {
        focus_ring(p, rect, 14.0);
    }
    let color = if focused { Color32::WHITE } else { theme::TEXT };
    let icon_rect = Rect::from_center_size(rect.center() - vec2(0.0, 16.0), vec2(38.0, 38.0));
    theme::paint_icon(p, icon_rect, icon, color);
    p.text(rect.center() + vec2(0.0, 30.0), Align2::CENTER_CENTER, label, FontId::proportional(19.0), color);
}

/// What a list row shows on its right: a check mark, a switch, or a value.
pub enum Trail<'a> {
    None,
    Check(bool),
    Switch(bool),
    Value(&'a str),
    /// A value changed with ← and → (drawn with arrows while focused).
    Adjust(&'a str),
    Text(&'a str),
}

/// One row of a list: optional icon, a title (cut to one line) and `trail`.
pub fn row(p: &Painter, rect: Rect, icon: Option<(Icon, Color32)>, title: &str, trail: Trail, focused: bool, current: bool) {
    let fill = if focused {
        theme::ACCENT
    } else if current {
        theme::ACCENT.gamma_multiply(0.22)
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 10.0, fill);
    let text_color = if focused { Color32::WHITE } else { theme::TEXT };
    let dim = if focused { Color32::from_white_alpha(210) } else { theme::TEXT_DIM };
    let mut left = rect.left() + 16.0;
    if let Some((icon, color)) = icon {
        let c = if focused { Color32::WHITE } else { color };
        theme::paint_icon(p, Rect::from_center_size(pos2(left + 11.0, rect.center().y), vec2(22.0, 22.0)), icon, c);
        left += 36.0;
    }
    let mut right = rect.right() - 16.0;
    let font = FontId::proportional(17.0);
    match trail {
        Trail::None => {}
        Trail::Check(on) => {
            if on {
                let c = pos2(right - 11.0, rect.center().y);
                p.circle_filled(c, 11.0, if focused { Color32::WHITE } else { theme::ACCENT });
                let tick = vec![c + vec2(-5.0, 0.0), c + vec2(-1.5, 4.0), c + vec2(5.0, -4.0)];
                let col = if focused { theme::ACCENT } else { Color32::WHITE };
                p.add(egui::Shape::line(tick, Stroke::new(2.2, col)));
            }
            right -= 34.0;
        }
        Trail::Switch(on) => {
            let track = Rect::from_center_size(pos2(right - 20.0, rect.center().y), vec2(40.0, 22.0));
            let bg = if on { if focused { Color32::WHITE } else { theme::ACCENT } } else { theme::TRACK };
            p.rect_filled(track, 11.0, bg);
            let x = if on { track.right() - 11.0 } else { track.left() + 11.0 };
            let knob = if on && focused { theme::ACCENT } else { Color32::WHITE };
            p.circle_filled(pos2(x, track.center().y), 8.0, knob);
            right -= 52.0;
        }
        Trail::Value(v) | Trail::Text(v) => {
            let r = p.text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, v, FontId::proportional(15.0), dim);
            right = r.left() - 12.0;
        }
        Trail::Adjust(v) => {
            let shown = if focused { format!("‹  {v}  ›") } else { v.to_owned() };
            let r = p.text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, shown, FontId::proportional(16.0), text_color);
            right = r.left() - 12.0;
        }
    }
    let mut job = egui::text::LayoutJob::simple(title.to_owned(), font, text_color, (right - left).max(40.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = p.layout_job(job);
    p.galley(pos2(left, rect.center().y - galley.size().y / 2.0), galley, text_color);
}

/// A vertical list the remote moves through: which row is focused, and
/// which row is at the top of the visible part.
#[derive(Default, Clone, Copy)]
pub struct ListNav {
    pub focus: usize,
    pub top: usize,
}

impl ListNav {
    /// Moves by `delta` rows within `len`; returns false at either end.
    pub fn step(&mut self, delta: i32, len: usize) -> bool {
        if len == 0 {
            self.focus = 0;
            return false;
        }
        let next = self.focus as i64 + delta as i64;
        if next < 0 || next >= len as i64 {
            self.focus = self.focus.min(len - 1);
            return false;
        }
        self.focus = next as usize;
        true
    }

    /// Keeps the focus inside `0..len` and on screen, `visible` rows at a time.
    pub fn clamp(&mut self, len: usize, visible: usize) {
        self.focus = self.focus.min(len.saturating_sub(1));
        let visible = visible.max(1);
        if self.focus < self.top {
            self.top = self.focus;
        } else if self.focus >= self.top + visible {
            self.top = self.focus + 1 - visible;
        }
        self.top = self.top.min(len.saturating_sub(visible));
    }
}

/// Small "more above / more below" arrows at the edge of a list that scrolls.
pub fn scroll_hints(p: &Painter, area: Rect, more_above: bool, more_below: bool) {
    let arrow = |c: egui::Pos2, up: bool| {
        let d = if up { -1.0 } else { 1.0 };
        let pts = vec![c + vec2(-8.0, -3.0 * d), c + vec2(0.0, 4.0 * d), c + vec2(8.0, -3.0 * d)];
        p.add(egui::Shape::line(pts, Stroke::new(2.0, theme::TEXT_DIM)));
    };
    if more_above {
        arrow(pos2(area.center().x, area.top() - 8.0), true);
    }
    if more_below {
        arrow(pos2(area.center().x, area.bottom() + 8.0), false);
    }
}

/// A hint line at the bottom of the screen, e.g. "OK — открыть   ◀ Назад".
pub fn hint(p: &Painter, screen: Rect, text: &str) {
    p.text(
        pos2(screen.left() + 40.0, screen.bottom() - 22.0),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(14.0),
        theme::TEXT_DIM,
    );
}

/// How far a held ← / → seeks per press: further the longer it is held.
pub fn seek_step_ms(held: u32) -> i64 {
    match held {
        0..=5 => 10_000,
        6..=15 => 30_000,
        _ => 60_000,
    }
}
