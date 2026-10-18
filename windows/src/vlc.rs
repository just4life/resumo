//! Minimal hand-written bindings to libVLC 3.x and safe wrappers around them.
//!
//! Only the subset of the API the player needs is declared here, so the crate
//! has no C code, no bindgen step and no `-sys` dependency: it links directly
//! against the `libvlc` shared library shipped with VLC.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};

pub type libvlc_time_t = i64;

#[repr(C)]
pub struct libvlc_instance_t {
    _p: [u8; 0],
}
#[repr(C)]
pub struct libvlc_media_t {
    _p: [u8; 0],
}
#[repr(C)]
pub struct libvlc_media_player_t {
    _p: [u8; 0],
}
#[repr(C)]
pub struct libvlc_event_manager_t {
    _p: [u8; 0],
}

#[repr(C)]
pub struct libvlc_track_description_t {
    pub i_id: c_int,
    pub psz_name: *mut c_char,
    pub p_next: *mut libvlc_track_description_t,
}

#[repr(C)]
pub struct libvlc_audio_output_device_t {
    pub p_next: *mut libvlc_audio_output_device_t,
    pub psz_device: *mut c_char,
    pub psz_description: *mut c_char,
}

/// Only the leading `type` field is read; the rest of the event union is ignored.
#[repr(C)]
pub struct libvlc_event_t {
    pub type_: c_int,
    pub p_obj: *mut c_void,
}

