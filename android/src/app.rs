//! The player window: video surface, controls, playlist and menus.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Margin, Rect, RichText, Sense, Stroke,
    TextureHandle, TextureOptions, Ui, UiBuilder,
};

#[cfg(target_os = "android")]
use crate::browser::{Browser, Outcome, Purpose};
use crate::crop::{self, Crop};
#[cfg(target_os = "android")]
use crate::history::History;
#[cfg(target_os = "android")]
use crate::lan;
#[cfg(target_os = "android")]
use crate::platform;
use crate::playlist::{self, Playlist, Repeat};
use crate::store::Store;
use crate::taskbar::{self, ThumbBar};
use crate::theme::{self, Icon};
use crate::video::VideoSink;
use crate::vlc::{self, Instance, MediaPlayer};
#[cfg(target_os = "android")]
use crate::surface::{PROBE_H, PROBE_W};
#[cfg(target_os = "android")]
use crate::zoom::{BarFinder, Output, ViewSettings, Zoom};

const SPEEDS: &[f32] = &[0.25, 0.5, 0.75, 1.0, 1.15, 1.25, 1.5, 1.75, 2.0, 3.0];
const HIDE_CONTROLS_AFTER: Duration = Duration::from_millis(2500);
const SAVE_EVERY: Duration = Duration::from_secs(5);
const VOLUME_STEP: i32 = 5;
/// Height of a row in the list of recently played files (Android).
#[cfg(target_os = "android")]
const RECENT_ROW: f32 = 56.0;

/// Flags set from libVLC's event thread and consumed on the UI thread.
#[derive(Default)]
struct Events {
    end_reached: AtomicBool,
    error: AtomicBool,
    playing: AtomicBool,
    repaint: Mutex<Option<egui::Context>>,
}

unsafe extern "C" fn on_vlc_event(event: *const vlc::libvlc_event_t, data: *mut c_void) {
    let events = &*(data as *const Events);
    match (*event).type_ {
        vlc::EVENT_END_REACHED => events.end_reached.store(true, Ordering::SeqCst),
        vlc::EVENT_ERROR => events.error.store(true, Ordering::SeqCst),
        vlc::EVENT_PLAYING => events.playing.store(true, Ordering::SeqCst),
        _ => {}
    }
    if let Some(ctx) = events.repaint.lock().unwrap().as_ref() {
        ctx.request_repaint();
    }
}

pub struct PlayerApp {
    // Field order matters: the player must be dropped before the sink and the
    // event flags it holds raw pointers to.
    player: MediaPlayer,
    _instance: Instance,
    sink: Arc<VideoSink>,
    events: Arc<Events>,

    texture: Option<TextureHandle>,
    frame_serial: u64,
    frame_size: (f32, f32),

    playlist: Playlist,
    show_playlist: bool,
    selected: Option<usize>,

    volume: i32,
    muted: bool,
    rate: f32,
    crop: Crop,
    subtitle_delay_ms: i64,
    audio_delay_ms: i64,

    seek_drag: Option<f32>,
    fullscreen: bool,
    last_activity: Instant,
    url_dialog: Option<String>,
    /// Subtitle files to attach once the current media is actually playing.
    pending_subtitles: Vec<PathBuf>,
    status: Option<(String, Instant)>,

    store: Store,
    /// Saved position to jump to once the new media starts playing.
    resume_at: Option<i64>,
    last_save: Instant,
    /// Unused fraction of mouse-wheel notches (touchpads scroll in pixels).
    wheel_acc: f32,
    icon: Option<TextureHandle>,
    thumb_bar: ThumbBar,
    #[cfg(target_os = "android")]
    android: AndroidState,
}

/// Touch-screen and file-browser state that only the Android build has.
#[cfg(target_os = "android")]
struct AndroidState {
    browser: Option<Browser>,
    /// Folder the browser was last in, to reopen it there.
    last_dir: Option<PathBuf>,
    storage_ok: bool,
    storage_checked: Instant,
    guard: platform::BackgroundGuard,
    /// Whether the control bar was showing in the previous frame, so a tap on
    /// the video can toggle it.
    controls_visible: bool,
    hide_controls: bool,
    keep_screen_on: bool,
    /// libVLC's login (or question) for a network share, while on screen.
    lan_dialog: Option<lan::Dialog>,
    /// Whether the Wi-Fi multicast lock is held (for network discovery).
    multicast: bool,
    swipe: Option<Swipe>,
    /// Whether the control bar was showing when the finger went down (any
    /// touch shows it, so later frames can't tell).
    controls_at_press: bool,
    /// Android took the touch over this frame (see `raw_input_hook`).
    touch_cancelled: bool,
    /// The last tap paused or resumed (undone if it was half of a double tap).
    tap_paused: bool,
    /// Showing as a small picture-in-picture window over other apps.
    pip: bool,
    pip_checked: Instant,
    /// Picture-in-picture settings last given to Android: (shape, auto-enter, playing).
    pip_sent: Option<(Option<(i32, i32)>, bool, bool)>,
    /// The last few files played, offered on the empty screen and in the open menu.
    history: History,
    /// How the picture fills the screen, and whether black bars in the video are cut.
    view: ViewSettings,
    bars: BarFinder,
    /// The hardware decoder's own output, when it could be set up.
    gpu: Option<crate::surface::SurfaceVideo>,
    /// The picture and subtitles it showed last frame.
    gpu_picture: Option<(egui::TextureId, (f32, f32))>,
    gpu_subtitles: Option<egui::TextureId>,
    /// The picture is in the video layer (its size in `frame_size`).
    layer_picture: bool,
    /// Whether the current video is HDR (worked out once it plays).
    hdr: Option<bool>,
    gpu_serial: u64,
    /// When to close the app after the video output was switched.
    exit_at: Option<Instant>,
}

/// A finger sliding over the video: up and down on the left half sets the
/// brightness, on the right half the volume; sideways seeks.
#[cfg(target_os = "android")]
struct Swipe {
    kind: SwipeKind,
    /// Where the finger was when the kind was decided.
    origin: egui::Pos2,
    /// Value at that moment: brightness 0–1, volume step, or time in ms.
    start: f32,
    /// The value now, shown on screen.
    value: f32,
    /// Highest volume step, or the media length in ms.
    max: f32,
}

#[cfg(target_os = "android")]
#[derive(Clone, Copy, PartialEq)]
enum SwipeKind {
    Undecided,
    Brightness,
    Volume,
    Seek,
}

/// How far a finger moves before a touch counts as a swipe.
#[cfg(target_os = "android")]
const SWIPE_SLOP: f32 = 16.0;
/// A swipe across the whole width seeks this far (or the whole video if shorter).
#[cfg(target_os = "android")]
const SWIPE_SEEK_RANGE_MS: f32 = 120_000.0;

impl PlayerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Result<Self, String> {
        theme::apply(&cc.egui_ctx);
        let store = Store::load();
        let icon = eframe::icon_data::from_png_bytes(crate::ICON_PNG).ok().map(|i| {
            let img = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
            let opts = TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
            cc.egui_ctx.load_texture("app_icon", img, opts)
        });

        #[cfg(target_os = "android")]
        let view = ViewSettings::load();
        #[allow(unused_mut)]
        let mut options = vec!["--no-video-title-show", "--quiet"];
        // The phone's hardware decoder: libVLC only uses it when asked (as
        // VLC for Android does); without it the CPU decodes, which heats the
        // phone and drains the battery with 4K.
        #[cfg(target_os = "android")]
        options.push("--codec=mediacodec_ndk,mediacodec_jni,all");
        // Straight to the surface (see `surface`), OpenGL only when that
        // can't open.
        #[cfg(target_os = "android")]
        if view.output != Output::Cpu {
            options.push("--vout=android_display,any");
        }
        let instance = Instance::new(&options)?;
        let player = MediaPlayer::new(&instance)?;
        #[cfg(target_os = "android")]
        let pauser = player.pauser();
        #[cfg(target_os = "android")]
        lan::install_dialogs(&instance);
        #[cfg(target_os = "android")]
        {
            // Fingers need bigger targets than a mouse pointer.
            cc.egui_ctx.set_zoom_factor(1.2);
        }
        // On a phone the system volume does the job, so start libVLC at 100 %.
        let default_volume = if cfg!(target_os = "android") { 100 } else { 80 };

        let ctx = cc.egui_ctx.clone();
        let sink = Arc::new(VideoSink::new(move || ctx.request_repaint()));
        let events = Arc::new(Events::default());
        *events.repaint.lock().unwrap() = Some(cc.egui_ctx.clone());
        // The decoder draws into a surface of its own; copying frames through
        // the CPU is the fallback.
        #[cfg(target_os = "android")]
        let gpu = cc
            .gl
            .as_ref()
            .filter(|_| view.output != Output::Cpu)
            .and_then(|gl| crate::surface::SurfaceVideo::new(gl, (1080, 1920), view.output == Output::Layer));
        #[cfg(target_os = "android")]
        let gpu_ok = gpu.is_some();
        #[cfg(not(target_os = "android"))]
        let gpu_ok = false;
        unsafe {
            #[cfg(target_os = "android")]
            if let Some(g) = &gpu {
                player.set_android_window(g.awindow());
            }
            if !gpu_ok {
                player.set_video_callbacks(&sink.callbacks());
            }
            player.attach_events(
                &[vlc::EVENT_END_REACHED, vlc::EVENT_ERROR, vlc::EVENT_PLAYING],
                on_vlc_event,
                Arc::as_ptr(&events) as *mut c_void,
            );
        }

