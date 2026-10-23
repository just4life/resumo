//! Sizing the window to a newly opened video: the video at its own size, but
//! never more than fits on the screen.

use egui::{vec2, Rect, Vec2};

/// The window size that shows `video` 1:1 beside `chrome` (title bar,
/// playlist), shrunk to fit in `room`, and at least `min`. One unit for all.
pub fn window_size(video: Vec2, chrome: Vec2, room: Vec2, min: Vec2) -> Vec2 {
    let avail = (room - chrome).max(Vec2::splat(1.0));
    let scale = (avail.x / video.x).min(avail.y / video.y).min(1.0);
    (video * scale + chrome).round().max(min).min(room.max(min))
}

/// Resizes the window around `video_area` (where the picture is drawn now,
/// in points) for a picture of `video` pixels.
pub fn fit(frame: &eframe::Frame, ctx: &egui::Context, video_area: Rect, video: Vec2, min: Vec2) {
    if video.x < 1.0 || video.y < 1.0 || video_area.width() < 1.0 {
        return;
    }
    #[cfg(windows)]
    if win::fit(frame, video_area.size() * ctx.pixels_per_point(), video, min * ctx.pixels_per_point()) {
        return;
    }
    let _ = frame;
    // Elsewhere: no work area to ask for, so leave room for panels and docks.
    let ppp = ctx.pixels_per_point();
    let (inner, monitor) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().monitor_size));
    let (Some(inner), Some(monitor)) = (inner, monitor) else { return };
    let chrome = inner.size() - video_area.size();
    let size = window_size(video / ppp, chrome, monitor * vec2(0.92, 0.88), min);
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use egui::{vec2, Vec2};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct WinRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: WinRect,
        work: WinRect,
        flags: u32,
    }
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const SWP_NOZORDER: u32 = 0x4;
    const SWP_NOACTIVATE: u32 = 0x10;

    #[link(name = "user32")]
    extern "system" {
        fn GetWindowRect(hwnd: *mut c_void, rect: *mut WinRect) -> i32;
        fn MonitorFromWindow(hwnd: *mut c_void, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(monitor: *mut c_void, info: *mut MonitorInfo) -> i32;
        fn SetWindowPos(hwnd: *mut c_void, after: *mut c_void, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    }

    /// Fits the window into the work area (the screen minus the taskbar) of
    /// its monitor, in physical pixels. Keeps its centre where it can.
    pub fn fit(frame: &eframe::Frame, video_area: Vec2, video: Vec2, min: Vec2) -> bool {
        let Ok(handle) = frame.window_handle() else { return false };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return false };
        let hwnd = h.hwnd.get() as *mut c_void;
        unsafe {
            let mut win = WinRect::default();
            let mut info = MonitorInfo {
                size: std::mem::size_of::<MonitorInfo>() as u32,
                monitor: WinRect::default(),
                work: WinRect::default(),
                flags: 0,
            };
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if GetWindowRect(hwnd, &mut win) == 0 || monitor.is_null() || GetMonitorInfoW(monitor, &mut info) == 0 {
                return false;
            }
            let work = info.work;
            let current = vec2((win.right - win.left) as f32, (win.bottom - win.top) as f32);
            let room = vec2((work.right - work.left) as f32, (work.bottom - work.top) as f32);
            let size = super::window_size(video, current - video_area, room, min);
            let (w, h) = (size.x as i32, size.y as i32);
            let cx = (win.left + win.right) / 2;
            let cy = (win.top + win.bottom) / 2;
            let x = (cx - w / 2).clamp(work.left, (work.right - w).max(work.left));
            let y = (cy - h / 2).clamp(work.top, (work.bottom - h).max(work.top));
            SetWindowPos(hwnd, std::ptr::null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_shown_whole_but_never_off_screen() {
        let chrome = vec2(0.0, 32.0);
        let min = vec2(640.0, 380.0);
        let room = vec2(2560.0, 1392.0);
        // Fits as is.
        assert_eq!(window_size(vec2(1920.0, 1080.0), chrome, room, min), vec2(1920.0, 1112.0));
        // 4K on a 1440p screen: shrunk by height, aspect kept.
        let s = window_size(vec2(3840.0, 2160.0), chrome, room, min);
        assert_eq!(s.y, 1392.0);
        assert!(((s.x) / (s.y - 32.0) - 16.0 / 9.0).abs() < 0.01);
        // Tiny video: the window's minimum size wins.
        assert_eq!(window_size(vec2(320.0, 240.0), chrome, room, min), min);
    }
}