pub type libvlc_callback_t = unsafe extern "C" fn(event: *const libvlc_event_t, data: *mut c_void);
pub type libvlc_video_lock_cb = unsafe extern "C" fn(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void;
pub type libvlc_video_unlock_cb =
    unsafe extern "C" fn(opaque: *mut c_void, picture: *mut c_void, planes: *const *mut c_void);
pub type libvlc_video_display_cb = unsafe extern "C" fn(opaque: *mut c_void, picture: *mut c_void);
pub type libvlc_video_format_cb = unsafe extern "C" fn(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint;
pub type libvlc_video_cleanup_cb = unsafe extern "C" fn(opaque: *mut c_void);

pub const EVENT_PLAYING: c_int = 0x104;
pub const EVENT_END_REACHED: c_int = 0x109;
pub const EVENT_ERROR: c_int = 0x10A;

const SLAVE_TYPE_SUBTITLE: c_int = 0;

#[cfg_attr(windows, link(name = "libvlc"))]
#[cfg_attr(not(windows), link(name = "vlc"))]
extern "C" {
    fn libvlc_new(argc: c_int, argv: *const *const c_char) -> *mut libvlc_instance_t;
    fn libvlc_release(inst: *mut libvlc_instance_t);
    fn libvlc_errmsg() -> *const c_char;
    fn libvlc_get_version() -> *const c_char;

    fn libvlc_media_new_path(inst: *mut libvlc_instance_t, path: *const c_char) -> *mut libvlc_media_t;
    fn libvlc_media_new_location(inst: *mut libvlc_instance_t, mrl: *const c_char) -> *mut libvlc_media_t;
    fn libvlc_media_release(md: *mut libvlc_media_t);

    fn libvlc_media_player_new(inst: *mut libvlc_instance_t) -> *mut libvlc_media_player_t;
    fn libvlc_media_player_release(mp: *mut libvlc_media_player_t);
    fn libvlc_media_player_set_media(mp: *mut libvlc_media_player_t, md: *mut libvlc_media_t);
    fn libvlc_media_player_event_manager(mp: *mut libvlc_media_player_t) -> *mut libvlc_event_manager_t;
    fn libvlc_media_player_is_playing(mp: *mut libvlc_media_player_t) -> c_int;
    fn libvlc_media_player_play(mp: *mut libvlc_media_player_t) -> c_int;
    fn libvlc_media_player_set_pause(mp: *mut libvlc_media_player_t, do_pause: c_int);
    fn libvlc_media_player_stop(mp: *mut libvlc_media_player_t);
    fn libvlc_media_player_get_length(mp: *mut libvlc_media_player_t) -> libvlc_time_t;
    fn libvlc_media_player_get_time(mp: *mut libvlc_media_player_t) -> libvlc_time_t;
    fn libvlc_media_player_set_time(mp: *mut libvlc_media_player_t, t: libvlc_time_t);
    fn libvlc_media_player_get_position(mp: *mut libvlc_media_player_t) -> f32;
    fn libvlc_media_player_set_position(mp: *mut libvlc_media_player_t, pos: f32);
    fn libvlc_media_player_set_rate(mp: *mut libvlc_media_player_t, rate: f32) -> c_int;
    fn libvlc_media_player_is_seekable(mp: *mut libvlc_media_player_t) -> c_int;
    fn libvlc_media_player_add_slave(
        mp: *mut libvlc_media_player_t,
        type_: c_int,
        uri: *const c_char,
        select: bool,
    ) -> c_int;

    fn libvlc_event_attach(
        em: *mut libvlc_event_manager_t,
        type_: c_int,
        cb: libvlc_callback_t,
        data: *mut c_void,
    ) -> c_int;

    fn libvlc_video_set_callbacks(
        mp: *mut libvlc_media_player_t,
        lock: libvlc_video_lock_cb,
        unlock: Option<libvlc_video_unlock_cb>,
        display: Option<libvlc_video_display_cb>,
        opaque: *mut c_void,
    );
    fn libvlc_video_set_format_callbacks(
        mp: *mut libvlc_media_player_t,
        setup: libvlc_video_format_cb,
        cleanup: Option<libvlc_video_cleanup_cb>,
    );
    fn libvlc_video_set_key_input(mp: *mut libvlc_media_player_t, on: c_uint);
    fn libvlc_video_get_size(mp: *mut libvlc_media_player_t, num: c_uint, px: *mut c_uint, py: *mut c_uint) -> c_int;
    fn libvlc_video_set_mouse_input(mp: *mut libvlc_media_player_t, on: c_uint);
    fn libvlc_video_get_spu(mp: *mut libvlc_media_player_t) -> c_int;
    fn libvlc_video_set_spu(mp: *mut libvlc_media_player_t, id: c_int) -> c_int;
    fn libvlc_video_get_spu_description(mp: *mut libvlc_media_player_t) -> *mut libvlc_track_description_t;
    fn libvlc_video_set_spu_delay(mp: *mut libvlc_media_player_t, delay_us: i64) -> c_int;

    fn libvlc_audio_get_track_description(mp: *mut libvlc_media_player_t) -> *mut libvlc_track_description_t;
    fn libvlc_audio_get_track(mp: *mut libvlc_media_player_t) -> c_int;
    fn libvlc_audio_set_track(mp: *mut libvlc_media_player_t, id: c_int) -> c_int;
    fn libvlc_audio_set_volume(mp: *mut libvlc_media_player_t, volume: c_int) -> c_int;
    fn libvlc_audio_set_mute(mp: *mut libvlc_media_player_t, mute: c_int);
    fn libvlc_audio_set_delay(mp: *mut libvlc_media_player_t, delay_us: i64) -> c_int;

    fn libvlc_track_description_list_release(list: *mut libvlc_track_description_t);

    fn libvlc_audio_output_device_enum(mp: *mut libvlc_media_player_t) -> *mut libvlc_audio_output_device_t;
    fn libvlc_audio_output_device_list_release(list: *mut libvlc_audio_output_device_t);
    fn libvlc_audio_output_device_set(mp: *mut libvlc_media_player_t, module: *const c_char, device: *const c_char);
    fn libvlc_audio_output_device_get(mp: *mut libvlc_media_player_t) -> *mut c_char;
    fn libvlc_free(ptr: *mut c_void);
}

fn last_error() -> String {
    unsafe {
        let p = libvlc_errmsg();
        if p.is_null() {
            "unknown libVLC error".to_owned()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

pub fn version() -> String {
    unsafe { CStr::from_ptr(libvlc_get_version()).to_string_lossy().into_owned() }
}

/// A libVLC engine instance.
pub struct Instance {
    raw: *mut libvlc_instance_t,
}

impl Instance {
    pub fn new(args: &[&str]) -> Result<Self, String> {
        let owned: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
        let argv: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
        let raw = unsafe { libvlc_new(argv.len() as c_int, argv.as_ptr()) };
        if raw.is_null() {
            Err(last_error())
        } else {
            Ok(Self { raw })
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { libvlc_release(self.raw) }
    }
}

/// A single audio or subtitle track as reported by libVLC.
#[derive(Clone, Debug)]
pub struct Track {
    pub id: i32,
    pub name: String,
}

unsafe fn collect_tracks(list: *mut libvlc_track_description_t) -> Vec<Track> {
    let mut out = Vec::new();
    let mut cur = list;
    while !cur.is_null() {
        let t = &*cur;
        let name = if t.psz_name.is_null() {
            format!("Track {}", t.i_id)
        } else {
            CStr::from_ptr(t.psz_name).to_string_lossy().into_owned()
        };
        out.push(Track { id: t.i_id, name });
        cur = t.p_next;
    }
    if !list.is_null() {
        libvlc_track_description_list_release(list);
    }
    out
}

/// An audio output device (speakers, headphones, HDMI, ...).
#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// The video output hooks a [`MediaPlayer`] renders into.
///
/// # Safety
/// `opaque` must stay valid until the player is released, and the callbacks
/// must be safe to call from libVLC's video output thread.
pub struct VideoCallbacks {
    pub format: libvlc_video_format_cb,
    pub cleanup: Option<libvlc_video_cleanup_cb>,
    pub lock: libvlc_video_lock_cb,
    pub unlock: Option<libvlc_video_unlock_cb>,
    pub display: Option<libvlc_video_display_cb>,
    pub opaque: *mut c_void,
}

pub struct MediaPlayer {
    raw: *mut libvlc_media_player_t,
    inst: *mut libvlc_instance_t,
}

/// Lets the video callbacks ask the player about the current video.
#[derive(Clone, Copy)]
pub struct PlayerHandle(*mut libvlc_media_player_t);

// SAFETY: libVLC's media player functions are thread-safe.
unsafe impl Send for PlayerHandle {}
unsafe impl Sync for PlayerHandle {}

impl PlayerHandle {
    /// Picture size as the container reports it (without decoder padding).
    pub fn video_size(&self) -> Option<(u32, u32)> {
        let (mut w, mut h) = (0, 0);
        let ok = unsafe { libvlc_video_get_size(self.0, 0, &mut w, &mut h) } == 0;
        (ok && w > 0 && h > 0).then_some((w, h))
    }
}

impl MediaPlayer {
    pub fn new(instance: &Instance) -> Result<Self, String> {
        let raw = unsafe { libvlc_media_player_new(instance.raw) };
        if raw.is_null() {
            return Err(last_error());
        }
        unsafe {
            // Keyboard and mouse belong to our UI, not to VLC's video window.
            libvlc_video_set_key_input(raw, 0);
            libvlc_video_set_mouse_input(raw, 0);
        }
        Ok(Self { raw, inst: instance.raw })
    }

    /// Valid as long as this player is.
    pub fn handle(&self) -> PlayerHandle {
        PlayerHandle(self.raw)
    }

    /// # Safety
    /// See [`VideoCallbacks`].
    pub unsafe fn set_video_callbacks(&self, cb: &VideoCallbacks) {
        libvlc_video_set_callbacks(self.raw, cb.lock, cb.unlock, cb.display, cb.opaque);
        libvlc_video_set_format_callbacks(self.raw, cb.format, cb.cleanup);
    }

    /// Registers `cb` for each event type in `events`.
    ///
    /// # Safety
    /// `data` must stay valid until the player is released. The callback runs
    /// on a libVLC thread and must not call back into libVLC.
    pub unsafe fn attach_events(&self, events: &[c_int], cb: libvlc_callback_t, data: *mut c_void) {
        let em = libvlc_media_player_event_manager(self.raw);
        for &e in events {
            libvlc_event_attach(em, e, cb, data);
        }
    }

    /// Loads a local path or a URL (anything with `://`).
    pub fn set_media(&self, location: &str) -> Result<(), String> {
        let c = CString::new(location).map_err(|e| e.to_string())?;
        unsafe {
            let md = if location.contains("://") {
                libvlc_media_new_location(self.inst, c.as_ptr())
            } else {
                libvlc_media_new_path(self.inst, c.as_ptr())
            };
            if md.is_null() {
                return Err(last_error());
            }
            libvlc_media_player_set_media(self.raw, md);
            libvlc_media_release(md);
        }
        Ok(())
    }

    pub fn play(&self) -> bool {
        unsafe { libvlc_media_player_play(self.raw) == 0 }
    }
    pub fn set_pause(&self, pause: bool) {
        unsafe { libvlc_media_player_set_pause(self.raw, pause as c_int) }
    }
    pub fn stop(&self) {
        unsafe { libvlc_media_player_stop(self.raw) }
    }
    pub fn is_playing(&self) -> bool {
        unsafe { libvlc_media_player_is_playing(self.raw) != 0 }
    }
    pub fn is_seekable(&self) -> bool {
        unsafe { libvlc_media_player_is_seekable(self.raw) != 0 }
    }
    /// Media length in milliseconds, or `None` when unknown.
    pub fn length(&self) -> Option<i64> {
        let l = unsafe { libvlc_media_player_get_length(self.raw) };
        (l > 0).then_some(l)
    }
    pub fn time(&self) -> i64 {
        unsafe { libvlc_media_player_get_time(self.raw) }.max(0)
    }
    pub fn set_time(&self, ms: i64) {
        unsafe { libvlc_media_player_set_time(self.raw, ms.max(0)) }
    }
    pub fn position(&self) -> f32 {
        unsafe { libvlc_media_player_get_position(self.raw) }.max(0.0)
    }
    pub fn set_position(&self, pos: f32) {
        unsafe { libvlc_media_player_set_position(self.raw, pos.clamp(0.0, 1.0)) }
    }
    pub fn set_rate(&self, rate: f32) {
        unsafe {
            libvlc_media_player_set_rate(self.raw, rate);
        }
    }

    pub fn set_volume(&self, v: i32) {
        unsafe {
            libvlc_audio_set_volume(self.raw, v.clamp(0, 200));
        }
    }
    pub fn set_mute(&self, mute: bool) {
        unsafe { libvlc_audio_set_mute(self.raw, mute as c_int) }
    }

    pub fn audio_tracks(&self) -> Vec<Track> {
        unsafe { collect_tracks(libvlc_audio_get_track_description(self.raw)) }
    }
    pub fn audio_track(&self) -> i32 {
        unsafe { libvlc_audio_get_track(self.raw) }
    }
    pub fn set_audio_track(&self, id: i32) {
        unsafe {
            libvlc_audio_set_track(self.raw, id);
        }
    }
    pub fn set_audio_delay_ms(&self, ms: i64) {
        unsafe {
            libvlc_audio_set_delay(self.raw, ms * 1000);
        }
    }

    pub fn subtitle_tracks(&self) -> Vec<Track> {
        unsafe { collect_tracks(libvlc_video_get_spu_description(self.raw)) }
    }
    pub fn subtitle_track(&self) -> i32 {
        unsafe { libvlc_video_get_spu(self.raw) }
    }
    pub fn set_subtitle_track(&self, id: i32) {
        unsafe {
            libvlc_video_set_spu(self.raw, id);
        }
    }
    pub fn set_subtitle_delay_ms(&self, ms: i64) {
        unsafe {
            libvlc_video_set_spu_delay(self.raw, ms * 1000);
        }
    }
    /// Devices of the current audio output. Usually empty until playback
    /// has started, because the output is created with the first media.
    pub fn audio_devices(&self) -> Vec<AudioDevice> {
        let mut out = Vec::new();
        unsafe {
            let list = libvlc_audio_output_device_enum(self.raw);
            let mut cur = list;
            while !cur.is_null() {
                let d = &*cur;
                if !d.psz_device.is_null() {
                    let id = CStr::from_ptr(d.psz_device).to_string_lossy().into_owned();
                    let name = if d.psz_description.is_null() {
                        id.clone()
                    } else {
                        CStr::from_ptr(d.psz_description).to_string_lossy().into_owned()
                    };
                    out.push(AudioDevice { id, name });
                }
                cur = d.p_next;
            }
            if !list.is_null() {
                libvlc_audio_output_device_list_release(list);
            }
        }
        out
    }
    pub fn audio_device(&self) -> Option<String> {
        unsafe {
            let p = libvlc_audio_output_device_get(self.raw);
            if p.is_null() {
                return None;
            }
            let id = CStr::from_ptr(p).to_string_lossy().into_owned();
            libvlc_free(p as *mut c_void);
            Some(id)
        }
    }
    pub fn set_audio_device(&self, id: &str) {
        if let Ok(c) = CString::new(id) {
            unsafe { libvlc_audio_output_device_set(self.raw, std::ptr::null(), c.as_ptr()) }
        }
    }

    /// Loads an external subtitle file into the current media and selects it.
    pub fn add_subtitle_file(&self, uri: &str) -> bool {
        let Ok(c) = CString::new(uri) else { return false };
        unsafe { libvlc_media_player_add_slave(self.raw, SLAVE_TYPE_SUBTITLE, c.as_ptr(), true) == 0 }
    }

}

impl Drop for MediaPlayer {
    fn drop(&mut self) {
        unsafe {
            libvlc_media_player_stop(self.raw);
            libvlc_media_player_release(self.raw);
        }
    }
}

/// Converts a local path into a `file://` URI that libVLC accepts for slaves.
pub fn path_to_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