        let mut app = Self {
            player,
            _instance: instance,
            sink,
            events,
            texture: None,
            frame_serial: 0,
            frame_size: (0.0, 0.0),
            playlist: Playlist::default(),
            show_playlist: true,
            selected: None,
            volume: store.volume.unwrap_or(default_volume).clamp(0, 150),
            muted: false,
            rate: 1.0,
            crop: Crop::None,
            subtitle_delay_ms: 0,
            audio_delay_ms: 0,
            seek_drag: None,
            fullscreen: false,
            last_activity: Instant::now(),
            url_dialog: None,
            pending_subtitles: Vec::new(),
            status: None,
            store,
            resume_at: None,
            last_save: Instant::now(),
            wheel_acc: 0.0,
            icon,
            thumb_bar: ThumbBar::default(),
            #[cfg(target_os = "android")]
            android: AndroidState {
                browser: None,
                last_dir: None,
                storage_ok: platform::has_storage_access(),
                storage_checked: Instant::now(),
                guard: platform::BackgroundGuard::new(pauser),
                controls_visible: true,
                hide_controls: false,
                keep_screen_on: false,
                lan_dialog: None,
                multicast: false,
                swipe: None,
                controls_at_press: true,
                touch_cancelled: false,
                tap_paused: false,
                pip: false,
                pip_checked: Instant::now(),
                pip_sent: None,
                history: History::load(),
                view,
                bars: BarFinder::default(),
                gpu,
                gpu_picture: None,
                gpu_subtitles: None,
                layer_picture: false,
                hdr: None,
                gpu_serial: 0,
                exit_at: None,
            },
        };
        app.add_paths(files, true);
        #[cfg(target_os = "android")]
        if let Some(loc) = platform::intent_location() {
            if loc.contains("://") {
                app.playlist.add_location(&loc);
                app.play_index(0);
            } else {
                app.add_paths(vec![PathBuf::from(loc)], true);
            }
        }
        Ok(app)
    }

    // ---------------------------------------------------------------- playback

    fn play_index(&mut self, index: usize) {
        let Some(item) = self.playlist.items.get(index) else { return };
        let location = item.location.clone();
        #[cfg(target_os = "android")]
        let title = item.title.clone();
        self.remember_position();
        self.player.stop();
        self.sink.clear();
        #[cfg(target_os = "android")]
        let options = lan::media_options(&location);
        #[cfg(not(target_os = "android"))]
        let options = Vec::new();
        if let Err(e) = self.player.set_media(&location, &options) {
            self.notify(format!("Не удалось открыть: {e}"));
            return;
        }
        self.playlist.current = Some(index);
        self.selected = Some(index);
        #[cfg(target_os = "android")]
        self.android.bars.reset();
        self.subtitle_delay_ms = 0;
        self.audio_delay_ms = 0;
        self.seek_drag = None;
        self.pending_subtitles.clear();
        self.resume_at = self.store.position(&location);
        if !self.player.play() {
            self.notify("Не удалось начать воспроизведение");
        }
        // Files shared by another app are only readable while that app allows it.
        #[cfg(target_os = "android")]
        if !location.starts_with("content:") {
            self.android.history.add(&location, &title);
        }
    }

    /// Plays a file from the history, reusing its playlist entry if it has one.
    #[cfg(target_os = "android")]
    fn play_recent(&mut self, location: &str, title: &str) {
        let index = match self.playlist.items.iter().position(|it| it.location == location) {
            Some(i) => i,
            None => {
                self.playlist.add_location(location);
                if let Some(added) = self.playlist.items.last_mut() {
                    added.title = title.to_owned();
                }
                self.playlist.items.len() - 1
            }
        };
        self.play_index(index);
        self.show_playlist = false;
    }

    fn is_active(&self) -> bool {
        self.playlist.current.is_some()
    }

    fn toggle_pause(&mut self) {
        if !self.is_active() {
            if let Some(i) = self.selected.or(self.playlist.next_index(false)) {
                self.play_index(i);
            }
            return;
        }
        let playing = self.player.is_playing();
        self.player.set_pause(playing);
    }

    fn stop(&mut self) {
        self.remember_position();
        self.store.save();
        self.player.stop();
        self.sink.clear();
        self.playlist.current = None;
    }

    fn next(&mut self, auto: bool) {
        match self.playlist.next_index(auto) {
            Some(i) => self.play_index(i),
            None => self.stop(),
        }
    }

    fn prev(&mut self) {
        // Like most players: restart the track unless we are near its start.
        if self.is_active() && self.player.time() > 3000 {
            self.player.set_time(0);
        } else if let Some(i) = self.playlist.prev_index() {
            self.play_index(i);
        }
    }

    fn seek_by(&mut self, delta_ms: i64) {
        if self.is_active() && self.player.is_seekable() {
            let t = self.player.time() + delta_ms;
            let t = self.player.length().map_or(t, |l| t.min(l - 500));
            self.player.set_time(t);
        }
    }

    /// Saves where the current video is, so it can continue from there later.
    fn remember_position(&mut self) {
        let Some(item) = self.playlist.current.and_then(|i| self.playlist.items.get(i)) else { return };
        // Live streams have no length and cannot be resumed.
        if let (Some(len), true) = (self.player.length(), self.player.is_seekable()) {
            self.store.set_position(&item.location, self.player.time(), Some(len));
        }
    }

    fn set_audio_device(&mut self, device: &vlc::AudioDevice) {
        self.player.set_audio_device(&device.id);
        self.store.set_audio_device(Some(device.id.clone()));
        self.store.save();
        self.notify(format!("Звук: {}", device.name));
    }

    fn set_volume(&mut self, v: i32) {
        self.volume = v.clamp(0, 150);
        self.player.set_volume(self.volume);
        self.store.set_volume(self.volume);
        if self.muted {
            self.muted = false;
            self.player.set_mute(false);
        }
    }

    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.player.set_mute(self.muted);
    }

    fn set_crop(&mut self, crop: Crop) {
        self.crop = crop;
        self.notify(format!("Кадрирование: {}", crop.label()));
    }

    fn cycle_crop(&mut self) {
        let modes: Vec<Crop> = std::iter::once(Crop::None).chain(crop::PRESETS.iter().map(|(_, c)| *c)).collect();
        let i = modes.iter().position(|c| *c == self.crop).map_or(0, |i| (i + 1) % modes.len());
        self.set_crop(modes[i]);
    }

    fn cycle_audio(&mut self) {
        let tracks: Vec<_> = self.player.audio_tracks().into_iter().filter(|t| t.id >= 0).collect();
        if tracks.is_empty() {
            return;
        }
        let cur = self.player.audio_track();
        let i = tracks.iter().position(|t| t.id == cur).map_or(0, |i| (i + 1) % tracks.len());
        self.player.set_audio_track(tracks[i].id);
        self.notify(format!("Аудио: {}", tracks[i].name));
    }

    fn cycle_subtitles(&mut self) {
        let tracks = self.player.subtitle_tracks();
        if tracks.is_empty() {
            self.notify("Субтитров нет");
            return;
        }
        let cur = self.player.subtitle_track();
        let i = tracks.iter().position(|t| t.id == cur).map_or(0, |i| (i + 1) % tracks.len());
        self.player.set_subtitle_track(tracks[i].id);
        self.notify(format!("Субтитры: {}", track_name(&tracks[i])));
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    // ------------------------------------------------------------ open & add

    fn add_paths(&mut self, paths: Vec<PathBuf>, play: bool) {
        // Subtitle files go to the current video, everything else to the playlist.
        let cwd = std::env::current_dir().unwrap_or_default();
        let (subs, media): (Vec<_>, Vec<_>) =
            paths.into_iter().map(|p| cwd.join(p)).partition(|p| playlist::has_extension(p, playlist::SUBTITLE_EXTENSIONS));
        let first_new = self.playlist.items.len();
        let added: usize = media.iter().map(|p| self.playlist.add_path(p)).sum();
        if added > 0 && (play || !self.is_active()) {
            self.play_index(first_new);
        }
        for s in subs {
            if self.is_active() && self.player.is_playing() {
                self.load_subtitle(&s);
            } else if self.is_active() {
                self.pending_subtitles.push(s);
            }
        }
    }

    fn load_subtitle(&mut self, path: &std::path::Path) {
        if self.player.add_subtitle_file(&vlc::path_to_uri(path)) {
            self.notify(format!("Субтитры загружены: {}", path.file_name().unwrap_or_default().to_string_lossy()));
        } else {
            self.notify("Не удалось загрузить субтитры");
        }
    }

    #[cfg(target_os = "android")]
    fn open_browser(&mut self, purpose: Purpose) {
        let dir = self.android.last_dir.clone().filter(|d| d.is_dir()).unwrap_or_else(platform::default_dir);
        self.android.browser = Some(Browser::new(purpose, dir, platform::storage_roots(), &self._instance));
    }

    #[cfg(target_os = "android")]
    fn open_files_dialog(&mut self, play: bool) {
        self.open_browser(Purpose::Open { play });
    }

    #[cfg(target_os = "android")]
    fn open_folder_dialog(&mut self) {
        self.open_browser(Purpose::Folder);
    }

    #[cfg(target_os = "android")]
    fn open_subtitle_dialog(&mut self) {
        self.open_browser(Purpose::Subtitle);
    }

    #[cfg(target_os = "android")]
    fn save_playlist_dialog(&mut self) {
        self.open_browser(Purpose::SaveM3u);
    }

    #[cfg(target_os = "android")]
    fn save_playlist_to(&mut self, f: &std::path::Path) {
        match self.playlist.save_m3u(f) {
            Ok(()) => self.notify("Плейлист сохранён"),
            Err(e) => self.notify(format!("Ошибка сохранения: {e}")),
        }
    }

    /// Adds files picked on the network under their own names.
    #[cfg(target_os = "android")]
    fn add_network(&mut self, items: Vec<lan::NetItem>, play: bool) {
        let first_new = self.playlist.items.len();
        for item in items {
            self.playlist.add_location(&item.mrl);
            let path = std::path::Path::new(&item.name);
            let title = match path.file_stem() {
                Some(stem) if playlist::has_extension(path, playlist::MEDIA_EXTENSIONS) => stem.to_string_lossy().into_owned(),
                _ => item.name,
            };
            if let Some(added) = self.playlist.items.last_mut() {
                added.title = title;
            }
        }
        if self.playlist.items.len() > first_new && (play || !self.is_active()) {
            self.play_index(first_new);
        }
    }

    /// The file browser, drawn over the whole screen while it is open.
    #[cfg(target_os = "android")]
    fn browser_window(&mut self, ctx: &egui::Context) {
        // Wi-Fi drivers drop multicast to save power; discovery needs it.
        let on_network = self.android.browser.as_ref().is_some_and(|b| b.on_network());
        if on_network != self.android.multicast {
            self.android.multicast = on_network;
            platform::set_multicast(on_network);
        }
        if self.android.browser.is_none() {
            return;
        }
        if self.android.storage_checked.elapsed() > Duration::from_millis(800) {
            self.android.storage_checked = Instant::now();
            let ok = platform::has_storage_access();
            if ok && !self.android.storage_ok {
                if let Some(b) = &mut self.android.browser {
                    b.refresh();
                }
            }
            self.android.storage_ok = ok;
        }
        if !self.android.storage_ok {
            ctx.request_repaint_after(Duration::from_millis(800));
        }
        let mut ask = false;
        let mut outcome = None;
        let has_access = self.android.storage_ok;
        let rect = ctx.content_rect();
        egui::Area::new(egui::Id::unique("file_browser")).order(egui::Order::Foreground).fixed_pos(rect.min).show(
            ctx,
            |ui| {
                // Also cover the strip by the camera cutout, which the browser keeps out of.
                let mut p = ui.painter().clone();
                p.set_clip_rect(ctx.viewport_rect());
                p.rect_filled(ctx.viewport_rect(), 0.0, theme::BG);
                ui.set_min_size(rect.size());
                if let Some(b) = &mut self.android.browser {
                    outcome = b.show(ui, rect, has_access, &mut ask);
                }
            },
        );
        if ask {
            platform::request_storage_access();
        }
        let Some(choice) = outcome else { return };
        let playing_before = self.playlist.current;
        if let Some(b) = self.android.browser.take() {
            self.android.last_dir = Some(b.dir().to_path_buf());
        }
        match choice {
            Some(Outcome::Files(files, play)) => self.add_paths(files, play),
            Some(Outcome::Folder(dir)) => self.add_paths(vec![dir], false),
            Some(Outcome::Subtitle(f)) => self.load_subtitle(&f),
            Some(Outcome::Save(f)) => self.save_playlist_to(&f),
            Some(Outcome::Network(items, play)) => self.add_network(items, play),
            None => {}
        }
        // Something picked started playing: give it the whole screen.
        if self.playlist.current != playing_before {
            self.show_playlist = false;
        }
    }

    /// The video's shape for the picture-in-picture window, within the
    /// range Android accepts.
    #[cfg(target_os = "android")]
    fn pip_aspect(&self) -> Option<(i32, i32)> {
        let (w, h) = self.frame_size;
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let uv = self.picture_uv();
        let aspect = ((uv.width() * w) / (uv.height() * h)).clamp(0.42, 2.38);
        Some(((aspect * 1000.0).round() as i32, 1000))
    }

    /// In the picture-in-picture window there is room for the picture only.
    #[cfg(target_os = "android")]
    fn pip_view(&mut self, ui: &mut Ui) {
        // Inside the window's shadow margins, which are left clear (with the
        // video layer, whose window can be see-through).
        let full = ui.max_rect();
        let (x, y) = crate::surface::window_inset();
        let rect = full.shrink2(vec2(x, y) / ui.ctx().pixels_per_point());
        if self.android.layer_picture {
            ui.painter().add(crate::surface::hole(full));
        }
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        if self.shows_picture() {
            let (w, h) = self.frame_size;
            let uv = self.picture_uv();
            let target = crop::fit(rect, (uv.width() * w) / (uv.height() * h));
            match self.picture_texture().filter(|_| !self.android.layer_picture) {
                Some(id) => {
                    ui.painter().image(id, target, uv, Color32::WHITE);
                }
                None => self.show_layer(&ui.painter().clone(), rect, target, uv, ui.ctx().pixels_per_point()),
            }
        }
    }

    /// The picture's texture: from the decoder's surface, or the frames
    /// copied through the CPU. (In the video layer there is none.)
    #[cfg(target_os = "android")]
    fn picture_texture(&self) -> Option<egui::TextureId> {
        self.android.gpu_picture.map(|(id, _)| id).or_else(|| self.texture.as_ref().map(|t| t.id()))
    }

    /// A picture of the video is on screen: in a texture, or in the video
    /// layer.
    #[cfg(target_os = "android")]
    fn shows_picture(&self) -> bool {
        self.android.layer_picture || self.picture_texture().is_some()
    }

    /// Shows the video layer under the window: the whole picture placed so
    /// that its `uv` part fills `target`, with the window see-through where
    /// `target` is inside `area` (the painter's black there stays around it).
    #[cfg(target_os = "android")]
    fn show_layer(&mut self, painter: &egui::Painter, area: Rect, target: Rect, uv: Rect, ppp: f32) {
        let shown = target.intersect(area);
        if shown.is_positive() {
            let mut p = painter.clone();
            p.set_clip_rect(shown.intersect(painter.clip_rect()));
            p.add(crate::surface::hole(shown));
        }
        let size = target.size() / uv.size();
        let min = target.min - uv.min.to_vec2() * size;
        let px = |v: f32| (v * ppp).round() as i32;
        if let Some(g) = &mut self.android.gpu {
            g.place_layer([px(min.x), px(min.y), px(size.x), px(size.y)]);
        }
    }

    /// Whether the current video looks like HDR10: 10-bit HEVC, VP9 or AV1
    /// at 4K-ish sizes (SDR 10-bit encodes are mostly smaller).
    #[cfg(target_os = "android")]
    fn looks_hdr(&self) -> bool {
        let Some(v) = self.player.video_info() else { return false };
        let codec = matches!(&v.codec, b"hevc" | b"VP90" | b"av01");
        let height = v.height.max(self.frame_size.1 as u32);
        codec && if v.profile == 2 { height >= 1440 } else { height >= 2000 }
    }

    /// Takes the decoder's newest frames into the GPU textures (see
    /// [`crate::surface`]), and looks for black bars in them.
    #[cfg(target_os = "android")]
    fn gpu_frame(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(at) = self.android.exit_at {
            if Instant::now() >= at {
                self.remember_position();
                self.store.save();
                std::process::exit(0);
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let Some(gl) = frame.gl().cloned() else { return };
        if self.android.gpu.is_none() {
            return;
        }
        if !self.is_active() {
            // A late frame of what was stopped must not show.
            self.android.gpu_picture = None;
            self.android.gpu_subtitles = None;
            self.android.layer_picture = false;
            return;
        }
        let serial = self.sink.with_frame(|f| f.serial);
        if serial != self.android.gpu_serial {
            // Stopped, or another file: nothing of it is on screen yet.
            self.android.gpu_serial = serial;
            self.android.hdr = None;
            if let Some(g) = &mut self.android.gpu {
                g.reset();
            }
        }
        let size = self.player.video_size();
        if self.android.hdr.is_none() && size.is_some() {
            self.android.hdr = Some(self.looks_hdr());
        }
        let hdr = self.android.view.hdr_to_sdr.unwrap_or(self.android.hdr == Some(true));
        let r = ctx.viewport_rect();
        let ppp = ctx.pixels_per_point();
        let window = ((r.width() * ppp).round() as i32, (r.height() * ppp).round() as i32);
        let Some(gpu) = &mut self.android.gpu else { return };
        let shown = gpu.frame(&gl, frame, window, size, hdr);
        let layer = gpu.is_layer();
        let layer_size = gpu.picture_size().or(size.map(|(w, h)| (w as f32, h as f32)));
        self.android.layer_picture = layer && size.is_some();
        if shown.reattached {
            self.player.restart_video();
        }
        if let Some(s) = layer_size.filter(|_| self.android.layer_picture) {
            self.frame_size = s;
        }
        self.android.gpu_picture = shown.picture;
        self.android.gpu_subtitles = shown.subtitles;
        if let Some((_, s)) = shown.picture {
            self.frame_size = s;
        }
        if self.android.view.cut_bars && self.shows_picture() && self.android.bars.due() {
            let pixels = self.android.gpu.as_mut().and_then(|g| g.probe(&gl));
            if let Some(px) = pixels {
                self.android.bars.feed(crate::zoom::find_bars(&px, PROBE_W, PROBE_H));
            }
        }
        // No callback says a frame is ready: look for one often. In the
        // video layer the picture needs no drawing, only the subtitles do.
        if self.player.is_playing() {
            if layer {
                ctx.request_repaint_after(Duration::from_millis(40));
            } else {
                ctx.request_repaint();
            }
        }
    }

    /// The part of the frame on screen: without the black bars found in the
    /// video (when that is on), then cropped as chosen.
    #[cfg(target_os = "android")]
    fn picture_uv(&self) -> Rect {
        let (w, h) = self.frame_size;
        let bars = if self.android.view.cut_bars { self.android.bars.content() } else { Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)) };
        let inner = self.crop.uv_rect(w * bars.width(), h * bars.height());
        Rect::from_min_max(bars.min + inner.min.to_vec2() * bars.size(), bars.min + inner.max.to_vec2() * bars.size())
    }

    /// [`Self::picture_uv`], easing into a newly found crop instead of jumping.
    #[cfg(target_os = "android")]
    fn eased_picture_uv(&self, ctx: &egui::Context) -> Rect {
        let uv = self.picture_uv();
        let ease = |name: &str, v: f32| ctx.animate_value_with_time(egui::Id::unique(("picture_uv", name)), v, 0.35);
        Rect::from_min_max(
            pos2(ease("left", uv.min.x), ease("top", uv.min.y)),
            pos2(ease("right", uv.max.x), ease("bottom", uv.max.y)),
        )
    }

    #[cfg(target_os = "android")]
    fn set_zoom(&mut self, zoom: Zoom) {
        self.android.view.zoom = zoom;
        self.android.view.save();
        self.notify(format!("Масштаб: {}", zoom.label()));
    }

    /// The system "back" gesture: closes whatever is on top, then leaves.
    #[cfg(target_os = "android")]
    fn handle_back(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.key_pressed(Key::BrowserBack)) {
            return;
        }
        if let Some(b) = &mut self.android.browser {
            if !b.go_up() {
                self.android.browser = None;
            }
        } else if self.url_dialog.is_some() {
            self.url_dialog = None;
        } else if egui::Popup::is_any_open(ctx) {
            egui::Popup::close_all(ctx);
        } else if self.fullscreen {
            self.set_fullscreen(ctx, false);
        } else if self.player.is_playing() && self.shows_picture() {
            platform::enter_pip(self.pip_aspect(), self.player.is_playing());
        } else {
            platform::move_to_back();
        }
    }

    #[cfg(not(target_os = "android"))]
    fn open_files_dialog(&mut self, play: bool) {
        let files = rfd::FileDialog::new()
            .set_title("Открыть медиафайлы")
            .add_filter("Видео и аудио", playlist::MEDIA_EXTENSIONS)
            .add_filter("Плейлист", playlist::PLAYLIST_EXTENSIONS)
            .add_filter("Все файлы", &["*"])
            .pick_files();
        if let Some(files) = files {
            self.add_paths(files, play);
        }
    }

    #[cfg(not(target_os = "android"))]
    fn open_folder_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Добавить папку").pick_folder() {
            self.add_paths(vec![dir], false);
        }
    }

    #[cfg(not(target_os = "android"))]
    fn open_subtitle_dialog(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("Загрузить субтитры")
            .add_filter("Субтитры", playlist::SUBTITLE_EXTENSIONS)
            .pick_file();
        if let Some(f) = file {
            self.load_subtitle(&f);
        }
    }

    #[cfg(not(target_os = "android"))]
    fn save_playlist_dialog(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("Сохранить плейлист")
            .add_filter("M3U", &["m3u"])
            .set_file_name("playlist.m3u")
            .save_file();
        if let Some(f) = file {
            match self.playlist.save_m3u(&f) {
                Ok(()) => self.notify("Плейлист сохранён"),
                Err(e) => self.notify(format!("Ошибка сохранения: {e}")),
            }
        }
    }

    // ------------------------------------------------------------ per frame

    fn poll(&mut self, ctx: &egui::Context) {
        if self.events.playing.swap(false, Ordering::SeqCst) {
            // The audio output exists only once playback started.
            self.player.set_volume(self.volume);
            self.player.set_mute(self.muted);
            self.player.set_rate(self.rate);
            if let Some(dev) = self.store.audio_device.clone() {
                self.player.set_audio_device(&dev);
            }
            for s in std::mem::take(&mut self.pending_subtitles) {
                self.load_subtitle(&s);
            }
            if let Some(ms) = self.resume_at.take() {
                if self.player.is_seekable() && self.player.length().is_none_or(|l| ms < l) {
                    self.player.set_time(ms);
                    self.notify(format!("Продолжаю с {}", format_time(ms)));
                }
            }
            if let (Some(i), Some(len)) = (self.playlist.current, self.player.length()) {
                if let Some(item) = self.playlist.items.get_mut(i) {
                    item.duration = Some(len);
                }
            }
        }
        if self.events.error.swap(false, Ordering::SeqCst) {
            let name = self.current_title().unwrap_or_default();
            self.notify(format!("Ошибка воспроизведения: {name}"));
            self.next(true);
        }
        if self.events.end_reached.swap(false, Ordering::SeqCst) {
            // Watched to the end: next time start from the beginning.
            if let Some(item) = self.playlist.current.and_then(|i| self.playlist.items.get(i)) {
                self.store.forget(&item.location);
            }
            self.next(true);
        }
        if self.last_save.elapsed() >= SAVE_EVERY {
            self.last_save = Instant::now();
            if self.player.is_playing() {
                self.remember_position();
            }
            self.store.save();
        }

        let serial = self.sink.with_frame(|f| f.serial);
        if serial != self.frame_serial {
            self.frame_serial = serial;
            let image = self.sink.with_frame(|f| {
                (f.width > 0)
                    .then(|| egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.pixels))
            });
            match image {
                Some(img) => {
                    self.frame_size = (img.size[0] as f32, img.size[1] as f32);
                    match &mut self.texture {
                        Some(t) if t.size() == img.size => t.set(img, TextureOptions::LINEAR),
                        _ => self.texture = Some(ctx.load_texture("video", img, TextureOptions::LINEAR)),
                    }
                }
                None => self.texture = None,
            }
        }

        if self.is_active() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        #[cfg(target_os = "android")]
        {
            let playing = self.player.is_playing();
            if playing != self.android.keep_screen_on {
                self.android.keep_screen_on = playing;
                platform::set_keep_screen_on(playing);
            }
            self.android.guard.tick(playing && self.shows_picture());
            // (With the decoder's own output, in `gpu_frame`.)
            if self.android.view.cut_bars && self.texture.is_some() {
                self.android.bars.update(&self.sink);
            }
            // Going Home with a video playing turns it into a small window,
            // whose button shows pause or play.
            let video = playing && self.shows_picture();
            let aspect = if self.shows_picture() { self.pip_aspect() } else { None };
            let pip = (aspect, video, playing);
            if self.android.pip_sent != Some(pip) {
                self.android.pip_sent = Some(pip);
                platform::set_auto_pip(pip.0, pip.1, pip.2);
            }
            if self.android.pip_checked.elapsed() > Duration::from_millis(300) {
                self.android.pip_checked = Instant::now();
                platform::refresh_insets();
                // A press is read (and dropped) even out of the small window,
                // so a late one can't pause the video the next time.
                let pressed = platform::pip_button_pressed();
                self.android.pip = platform::in_pip();
                if self.android.pip && pressed {
                    self.toggle_pause();
                }
            }
        }
        let title = match self.current_title() {
            Some(t) => format!("{t} — VPlayer"),
            None => "VPlayer".to_owned(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn current_title(&self) -> Option<String> {
        self.playlist.current.and_then(|i| self.playlist.items.get(i)).map(|it| it.title.clone())
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.add_paths(dropped, false);
        }
        if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || !i.events.is_empty()) {
            self.last_activity = Instant::now();
        }
        #[cfg(target_os = "android")]
        self.handle_back(ctx);
        if ctx.egui_wants_keyboard_input() || self.url_dialog.is_some() {
            return;
        }
        let (shift, ctrl) = ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let step = if shift { 30_000 } else { 5_000 };
        if pressed(Key::Space) {
            self.toggle_pause();
        }
        if pressed(Key::ArrowRight) {
            self.seek_by(step);
        }
        if pressed(Key::ArrowLeft) {
            self.seek_by(-step);
        }
        if pressed(Key::ArrowUp) {
            self.set_volume(self.volume + 5);
        }
        if pressed(Key::ArrowDown) {
            self.set_volume(self.volume - 5);
        }
        if pressed(Key::M) {
            self.toggle_mute();
        }
        if pressed(Key::N) {
            self.next(false);
        }
        if pressed(Key::P) {
            self.prev();
        }
        if pressed(Key::A) {
            self.cycle_audio();
        }
        if pressed(Key::S) {
            self.cycle_subtitles();
        }
        if pressed(Key::C) {
            self.cycle_crop();
        }
        if pressed(Key::L) {
            self.show_playlist = !self.show_playlist;
        }
        if pressed(Key::F) || pressed(Key::F11) {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        if pressed(Key::Escape) && self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
        if ctrl && pressed(Key::O) {
            self.open_files_dialog(true);
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        #[cfg(not(target_os = "android"))]
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        #[cfg(target_os = "android")]
        {
            let _ = ctx;
            let (w, h) = self.frame_size;
            platform::set_fullscreen(on, w > 0.0 && w >= h);
        }
    }

    // ------------------------------------------------------------ widgets

    /// Our own window caption: drag to move, double-click to maximise, and
    /// minimise / maximise / close buttons drawn in the player's style.
    fn title_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let rect = ui.max_rect();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let drag = ui.interact(rect, ui.scope_id().with("caption_drag"), Sense::click_and_drag());
        if drag.double_clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.add_space(10.0);
            let (logo, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            self.paint_app_icon(ui.painter(), logo);
            ui.add_space(2.0);
            ui.label(RichText::new("VPlayer").size(12.5).color(theme::TEXT));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if theme::caption_button(ui, Icon::Close, "Закрыть").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                let (icon, tip) =
                    if maximized { (Icon::Restore, "Свернуть в окно") } else { (Icon::Maximize, "Развернуть") };
                if theme::caption_button(ui, icon, tip).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                if theme::caption_button(ui, Icon::Minimize, "Свернуть").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                ui.add_space(12.0);
                if let Some(t) = self.current_title() {
                    ui.add(
                        egui::Label::new(RichText::new(t).size(12.5).color(theme::TEXT_DIM))
                            .truncate()
                            .selectable(false),
                    );
                }
            });
        });
    }

    /// Lets the undecorated window be resized from its edges.
    fn resize_edges(&self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if self.fullscreen || maximized {
            return;
        }
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let r = ctx.content_rect();
        let m = 5.0;
        let (w, e, n, s) = (p.x - r.left() < m, r.right() - p.x < m, p.y - r.top() < m, r.bottom() - p.y < m);
        use egui::{CursorIcon as C, ResizeDirection as D};
        let (dir, cursor) = match (w, e, n, s) {
            (true, _, true, _) => (D::NorthWest, C::ResizeNorthWest),
            (_, true, true, _) => (D::NorthEast, C::ResizeNorthEast),
            (true, _, _, true) => (D::SouthWest, C::ResizeSouthWest),
            (_, true, _, true) => (D::SouthEast, C::ResizeSouthEast),
            (true, ..) => (D::West, C::ResizeWest),
            (_, true, ..) => (D::East, C::ResizeEast),
            (_, _, true, _) => (D::North, C::ResizeNorth),
            (_, _, _, true) => (D::South, C::ResizeSouth),
            _ => return,
        };
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }

    /// Draws the seek bar and returns the bottom edge of its (unhovered) track.
    fn seek_bar(&mut self, ui: &mut Ui) -> f32 {
        let length = self.player.length();
        let pos = self.seek_drag.unwrap_or_else(|| if self.is_active() { self.player.position() } else { 0.0 });
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click_and_drag());
        let hovered = resp.hovered() || resp.dragged();
        let track_h: f32 = if hovered { 16.0 } else { 12.0 };
        let radius = CornerRadius::same((track_h / 2.0) as u8);
        let track = Rect::from_center_size(rect.center(), vec2(rect.width(), track_h));
        let painter = ui.painter_at(rect.expand(12.0));
        painter.rect_filled(track, radius, Color32::from_white_alpha(56));

        let to_pos = |x: f32| ((x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        if let Some(p) = resp.hover_pos() {
            let at = to_pos(p.x);
            painter.rect_filled(Rect::from_min_max(track.min, pos2(p.x, track.max.y)), radius, Color32::from_white_alpha(40));
            if let Some(len) = length {
                resp.clone().on_hover_text_at_pointer(format_time((len as f32 * at) as i64));
            }
        }
        let filled = Rect::from_min_max(track.min, pos2(track.min.x + track.width() * pos, track.max.y));
        painter.rect_filled(filled, radius, theme::ACCENT);
        if hovered || self.seek_drag.is_some() {
            painter.circle_filled(pos2(filled.max.x, track.center().y), 11.0, Color32::WHITE);
        }

        if self.is_active() && self.player.is_seekable() {
            if resp.dragged() || resp.drag_started() {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.seek_drag = Some(to_pos(p.x));
                }
            }
            if resp.drag_stopped() || resp.clicked() {
                let target = resp.interact_pointer_pos().map(|p| to_pos(p.x)).or(self.seek_drag);
                if let Some(t) = target {
                    self.player.set_position(t);
                }
                self.seek_drag = None;
            }
        }
        rect.center().y + 6.0
    }

    /// Seek bar on top, then the button row centred between it and `bottom`.
    fn controls(&mut self, ui: &mut Ui, ctx: &egui::Context, bottom: f32) {
        let track_bottom = self.seek_bar(ui);
        let full = ui.max_rect();
        let row_at = |mid: f32| Rect::from_min_max(pos2(full.left(), mid - 15.0), pos2(full.right(), mid + 15.0));
        let row_ui = |ui: &mut Ui, row: Rect, add: &mut dyn FnMut(&mut Ui)| {
            ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                add(ui);
            });
        };
        if is_narrow(full.width()) {
            // Phones held upright: two rows of buttons.
            let h = (bottom - track_bottom) / 2.0;
            row_ui(ui, row_at(track_bottom + h / 2.0), &mut |ui| {
                self.transport_buttons(ui);
                ui.add_space(8.0);
                self.time_label(ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    self.view_buttons(ui, ctx);
                });
            });
            row_ui(ui, row_at(track_bottom + h * 1.5), &mut |ui| {
                self.volume_control(ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    self.menu_buttons(ui);
                });
            });
            return;
        }
        row_ui(ui, row_at((track_bottom + bottom) / 2.0), &mut |ui| {
            self.transport_buttons(ui);
            self.volume_control(ui);
            ui.add_space(8.0);
            self.time_label(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                self.view_buttons(ui, ctx);
                ui.add_space(6.0);
                self.menu_buttons(ui);
            });
        });
    }

    fn transport_buttons(&mut self, ui: &mut Ui) {
        let playing = self.player.is_playing();
        if theme::play_button(ui, playing).on_hover_text("Пауза / воспроизведение (Пробел)").clicked() {
            self.toggle_pause();
        }
        if theme::icon_button(ui, Icon::Prev, "Предыдущий (P)", false).clicked() {
            self.prev();
        }
        if theme::icon_button(ui, Icon::Next, "Следующий (N)", false).clicked() {
            self.next(false);
        }
    }

    fn time_label(&self, ui: &mut Ui) {
        let time = if self.is_active() {
            let t = self
                .seek_drag
                .and_then(|p| self.player.length().map(|l| (l as f32 * p) as i64))
                .unwrap_or_else(|| self.player.time());
            let len = self.player.length().map_or("--:--".to_owned(), format_time);
            format!("{} / {}", format_time(t), len)
        } else {
            "--:-- / --:--".to_owned()
        };
        ui.label(RichText::new(time).font(FontId::monospace(11.5)).color(theme::TEXT_DIM));
    }

    /// Fullscreen, playlist and open (laid out right to left).
    fn view_buttons(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let fs_icon = if self.fullscreen { Icon::ExitFullscreen } else { Icon::Fullscreen };
        if theme::icon_button(ui, fs_icon, "Полный экран (F)", false).clicked() {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        if !self.fullscreen && theme::icon_button(ui, Icon::Playlist, "Плейлист (L)", self.show_playlist).clicked() {
            self.show_playlist = !self.show_playlist;
        }
        theme::menu_icon(ui, Icon::Folder, "Открыть", false, |ui| self.open_menu(ui));
    }

    /// Crop, subtitles, audio and speed menus (laid out right to left).
    fn menu_buttons(&mut self, ui: &mut Ui) {
        #[cfg(not(target_os = "android"))]
        let cropped = self.crop != Crop::None;
        #[cfg(target_os = "android")]
        let cropped = self.crop != Crop::None || self.android.view.zoom != Zoom::Fit || self.android.view.cut_bars;
        theme::menu_icon(ui, Icon::Crop, "Кадрирование (C)", cropped, |ui| self.crop_menu(ui));
        theme::menu_icon(ui, Icon::Subtitles, "Субтитры (S)", false, |ui| self.subtitle_menu(ui));
        theme::menu_icon(ui, Icon::Audio, "Аудиодорожка (A)", false, |ui| self.audio_menu(ui));
        let speed = format!("{}×", trim_float(self.rate));
        theme::menu_text(ui, &speed, "Скорость", |ui| self.speed_menu(ui));
    }

    fn open_menu(&mut self, ui: &mut Ui) {
        if ui.button("Открыть файлы…").clicked() {
            ui.close();
            self.open_files_dialog(true);
        }
        if ui.button("Добавить папку…").clicked() {
            ui.close();
            self.open_folder_dialog();
        }
        if ui.button("Открыть ссылку…").clicked() {
            ui.close();
            self.url_dialog = Some(String::new());
        }
        #[cfg(target_os = "android")]
        if !self.android.history.entries.is_empty() {
            ui.separator();
            ui.label(RichText::new("Недавние").size(12.0).color(theme::TEXT_DIM));
            ui.set_max_width(320.0);
            let mut pick = None;
            for e in &self.android.history.entries {
                if ui.add(egui::Button::new(&e.title).truncate()).clicked() {
                    pick = Some((e.location.clone(), e.title.clone()));
                }
            }
            if let Some((location, title)) = pick {
                ui.close();
                self.play_recent(&location, &title);
            }
        }
    }

    fn volume_control(&mut self, ui: &mut Ui) {
        let icon = if self.muted || self.volume == 0 {
            Icon::Mute
        } else if self.volume < 50 {
            Icon::Volume(1)
        } else {
            Icon::Volume(2)
        };
        if theme::icon_button(ui, icon, "Без звука (M)", false).clicked() {
            self.toggle_mute();
        }
        let mut v = self.volume as f32;
        let slider = egui::Slider::new(&mut v, 0.0..=150.0).show_value(false).trailing_fill(true);
        let resp = ui.add_sized([72.0, 16.0], slider).on_hover_text(format!("Громкость {}%", self.volume));
        if resp.changed() {
            self.set_volume(v.round() as i32);
        }
    }

    fn audio_menu(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Аудиодорожка").strong());
        let tracks = self.player.audio_tracks();
        if tracks.is_empty() {
            ui.label(RichText::new("Нет аудиодорожек").color(theme::TEXT_DIM));
        }
        let cur = self.player.audio_track();
        for t in tracks {
            let name = if t.id < 0 { "Отключить".to_owned() } else { t.name.clone() };
            if ui.add(egui::Button::selectable(t.id == cur, name)).clicked() {
                self.player.set_audio_track(t.id);
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Задержка звука");
            if delay_editor(ui, &mut self.audio_delay_ms) {
                self.player.set_audio_delay_ms(self.audio_delay_ms);
            }
        });
    }

    fn subtitle_menu(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Субтитры").strong());
        let tracks = self.player.subtitle_tracks();
        let cur = self.player.subtitle_track();
        if tracks.is_empty() {
            ui.label(RichText::new("В файле нет субтитров").color(theme::TEXT_DIM));
        }
        for t in &tracks {
            if ui.add(egui::Button::selectable(t.id == cur, track_name(t))).clicked() {
                self.player.set_subtitle_track(t.id);
            }
        }
        ui.separator();
        if ui.add_enabled(self.is_active(), egui::Button::new("Загрузить из файла…")).clicked() {
            ui.close();
            self.open_subtitle_dialog();
        }
        ui.horizontal(|ui| {
            ui.label("Задержка");
            if delay_editor(ui, &mut self.subtitle_delay_ms) {
                self.player.set_subtitle_delay_ms(self.subtitle_delay_ms);
            }
        });
    }

    fn crop_menu(&mut self, ui: &mut Ui) {
        #[cfg(target_os = "android")]
        {
            ui.label(RichText::new("Масштаб").strong());
            egui::Grid::new("zoom_modes").num_columns(2).spacing([4.0, 4.0]).show(ui, |ui| {
                for (i, (zoom, name)) in Zoom::ALL.iter().enumerate() {
                    let on = self.android.view.zoom == *zoom;
                    if ui.add(egui::Button::selectable(on, *name).min_size(vec2(110.0, 0.0))).clicked() {
                        self.set_zoom(*zoom);
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            let mut cut = self.android.view.cut_bars;
            if ui.checkbox(&mut cut, "Убирать чёрные полосы в самом видео").changed() {
                self.android.view.cut_bars = cut;
                self.android.view.save();
                self.android.bars.reset();
                self.notify(if cut { "Чёрные полосы убираются" } else { "Чёрные полосы не убираются" });
            }
            ui.separator();
            self.output_menu(ui);
            ui.separator();
        }
        ui.label(RichText::new("Кадрирование").strong());
        if ui.add(egui::Button::selectable(self.crop == Crop::None, "Без кадрирования")).clicked() {
            self.set_crop(Crop::None);
        }
        egui::Grid::new("crop_presets").num_columns(3).spacing([4.0, 4.0]).show(ui, |ui| {
            for (i, (name, c)) in crop::PRESETS.iter().enumerate() {
                if ui.add(egui::Button::selectable(self.crop == *c, *name).min_size(vec2(64.0, 0.0))).clicked() {
                    self.set_crop(*c);
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        ui.separator();
        let is_custom = matches!(self.crop, Crop::Custom { .. });
        if ui.add(egui::Button::selectable(is_custom, "Вручную (обрезка краёв)")).clicked() && !is_custom {
            self.crop = Crop::Custom { left: 0.0, top: 0.0, right: 0.0, bottom: 0.0 };
        }
        if let Crop::Custom { left, top, right, bottom } = &mut self.crop {
            egui::Grid::new("crop_custom").num_columns(2).show(ui, |ui| {
                for (label, v) in [("Слева", left), ("Сверху", top), ("Справа", right), ("Снизу", bottom)] {
                    ui.label(label);
                    ui.add(egui::Slider::new(v, 0.0..=45.0).suffix(" %").fixed_decimals(0));
                    ui.end_row();
                }
            });
        }
    }

    /// How the video gets on screen, for a phone where the video layer
    /// doesn't work; and HDR to SDR, for the picture drawn by the app.
    #[cfg(target_os = "android")]
    fn output_menu(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Вывод видео").strong());
        ui.horizontal_wrapped(|ui| {
            for (output, name) in Output::ALL {
                let on = self.android.view.output == output;
                if ui.add(egui::Button::selectable(on, name)).clicked() && !on {
                    self.android.view.output = output;
                    self.android.view.save();
                    self.notify(format!("Вывод видео: {name}\nПриложение закроется, откройте его снова"));
                    self.android.exit_at = Some(Instant::now() + Duration::from_secs(3));
                }
            }
        });
        let drawn = self.android.gpu.as_ref().is_some_and(|g| !g.is_layer());
        if drawn {
            ui.horizontal(|ui| {
                ui.label("HDR в SDR");
                let auto = format!("Авто ({})", if self.android.hdr == Some(true) { "вкл" } else { "выкл" });
                for (value, name) in [(None, auto.as_str()), (Some(true), "Вкл"), (Some(false), "Выкл")] {
                    if ui.add(egui::Button::selectable(self.android.view.hdr_to_sdr == value, name)).clicked() {
                        self.android.view.hdr_to_sdr = value;
                        self.android.view.save();
                    }
                }
            });
        }
    }

    fn audio_device_menu(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Аудиоустройство").strong());
        let devices = self.player.audio_devices();
        if devices.is_empty() {
            ui.label(RichText::new("Список появится после начала воспроизведения").color(theme::TEXT_DIM));
            return;
        }
        // An empty id is the system default device.
        let current = self.player.audio_device().or_else(|| self.store.audio_device.clone()).unwrap_or_default();
        for d in &devices {
            if ui.add(egui::Button::selectable(d.id == current, &d.name)).clicked() {
                self.set_audio_device(d);
                ui.close();
            }
        }
    }

    /// Mouse wheel over the video changes the volume.
    fn wheel_volume(&mut self, ctx: &egui::Context, rect: Rect) {
        let over = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p));
        // Events repeat when egui runs a second layout pass in the same frame.
        if !over || egui::Popup::is_any_open(ctx) || ctx.current_pass_index() > 0 {
            return;
        }
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::MouseWheel { unit, delta, .. } = e {
                    self.wheel_acc += match unit {
                        egui::MouseWheelUnit::Point => delta.y / 40.0,
                        _ => delta.y,
                    };
                }
            }
        });
        let steps = self.wheel_acc.trunc() as i32;
        if steps != 0 {
            self.wheel_acc -= steps as f32;
            self.set_volume(self.volume + steps * VOLUME_STEP);
            self.notify(format!("Громкость {}%", self.volume));
        }
    }

    fn paint_app_icon(&self, painter: &egui::Painter, rect: Rect) {
        match &self.icon {
            Some(tex) => {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(tex.id(), rect, uv, Color32::WHITE);
            }
            None => {
                painter.circle_filled(rect.center(), rect.width() / 2.0, theme::ACCENT);
                theme::paint_icon(painter, rect.shrink(rect.width() * 0.28), Icon::Play, Color32::WHITE);
            }
        }
    }

    fn speed_menu(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Скорость").strong());
        for &s in SPEEDS {
            let label = if s == 1.0 { "Обычная".to_owned() } else { format!("{}×", trim_float(s)) };
            if ui.add(egui::Button::selectable(self.rate == s, label)).clicked() {
                self.rate = s;
                self.player.set_rate(s);
            }
        }
    }

    fn playlist_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Плейлист").strong().size(15.0).color(theme::TEXT));
            ui.label(RichText::new(format!("{}", self.playlist.items.len())).color(theme::TEXT_DIM));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::menu_icon(ui, Icon::More, "Действия", false, |ui| {
                    if ui.button("Добавить файлы…").clicked() {
                        ui.close();
                        self.open_files_dialog(false);
                    }
                    if ui.button("Добавить папку…").clicked() {
                        ui.close();
                        self.open_folder_dialog();
                    }
                    if ui.button("Добавить ссылку…").clicked() {
                        ui.close();
                        self.url_dialog = Some(String::new());
                    }
                    ui.separator();
                    if ui.button("Сохранить как M3U…").clicked() {
                        ui.close();
                        self.save_playlist_dialog();
                    }
                    if ui.button("Очистить").clicked() {
                        ui.close();
                        self.stop();
                        self.playlist.clear();
                        self.selected = None;
                    }
                });
                let (rep_icon, rep_tip) = match self.playlist.repeat {
                    Repeat::Off => (Icon::Repeat, "Повтор: выкл"),
                    Repeat::All => (Icon::Repeat, "Повтор: весь список"),
                    Repeat::One => (Icon::RepeatOne, "Повтор: один трек"),
                };
                if theme::icon_button(ui, rep_icon, rep_tip, self.playlist.repeat != Repeat::Off).clicked() {
                    self.playlist.repeat = match self.playlist.repeat {
                        Repeat::Off => Repeat::All,
                        Repeat::All => Repeat::One,
                        Repeat::One => Repeat::Off,
                    };
                }
                if theme::icon_button(ui, Icon::Shuffle, "Перемешать", self.playlist.shuffle).clicked() {
                    self.playlist.shuffle = !self.playlist.shuffle;
                }
            });
        });
        ui.add_space(6.0);

        if self.playlist.items.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Пусто").color(theme::TEXT_DIM));
                if !cfg!(target_os = "android") {
                    ui.label(RichText::new("Перетащите файлы или папку в окно").small().color(theme::TEXT_DIM));
                }
                ui.add_space(8.0);
                if theme::pill(ui, Some(Icon::Plus), "Добавить файлы").clicked() {
                    self.open_files_dialog(false);
                }
            });
            #[cfg(target_os = "android")]
            self.recent_in_panel(ui);
            return;
        }

        let mut action: Option<RowAction> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, item) in self.playlist.items.iter().enumerate() {
                let current = self.playlist.current == Some(i);
                let selected = self.selected == Some(i);
                let r = theme::playlist_row(ui, i, &item.title, item.duration.map(format_time), current, selected);
                if r.row.double_clicked() || (cfg!(target_os = "android") && r.row.clicked()) {
                    action = Some(RowAction::Play(i));
                } else if r.row.clicked() {
                    action = Some(RowAction::Select(i));
                }
                if r.remove {
                    action = Some(RowAction::Remove(i));
                }
                r.row.context_menu(|ui| {
                    if ui.button("Воспроизвести").clicked() {
                        action = Some(RowAction::Play(i));
                        ui.close();
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("Выше")).clicked() {
                        action = Some(RowAction::Move(i, i - 1));
                        ui.close();
                    }
                    if ui.add_enabled(i + 1 < self.playlist.items.len(), egui::Button::new("Ниже")).clicked() {
                        action = Some(RowAction::Move(i, i + 1));
                        ui.close();
                    }
                    if ui.button("Удалить").clicked() {
                        action = Some(RowAction::Remove(i));
                        ui.close();
                    }
                });
            }
        });
        match action {
            Some(RowAction::Play(i)) => {
                self.play_index(i);
                // On a phone the picked video gets the whole screen.
                if cfg!(target_os = "android") {
                    self.show_playlist = false;
                }
            }
            Some(RowAction::Select(i)) => self.selected = Some(i),
            Some(RowAction::Move(a, b)) => {
                self.playlist.move_item(a, b);
                if self.selected == Some(a) {
                    self.selected = Some(b);
                }
            }
            Some(RowAction::Remove(i)) => {
                let was_current = self.playlist.current == Some(i);
                self.playlist.remove(i);
                self.selected = None;
                if was_current {
                    self.player.stop();
                    self.sink.clear();
                }
            }
            None => {}
        }
    }

    /// Whether the empty video area lists recently played files instead of
    /// the logo. (With the playlist open and empty, they are listed there.)
    fn has_recent(&self) -> bool {
        #[cfg(target_os = "android")]
        return !self.android.history.entries.is_empty() && !self.recent_in_playlist();
        #[cfg(not(target_os = "android"))]
        false
    }

    #[cfg(target_os = "android")]
    fn recent_in_playlist(&self) -> bool {
        self.show_playlist && !self.fullscreen && self.playlist.items.is_empty()
    }

    /// One recently played file: a tap plays it again (from where it was stopped).
    #[cfg(target_os = "android")]
    fn recent_row(&self, ui: &Ui, row: Rect, index: usize) -> bool {
        let Some(e) = self.android.history.entries.get(index) else { return false };
        let resp = ui.interact(row, egui::Id::unique(("recent_file", index)), Sense::click());
        let p = ui.painter_at(row);
        let bg = if resp.is_pointer_button_down_on() { theme::SURFACE_HI } else { theme::SURFACE };
        p.rect_filled(row, 10.0, bg);
        p.rect_stroke(row, 10.0, Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
        let c = row.left_center() + vec2(26.0, 0.0);
        p.circle_filled(c, 15.0, theme::ACCENT.gamma_multiply(0.25));
        theme::paint_icon(&p, Rect::from_center_size(c + vec2(1.5, 0.0), vec2(14.0, 14.0)), Icon::Play, theme::ACCENT);
        let mut right = row.right() - 14.0;
        if let Some(ms) = self.store.position(&e.location) {
            let r = p.text(
                pos2(right, row.center().y),
                Align2::RIGHT_CENTER,
                format!("с {}", format_time(ms)),
                FontId::proportional(13.0),
                theme::TEXT_DIM,
            );
            right = r.left() - 12.0;
        }
        let text_left = row.left() + 52.0;
        let mut job =
            egui::text::LayoutJob::simple(e.title.clone(), FontId::proportional(15.0), theme::TEXT, (right - text_left).max(40.0));
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = p.layout_job(job);
        p.galley(pos2(text_left, row.center().y - galley.size().y / 2.0), galley, theme::TEXT);
        resp.clicked()
    }

    /// The history under the empty playlist's "Add files" button.
    #[cfg(target_os = "android")]
    fn recent_in_panel(&mut self, ui: &mut Ui) {
        if self.android.history.entries.is_empty() {
            return;
        }
        ui.add_space(22.0);
        ui.label(RichText::new("Недавние").size(13.5).color(theme::TEXT_DIM));
        ui.add_space(4.0);
        let mut pick = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for i in 0..self.android.history.entries.len() {
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), RECENT_ROW), Sense::hover());
                if self.recent_row(ui, row.shrink2(vec2(0.0, 3.0)), i) {
                    pick = Some(i);
                }
            }
        });
        if let Some(e) = pick.and_then(|i| self.android.history.entries.get(i)) {
            let (location, title) = (e.location.clone(), e.title.clone());
            self.play_recent(&location, &title);
        }
    }

    /// The history on the empty video area, above the control bar: as many
    /// files as fit. A tap anywhere else still opens the file browser.
    #[cfg(target_os = "android")]
    fn recent_on_video(&mut self, ui: &mut Ui, rect: Rect) {
        const HEADER: f32 = 30.0;
        const HINT: f32 = 40.0;
        let bar_h = if is_narrow(rect.width()) { 106.0 } else { 70.0 };
        let area = Rect::from_min_max(rect.min + vec2(0.0, 12.0), pos2(rect.right(), rect.bottom() - bar_h - 8.0));
        let width = (area.width() - 32.0).min(560.0);
        let fit = ((area.height() - HEADER) / RECENT_ROW).floor().max(1.0) as usize;
        let shown = self.android.history.entries.len().min(fit);
        let rows_h = HEADER + shown as f32 * RECENT_ROW;
        let hint = area.height() - rows_h >= HINT;
        let total = rows_h + if hint { HINT } else { 0.0 };
        let top = (area.center().y - total / 2.0).max(area.top());
        let left = area.center().x - width / 2.0;
        let p = ui.painter_at(rect);
        p.text(pos2(left + 4.0, top + HEADER / 2.0), Align2::LEFT_CENTER, "Недавние", FontId::proportional(13.5), theme::TEXT_DIM);
        let mut pick = None;
        for i in 0..shown {
            let row = Rect::from_min_size(pos2(left, top + HEADER + i as f32 * RECENT_ROW + 3.0), vec2(width, RECENT_ROW - 6.0));
            if self.recent_row(ui, row, i) {
                pick = Some(i);
            }
        }
        if hint {
            p.text(
                pos2(area.center().x, top + rows_h + HINT / 2.0),
                Align2::CENTER_CENTER,
                "Нажмите на свободное место, чтобы открыть видео",
                FontId::proportional(13.0),
                theme::TEXT_DIM,
            );
        }
        if let Some(e) = pick.and_then(|i| self.android.history.entries.get(i)) {
            let (location, title) = (e.location.clone(), e.title.clone());
            self.play_recent(&location, &title);
        }
    }

    fn video_area(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let rect = ui.max_rect();
        let sense = if cfg!(target_os = "android") { Sense::click_and_drag() } else { Sense::click() };
        let resp = ui.allocate_rect(rect, sense);
        self.wheel_volume(ctx, rect);
        #[cfg(not(target_os = "android"))]
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        #[cfg(target_os = "android")]
        let (picture_area, picture_painter) = {
            let r = picture_area(ctx, rect);
            let mut p = ui.painter().clone();
            p.set_clip_rect(r);
            p.rect_filled(r, 0.0, Color32::BLACK);
            (r, p)
        };

        #[cfg(not(target_os = "android"))]
        let shows_picture = self.texture.is_some();
        #[cfg(target_os = "android")]
        let shows_picture = self.shows_picture();
        if shows_picture {
            let (w, h) = self.frame_size;
            #[cfg(not(target_os = "android"))]
            if let Some(tex) = &self.texture {
                let uv = self.crop.uv_rect(w, h);
                let aspect = (uv.width() * w) / (uv.height() * h);
                let target = crop::fit(rect, aspect);
                ui.painter().image(tex.id(), target, uv, Color32::WHITE);
            }
            // The chosen zoom can make the picture larger than the area: clip it.
            #[cfg(target_os = "android")]
            {
                let uv = self.eased_picture_uv(ctx);
                let aspect = (uv.width() * w) / (uv.height() * h);
                let target = self.android.view.zoom.place(picture_area, aspect);
                match self.picture_texture().filter(|_| !self.android.layer_picture) {
                    Some(id) => {
                        picture_painter.image(id, target, uv, Color32::WHITE);
                    }
                    None => self.show_layer(&picture_painter, picture_area, target, uv, ctx.pixels_per_point()),
                }
                // libVLC lays the subtitles out over the whole picture: shown
                // where the whole picture would fit, so that zooming in
                // doesn't push them off the screen.
                if let Some(subs) = self.android.gpu_subtitles {
                    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    picture_painter.image(subs, crop::fit(picture_area, w / h), full, Color32::WHITE);
                }
            }
        } else if !self.is_active() && !self.has_recent() {
            let c = rect.center() - vec2(0.0, 30.0);
            self.paint_app_icon(ui.painter(), Rect::from_center_size(c - vec2(0.0, 24.0), vec2(72.0, 72.0)));
            ui.painter().text(
                c + vec2(0.0, 34.0),
                Align2::CENTER_CENTER,
                if cfg!(target_os = "android") {
                    "Нажмите, чтобы открыть видео"
                } else {
                    "Перетащите сюда видео или щёлкните, чтобы открыть"
                },
                FontId::proportional(14.0),
                theme::TEXT_DIM,
            );
        } else if !self.player.is_playing() {
            // Audio-only media or still opening.
            if let Some(t) = self.current_title() {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, t, FontId::proportional(18.0), theme::TEXT_DIM);
            }
        }

        #[cfg(target_os = "android")]
        if !shows_picture && !self.is_active() && self.has_recent() {
            self.recent_on_video(ui, rect);
        }

        if self.is_active() && !self.player.is_playing() && shows_picture {
            ui.painter().circle_filled(rect.center(), 30.0, Color32::from_black_alpha(140));
            let icon = Rect::from_center_size(rect.center() + vec2(2.0, 0.0), vec2(24.0, 24.0));
            theme::paint_icon(ui.painter(), icon, Icon::Play, Color32::WHITE);
        }

        #[cfg(not(target_os = "android"))]
        if resp.double_clicked() {
            self.set_fullscreen(ctx, !self.fullscreen);
        } else if resp.clicked() && !self.is_active() {
            self.open_files_dialog(true);
        }
        #[cfg(target_os = "android")]
        self.video_taps(ctx, &resp, rect);
        resp.context_menu(|ui| {
            ui.set_min_width(200.0);
            ui.menu_button("Аудиодорожка", |ui| self.audio_menu(ui));
            ui.menu_button("Субтитры", |ui| self.subtitle_menu(ui));
            ui.menu_button("Кадрирование", |ui| self.crop_menu(ui));
            ui.menu_button("Скорость", |ui| self.speed_menu(ui));
            ui.menu_button("Аудиоустройство", |ui| self.audio_device_menu(ui));
            ui.separator();
            if ui.button("Открыть файлы…").clicked() {
                ui.close();
                self.open_files_dialog(true);
            }
            if ui.add_enabled(self.is_active(), egui::Button::new("Стоп")).clicked() {
                ui.close();
                self.stop();
            }
            if ui.button(if self.fullscreen { "Выйти из полного экрана" } else { "Полный экран" }).clicked() {
                ui.close();
                self.set_fullscreen(ctx, !self.fullscreen);
            }
        });

        self.overlay_controls(ui, ctx, rect);
        #[cfg(target_os = "android")]
        self.paint_swipe(ui, rect);

        // Short on-screen message (track switched, crop changed, ...).
        if let Some((text, at)) = &self.status {
            let age = at.elapsed().as_secs_f32();
            if age < 2.0 {
                let alpha = (1.0 - (age - 1.5).max(0.0) * 2.0).clamp(0.0, 1.0);
                let galley = ui.painter().layout_no_wrap(text.clone(), FontId::proportional(14.0), Color32::WHITE);
                // On Android the title sits there while paused: go below it.
                let below_title =
                    cfg!(target_os = "android") && (self.fullscreen || self.is_active() && !self.player.is_playing());
                let top = if below_title { 76.0 } else { 16.0 };
                let bubble = Rect::from_min_size(rect.min + vec2(16.0, top), galley.size() + vec2(28.0, 14.0));
                ui.painter().rect_filled(bubble, 8.0, Color32::from_black_alpha((170.0 * alpha) as u8));
                ui.painter().galley(bubble.min + vec2(14.0, 7.0), galley, Color32::WHITE.gamma_multiply(alpha));
                ctx.request_repaint();
            } else {
                self.status = None;
            }
        }
    }

    /// Touch gestures on the video: a tap in the middle pauses or resumes,
    /// elsewhere it shows or hides the controls; a double tap on the left or
    /// right third seeks 10 s, in the middle it toggles fullscreen.
    #[cfg(target_os = "android")]
    fn video_taps(&mut self, ctx: &egui::Context, resp: &egui::Response, rect: Rect) {
        self.video_swipes(ctx, resp, rect);
        if !self.is_active() {
            if resp.clicked() {
                self.open_files_dialog(true);
            }
            return;
        }
        if resp.double_clicked() {
            let x = resp.interact_pointer_pos().map_or(0.5, |p| (p.x - rect.left()) / rect.width());
            if x < 1.0 / 3.0 {
                self.seek_by(-10_000);
                self.notify("−10 с");
            } else if x > 2.0 / 3.0 {
                self.seek_by(10_000);
                self.notify("+10 с");
            } else {
                self.set_fullscreen(ctx, !self.fullscreen);
            }
            // The first tap of the pair toggled the controls or the pause; undo that.
            self.android.hide_controls = false;
            if std::mem::take(&mut self.android.tap_paused) {
                self.toggle_pause();
            }
            self.last_activity = Instant::now();
            return;
        }
        if !resp.clicked() {
            return;
        }
        let middle = Rect::from_center_size(rect.center(), vec2(rect.width() / 3.0, rect.height() / 3.0));
        self.android.tap_paused = resp.interact_pointer_pos().is_some_and(|p| middle.contains(p));
        if self.android.tap_paused {
            self.toggle_pause();
        } else if self.android.controls_at_press
            && self.player.is_playing()
            && resp.interact_pointer_pos().is_none_or(|p| p.y < rect.bottom() - if is_narrow(rect.width()) { 106.0 } else { 70.0 })
        {
            self.android.hide_controls = true;
        }
    }

    #[cfg(target_os = "android")]
    fn video_swipes(&mut self, ctx: &egui::Context, resp: &egui::Response, rect: Rect) {
        let cancelled = std::mem::take(&mut self.android.touch_cancelled);
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.android.controls_at_press = self.android.controls_visible;
        }
        if resp.drag_started() {
            let origin = ctx.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
            let bar_top = rect.bottom() - if is_narrow(rect.width()) { 106.0 } else { 70.0 };
            // A touch on the visible control bar belongs to its buttons and slider.
            let on_bar = |o: &egui::Pos2| self.android.controls_at_press && o.y > bar_top;
            self.android.swipe = origin.filter(|o| !on_bar(o)).map(|origin| Swipe {
                kind: SwipeKind::Undecided,
                origin,
                start: 0.0,
                value: 0.0,
                max: 0.0,
            });
        }
        if resp.drag_stopped() || !ctx.input(|i| i.pointer.any_down()) {
            if let Some(sw) = self.android.swipe.take() {
                // A touch Android took over (a system gesture) doesn't seek.
                if sw.kind == SwipeKind::Seek && !cancelled {
                    self.player.set_time(sw.value as i64);
                }
            }
            return;
        }
        if !resp.dragged() {
            return;
        }
        let seek_length = self.player.length().filter(|&l| l > 0 && self.is_active() && self.player.is_seekable());
        let now_ms = self.player.time();
        let volume_now = self.volume;
        let mut new_volume = None;
        let (Some(sw), Some(pos)) = (&mut self.android.swipe, resp.interact_pointer_pos()) else { return };
        if sw.kind == SwipeKind::Undecided {
            let d = pos - sw.origin;
            if d.length() < SWIPE_SLOP {
                return;
            }
            sw.origin = pos;
            if d.x.abs() > d.y.abs() {
                let Some(len) = seek_length else {
                    self.android.swipe = None;
                    return;
                };
                sw.kind = SwipeKind::Seek;
                sw.start = now_ms as f32;
                sw.max = len as f32;
            } else if sw.origin.x < rect.center().x {
                sw.kind = SwipeKind::Brightness;
                sw.start = platform::brightness();
                sw.max = 1.0;
            } else {
                // The player's own volume, the same as the slider (0–150 %).
                sw.kind = SwipeKind::Volume;
                sw.start = volume_now as f32;
                sw.max = 150.0;
            }
            sw.value = sw.start;
        }
        let d = pos - sw.origin;
        // Up is brighter / louder; most of the height covers the whole range.
        let up = -d.y / (rect.height() * 0.7);
        match sw.kind {
            SwipeKind::Brightness => {
                let v = (sw.start + up).clamp(0.01, 1.0);
                if (v - sw.value).abs() > 0.004 {
                    sw.value = v;
                    platform::set_brightness(v);
                }
            }
            SwipeKind::Volume => {
                let v = (sw.start + up * sw.max).round().clamp(0.0, sw.max);
                if v != sw.value {
                    sw.value = v;
                    new_volume = Some(v as i32);
                }
            }
            SwipeKind::Seek => {
                let range = sw.max.min(SWIPE_SEEK_RANGE_MS);
                sw.value = (sw.start + d.x / rect.width() * range).clamp(0.0, (sw.max - 500.0).max(0.0));
            }
            SwipeKind::Undecided => {}
        }
        // Brightness and volume don't need the control bar; keep it as it was.
        if matches!(sw.kind, SwipeKind::Brightness | SwipeKind::Volume) && !self.android.controls_at_press {
            self.android.hide_controls = true;
        }
        if let Some(v) = new_volume {
            self.set_volume(v);
        }
    }

    /// What a swipe is changing, in a bubble over the video.
    #[cfg(target_os = "android")]
    fn paint_swipe(&self, ui: &Ui, rect: Rect) {
        let Some(sw) = &self.android.swipe else { return };
        let (title, detail, fraction) = match sw.kind {
            SwipeKind::Undecided => return,
            SwipeKind::Brightness => ("Яркость".to_owned(), format!("{} %", (sw.value * 100.0).round()), Some(sw.value)),
            SwipeKind::Volume => ("Громкость".to_owned(), format!("{} %", sw.value), Some(sw.value / sw.max)),
            SwipeKind::Seek => {
                let delta = (sw.value - sw.start) as i64;
                let sign = if delta < 0 { "−" } else { "+" };
                (
                    format!("{} / {}", format_time(sw.value as i64), format_time(sw.max as i64)),
                    format!("{sign}{}", format_time(delta.abs())),
                    None,
                )
            }
        };
        let p = ui.painter();
        let center = pos2(rect.center().x, rect.top() + rect.height() * 0.3);
        let bubble = Rect::from_center_size(center, vec2(200.0, if fraction.is_some() { 84.0 } else { 70.0 }));
        p.rect_filled(bubble, 14.0, Color32::from_black_alpha(170));
        p.text(bubble.center_top() + vec2(0.0, 22.0), Align2::CENTER_CENTER, title, FontId::proportional(17.0), Color32::WHITE);
        p.text(bubble.center_top() + vec2(0.0, 46.0), Align2::CENTER_CENTER, detail, FontId::proportional(14.0), theme::TEXT_DIM);
        if let Some(f) = fraction {
            let track = Rect::from_min_size(bubble.left_bottom() + vec2(20.0, -18.0), vec2(bubble.width() - 40.0, 4.0));
            p.rect_filled(track, 2.0, Color32::from_white_alpha(50));
            let fill = Rect::from_min_size(track.min, vec2(track.width() * f.clamp(0.0, 1.0), track.height()));
            p.rect_filled(fill, 2.0, theme::ACCENT);
        }
    }

    /// The control bar: translucent, drawn over the bottom of the video and
    /// faded out while the video plays and the mouse is idle.
    fn overlay_controls(&mut self, ui: &mut Ui, ctx: &egui::Context, rect: Rect) {
        let bar_h = if is_narrow(rect.width()) { 106.0 } else { 70.0 };
        let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - bar_h), rect.max);
        #[cfg(target_os = "android")]
        if std::mem::take(&mut self.android.hide_controls) {
            self.last_activity = Instant::now() - HIDE_CONTROLS_AFTER;
        }
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let over_bar = pointer.is_some_and(|p| bar.expand(12.0).contains(p));
        let visible = !self.is_active()
            || !self.player.is_playing()
            || over_bar
            || self.seek_drag.is_some()
            || self.last_activity.elapsed() < HIDE_CONTROLS_AFTER
            || egui::Popup::is_any_open(ctx);
        let alpha = ctx.animate_bool_with_time(egui::Id::unique("controls_fade"), visible, 0.2);
        #[cfg(target_os = "android")]
        {
            self.android.controls_visible = visible;
        }
        if visible {
            ctx.request_repaint_after(HIDE_CONTROLS_AFTER);
        } else if pointer.is_some_and(|p| rect.contains(p)) {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        if alpha <= 0.01 {
            return;
        }

        #[cfg(not(target_os = "android"))]
        let (span, painter) = (rect, ui.painter().clone());
        #[cfg(target_os = "android")]
        let (span, painter) = {
            let span = picture_area(ctx, rect);
            let mut p = ui.painter().clone();
            p.set_clip_rect(span);
            (span, p)
        };
        let shade = |top: f32, bottom: f32, from_top: bool, strength: f32| {
            let g = Rect::from_min_max(pos2(span.left(), top), pos2(span.right(), bottom));
            let dark = Color32::from_black_alpha((strength * alpha) as u8);
            let (t, b) = if from_top { (dark, Color32::TRANSPARENT) } else { (Color32::TRANSPARENT, dark) };
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(g.left_top(), t);
            mesh.colored_vertex(g.right_top(), t);
            mesh.colored_vertex(g.right_bottom(), b);
            mesh.colored_vertex(g.left_bottom(), b);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            egui::Shape::mesh(mesh)
        };
        painter.add(shade((rect.bottom() - bar_h - 60.0).max(rect.top()), span.bottom(), false, 200.0));

        // In fullscreen there is no title bar, so show the title on the video.
        // Android has no title bar at all, so there it also shows while paused.
        let paused = cfg!(target_os = "android") && self.is_active() && !self.player.is_playing();
        if self.fullscreen || paused {
            if let Some(t) = self.current_title() {
                painter.add(shade(span.top(), rect.top() + 90.0, true, 170.0));
                #[cfg(not(target_os = "android"))]
                ui.painter().text(
                    rect.left_top() + vec2(20.0, 18.0),
                    Align2::LEFT_TOP,
                    t,
                    FontId::proportional(15.0),
                    Color32::WHITE.gamma_multiply(alpha),
                );
                // Names of files on a network share are often long: wrap to two lines on a phone.
                #[cfg(target_os = "android")]
                {
                    let color = Color32::WHITE.gamma_multiply(alpha);
                    let mut job =
                        egui::text::LayoutJob::simple(t, FontId::proportional(17.0), color, rect.width() - 40.0);
                    job.wrap.max_rows = 2;
                    let galley = ui.painter().layout_job(job);
                    ui.painter().galley(rect.left_top() + vec2(20.0, 18.0), galley, color);
                }
            }
        }

        let inner = bar.shrink2(vec2(14.0, 4.0));
        ui.scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)), |ui| {
            ui.multiply_opacity(alpha);
            self.controls(ui, ctx, bar.bottom());
        });
    }

    fn url_window(&mut self, ctx: &egui::Context) {
        let Some(mut url) = self.url_dialog.take() else { return };
        let mut open = true;
        let mut submit = false;
        egui::Window::new("Открыть поток")
            .collapsible(false)
            .resizable(false)
            // On a phone the soft keyboard covers the lower half, so keep the dialog at the top.
            .anchor(
                if cfg!(target_os = "android") { Align2::CENTER_TOP } else { Align2::CENTER_CENTER },
                vec2(0.0, if cfg!(target_os = "android") { 24.0 } else { 0.0 }),
            )
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Адрес (http, https, rtsp, udp, …)");
                let width = (ctx.content_rect().width() - 80.0).min(380.0);
                let r = ui.add(egui::TextEdit::singleline(&mut url).desired_width(width).hint_text("https://…"));
                if !cfg!(target_os = "android") {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if theme::pill(ui, None, "Открыть").clicked() {
                        submit = true;
                    }
                    // Typing a link on a phone is no fun; take it from the clipboard.
                    #[cfg(target_os = "android")]
                    if ui.add(egui::Button::new("Вставить").min_size(vec2(0.0, 30.0))).clicked() {
                        if let Some(t) = platform::clipboard_text() {
                            url = t.trim().to_owned();
                        }
                    }
                });
            });
        if submit && !url.trim().is_empty() {
            self.playlist.add_location(&url);
            self.play_index(self.playlist.items.len() - 1);
        } else if open && !ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.url_dialog = Some(url);
        }
    }
}

