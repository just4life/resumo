//! Receives decoded frames from libVLC's video thread and hands the newest
//! one to the UI thread.

use std::cell::UnsafeCell;
use std::ffi::{c_char, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::vlc::{PlayerHandle, VideoCallbacks};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PixelFormat {
    /// 8-bit RGBA, converted by libVLC. Fine for SDR video.
    #[default]
    Rgba,
    /// 10-bit 4:2:0 YUV as it comes from the decoder: a plane of 16-bit Y
    /// samples followed by a half-size plane of interleaved 16-bit U, V.
    /// Values sit in the high bits. Used for HDR, which we convert ourselves.
    P010,
}

/// The latest finished frame, tightly packed.
#[derive(Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
    /// Incremented on every new frame so the UI knows when to re-upload.
    pub serial: u64,
}

impl Frame {
    /// Y and interleaved UV planes of a P010 frame.
    pub fn p010_planes(&self) -> (&[u8], &[u8]) {
        self.pixels.split_at(self.width * self.height * 2)
    }
}

/// Size of the chroma plane of a 4:2:0 picture.
pub fn chroma_size(width: usize, height: usize) -> (usize, usize) {
    (width.div_ceil(2), height.div_ceil(2))
}

struct Layout {
    width: usize,
    height: usize,
    format: PixelFormat,
    pitches: [usize; 2],
    /// Offset of the second plane in `back`.
    plane1: usize,
}

pub struct VideoSink {
    /// Buffer libVLC decodes into. Only touched on the video thread, between
    /// the format, lock, unlock and display callbacks, which libVLC serialises.
    back: UnsafeCell<Vec<u8>>,
    layout: UnsafeCell<Layout>,
    front: Mutex<Frame>,
    /// Ask libVLC for P010 instead of RGBA from the next format negotiation.
    want_p010: AtomicBool,
    player: OnceLock<PlayerHandle>,
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
            layout: UnsafeCell::new(Layout {
                width: 0,
                height: 0,
                format: PixelFormat::Rgba,
                pitches: [0; 2],
                plane1: 0,
            }),
            front: Mutex::new(Frame::default()),
            want_p010: AtomicBool::new(false),
            player: OnceLock::new(),
            repaint: Box::new(repaint),
        }
    }

    /// Picks the format for the next media; takes effect when libVLC sets up
    /// its video output, i.e. call it before starting playback.
    pub fn set_format(&self, format: PixelFormat) {
        self.want_p010.store(format == PixelFormat::P010, Ordering::SeqCst);
    }

    /// The player feeding this sink; used to learn the real picture size.
    pub fn set_player(&self, player: PlayerHandle) {
        let _ = self.player.set(player);
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
    // libVLC offers the decoder's buffer size, padded to a multiple of 16 or
    // 32 rows (1080 becomes 1088), and would stretch the picture to fill it.
    // Ask for the real size instead.
    if let Some((vw, vh)) = sink.player.get().and_then(|p| p.video_size()) {
        if vw <= *width && vh <= *height {
            *width = vw;
            *height = vh;
        }
    }
    let w = *width as usize;
    let h = *height as usize;
    if w == 0 || h == 0 {
        return 0;
    }
    let format = if sink.want_p010.load(Ordering::SeqCst) { PixelFormat::P010 } else { PixelFormat::Rgba };
    let fourcc = match format {
        PixelFormat::Rgba => b"RGBA",
        PixelFormat::P010 => b"P010",
    };
    std::ptr::copy_nonoverlapping(fourcc.as_ptr() as *const c_char, chroma, 4);
    // VLC's converters like 32-byte aligned rows and a few spare lines.
    let rows = align(h, 32);
    let (pitches_px, lines_px) = match format {
        PixelFormat::Rgba => ([align(w * 4, 32), 0], [rows, 0]),
        PixelFormat::P010 => {
            let (cw, _) = chroma_size(w, h);
            ([align(w * 2, 32), align(cw * 4, 32)], [rows, rows / 2])
        }
    };
    let pitches = std::slice::from_raw_parts_mut(pitches, 2);
    let lines = std::slice::from_raw_parts_mut(lines, 2);
    let planes = if format == PixelFormat::P010 { 2 } else { 1 };
    for i in 0..planes {
        pitches[i] = pitches_px[i] as c_uint;
        lines[i] = lines_px[i] as c_uint;
    }
    let plane1 = pitches_px[0] * lines_px[0];
    let back = &mut *sink.back.get();
    back.clear();
    back.resize(plane1 + pitches_px[1] * lines_px[1], 0);
    *sink.layout.get() = Layout { width: w, height: h, format, pitches: pitches_px, plane1 };
    1
}

unsafe extern "C" fn cleanup_cb(opaque: *mut c_void) {
    let sink = &*(opaque as *const VideoSink);
    sink.clear();
    (sink.repaint)();
}

unsafe extern "C" fn lock_cb(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    let sink = &*(opaque as *const VideoSink);
    let back = (*sink.back.get()).as_mut_ptr();
    let layout = &*sink.layout.get();
    *planes = back as *mut c_void;
    if layout.format == PixelFormat::P010 {
        *planes.add(1) = back.add(layout.plane1) as *mut c_void;
    }
    std::ptr::null_mut()
}

unsafe extern "C" fn display_cb(opaque: *mut c_void, _picture: *mut c_void) {
    let sink = &*(opaque as *const VideoSink);
    let back = &*sink.back.get();
    let layout = &*sink.layout.get();
    let (w, h) = (layout.width, layout.height);
    // (offset in `back`, pitch, bytes per row, rows) of each plane.
    let planes = match layout.format {
        PixelFormat::Rgba => vec![(0, layout.pitches[0], w * 4, h)],
        PixelFormat::P010 => {
            let (cw, ch) = chroma_size(w, h);
            vec![(0, layout.pitches[0], w * 2, h), (layout.plane1, layout.pitches[1], cw * 4, ch)]
        }
    };
    {
        let mut front = sink.front.lock().unwrap();
        front.width = w;
        front.height = h;
        front.format = layout.format;
        front.pixels.clear();
        for (offset, pitch, row, rows) in planes {
            for y in 0..rows {
                front.pixels.extend_from_slice(&back[offset + y * pitch..offset + y * pitch + row]);
            }
        }
        front.serial += 1;
    }
    (sink.repaint)();
}
