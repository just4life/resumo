//! The player window: video surface, controls, playlist and menus.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Margin, Rect, RichText, Sense, Stroke,
    TextureHandle, TextureOptions, Ui, UiBuilder,
};

use crate::crop::{self, Crop};
use crate::hdr::{self, HdrInfo, Transfer};
use crate::hdr_gl;
use crate::playlist::{self, Playlist, Repeat};
use crate::snapshot::{self, Image};
use crate::store::Store;
use crate::taskbar::{self, ThumbBar};
use crate::theme::{self, Icon};
use crate::video::{PixelFormat, VideoSink};
use crate::vlc::{self, Instance, MediaPlayer};

const SPEEDS: &[f32] = &[0.25, 0.5, 0.75, 1.0, 1.15, 1.25, 1.5, 1.75, 2.0, 3.0];
const HIDE_CONTROLS_AFTER: Duration = Duration::from_millis(2500);
const SAVE_EVERY: Duration = Duration::from_secs(5);
const VOLUME_STEP: i32 = 5;
/// If no 10-bit frame shows up this long after an HDR video started, libVLC
/// could not deliver them; fall back to its own (washed-out) RGBA.
const HDR_FRAME_TIMEOUT: Duration = Duration::from_secs(4);
/// HDR brightness (nits) shown as white at 100 % in the HDR menu. BT.2408's
/// 203 nits came out brighter than other players; this matches PotPlayer.
const HDR_WHITE_NITS: f32 = 290.0;
const HDR_BRIGHTNESS: std::ops::RangeInclusive<i32> = 50..=150;
/// How long to wait for the GPU to hand back an HDR frame being saved.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

/// A frame to save once the HDR renderer has drawn it off screen.
struct PendingCapture {
    since: Instant,
    dir: PathBuf,
    name: String,
}

/// Flags set from libVLC's event thread and consumed on the UI thread.
#[derive(Default)]
struct Events {
    end_reached: AtomicBool,
    error: AtomicBool,
    playing: AtomicBool,
    repaint: Mutex<Option<egui::Context>>,
}

impl Events {
    /// Forgets what the media before reported. libVLC's stop waits for that
    /// media's thread, so nothing of it arrives later: its end or error must
    /// not move on from the next one.
    fn reset(&self) {
        self.end_reached.store(false, Ordering::SeqCst);
        self.error.store(false, Ordering::SeqCst);
        self.playing.store(false, Ordering::SeqCst);
    }
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

    /// GPU tone mapper; `None` when the GL context can't run it.
    hdr_gl: Option<hdr_gl::Shared>,
    /// What the current file's metadata says, even if conversion is off.
    hdr: Option<HdrInfo>,
    /// The current video is requested as P010 and tone-mapped by us.
    hdr_active: bool,
    /// The newest frame is P010 (drawn by `hdr_gl` instead of `texture`).
    hdr_frame: bool,
    /// Set when an HDR video started; cleared by its first P010 frame.
    hdr_wait: Option<Instant>,
    /// Location for which libVLC failed to deliver P010 frames.
    hdr_failed: Option<String>,

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
    /// Don't announce the resume (the video was restarted on purpose).
    resume_quietly: bool,
    last_save: Instant,
    /// Unused fraction of mouse-wheel notches (touchpads scroll in pixels).
    wheel_acc: f32,
    icon: Option<TextureHandle>,
    thumb_bar: ThumbBar,

    capture: Option<PendingCapture>,
    /// Results of saving frames, from the background writer.
    saved_tx: mpsc::Sender<Result<PathBuf, String>>,
    saved_rx: mpsc::Receiver<Result<PathBuf, String>>,
    last_saved: Option<PathBuf>,
}

impl PlayerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Result<Self, String> {
        theme::apply(&cc.egui_ctx);
        let store = Store::load();
        let icon = eframe::icon_data::from_png_bytes(crate::ICON_PNG).ok().map(|i| {
            let img = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
            let opts = TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
            cc.egui_ctx.load_texture("app_icon", img, opts)
        });

        let hdr_gl = cc.gl.as_ref().and_then(|gl| hdr_gl::Renderer::new(gl)).map(|r| Arc::new(Mutex::new(r)));

        let instance = Instance::new(&["--no-video-title-show", "--quiet"])?;
        let player = MediaPlayer::new(&instance)?;

