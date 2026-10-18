//! "Previous / play-pause / next" buttons under the window's thumbnail on the
//! Windows taskbar (the thumbnail toolbar, as in Spotify). On other systems
//! [`ThumbBar::update`] does nothing.
//!
//! Windows sends button clicks to the window as `WM_COMMAND`, so the window is
//! subclassed to catch them before winit sees the message.

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Prev,
    PlayPause,
    Next,
}

// The glyphs are only drawn on Windows (and in tests).
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Prev,
    Play,
    Pause,
    Next,
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Convex polygons of a glyph in a unit square (y down).
fn shapes(glyph: Glyph) -> Vec<Vec<(f32, f32)>> {
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    match glyph {
        Glyph::Play => vec![vec![(0.31, 0.19), (0.81, 0.5), (0.31, 0.81)]],
        Glyph::Pause => vec![rect(0.27, 0.2, 0.43, 0.8), rect(0.57, 0.2, 0.73, 0.8)],
        Glyph::Prev => vec![rect(0.2, 0.24, 0.32, 0.76), vec![(0.8, 0.24), (0.8, 0.76), (0.36, 0.5)]],
        Glyph::Next => vec![rect(0.68, 0.24, 0.8, 0.76), vec![(0.2, 0.24), (0.64, 0.5), (0.2, 0.76)]],
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut sign = 0.0f32;
    for (i, &(ax, ay)) in poly.iter().enumerate() {
        let (bx, by) = poly[(i + 1) % poly.len()];
        let cross = (bx - ax) * (y - ay) - (by - ay) * (x - ax);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}

#[cfg_attr(not(windows), allow(dead_code))]
/// White glyph on transparency, `size`×`size` straight-alpha RGBA,
/// antialiased with 4×4 samples per pixel.
pub fn glyph_rgba(glyph: Glyph, size: u32) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let shapes = shapes(glyph);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    if shapes.iter().any(|s| inside(s, x, y)) {
                        hits += 1;
                    }
                }
            }
            let alpha = (hits * 255 / (SAMPLES * SAMPLES)) as u8;
            out.extend_from_slice(&[255, 255, 255, alpha]);
        }
    }
    out
}

#[cfg(not(windows))]
#[derive(Default)]
pub struct ThumbBar;

#[cfg(not(windows))]
impl ThumbBar {
    pub fn update(&mut self, _: &eframe::Frame, _: &egui::Context, _playing: bool, _has_items: bool) -> Option<Action> {
        None
    }
}

#[cfg(windows)]
pub use win::ThumbBar;