enum RowAction {
    Play(usize),
    Select(usize),
    Move(usize, usize),
    Remove(usize),
}

impl eframe::App for PlayerApp {
    /// Opaque: the window is see-through only where the video layer's
    /// picture is (see `show_layer`).
    #[cfg(target_os = "android")]
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    /// When Android takes a touch over (a system gesture, a window on top),
    /// winit reports it as cancelled and egui only hears "pointer gone", so it
    /// would think the finger is still down: the drag never ends and the next
    /// tap continues it. Treat a cancelled touch as the finger lifting.
    #[cfg(target_os = "android")]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let mut down = ctx.input(|i| i.pointer.any_down());
        let mut i = 0;
        while i < raw_input.events.len() {
            match raw_input.events[i] {
                egui::Event::PointerButton { pressed, .. } => down = pressed,
                egui::Event::Touch { phase: egui::TouchPhase::Cancel, pos, .. } if down => {
                    let release =
                        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() };
                    raw_input.events.insert(i + 1, release);
                    self.android.touch_cancelled = true;
                    down = false;
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }
        // The camera cutout and the taskbar: egui's content rect leaves them
        // out, so windows and menus stay clear of them (the root panels too,
        // see `ui`).
        let ppp = ctx.pixels_per_point();
        let [left, top, right, bottom] = platform::safe_insets().map(|px| px / ppp);
        raw_input.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 { left, right, top, bottom }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.remember_position();
        self.store.save();
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.handle_input(&ctx);
        let has_items = !self.playlist.items.is_empty();
        match self.thumb_bar.update(frame, &ctx, self.player.is_playing(), has_items) {
            Some(taskbar::Action::Prev) => self.prev(),
            Some(taskbar::Action::PlayPause) => self.toggle_pause(),
            Some(taskbar::Action::Next) => self.next(false),
            None => {}
        }

        #[cfg(not(target_os = "android"))]
        if !self.fullscreen {
            egui::Panel::top("title_bar")
                .exact_size(32.0)
                .frame(egui::Frame::new().fill(theme::BG))
                .show_separator_line(false)
                .show(ui, |ui| self.title_bar(ui, &ctx));
        }
        #[cfg(target_os = "android")]
        self.gpu_frame(&ctx, frame);
        #[cfg(target_os = "android")]
        if self.android.pip {
            egui::CentralPanel::no_frame().show(ui, |ui| self.pip_view(ui));
            return;
        }
        // Lay everything out clear of the camera cutout; only the video
        // picture reaches past it (see `video_area`).
        #[cfg(target_os = "android")]
        let mut safe_ui = ui.new_child(UiBuilder::new().max_rect(ctx.content_rect()));
        #[cfg(target_os = "android")]
        let ui = &mut safe_ui;
        let content = ctx.content_rect();
        let portrait = cfg!(target_os = "android") && content.height() > content.width();
        if self.show_playlist && !self.fullscreen && portrait {
            // Phone held upright: video on top, playlist below it.
            // At least tall enough for the two-row control bar over a 16:9 picture.
            let video_h = (content.width() * 9.0 / 16.0).max(260.0).min(content.height() * 0.6);
            egui::Panel::bottom("playlist_bottom")
                .exact_size((content.height() - video_h).max(120.0))
                .frame(
                    egui::Frame::new()
                        .fill(theme::SURFACE)
                        .inner_margin(Margin::same(10))
                        .stroke(Stroke::new(1.0, theme::BORDER)),
                )
                .show(ui, |ui| self.playlist_panel(ui));
        } else if self.show_playlist && !self.fullscreen {
            egui::Panel::right("playlist")
                .resizable(true)
                .default_size(280.0)
                .size_range(200.0..=520.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::SURFACE)
                        .inner_margin(Margin::same(10))
                        .stroke(Stroke::new(1.0, theme::BORDER)),
                )
                .show(ui, |ui| self.playlist_panel(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| self.video_area(ui, &ctx));
        self.url_window(&ctx);
        #[cfg(not(target_os = "android"))]
        self.resize_edges(&ctx);
        #[cfg(target_os = "android")]
        {
            self.browser_window(&ctx);
            lan::dialog_window(&ctx, &mut self.android.lan_dialog);
        }
    }
}

/// Too narrow for one row of controls (a phone held upright).
fn is_narrow(width: f32) -> bool {
    cfg!(target_os = "android") && width < 560.0
}

fn track_name(t: &vlc::Track) -> String {
    if t.id < 0 {
        "Отключить".to_owned()
    } else {
        t.name.clone()
    }
}

/// `-100 ms [ 0 ] +100 ms` editor; returns true when the value changed.
fn delay_editor(ui: &mut Ui, ms: &mut i64) -> bool {
    let before = *ms;
    if ui.small_button("−").clicked() {
        *ms -= 100;
    }
    ui.add(egui::DragValue::new(ms).speed(10).suffix(" мс"));
    if ui.small_button("+").clicked() {
        *ms += 100;
    }
    if ui.small_button("0").on_hover_text("Сбросить").clicked() {
        *ms = 0;
    }
    *ms != before
}

/// Controls keep clear of the camera cutout, but the picture (and the shading
/// under the controls) goes on to the screen's edge where the video area
/// meets it, around the camera.
#[cfg(target_os = "android")]
fn picture_area(ctx: &egui::Context, rect: Rect) -> Rect {
    let (full, safe) = (ctx.viewport_rect(), ctx.content_rect());
    let mut r = rect;
    if rect.left() <= safe.left() + 0.5 {
        r.min.x = full.left();
    }
    if rect.top() <= safe.top() + 0.5 {
        r.min.y = full.top();
    }
    if rect.right() >= safe.right() - 0.5 {
        r.max.x = full.right();
    }
    if rect.bottom() >= safe.bottom() - 0.5 {
        r.max.y = full.bottom();
    }
    r
}

pub fn format_time(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn trim_float(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0), "00:00");
        assert_eq!(format_time(65_400), "01:05");
        assert_eq!(format_time(3_725_000), "1:02:05");
        assert_eq!(trim_float(1.5), "1.5");
        assert_eq!(trim_float(1.15), "1.15");
        assert_eq!(trim_float(2.0), "2");
    }
}