        let ctx = cc.egui_ctx.clone();
        let sink = Arc::new(VideoSink::new(move || ctx.request_repaint()));
        let events = Arc::new(Events::default());
        *events.repaint.lock().unwrap() = Some(cc.egui_ctx.clone());
        sink.set_player(player.handle());
        unsafe {
            player.set_video_callbacks(&sink.callbacks());
            player.attach_events(
                &[vlc::EVENT_END_REACHED, vlc::EVENT_ERROR, vlc::EVENT_PLAYING],
                on_vlc_event,
                Arc::as_ptr(&events) as *mut c_void,
            );
        }

        let (saved_tx, saved_rx) = mpsc::channel();
        let mut app = Self {
            player,
            _instance: instance,
            sink,
            events,
            texture: None,
            frame_serial: 0,
            frame_size: (0.0, 0.0),
            hdr_gl,
            hdr: None,
            hdr_active: false,
            hdr_frame: false,
            hdr_wait: None,
            hdr_failed: None,
            playlist: Playlist::default(),
            // Opened with a file (e.g. from Explorer): go straight to the video.
            show_playlist: files.is_empty(),
            selected: None,
            volume: store.volume.unwrap_or(80).clamp(0, 150),
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
            resume_quietly: false,
            last_save: Instant::now(),
            wheel_acc: 0.0,
            icon,
            thumb_bar: ThumbBar::default(),
            capture: None,
            saved_tx,
            saved_rx,
            last_saved: None,
        };
        app.add_paths(files, true);
        Ok(app)
    }

    // ---------------------------------------------------------------- playback

    fn play_index(&mut self, index: usize) {
        let Some(item) = self.playlist.items.get(index) else { return };
        let location = item.location.clone();
        self.remember_position();
        self.player.stop();
        self.events.reset();
        self.sink.clear();
        // libVLC doesn't report the transfer function, so read it ourselves.
        self.hdr = if location.contains("://") { None } else { hdr::probe(Path::new(&location)) };
        self.hdr_active = self.hdr.is_some()
            && self.hdr_gl.is_some()
            && !self.store.hdr_off
            && self.hdr_failed.as_deref() != Some(location.as_str());
        self.hdr_wait = None;
        self.sink.set_format(if self.hdr_active { PixelFormat::P010 } else { PixelFormat::Rgba });
        if let Err(e) = self.player.set_media(&location) {
            self.notify(format!("Не удалось открыть: {e}"));
            return;
        }
        self.playlist.current = Some(index);
        self.selected = Some(index);
        self.subtitle_delay_ms = 0;
        self.audio_delay_ms = 0;
        self.seek_drag = None;
        self.pending_subtitles.clear();
        self.resume_at = self.store.position(&location);
        self.resume_quietly = false;
        if !self.player.play() {
            self.notify("Не удалось начать воспроизведение");
        }
    }

    /// Starts the current video again where it is (after an HDR mode change).
    fn reload_current(&mut self) {
        let Some(i) = self.playlist.current else { return };
        let t = self.player.time();
        self.play_index(i);
        if t > 0 {
            self.resume_at = Some(t);
            self.resume_quietly = true;
        }
    }

    fn set_hdr_conversion(&mut self, on: bool) {
        if self.store.hdr_off == !on {
            return;
        }
        self.store.set_hdr_off(!on);
        self.store.save();
        self.hdr_failed = None;
        self.notify(if on { "Преобразование HDR: включено" } else { "Преобразование HDR: выключено" });
        if self.hdr.is_some() {
            self.reload_current();
        }
    }

    /// Nits shown as white, from the brightness setting.
    fn hdr_white(&self) -> f32 {
        HDR_WHITE_NITS * 100.0 / self.store.hdr_brightness.unwrap_or(100) as f32
    }

    fn is_active(&self) -> bool {
        self.playlist.current.is_some()
    }

    /// Saves the frame on screen (cropped as shown, at the video's own
    /// resolution) as a PNG in Pictures/VPlayer.
    fn save_frame(&mut self, ctx: &egui::Context) {
        if !self.has_picture() {
            self.notify("Нет кадра для сохранения");
            return;
        }
        let dir = snapshot::folder();
        let name = snapshot::file_name(&self.current_title().unwrap_or_default(), self.player.time());
        if self.hdr_frame {
            // Only the GPU has the converted picture; it reads it back while painting.
            if let Some(r) = &self.hdr_gl {
                r.lock().unwrap().capture = true;
                self.capture = Some(PendingCapture { since: Instant::now(), dir, name });
                ctx.request_repaint();
            }
            return;
        }
        let (w, h) = self.frame_size;
        let uv = self.crop.uv_rect(w, h);
        let image = self.sink.with_frame(|f| {
            (f.width > 0 && f.format == PixelFormat::Rgba)
                .then(|| Image { width: f.width, height: f.height, rgba: f.pixels.clone() })
        });
        match image {
            Some(img) => self.write_frame(ctx, img.crop(uv), dir, name),
            None => self.notify("Нет кадра для сохранения"),
        }
    }

    fn write_frame(&self, ctx: &egui::Context, image: Image, dir: PathBuf, name: String) {
        let tx = self.saved_tx.clone();
        let ctx = ctx.clone();
        // PNG encoding of a 4K frame takes a moment; keep the UI responsive.
        std::thread::spawn(move || {
            let result = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|()| {
                let path = snapshot::unique_path(&dir, &name);
                image.save_png(&path).map(|()| path)
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn poll_saved_frames(&mut self, ctx: &egui::Context) {
        if let Some(pending) = &self.capture {
            let got = self.hdr_gl.as_ref().and_then(|r| r.lock().unwrap().captured.take());
            match got {
                Some(Some(image)) => {
                    let pending = self.capture.take().unwrap();
                    self.write_frame(ctx, image, pending.dir, pending.name);
                }
                Some(None) => {
                    self.capture = None;
                    self.notify("Не удалось сохранить кадр");
                }
                None if pending.since.elapsed() >= CAPTURE_TIMEOUT => {
                    self.capture = None;
                    if let Some(r) = &self.hdr_gl {
                        r.lock().unwrap().capture = false;
                    }
                    self.notify("Не удалось сохранить кадр");
                }
                None => ctx.request_repaint(),
            }
        }
        while let Ok(result) = self.saved_rx.try_recv() {
            match result {
                Ok(path) => {
                    // "Pictures\VPlayer": enough to find it, short enough for the OSD.
                    let dir = path.parent().unwrap_or(&path);
                    let folder = match (dir.parent().and_then(Path::file_name), dir.file_name()) {
                        (Some(parent), Some(name)) => Path::new(parent).join(name),
                        _ => dir.to_path_buf(),
                    };
                    self.notify(format!("Кадр сохранён в {}", folder.display()));
                    self.last_saved = Some(path);
                }
                Err(e) => self.notify(format!("Не удалось сохранить кадр: {e}")),
            }
        }
    }

    fn open_frames_folder(&mut self) {
        match &self.last_saved {
            Some(path) if path.exists() => snapshot::reveal(path),
            _ => {
                let dir = snapshot::folder();
                let _ = std::fs::create_dir_all(&dir);
                snapshot::reveal(&dir);
            }
        }
    }

    fn has_picture(&self) -> bool {
        self.texture.is_some() || self.hdr_frame
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
        self.events.reset();
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
            let t = self.player.length().map_or(t, |l| t.min(l - 500)).max(0);
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

    fn open_folder_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Добавить папку").pick_folder() {
            self.add_paths(vec![dir], false);
        }
    }

    fn open_subtitle_dialog(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title("Загрузить субтитры")
            .add_filter("Субтитры", playlist::SUBTITLE_EXTENSIONS)
            .pick_file();
        if let Some(f) = file {
            self.load_subtitle(&f);
        }
    }

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
                    if !self.resume_quietly {
                        self.notify(format!("Продолжаю с {}", format_time(ms)));
                    }
                }
            }
            if self.hdr_active && !self.hdr_frame {
                self.hdr_wait = Some(Instant::now());
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
            // P010 frames are uploaded by the HDR renderer while painting.
            let (format, w, h) = self.sink.with_frame(|f| (f.format, f.width, f.height));
            self.hdr_frame = w > 0 && format == PixelFormat::P010;
            let image = self.sink.with_frame(|f| {
                (f.width > 0 && f.format == PixelFormat::Rgba)
                    .then(|| egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.pixels))
            });
            if self.hdr_frame {
                self.hdr_wait = None;
                self.frame_size = (w as f32, h as f32);
            }
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
        self.poll_saved_frames(ctx);
        if self.hdr_wait.is_some_and(|t| t.elapsed() >= HDR_FRAME_TIMEOUT) && self.player.is_playing() {
            self.hdr_wait = None;
            if let Some(item) = self.playlist.current.and_then(|i| self.playlist.items.get(i)) {
                self.hdr_failed = Some(item.location.clone());
                self.reload_current();
                self.notify("HDR: 10-битное видео не получилось, показываю как есть");
            }
        }

        if self.is_active() {
            ctx.request_repaint_after(Duration::from_millis(250));
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
        // Only real input counts: switching to another window must not bring
        // the controls (and the fullscreen title) back.
        let user_input = ctx.input(|i| {
            i.pointer.delta() != egui::Vec2::ZERO
                || i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { .. }
                            | egui::Event::Text(_)
                            | egui::Event::PointerButton { .. }
                            | egui::Event::MouseWheel { .. }
                            | egui::Event::Touch { .. }
                    )
                })
        });
        if user_input {
            self.last_activity = Instant::now();
        }
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
        if ctrl && pressed(Key::E) {
            self.save_frame(ctx);
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
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
        let mid = (track_bottom + bottom) / 2.0;
        let full = ui.max_rect();
        let row = Rect::from_min_max(pos2(full.left(), mid - 18.0), pos2(full.right(), mid + 18.0));
        ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let playing = self.player.is_playing();
            if theme::play_button(ui, playing).on_hover_text("Пауза / воспроизведение (Пробел)").clicked() {
                self.toggle_pause();
            }
            // Switching between videos only makes sense with more than one.
            let several = self.playlist.items.len() > 1;
            theme::capsule(ui, |ui| {
                if several && theme::icon_button(ui, Icon::Prev, "Предыдущий (P)", false).clicked() {
                    self.prev();
                }
                if theme::icon_button(ui, Icon::Back10, "Назад на 10 с", false).clicked() {
                    self.seek_by(-10_000);
                }
                if theme::icon_button(ui, Icon::Forward10, "Вперёд на 10 с", false).clicked() {
                    self.seek_by(10_000);
                }
                if several && theme::icon_button(ui, Icon::Next, "Следующий (N)", false).clicked() {
                    self.next(false);
                }
            });
            theme::capsule(ui, |ui| self.volume_control(ui));
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
            theme::capsule(ui, |ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(time).font(FontId::monospace(12.0)).color(theme::TEXT));
                ui.add_space(10.0);
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                theme::capsule(ui, |ui| {
                    let fs_icon = if self.fullscreen { Icon::ExitFullscreen } else { Icon::Fullscreen };
                    if theme::icon_button(ui, fs_icon, "Полный экран (F)", false).clicked() {
                        self.set_fullscreen(ctx, !self.fullscreen);
                    }
                    if !self.fullscreen
                        && theme::icon_button(ui, Icon::Playlist, "Плейлист (L)", self.show_playlist).clicked()
                    {
                        self.show_playlist = !self.show_playlist;
                    }
                    theme::menu_icon(ui, Icon::Folder, "Открыть", false, |ui| self.open_menu(ui));
                });
                theme::capsule(ui, |ui| {
                    theme::menu_icon(ui, Icon::Crop, "Кадрирование (C)", self.crop != Crop::None, |ui| {
                        self.crop_menu(ui)
                    });
                    theme::menu_icon(ui, Icon::Subtitles, "Субтитры (S)", false, |ui| self.subtitle_menu(ui));
                    theme::menu_icon(ui, Icon::Audio, "Аудиодорожка (A)", false, |ui| self.audio_menu(ui));
                    let speed = format!("{}×", trim_float(self.rate));
                    theme::menu_text(ui, &speed, "Скорость", |ui| self.speed_menu(ui));
                });
                if self.hdr.is_some() {
                    theme::capsule(ui, |ui| {
                        theme::menu_text(ui, "HDR", "HDR-видео", |ui| self.hdr_menu(ui));
                    });
                }
            });
        });
    }

    fn hdr_menu(&mut self, ui: &mut Ui) {
        let Some(info) = self.hdr else { return };
        let title = match info.transfer {
            Transfer::Pq => format!("HDR10, пик {} нит", info.peak_nits.round()),
            Transfer::Hlg => "HDR (HLG)".to_owned(),
        };
        ui.label(RichText::new(title).strong());
        ui.set_min_width(220.0);
        let on = !self.store.hdr_off;
        let can = self.hdr_gl.is_some();
        if ui
            .add_enabled(can, egui::Button::selectable(on && can, "Преобразовывать в обычное (SDR)"))
            .on_disabled_hover_text("Видеокарта не поддерживает 16-битные текстуры")
            .clicked()
        {
            ui.close();
            self.set_hdr_conversion(true);
        }
        if ui.add(egui::Button::selectable(!on || !can, "Показывать как есть")).clicked() {
            ui.close();
            self.set_hdr_conversion(false);
        }
        if on && can {
            ui.separator();
            let mut b = self.store.hdr_brightness.unwrap_or(100);
            ui.horizontal(|ui| {
                ui.label("Яркость");
                if ui.add(egui::Slider::new(&mut b, HDR_BRIGHTNESS).step_by(5.0).suffix(" %")).changed() {
                    self.store.set_hdr_brightness(b);
                }
                if b != 100 && ui.small_button("Сброс").clicked() {
                    self.store.set_hdr_brightness(100);
                }
            });
        }
        if self.hdr_failed.is_some() && self.playlist.current.is_some() && !self.hdr_active {
            ui.label(RichText::new("Не удалось получить 10-битные кадры").color(theme::TEXT_DIM).size(12.0));
        }
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
        ui.add_space(10.0);
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
                ui.label(RichText::new("Перетащите файлы или папку в окно").small().color(theme::TEXT_DIM));
                ui.add_space(8.0);
                if theme::pill(ui, Some(Icon::Plus), "Добавить файлы").clicked() {
                    self.open_files_dialog(false);
                }
            });
            return;
        }

        let mut action: Option<RowAction> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, item) in self.playlist.items.iter().enumerate() {
                let current = self.playlist.current == Some(i);
                let selected = self.selected == Some(i);
                let r = theme::playlist_row(ui, i, &item.title, item.duration.map(format_time), current, selected);
                if r.row.double_clicked() {
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
            Some(RowAction::Play(i)) => self.play_index(i),
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
                    self.events.reset();
                    self.sink.clear();
                }
            }
            None => {}
        }
    }

    fn video_area(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let rect = ui.max_rect();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        self.wheel_volume(ctx, rect);
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);

        if self.has_picture() {
            let (w, h) = self.frame_size;
            let uv = self.crop.uv_rect(w, h);
            let aspect = (uv.width() * w) / (uv.height() * h);
            let target = crop::fit(rect, aspect);
            match (&self.texture, &self.hdr_gl, self.hdr) {
                (Some(tex), _, _) => {
                    ui.painter().image(tex.id(), target, uv, Color32::WHITE);
                }
                (None, Some(renderer), Some(info)) => {
                    hdr_gl::paint(ui, renderer, &self.sink, target, uv, info, self.hdr_white());
                }
                _ => {}
            }
        } else if !self.is_active() {
            let c = rect.center() - vec2(0.0, 30.0);
            self.paint_app_icon(ui.painter(), Rect::from_center_size(c - vec2(0.0, 24.0), vec2(72.0, 72.0)));
            ui.painter().text(
                c + vec2(0.0, 34.0),
                Align2::CENTER_CENTER,
                "Перетащите сюда видео или щёлкните, чтобы открыть",
                FontId::proportional(14.0),
                theme::TEXT_DIM,
            );
        } else if !self.player.is_playing() {
            // Audio-only media or still opening.
            if let Some(t) = self.current_title() {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, t, FontId::proportional(18.0), theme::TEXT_DIM);
            }
        }

        // Big play button in the middle while paused.
        let mut over_play = false;
        if self.is_active() && !self.player.is_playing() && self.has_picture() {
            over_play = resp.hover_pos().is_some_and(|p| p.distance(rect.center()) <= 32.0);
            let shade = if over_play { 200 } else { 140 };
            ui.painter().circle_filled(rect.center(), 32.0, Color32::from_black_alpha(shade));
            let icon = Rect::from_center_size(rect.center() + vec2(2.0, 0.0), vec2(26.0, 26.0));
            theme::paint_icon(ui.painter(), icon, Icon::Play, if over_play { theme::ACCENT_HOVER } else { Color32::WHITE });
            if over_play {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        if resp.double_clicked() {
            self.set_fullscreen(ctx, !self.fullscreen);
        } else if resp.clicked() && !self.is_active() {
            self.open_files_dialog(true);
        } else if resp.clicked() && over_play {
            self.toggle_pause();
        } else if resp.drag_started_by(egui::PointerButton::Primary) && !self.fullscreen {
            // The video doubles as a handle for moving the window.
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        resp.context_menu(|ui| {
            ui.set_min_width(200.0);
            ui.menu_button("Аудиодорожка", |ui| self.audio_menu(ui));
            ui.menu_button("Субтитры", |ui| self.subtitle_menu(ui));
            ui.menu_button("Кадрирование", |ui| self.crop_menu(ui));
            ui.menu_button("Скорость", |ui| self.speed_menu(ui));
            ui.menu_button("Аудиоустройство", |ui| self.audio_device_menu(ui));
            if self.hdr.is_some() {
                ui.menu_button("HDR", |ui| self.hdr_menu(ui));
            }
            ui.separator();
            if ui
                .add_enabled(self.has_picture(), egui::Button::new("Сохранить кадр").shortcut_text("Ctrl+E"))
                .clicked()
            {
                ui.close();
                self.save_frame(ctx);
            }
            if ui.button("Папка с кадрами").clicked() {
                ui.close();
                self.open_frames_folder();
            }
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

        // Short on-screen message (track switched, crop changed, ...).
        if let Some((text, at)) = &self.status {
            let age = at.elapsed().as_secs_f32();
            if age < 2.0 {
                let alpha = (1.0 - (age - 1.5).max(0.0) * 2.0).clamp(0.0, 1.0);
                let galley = ui.painter().layout_no_wrap(text.clone(), FontId::proportional(20.0), Color32::WHITE);
                // In fullscreen the video title sits in the top-left corner; stay below it.
                let top = if self.fullscreen { 64.0 } else { 20.0 };
                let bubble = Rect::from_min_size(rect.min + vec2(20.0, top), galley.size() + vec2(36.0, 20.0));
                ui.painter().rect_filled(bubble, 10.0, Color32::from_black_alpha((170.0 * alpha) as u8));
                ui.painter().galley(bubble.min + vec2(18.0, 10.0), galley, Color32::WHITE.gamma_multiply(alpha));
                ctx.request_repaint();
            } else {
                self.status = None;
            }
        }
    }

    /// The control bar: translucent, drawn over the bottom of the video and
    /// faded out while the video plays and the mouse is idle.
    fn overlay_controls(&mut self, ui: &mut Ui, ctx: &egui::Context, rect: Rect) {
        let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 78.0), rect.max);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let over_bar = pointer.is_some_and(|p| bar.expand(12.0).contains(p));
        let visible = !self.is_active()
            || !self.player.is_playing()
            || over_bar
            || self.seek_drag.is_some()
            || self.last_activity.elapsed() < HIDE_CONTROLS_AFTER
            || egui::Popup::is_any_open(ctx);
        let alpha = ctx.animate_bool_with_time(egui::Id::unique("controls_fade"), visible, 0.2);
        if visible {
            ctx.request_repaint_after(HIDE_CONTROLS_AFTER);
        } else if pointer.is_some_and(|p| rect.contains(p)) {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        if alpha <= 0.01 {
            return;
        }

        // In fullscreen there is no title bar, so show the title on the video,
        // on a capsule like the controls rather than a darkened band.
        if self.fullscreen {
            if let Some(t) = self.current_title() {
                let max_w = (rect.width() - 80.0).max(100.0);
                let white = Color32::WHITE.gamma_multiply(alpha);
                let galley = ui.painter().layout(t, FontId::proportional(15.0), white, max_w);
                let pill = Rect::from_min_size(rect.left_top() + vec2(16.0, 14.0), galley.size() + vec2(32.0, 16.0));
                ui.painter().rect_filled(pill, 18.0, theme::CAPSULE.gamma_multiply(alpha));
                ui.painter().galley(pill.min + vec2(16.0, 8.0), galley, white);
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
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Адрес (http, https, rtsp, udp, …)");
                let r = ui.add(egui::TextEdit::singleline(&mut url).desired_width(380.0).hint_text("https://…"));
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if theme::pill(ui, None, "Открыть").clicked() {
                        submit = true;
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
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.remember_position();
        self.store.save();
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.handle_input(&ctx);
        match self.thumb_bar.update(frame, &ctx, self.player.is_playing(), self.playlist.items.len()) {
            Some(taskbar::Action::Prev) => self.prev(),
            Some(taskbar::Action::PlayPause) => self.toggle_pause(),
            Some(taskbar::Action::Next) => self.next(false),
            None => {}
        }

        if !self.fullscreen {
            egui::Panel::top("title_bar")
                .exact_size(32.0)
                .frame(egui::Frame::new().fill(theme::BG))
                .show_separator_line(false)
                .show(ui, |ui| self.title_bar(ui, &ctx));
        }
        if self.show_playlist && !self.fullscreen {
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
        self.resize_edges(&ctx);
    }
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