#[cfg(windows)]
mod win {
    use super::{glyph_rgba, Action, Glyph};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    type Hwnd = *mut c_void;
    type Handle = *mut c_void;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);

    const CLSID_TASKBAR_LIST: Guid = Guid(0x56FD_F344, 0xFD6D, 0x11D0, [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90]);
    const IID_ITASKBAR_LIST3: Guid = Guid(0xEA1A_FB91, 0x9E28, 0x4B86, [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF]);
    const CLSCTX_INPROC_SERVER: u32 = 1;
    const COINIT_APARTMENTTHREADED: u32 = 2;

    const WM_COMMAND: u32 = 0x0111;
    const THBN_CLICKED: usize = 0x1800;
    const THB_ICON: u32 = 0x2;
    const THB_TOOLTIP: u32 = 0x4;
    const THB_FLAGS: u32 = 0x8;
    const THBF_ENABLED: u32 = 0x0;
    const THBF_DISABLED: u32 = 0x1;
    const SM_CXSMICON: i32 = 49;

    const ID_PREV: u32 = 1;
    const ID_PLAY_PAUSE: u32 = 2;
    const ID_NEXT: u32 = 3;
    const SUBCLASS_ID: usize = 0x5650_4C59; // "VPLY"
    const RETRY_AFTER: Duration = Duration::from_secs(1);

    /// `ITaskbarList3` vtable up to the methods used here.
    #[repr(C)]
    struct Vtbl {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut Com) -> u32,
        hr_init: unsafe extern "system" fn(*mut Com) -> i32,
        _list1: [usize; 4], // AddTab, DeleteTab, ActivateTab, SetActiveAlt
        _list2: [usize; 1], // MarkFullscreenWindow
        _list3: [usize; 6], // SetProgressValue … SetTabActive
        thumb_bar_add_buttons: unsafe extern "system" fn(*mut Com, Hwnd, u32, *const ThumbButton) -> i32,
        thumb_bar_update_buttons: unsafe extern "system" fn(*mut Com, Hwnd, u32, *const ThumbButton) -> i32,
    }

    #[repr(C)]
    struct Com {
        vtbl: *const Vtbl,
    }

    #[repr(C)]
    struct ThumbButton {
        mask: u32,
        id: u32,
        bitmap: u32,
        icon: Handle,
        tip: [u16; 260],
        flags: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_ppm: i32,
        y_ppm: i32,
        clr_used: u32,
        clr_important: u32,
        colors: [u32; 1],
    }

    #[repr(C)]
    struct IconInfo {
        is_icon: i32,
        x_hotspot: u32,
        y_hotspot: u32,
        mask: Handle,
        color: Handle,
    }

    type SubclassProc = unsafe extern "system" fn(Hwnd, u32, usize, isize, usize, usize) -> isize;

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
        fn CoCreateInstance(clsid: *const Guid, outer: *mut c_void, ctx: u32, iid: *const Guid, out: *mut *mut c_void) -> i32;
    }
    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(hwnd: Hwnd, proc_: SubclassProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    }
    #[link(name = "user32")]
    extern "system" {
        fn RegisterWindowMessageW(name: *const u16) -> u32;
        fn GetSystemMetrics(index: i32) -> i32;
        fn CreateIconIndirect(info: *const IconInfo) -> Handle;
        fn DestroyIcon(icon: Handle) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateDIBSection(hdc: Handle, info: *const BitmapInfo, usage: u32, bits: *mut *mut c_void, section: Handle, offset: u32) -> Handle;
        fn CreateBitmap(width: i32, height: i32, planes: u32, bit_count: u32, bits: *const c_void) -> Handle;
        fn DeleteObject(object: Handle) -> i32;
    }

    // Shared with the window procedure, which has no access to the app.
    static CLICKED: AtomicU32 = AtomicU32::new(0);
    static NEED_ADD: AtomicBool = AtomicBool::new(true);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static REPAINT: OnceLock<egui::Context> = OnceLock::new();

    unsafe extern "system" fn subclass_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize, _: usize, _: usize) -> isize {
        if msg == WM_COMMAND && (wparam >> 16) & 0xFFFF == THBN_CLICKED {
            CLICKED.store((wparam & 0xFFFF) as u32, Relaxed);
            repaint();
            return 0;
        }
        // Sent when the taskbar button appears, also after Explorer restarts;
        // buttons can only be added from then on.
        let created = TASKBAR_CREATED.load(Relaxed);
        if created != 0 && msg == created {
            NEED_ADD.store(true, Relaxed);
            repaint();
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    fn repaint() {
        if let Some(ctx) = REPAINT.get() {
            ctx.request_repaint();
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    unsafe fn make_icon(glyph: Glyph, size: i32) -> Handle {
        let info = BitmapInfo {
            size: 40,
            width: size,
            height: -size, // top-down rows
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_ppm: 0,
            y_ppm: 0,
            clr_used: 0,
            clr_important: 0,
            colors: [0],
        };
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &info, 0, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        let rgba = glyph_rgba(glyph, size as u32);
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
        for (d, s) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        // The alpha channel decides transparency; the mask only has to exist.
        let mask_bits = vec![0u8; ((size as usize + 15) / 16 * 2) * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr() as *const c_void);
        let icon = CreateIconIndirect(&IconInfo { is_icon: 1, x_hotspot: 0, y_hotspot: 0, mask, color });
        DeleteObject(color);
        if !mask.is_null() {
            DeleteObject(mask);
        }
        icon
    }

    pub struct ThumbBar {
        hwnd: Hwnd,
        list: *mut Com,
        icons: [Handle; 4], // prev, play, pause, next
        /// (playing, has_items) the buttons currently show.
        shown: Option<(bool, bool)>,
        /// Fallback in case the TaskbarButtonCreated message was missed.
        retries: u32,
        retry_at: Option<Instant>,
        failed: bool,
    }

    impl Default for ThumbBar {
        fn default() -> Self {
            Self { hwnd: null_mut(), list: null_mut(), icons: [null_mut(); 4], shown: None, retries: 0, retry_at: None, failed: false }
        }
    }

    impl ThumbBar {
        /// Call once per frame. Adds the buttons when the taskbar is ready,
        /// keeps the play/pause icon in sync and returns a clicked button.
        pub fn update(&mut self, frame: &eframe::Frame, ctx: &egui::Context, playing: bool, has_items: bool) -> Option<Action> {
            if self.failed {
                return None;
            }
            if self.list.is_null() {
                if let Err(()) = unsafe { self.init(frame, ctx) } {
                    self.failed = true;
                    return None;
                }
            }
            let state = (playing, has_items);
            let retry = self.shown.is_none() && self.retry_at.is_some_and(|t| Instant::now() >= t);
            if NEED_ADD.swap(false, Relaxed) || retry {
                let buttons = self.buttons(state);
                let hr = unsafe { ((*(*self.list).vtbl).thumb_bar_add_buttons)(self.list, self.hwnd, 3, buttons.as_ptr()) };
                // Fails until the taskbar button exists; TaskbarButtonCreated
                // asks again, and a few timed retries cover a missed message.
                self.retry_at = None;
                if hr >= 0 {
                    self.shown = Some(state);
                } else if self.shown.is_none() && self.retries < 10 {
                    self.retries += 1;
                    self.retry_at = Some(Instant::now() + RETRY_AFTER);
                    ctx.request_repaint_after(RETRY_AFTER);
                }
            } else if self.shown.is_some_and(|s| s != state) {
                let buttons = self.buttons(state);
                let hr = unsafe { ((*(*self.list).vtbl).thumb_bar_update_buttons)(self.list, self.hwnd, 3, buttons.as_ptr()) };
                if hr >= 0 {
                    self.shown = Some(state);
                }
            }
            match CLICKED.swap(0, Relaxed) {
                ID_PREV => Some(Action::Prev),
                ID_PLAY_PAUSE => Some(Action::PlayPause),
                ID_NEXT => Some(Action::Next),
                _ => None,
            }
        }

        unsafe fn init(&mut self, frame: &eframe::Frame, ctx: &egui::Context) -> Result<(), ()> {
            let Ok(handle) = frame.window_handle() else { return Err(()) };
            let RawWindowHandle::Win32(h) = handle.as_raw() else { return Err(()) };
            self.hwnd = h.hwnd.get() as Hwnd;

            // winit has normally initialised COM already; this is a no-op then.
            CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED);
            let mut list = null_mut();
            let hr = CoCreateInstance(&CLSID_TASKBAR_LIST, null_mut(), CLSCTX_INPROC_SERVER, &IID_ITASKBAR_LIST3, &mut list);
            if hr < 0 || list.is_null() {
                return Err(());
            }
            self.list = list as *mut Com;
            if ((*(*self.list).vtbl).hr_init)(self.list) < 0 {
                return Err(());
            }

            let size = GetSystemMetrics(SM_CXSMICON).clamp(16, 64);
            self.icons = [Glyph::Prev, Glyph::Play, Glyph::Pause, Glyph::Next].map(|g| make_icon(g, size));

            let _ = REPAINT.set(ctx.clone());
            TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarButtonCreated").as_ptr()), Relaxed);
            if SetWindowSubclass(self.hwnd, subclass_proc, SUBCLASS_ID, 0) == 0 {
                return Err(());
            }
            Ok(())
        }

        fn buttons(&self, (playing, has_items): (bool, bool)) -> [ThumbButton; 3] {
            let flags = if has_items { THBF_ENABLED } else { THBF_DISABLED };
            let button = |id: u32, icon: Handle, tip: &str| {
                let mut b = ThumbButton { mask: THB_ICON | THB_TOOLTIP | THB_FLAGS, id, bitmap: 0, icon, tip: [0; 260], flags };
                for (d, s) in b.tip.iter_mut().zip(tip.encode_utf16().take(259)) {
                    *d = s;
                }
                b
            };
            let [prev, play, pause, next] = self.icons;
            [
                button(ID_PREV, prev, "Предыдущий"),
                if playing { button(ID_PLAY_PAUSE, pause, "Пауза") } else { button(ID_PLAY_PAUSE, play, "Воспроизвести") },
                button(ID_NEXT, next, "Следующий"),
            ]
        }
    }

    impl Drop for ThumbBar {
        fn drop(&mut self) {
            unsafe {
                if !self.list.is_null() {
                    ((*(*self.list).vtbl).release)(self.list);
                }
                for icon in self.icons {
                    if !icon.is_null() {
                        DestroyIcon(icon);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_drawn_where_expected() {
        let alpha = |g: Glyph, x: usize, y: usize| glyph_rgba(g, 16)[(y * 16 + x) * 4 + 3];
        for g in [Glyph::Prev, Glyph::Play, Glyph::Pause, Glyph::Next] {
            let img = glyph_rgba(g, 16);
            assert_eq!(img.len(), 16 * 16 * 4);
            assert_eq!(alpha(g, 0, 0), 0, "{g:?}: corner must be transparent");
        }
        assert_eq!(alpha(Glyph::Play, 7, 8), 255);
        assert_eq!(alpha(Glyph::Pause, 8, 8), 0, "gap between the pause bars");
        assert_eq!(alpha(Glyph::Pause, 5, 8), 255);
        assert_eq!(alpha(Glyph::Prev, 4, 8), 255, "bar on the left");
        assert_eq!(alpha(Glyph::Next, 11, 8), 255, "bar on the right");
    }
}
