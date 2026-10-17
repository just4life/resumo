//! Receives decoded RGBA frames from libVLC's video thread and hands the
//! newest one to the UI thread.

use std::cell::UnsafeCell;
use std::ffi::{c_char, c_uint, c_void};
use std::sync::Mutex;

use crate::vlc::VideoCallbacks;

/// The latest finished frame, tightly packed RGBA.
#[derive(Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    /// Incremented on every new frame so the UI knows when to re-upload.
    pub serial: u64,
}

struct Layout {
    width: usize,
    height: usize,
    pitch: usize,
}

pub struct VideoSink {
    /// Buffer libVLC decodes into. Only touched on the video thread, between
    /// the format, lock, unlock and display callbacks, which libVLC serialises.
    back: UnsafeCell<Vec<u8>>,
    layout: UnsafeCell<Layout>,
    front: Mutex<Frame>,
    repaint: Box<dyn Fn() + Send + Sync>,
}

// SAFETY: `back` and `layout` are only accessed from libVLC's (single) video
// output thread; everything shared with the UI goes through `front`.
unsafe impl Sync for VideoSink {}
unsafe impl Send for VideoSink {}

fn align(v: usize, to: usize) -> usize {
    v.div_ceil(to) * to
}

impl VideoSink {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            back: UnsafeCell::new(Vec::new()),
            layout: UnsafeCell::new(Layout { width: 0, height: 0, pitch: 0 }),
            front: Mutex::new(Frame::default()),
            repaint: Box::new(repaint),
        }
    }

    /// Runs `f` with the current frame.
    pub fn with_frame<R>(&self, f: impl FnOnce(&Frame) -> R) -> R {
        f(&self.front.lock().unwrap())
    }

    /// Forgets the current picture (used when playback stops).
    pub fn clear(&self) {
        let mut front = self.front.lock().unwrap();
        front.width = 0;
        front.height = 0;
        front.pixels.clear();
        front.serial += 1;
    }

    /// Callback table pointing at `self`; `self` must outlive the player.
    pub fn callbacks(&self) -> VideoCallbacks {
        VideoCallbacks {
            format: format_cb,
            cleanup: Some(cleanup_cb),
            lock: lock_cb,
            unlock: None,
            display: Some(display_cb),
            opaque: self as *const Self as *mut c_void,
        }
    }
}

unsafe extern "C" fn format_cb(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    let sink = &*(*opaque as *const VideoSink);
    std::ptr::copy_nonoverlapping(b"RGBA".as_ptr() as *const c_char, chroma, 4);
    let w = *width as usize;
    let h = *height as usize;
    if w == 0 || h == 0 {
        return 0;
    }
    // VLC's converters like 32-byte aligned rows and a few spare lines.
    let pitch = align(w * 4, 32);
    let rows = align(h, 32);
    *pitches = pitch as c_uint;
    *lines = rows as c_uint;
    let back = &mut *sink.back.get();
    back.clear();
    back.resize(pitch * rows, 0);
    *sink.layout.get() = Layout { width: w, height: h, pitch };
    1
}

unsafe extern "C" fn cleanup_cb(opaque: *mut c_void) {
    let sink = &*(opaque as *const VideoSink);
    sink.clear();
    (sink.repaint)();
}

unsafe extern "C" fn lock_cb(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    let sink = &*(opaque as *const VideoSink);
    *planes = (*sink.back.get()).as_mut_ptr() as *mut c_void;
    std::ptr::null_mut()
}

unsafe extern "C" fn display_cb(opaque: *mut c_void, _picture: *mut c_void) {
    let sink = &*(opaque as *const VideoSink);
    let back = &*sink.back.get();
    let layout = &*sink.layout.get();
    let row = layout.width * 4;
    {
        let mut front = sink.front.lock().unwrap();
        front.width = layout.width;
        front.height = layout.height;
        front.pixels.resize(row * layout.height, 0);
        for y in 0..layout.height {
            let src = &back[y * layout.pitch..y * layout.pitch + row];
            front.pixels[y * row..(y + 1) * row].copy_from_slice(src);
        }
        front.serial += 1;
    }
    (sink.repaint)();
}
