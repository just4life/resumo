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

#[repr(C)]
pub struct libvlc_audio_track_t {
    pub i_channels: c_uint,
    pub i_rate: c_uint,
}

#[repr(C)]
pub struct libvlc_video_track_t {
    pub i_height: c_uint,
    pub i_width: c_uint,
    pub i_sar_num: c_uint,
    pub i_sar_den: c_uint,
    pub i_frame_rate_num: c_uint,
    pub i_frame_rate_den: c_uint,
    // Orientation, projection and viewpoint follow; never read.
}

#[repr(C)]
pub struct libvlc_media_track_t {
    pub i_codec: u32,
    pub i_original_fourcc: u32,
    pub i_id: c_int,
    pub i_type: c_int,
    pub i_profile: c_int,
    pub i_level: c_int,
    /// `libvlc_audio_track_t` or `libvlc_video_track_t`, depending on `i_type`.
    pub details: *mut c_void,
    pub i_bitrate: c_uint,
    pub psz_language: *mut c_char,
    pub psz_description: *mut c_char,
}

#[repr(C)]
#[derive(Default)]
pub struct libvlc_media_stats_t {
    pub i_read_bytes: c_int,
    pub f_input_bitrate: f32,
    pub i_demux_read_bytes: c_int,
    pub f_demux_bitrate: f32,
    pub i_demux_corrupted: c_int,
    pub i_demux_discontinuity: c_int,
    pub i_decoded_video: c_int,
    pub i_decoded_audio: c_int,
    pub i_displayed_pictures: c_int,
    pub i_lost_pictures: c_int,
    pub i_played_abuffers: c_int,
    pub i_lost_abuffers: c_int,
    pub i_sent_packets: c_int,
    pub i_sent_bytes: c_int,
    pub f_send_bitrate: f32,
}

const TRACK_AUDIO: c_int = 0;
const TRACK_VIDEO: c_int = 1;

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
    fn libvlc_media_tracks_get(md: *mut libvlc_media_t, tracks: *mut *mut *mut libvlc_media_track_t) -> c_uint;
    fn libvlc_media_tracks_release(tracks: *mut *mut libvlc_media_track_t, count: c_uint);
    fn libvlc_media_get_codec_description(type_: c_int, codec: u32) -> *const c_char;
    fn libvlc_media_get_stats(md: *mut libvlc_media_t, stats: *mut libvlc_media_stats_t) -> c_int;

    fn libvlc_media_player_new(inst: *mut libvlc_instance_t) -> *mut libvlc_media_player_t;
    fn libvlc_media_player_release(mp: *mut libvlc_media_player_t);
    fn libvlc_media_player_set_media(mp: *mut libvlc_media_player_t, md: *mut libvlc_media_t);
    fn libvlc_media_player_get_media(mp: *mut libvlc_media_player_t) -> *mut libvlc_media_t;
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
    fn libvlc_video_get_track(mp: *mut libvlc_media_player_t) -> c_int;
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

// A few libVLC core functions for what the public API can't do: subtitle text
// size and position. They work on object variables, as VLC's own hotkeys do.
// A media player is a core object (`vlc_object_t`), and its video outputs are
// its children.
#[repr(C)]
#[derive(Clone, Copy)]
union vlc_value_t {
    i_int: i64,
    p_address: *mut c_void,
}
#[repr(C)]
struct vlc_list_t {
    i_type: c_int,
    i_count: c_int,
    p_values: *mut vlc_value_t,
}
const VLC_VAR_INTEGER: c_int = 0x0030;
const VLC_VAR_DOINHERIT: c_int = 0x8000;

#[cfg_attr(windows, link(name = "libvlccore"))]
#[cfg_attr(not(windows), link(name = "vlccore"))]
extern "C" {
    fn var_Create(obj: *mut c_void, name: *const c_char, type_: c_int) -> c_int;
    fn var_Type(obj: *mut c_void, name: *const c_char) -> c_int;
    fn var_SetChecked(obj: *mut c_void, name: *const c_char, type_: c_int, val: vlc_value_t) -> c_int;
    fn vlc_list_children(obj: *mut c_void) -> *mut vlc_list_t;
    fn vlc_list_release(list: *mut vlc_list_t);
}

/// Subtitle text size in percent, as VLC's `--sub-text-scale` allows.
pub const SUB_SCALE_RANGE: std::ops::RangeInclusive<i32> = 10..=500;

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

/// What the info overlay shows about the video or audio stream being played.
#[derive(Clone, Debug, Default)]
pub struct StreamInfo {
    /// FourCC, e.g. `h264`.
    pub fourcc: String,
    /// libVLC's long name for the codec, e.g. "H264 - MPEG-4 AVC (part 10)".
    pub description: String,
    pub profile: i32,
    /// Bits per second, when the container says.
    pub bitrate: u32,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub channels: u32,
    pub rate: u32,
}

#[derive(Clone, Debug, Default)]
pub struct MediaInfo {
    pub video: Option<StreamInfo>,
    pub audio: Option<StreamInfo>,
    /// Bits per second currently read from the file or stream.
    pub bitrate: f32,
}

fn fourcc(code: u32) -> String {
    code.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { ' ' }).collect::<String>().trim().to_owned()
}

unsafe fn stream_info(t: &libvlc_media_track_t) -> StreamInfo {
    let desc = libvlc_media_get_codec_description(t.i_type, t.i_codec);
    let mut info = StreamInfo {
        fourcc: fourcc(t.i_codec),
        description: if desc.is_null() { String::new() } else { CStr::from_ptr(desc).to_string_lossy().into_owned() },
        profile: t.i_profile,
        bitrate: t.i_bitrate,
        ..Default::default()
    };
    if !t.details.is_null() {
        if t.i_type == TRACK_VIDEO {
            let v = &*(t.details as *const libvlc_video_track_t);
            info.width = v.i_width;
            info.height = v.i_height;
            if v.i_frame_rate_den > 0 {
                info.fps = v.i_frame_rate_num as f32 / v.i_frame_rate_den as f32;
            }
        } else if t.i_type == TRACK_AUDIO {
            let a = &*(t.details as *const libvlc_audio_track_t);
            info.channels = a.i_channels;
            info.rate = a.i_rate;
        }
    }
    info
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

    /// Codecs and sizes of the playing video and audio tracks, and the bitrate.
    pub fn media_info(&self) -> MediaInfo {
        let mut out = MediaInfo::default();
        unsafe {
            let md = libvlc_media_player_get_media(self.raw);
            if md.is_null() {
                return out;
            }
            let (video_id, audio_id) = (libvlc_video_get_track(self.raw), libvlc_audio_get_track(self.raw));
            let mut tracks = std::ptr::null_mut();
            let n = libvlc_media_tracks_get(md, &mut tracks);
            if !tracks.is_null() {
                let list: Vec<&libvlc_media_track_t> =
                    std::slice::from_raw_parts(tracks, n as usize).iter().filter_map(|t| t.as_ref()).collect();
                // The selected track, or the first of its kind if libVLC hasn't said yet.
                let pick = |kind: c_int, id: c_int| {
                    let of_kind = || list.iter().filter(|t| t.i_type == kind);
                    of_kind().find(|t| t.i_id == id).or_else(|| (id != -1).then(|| of_kind().next()).flatten())
                };
                out.video = pick(TRACK_VIDEO, video_id).map(|t| stream_info(t));
                out.audio = pick(TRACK_AUDIO, audio_id).map(|t| stream_info(t));
                libvlc_media_tracks_release(tracks, n);
            }
            let mut stats = libvlc_media_stats_t::default();
            if libvlc_media_get_stats(md, &mut stats) != 0 {
                // libVLC reports bytes per microsecond.
                let rate = if stats.f_demux_bitrate > 0.0 { stats.f_demux_bitrate } else { stats.f_input_bitrate };
                out.bitrate = rate * 8_000_000.0;
            }
            libvlc_media_release(md);
        }
        out
    }

    /// Loads an external subtitle file into the current media and selects it.
    pub fn add_subtitle_file(&self, uri: &str) -> bool {
        let Ok(c) = CString::new(uri) else { return false };
        unsafe { libvlc_media_player_add_slave(self.raw, SLAVE_TYPE_SUBTITLE, c.as_ptr(), true) == 0 }
    }

    /// Sets an integer variable on the player, so video outputs created
    /// later inherit it, and on every live video output that has it.
    fn set_int_var(&self, name: &str, value: i64, live_outputs: bool) {
        let Ok(name) = CString::new(name) else { return };
        let obj = self.raw as *mut c_void;
        let val = vlc_value_t { i_int: value };
        unsafe {
            if var_Type(obj, name.as_ptr()) == 0 {
                var_Create(obj, name.as_ptr(), VLC_VAR_INTEGER | VLC_VAR_DOINHERIT);
            }
            var_SetChecked(obj, name.as_ptr(), VLC_VAR_INTEGER, val);
            if !live_outputs {
                return;
            }
            let list = vlc_list_children(obj);
            if list.is_null() {
                return;
            }
            for i in 0..(*list).i_count.max(0) as usize {
                let child = (*(*list).p_values.add(i)).p_address;
                if !child.is_null() && var_Type(child, name.as_ptr()) & 0xf0 == VLC_VAR_INTEGER {
                    var_SetChecked(child, name.as_ptr(), VLC_VAR_INTEGER, val);
                }
            }
            vlc_list_release(list);
        }
    }

    /// Size of text subtitles in percent (100 = VLC's default).
    /// The text renderer reads it for every subtitle it draws.
    pub fn set_subtitle_scale(&self, percent: i32) {
        let p = percent.clamp(*SUB_SCALE_RANGE.start(), *SUB_SCALE_RANGE.end());
        self.set_int_var("sub-text-scale", p as i64, false);
    }

    /// Lifts subtitles this many video pixels above their usual place
    /// (negative: lower; VLC keeps them inside the picture).
    pub fn set_subtitle_margin(&self, pixels: i32) {
        self.set_int_var("sub-margin", pixels as i64, true);
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
