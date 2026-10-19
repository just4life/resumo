//! The player's screens on Android TV: the home screen (sources and recent
//! videos), the video with its control bar, and the side panels for audio,
//! subtitles, picture, speed and the playlist. Everything moves with the
//! remote's arrows, OK and Back; the play, pause, rewind and fast-forward
//! buttons work on every screen.

use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Stroke, Ui};

use super::{format_time, track_name, trim_float, PlayerApp, SPEEDS};
use crate::browser::{Action, Outcome};
use crate::crop::{self, Crop};
use crate::platform;
use crate::playlist::Repeat;
use crate::theme::{self, Icon};
use crate::tv::{self, ListNav, Nav, Press, Trail};
use crate::zoom::{Output, Zoom};

/// How long the control bar stays after the last press while the video plays.
const BAR_FOR: Duration = Duration::from_secs(5);
/// A seek made with ← / → is applied this long after the last press, so
/// holding the button runs through the video without stopping on each step.
const SEEK_SETTLE: Duration = Duration::from_millis(650);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Button {
    PlayPause,
    Prev,
    Next,
    Audio,
    Subtitles,
    Picture,
    Speed,
    Playlist,
}

const BUTTONS: [Button; 8] = [
    Button::PlayPause,
    Button::Prev,
    Button::Next,
    Button::Audio,
    Button::Subtitles,
    Button::Picture,
    Button::Speed,
    Button::Playlist,
];

impl Button {
    fn label(self, playing: bool) -> &'static str {
        match self {
            Button::PlayPause if playing => "Пауза",
            Button::PlayPause => "Воспроизвести",
            Button::Prev => "Предыдущее",
            Button::Next => "Следующее",
            Button::Audio => "Звук",
            Button::Subtitles => "Субтитры",
            Button::Picture => "Изображение",
            Button::Speed => "Скорость",
            Button::Playlist => "Плейлист",
        }
    }

    fn icon(self, playing: bool) -> Icon {
        match self {
            Button::PlayPause if playing => Icon::Pause,
            Button::PlayPause => Icon::Play,
            Button::Prev => Icon::Prev,
            Button::Next => Icon::Next,
            Button::Audio => Icon::Audio,
            Button::Subtitles => Icon::Subtitles,
            Button::Picture => Icon::Crop,
            Button::Speed => Icon::Speed,
            Button::Playlist => Icon::Playlist,
        }
    }

    fn panel(self) -> Option<Panel> {
        match self {
            Button::Audio => Some(Panel::Audio),
            Button::Subtitles => Some(Panel::Subtitles),
            Button::Picture => Some(Panel::Picture),
            Button::Speed => Some(Panel::Speed),
            Button::Playlist => Some(Panel::Playlist),
            _ => None,
        }
    }
}

/// Where the remote is on the control bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BarFocus {
    Seek,
    Button(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Audio,
    Subtitles,
    Picture,
    Speed,
    Playlist,
}

impl Panel {
    fn title(self) -> &'static str {
        match self {
            Panel::Audio => "Звук",
            Panel::Subtitles => "Субтитры",
            Panel::Picture => "Изображение",
            Panel::Speed => "Скорость",
            Panel::Playlist => "Плейлист",
        }
    }

    fn button(self) -> usize {
        let b = match self {
            Panel::Audio => Button::Audio,
            Panel::Subtitles => Button::Subtitles,
            Panel::Picture => Button::Picture,
            Panel::Speed => Button::Speed,
            Panel::Playlist => Button::Playlist,
        };
        BUTTONS.iter().position(|x| *x == b).unwrap_or(0)
    }
}

/// A row of a side panel. Headers can't take the focus.
#[derive(Clone, PartialEq)]
enum PanelRow {
    Header(&'static str),
    AudioTrack(i32, String),
    AudioDelay,
    SubTrack(i32, String),
    SubFile,
    SubDelay,
    Zoom(Zoom),
    CutBars,
    Hdr,
    Output,
    Crop(Crop),
    Speed(f32),
    Repeat,
    Shuffle,
    Item(usize),
    AddFiles,
    Save,
    Clear,
}

/// A seek being chosen with ← / → (or rewind / fast forward).
pub struct SeekPreview {
    target: i64,
    /// Presses in a row while held, for the growing step.
    held: u32,
    last: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tile {
    Files,
    Network,
    Link,
    Playlist,
}

pub struct TvState {
    pub presses: Vec<Press>,
    /// Home screen: on the tiles (`None`) or on a recent video.
    home_recent: Option<usize>,
    home_tile: usize,
    bar_focus: BarFocus,
    bar_until: Instant,
    panel: Option<Panel>,
    panel_nav: ListNav,
    panel_rows_visible: usize,
    seek: Option<SeekPreview>,
    /// Back on the libVLC login: cancel it.
    pub cancel_dialog: bool,
    /// The text field that had the focus last frame.
    text_focus: Option<egui::Id>,
    pub osk: crate::osk::Keyboard,
}

impl Default for TvState {
    fn default() -> Self {
        Self {
            presses: Vec::new(),
            home_recent: None,
            home_tile: 0,
            bar_focus: BarFocus::Button(0),
            bar_until: Instant::now(),
            panel: None,
            panel_nav: ListNav::default(),
            panel_rows_visible: 8,
            seek: None,
            cancel_dialog: false,
            text_focus: None,
            osk: Default::default(),
        }
    }
}

impl PlayerApp {
    /// A text field is on screen: the remote's arrows and OK belong to egui.
    pub(super) fn tv_typing(&self) -> bool {
        self.url_dialog.is_some()
            || self.android.lan_dialog.is_some()
            || self.android.browser.as_ref().is_some_and(|b| b.wants_text())
    }

    /// Shows the control bar (for a while, if the video plays).
    pub(super) fn tv_show_bar(&mut self) {
        if !self.bar_visible() {
            self.android.tv.bar_focus = BarFocus::Button(0);
        }
        self.android.tv.bar_until = Instant::now() + BAR_FOR;
    }

    fn bar_visible(&self) -> bool {
        let tv = &self.android.tv;
        tv.panel.is_none() && (Instant::now() < tv.bar_until || !self.player.is_playing())
    }

    // ---------------------------------------------------------------- input

    pub(super) fn tv_press(&mut self, ctx: &egui::Context, press: Press) {
        // The playback buttons work everywhere.
        match press.nav {
            Nav::PlayPause => {
                self.toggle_pause();
                return;
            }
            Nav::Play => {
                if !self.is_active() {
                    self.toggle_pause();
                } else if !self.player.is_playing() {
                    self.player.set_pause(false);
                }
                return;
            }
            Nav::Pause => {
                if self.player.is_playing() {
                    self.player.set_pause(true);
                    self.tv_show_bar();
                }
                return;
            }
            Nav::Stop => {
                self.tv_stop();
                return;
            }
            Nav::Next => {
                self.next(false);
                return;
            }
            Nav::Prev => {
                self.prev();
                return;
            }
            Nav::Rewind | Nav::Forward if self.is_active() => {
                let dir = if press.nav == Nav::Rewind { -1 } else { 1 };
                self.tv_seek(dir, 30_000, press.repeat);
                return;
            }
            _ => {}
        }
        if self.android.lan_dialog.is_some() {
            if press.nav == Nav::Back {
                self.android.tv.cancel_dialog = true;
            }
            return;
        }
        if self.url_dialog.is_some() {
            if press.nav == Nav::Back {
                self.url_dialog = None;
            }
            return;
        }
        if let Some(b) = &mut self.android.browser {
            match b.handle(press, self.android.storage_ok) {
                Action::None => {}
                Action::AskAccess => platform::request_storage_access(),
                Action::Close => self.close_browser(),
                Action::Keep(o) => self.apply_outcome(o),
                Action::Done(o) => {
                    self.close_browser();
                    self.apply_outcome(o);
                }
            }
            return;
        }
        if self.is_active() {
            self.tv_player_press(ctx, press);
        } else {
            self.tv_home_press(press);
        }
    }

    fn close_browser(&mut self) {
        if let Some(b) = self.android.browser.take() {
            if !b.on_network() {
                self.android.last_dir = Some(b.dir().to_path_buf());
            }
        }
    }

    /// Stops and goes back to the home screen (the position is kept).
    fn tv_stop(&mut self) {
        self.stop();
        self.android.tv.panel = None;
        self.android.tv.seek = None;
        self.android.tv.home_recent = (!self.android.history.entries.is_empty()).then_some(0);
    }

    fn apply_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Files(files, Some(start)) => {
                self.replace_playlist();
                for f in &files {
                    self.playlist.add_path(f);
                }
                self.play_index(start);
            }
            Outcome::Files(files, None) => {
                let n: usize = files.iter().map(|f| self.playlist.add_path(f)).sum();
                self.notify_added(n, files.first().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()));
            }
            Outcome::Folder(dir) => {
                let n = self.playlist.add_path(&dir);
                self.notify_added(n, None);
            }
            Outcome::Subtitle(f) => self.load_subtitle(&f),
            Outcome::Save(f) => self.save_playlist_to(&f),
            Outcome::Network(items, Some(start)) => {
                self.replace_playlist();
                self.add_network_items(items);
                self.play_index(start);
            }
            Outcome::Network(items, None) => {
                let name = items.first().map(|i| i.name.clone());
                let n = self.add_network_items(items);
                self.notify_added(n, name);
            }
        }
    }

    fn replace_playlist(&mut self) {
        self.stop();
        self.playlist.clear();
        self.selected = None;
    }

    fn notify_added(&mut self, n: usize, name: Option<String>) {
        match (n, name) {
            (0, _) => self.notify("Нечего добавить"),
            (1, Some(name)) => self.notify(format!("В плейлисте: {name}")),
            (n, _) => self.notify(format!("Добавлено в плейлист: {n}")),
        }
    }

    /// ← / → or rewind / fast forward: moves the seek target; applied once
    /// the presses stop.
    fn tv_seek(&mut self, dir: i64, base_ms: i64, repeat: bool) {
        let Some(len) = self.player.length().filter(|_| self.player.is_seekable()) else {
            self.notify("Перемотка недоступна");
            return;
        };
        let now = self.player.time();
        let s = self.android.tv.seek.get_or_insert(SeekPreview { target: now, held: 0, last: Instant::now() });
        s.held = if repeat { s.held + 1 } else { 0 };
        let step = base_ms.max(tv::seek_step_ms(s.held));
        s.target = (s.target + dir * step).clamp(0, (len - 500).max(0));
        s.last = Instant::now();
    }

    /// Jumps to `tenths` of the video (0 is the start), as YouTube's digits do.
    fn tv_jump_to(&mut self, tenths: u8) {
        let Some(len) = self.player.length().filter(|_| self.player.is_seekable()) else { return };
        self.android.tv.seek = None;
        self.player.set_time(len * i64::from(tenths.min(9)) / 10);
        self.tv_show_bar();
    }

    fn commit_seek(&mut self) {
        if let Some(s) = self.android.tv.seek.take() {
            self.player.set_time(s.target);
        }
    }

    fn tv_player_press(&mut self, _ctx: &egui::Context, press: Press) {
        if let Nav::Digit(tenths) = press.nav {
            self.tv_jump_to(tenths);
            return;
        }
        if let Some(panel) = self.android.tv.panel {
            self.tv_panel_press(panel, press);
            return;
        }
        if !self.bar_visible() {
            match press.nav {
                Nav::Left => self.tv_seek(-1, 10_000, press.repeat),
                Nav::Right => self.tv_seek(1, 10_000, press.repeat),
                Nav::Ok => {
                    self.commit_seek();
                    self.toggle_pause();
                    self.tv_show_bar();
                }
                Nav::Up | Nav::Down | Nav::Menu => self.tv_show_bar(),
                Nav::Back => self.tv_stop(),
                _ => {}
            }
            return;
        }
        let timed = self.player.is_playing();
        self.android.tv.bar_until = Instant::now() + BAR_FOR;
        match (self.android.tv.bar_focus, press.nav) {
            (_, Nav::Back) => {
                if timed {
                    self.android.tv.bar_until = Instant::now();
                } else {
                    self.tv_stop();
                }
            }
            (BarFocus::Seek, Nav::Left) => self.tv_seek(-1, 10_000, press.repeat),
            (BarFocus::Seek, Nav::Right) => self.tv_seek(1, 10_000, press.repeat),
            (BarFocus::Seek, Nav::Ok) => {
                if self.android.tv.seek.is_some() {
                    self.commit_seek();
                } else {
                    self.toggle_pause();
                }
            }
            (BarFocus::Seek, Nav::Down) => {
                self.commit_seek();
                self.android.tv.bar_focus = BarFocus::Button(0);
            }
            (BarFocus::Button(i), Nav::Left) => self.android.tv.bar_focus = BarFocus::Button(i.saturating_sub(1)),
            (BarFocus::Button(i), Nav::Right) => self.android.tv.bar_focus = BarFocus::Button((i + 1).min(BUTTONS.len() - 1)),
            (BarFocus::Button(_), Nav::Up) => {
                if self.player.is_seekable() {
                    self.android.tv.bar_focus = BarFocus::Seek;
                }
            }
            (BarFocus::Button(i), Nav::Ok) => self.press_button(BUTTONS[i]),
            (_, Nav::Menu) => self.open_panel(Panel::Picture),
            _ => {}
        }
    }

    fn press_button(&mut self, b: Button) {
        match b {
            Button::PlayPause => self.toggle_pause(),
            Button::Prev => self.prev(),
            Button::Next => self.next(false),
            other => {
                if let Some(p) = other.panel() {
                    self.open_panel(p);
                }
            }
        }
    }

    fn open_panel(&mut self, panel: Panel) {
        self.commit_seek();
        let rows = self.panel_rows(panel);
        // Start on what is chosen now (the current track, the speed...).
        let chosen = rows.iter().position(|r| self.row_is_current(r));
        let first = rows.iter().position(|r| !matches!(r, PanelRow::Header(_))).unwrap_or(0);
        let tv = &mut self.android.tv;
        tv.panel = Some(panel);
        tv.panel_nav = ListNav { focus: chosen.unwrap_or(first), top: 0 };
    }

    fn close_panel(&mut self) {
        if let Some(p) = self.android.tv.panel.take() {
            self.android.tv.bar_focus = BarFocus::Button(p.button());
            self.android.tv.bar_until = Instant::now() + BAR_FOR;
        }
    }

    fn tv_panel_press(&mut self, panel: Panel, press: Press) {
        let rows = self.panel_rows(panel);
        let focus = self.android.tv.panel_nav.focus.min(rows.len().saturating_sub(1));
        let row = rows.get(focus).cloned();
        let delta = match press.nav {
            Nav::Up => -1,
            Nav::Down => 1,
            _ => 0,
        };
        if delta != 0 {
            // Skip the headers.
            let mut nav = self.android.tv.panel_nav;
            loop {
                if !nav.step(delta, rows.len()) {
                    break;
                }
                if !matches!(rows[nav.focus], PanelRow::Header(_)) {
                    self.android.tv.panel_nav = nav;
                    break;
                }
            }
            return;
        }
        match (press.nav, row) {
            (Nav::Left | Nav::Right, Some(PanelRow::AudioDelay)) => {
                self.audio_delay_ms += if press.nav == Nav::Left { -100 } else { 100 };
                self.player.set_audio_delay_ms(self.audio_delay_ms);
            }
            (Nav::Left | Nav::Right, Some(PanelRow::SubDelay)) => {
                self.subtitle_delay_ms += if press.nav == Nav::Left { -100 } else { 100 };
                self.player.set_subtitle_delay_ms(self.subtitle_delay_ms);
            }
            (Nav::Back | Nav::Left | Nav::Menu, _) => self.close_panel(),
            (Nav::Ok, Some(row)) => self.activate_panel_row(row),
            _ => {}
        }
    }

    fn panel_rows(&self, panel: Panel) -> Vec<PanelRow> {
        let mut rows = Vec::new();
        match panel {
            Panel::Audio => {
                let tracks = self.player.audio_tracks();
                if !tracks.is_empty() {
                    rows.push(PanelRow::Header("Аудиодорожка"));
                }
                for t in tracks {
                    let name = if t.id < 0 { "Отключить".to_owned() } else { t.name };
                    rows.push(PanelRow::AudioTrack(t.id, name));
                }
                rows.push(PanelRow::AudioDelay);
            }
            Panel::Subtitles => {
                for t in self.player.subtitle_tracks() {
                    rows.push(PanelRow::SubTrack(t.id, track_name(&t)));
                }
                rows.push(PanelRow::SubFile);
                rows.push(PanelRow::SubDelay);
            }
            Panel::Picture => {
                rows.push(PanelRow::Header("Масштаб"));
                rows.extend(Zoom::ALL.iter().map(|(z, _)| PanelRow::Zoom(*z)));
                // Black bars are found in the picture's pixels, which only the
                // CPU path has; the texture path turns HDR into SDR instead,
                // and the video layer passes HDR on to the TV.
                match &self.android.gpu {
                    None => rows.push(PanelRow::CutBars),
                    Some(g) if !g.is_layer() => rows.push(PanelRow::Hdr),
                    Some(_) => {}
                }
                rows.push(PanelRow::Output);
                rows.push(PanelRow::Header("Кадрирование"));
                rows.push(PanelRow::Crop(Crop::None));
                rows.extend(crop::PRESETS.iter().map(|(_, c)| PanelRow::Crop(*c)));
            }
            Panel::Speed => rows.extend(SPEEDS.iter().map(|s| PanelRow::Speed(*s))),
            Panel::Playlist => {
                rows.push(PanelRow::Repeat);
                rows.push(PanelRow::Shuffle);
                if !self.playlist.items.is_empty() {
                    rows.push(PanelRow::Header("Файлы"));
                }
                rows.extend((0..self.playlist.items.len()).map(PanelRow::Item));
                rows.push(PanelRow::Header("Действия"));
                rows.push(PanelRow::AddFiles);
                rows.push(PanelRow::Save);
                rows.push(PanelRow::Clear);
            }
        }
        rows
    }

    fn row_is_current(&self, row: &PanelRow) -> bool {
        match row {
            PanelRow::AudioTrack(id, _) => *id == self.player.audio_track(),
            PanelRow::SubTrack(id, _) => *id == self.player.subtitle_track(),
            PanelRow::Zoom(z) => self.android.view.zoom == *z,
            PanelRow::Speed(s) => self.rate == *s,
            PanelRow::Item(i) => self.playlist.current == Some(*i),
            _ => false,
        }
    }

    fn activate_panel_row(&mut self, row: PanelRow) {
        match row {
            PanelRow::Header(_) | PanelRow::AudioDelay | PanelRow::SubDelay => {}
            PanelRow::AudioTrack(id, name) => {
                self.player.set_audio_track(id);
                self.notify(format!("Аудио: {name}"));
            }
            PanelRow::SubTrack(id, name) => {
                self.player.set_subtitle_track(id);
                self.notify(format!("Субтитры: {name}"));
            }
            PanelRow::SubFile => {
                self.close_panel();
                self.open_subtitle_dialog();
            }
            PanelRow::Zoom(z) => self.set_zoom(z),
            PanelRow::CutBars => {
                let cut = !self.android.view.cut_bars;
                self.android.view.cut_bars = cut;
                self.android.view.save();
                self.android.bars.reset();
                self.notify(if cut { "Чёрные полосы убираются" } else { "Чёрные полосы не убираются" });
            }
            PanelRow::Hdr => {
                let next = match self.android.view.hdr_to_sdr {
                    None => Some(true),
                    Some(true) => Some(false),
                    Some(false) => None,
                };
                self.android.view.hdr_to_sdr = next;
                self.android.view.save();
                self.notify(format!("HDR → SDR: {}", self.hdr_label()));
            }
            PanelRow::Output => {
                // libVLC's output is fixed once the player exists: start afresh.
                let all = Output::ALL;
                let i = all.iter().position(|o| *o == self.android.view.output).unwrap_or(0);
                self.android.view.output = all[(i + 1) % all.len()];
                self.android.view.save();
                self.remember_position();
                self.store.save();
                self.notify(format!("Вывод видео: {}. Приложение закроется, откройте его снова", self.output_label()));
                self.android.exit_at = Some(Instant::now() + Duration::from_secs(3));
            }
            PanelRow::Crop(c) => self.set_crop(c),
            PanelRow::Speed(s) => {
                self.rate = s;
                self.player.set_rate(s);
                self.notify(format!("Скорость {}×", trim_float(s)));
            }
            PanelRow::Repeat => {
                self.playlist.repeat = match self.playlist.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                };
            }
            PanelRow::Shuffle => self.playlist.shuffle = !self.playlist.shuffle,
            PanelRow::Item(i) => {
                self.close_panel();
                self.play_index(i);
            }
            PanelRow::AddFiles => {
                self.close_panel();
                self.open_files_dialog(false);
            }
            PanelRow::Save => {
                self.close_panel();
                self.save_playlist_dialog();
            }
            PanelRow::Clear => {
                self.android.tv.panel = None;
                self.tv_stop();
                self.playlist.clear();
                self.selected = None;
            }
        }
    }

    fn home_tiles(&self) -> Vec<Tile> {
        let mut t = vec![Tile::Files, Tile::Network, Tile::Link];
        if !self.playlist.items.is_empty() {
            t.push(Tile::Playlist);
        }
        t
    }

    fn tv_home_press(&mut self, press: Press) {
        let tiles = self.home_tiles();
        let recent = self.android.history.entries.len();
        let tv = &mut self.android.tv;
        tv.home_tile = tv.home_tile.min(tiles.len() - 1);
        if tv.home_recent.is_some_and(|r| r >= recent) {
            tv.home_recent = recent.checked_sub(1);
        }
        match (tv.home_recent, press.nav) {
            (None, Nav::Left) => tv.home_tile = tv.home_tile.saturating_sub(1),
            (None, Nav::Right) => tv.home_tile = (tv.home_tile + 1).min(tiles.len() - 1),
            (None, Nav::Down) if recent > 0 => tv.home_recent = Some(0),
            (Some(0), Nav::Up) => tv.home_recent = None,
            (Some(r), Nav::Up) => tv.home_recent = Some(r - 1),
            (Some(r), Nav::Down) => tv.home_recent = Some((r + 1).min(recent - 1)),
            (_, Nav::Back) => platform::move_to_back(),
            (None, Nav::Ok) => match tiles[tv.home_tile] {
                Tile::Files => self.open_files_dialog(true),
                Tile::Network => {
                    self.open_files_dialog(true);
                    if let Some(b) = &mut self.android.browser {
                        b.show_network();
                    }
                }
                Tile::Link => self.url_dialog = Some(String::new()),
                Tile::Playlist => {
                    let i = self.selected.unwrap_or(0);
                    self.play_index(i);
                    self.open_panel(Panel::Playlist);
                }
            },
            (Some(r), Nav::Ok) => {
                if let Some(e) = self.android.history.entries.get(r) {
                    let (location, title) = (e.location.clone(), e.title.clone());
                    self.play_recent(&location, &title);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- drawing

    pub(super) fn tv_home(&mut self, ui: &mut Ui) {
        let screen = ui.max_rect();
        let p = ui.painter().clone();
        p.rect_filled(screen, 0.0, theme::BG);
        let logo = Rect::from_min_size(screen.min + vec2(40.0, 28.0), vec2(34.0, 34.0));
        self.paint_app_icon(&p, logo);
        p.text(logo.right_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, "Resumo", FontId::proportional(26.0), theme::TEXT);

        let tiles = self.home_tiles();
        let tv = &self.android.tv;
        let (w, h, gap) = (196.0, 112.0, 20.0);
        let top = screen.top() + 86.0;
        for (i, t) in tiles.iter().enumerate() {
            let r = Rect::from_min_size(pos2(screen.left() + 40.0 + i as f32 * (w + gap), top), vec2(w, h));
            let (icon, label) = match t {
                Tile::Files => (Icon::Folder, "Файлы".to_owned()),
                Tile::Network => (Icon::Network, "Сеть".to_owned()),
                Tile::Link => (Icon::Link, "Ссылка".to_owned()),
                Tile::Playlist => (Icon::Playlist, format!("Плейлист · {}", self.playlist.items.len())),
            };
            tv::tile(&p, r, icon, &label, tv.home_recent.is_none() && tv.home_tile == i);
        }

        let list_top = top + h + 30.0;
        let entries = &self.android.history.entries;
        if entries.is_empty() {
            p.text(
                pos2(screen.left() + 40.0, list_top + 10.0),
                Align2::LEFT_TOP,
                "Откройте видео с устройства, из сети или по ссылке. Здесь появятся недавние.",
                FontId::proportional(17.0),
                theme::TEXT_DIM,
            );
        } else {
            p.text(pos2(screen.left() + 44.0, list_top), Align2::LEFT_TOP, "Продолжить просмотр", FontId::proportional(17.0), theme::TEXT_DIM);
            let step = tv::ROW_H + 4.0;
            for (i, e) in entries.iter().enumerate() {
                let r = Rect::from_min_size(
                    pos2(screen.left() + 40.0, list_top + 30.0 + i as f32 * step),
                    vec2(screen.width() - 80.0, tv::ROW_H),
                );
                if r.bottom() > screen.bottom() - 40.0 {
                    break;
                }
                let seen = self.store.seen(&e.location);
                tv::seen_row(&p, r, Some((Icon::Play, theme::ACCENT)), &e.title, seen, tv.home_recent == Some(i));
            }
        }
        tv::hint(&p, screen, "OK — открыть     Назад — выйти");
    }

    pub(super) fn tv_player(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let screen = ui.max_rect();
        let p = ui.painter().clone();
        let layer = self.android.layer_picture;
        if !layer {
            p.rect_filled(screen, 0.0, Color32::BLACK);
        }
        let playing = self.player.is_playing();

        if self.video_id.is_some() || layer {
            let (w, h) = self.frame_size;
            let uv = self.eased_picture_uv(ctx);
            let aspect = (uv.width() * w) / (uv.height() * h);
            let target = self.android.view.zoom.place(screen, aspect);
            if let Some(id) = self.video_id.filter(|_| !layer) {
                let mut clipped = p.clone();
                clipped.set_clip_rect(screen);
                clipped.image(id, target, uv, Color32::WHITE);
            } else {
                self.place_layer(&p, screen, target, uv, ctx.pixels_per_point());
            }
            // libVLC lays the subtitles out over the whole picture: shown
            // where the whole picture would fit, so that zooming in doesn't
            // push them off the screen.
            if let Some(subs) = self.android.gpu_subtitles {
                let area = crop::fit(screen, w / h);
                p.image(subs, area, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        } else if let Some(t) = self.current_title() {
            // Still opening, or music.
            let text = if playing { t } else { format!("{t}\nОткрываю…") };
            p.text(screen.center(), Align2::CENTER_CENTER, text, FontId::proportional(22.0), theme::TEXT_DIM);
        }

        // The settled seek.
        if self.android.tv.seek.as_ref().is_some_and(|s| s.last.elapsed() >= SEEK_SETTLE) {
            self.commit_seek();
        }
        if self.android.tv.seek.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        if !playing && self.shows_picture() && self.android.tv.seek.is_none() {
            p.circle_filled(screen.center(), 38.0, Color32::from_black_alpha(150));
            let icon = Rect::from_center_size(screen.center() + vec2(2.0, 0.0), vec2(30.0, 30.0));
            theme::paint_icon(&p, icon, Icon::Play, Color32::WHITE);
        }

        let bar = self.bar_visible();
        let alpha = ctx.animate_bool_with_time(egui::Id::unique("tv_bar"), bar, 0.2);
        if bar && playing {
            let left = self.android.tv.bar_until.saturating_duration_since(Instant::now());
            ctx.request_repaint_after(left + Duration::from_millis(30));
        }
        if alpha > 0.01 {
            self.paint_bar(&p, screen, alpha);
        } else if let Some(s) = &self.android.tv.seek {
            self.paint_seek_osd(&p, screen, s.target);
        }
        if let Some(panel) = self.android.tv.panel {
            self.paint_panel(&p, screen, panel);
        }
    }

    /// Time, seek bar and buttons over the bottom of the video, the title on top.
    fn paint_bar(&self, p: &egui::Painter, screen: Rect, alpha: f32) {
        let shade = |r: Rect, from_top: bool, strength: f32| {
            let dark = Color32::from_black_alpha((strength * alpha) as u8);
            let (t, b) = if from_top { (dark, Color32::TRANSPARENT) } else { (Color32::TRANSPARENT, dark) };
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(r.left_top(), t);
            mesh.colored_vertex(r.right_top(), t);
            mesh.colored_vertex(r.right_bottom(), b);
            mesh.colored_vertex(r.left_bottom(), b);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            p.add(egui::Shape::mesh(mesh));
        };
        shade(Rect::from_min_max(pos2(screen.left(), screen.bottom() - 190.0), screen.max), false, 220.0);
        shade(Rect::from_min_max(screen.min, pos2(screen.right(), screen.top() + 110.0)), true, 190.0);
        let white = Color32::WHITE.gamma_multiply(alpha);
        let dim = theme::TEXT_DIM.gamma_multiply(alpha);
        // Lighter than `dim`: it sits over the picture, not a dark panel.
        let soft = Color32::from_gray(220).gamma_multiply(alpha);
        let tv = &self.android.tv;
        let length = self.player.length();
        let shown_ms = tv.seek.as_ref().map_or_else(|| self.player.time(), |s| s.target);

        // The clock, when the video ends, and where it is in the playlist.
        let clock = |minutes: i64| format!("{:02}:{:02}", minutes / 60, minutes % 60);
        let corner = screen.right_top() + vec2(-40.0, 22.0);
        let now = p.text(corner, Align2::RIGHT_TOP, clock(platform::local_minutes(0)), FontId::proportional(28.0), white);
        let mut info_left = now.left();
        if let Some(len) = length.filter(|&l| l > 0) {
            let left_ms = ((len - shown_ms).max(0) as f64 / self.rate.max(0.1) as f64) as i64;
            let ends = format!("закончится в {}", clock(platform::local_minutes(left_ms)));
            let r = p.text(pos2(corner.x, now.bottom() + 4.0), Align2::RIGHT_TOP, ends, FontId::proportional(17.0), soft);
            info_left = info_left.min(r.left());
        }
        if let Some(i) = self.playlist.current {
            let n = self.playlist.items.len();
            if n > 1 {
                let at = pos2(info_left - 28.0, now.center().y);
                let r = p.text(at, Align2::RIGHT_CENTER, format!("{} из {n}", i + 1), FontId::proportional(17.0), soft);
                info_left = r.left();
            }
        }
        // Again on the next minute, when nothing else redraws (paused).
        let into_minute = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() % 60_000);
        p.ctx().request_repaint_after(Duration::from_millis((60_000 - into_minute as u64) + 50));

        if let Some(t) = self.current_title() {
            let width = (info_left - 40.0 - (screen.left() + 40.0)).max(200.0);
            let mut job = egui::text::LayoutJob::simple(t, FontId::proportional(22.0), white, width);
            job.wrap.max_rows = 2;
            let g = p.layout_job(job);
            p.galley(screen.min + vec2(40.0, 26.0), g, white);
        }

        let playing = self.player.is_playing();
        // Seek bar.
        let frac = match (tv.seek.as_ref(), length) {
            (Some(s), Some(l)) => s.target as f32 / l as f32,
            _ => self.player.position(),
        };
        let y = screen.bottom() - 116.0;
        let track = Rect::from_min_max(pos2(screen.left() + 130.0, y - 4.0), pos2(screen.right() - 130.0, y + 4.0));
        let seek_focus = tv.bar_focus == BarFocus::Seek && tv.panel.is_none();
        let track = if seek_focus { track.expand2(vec2(0.0, 3.0)) } else { track };
        p.rect_filled(track, track.height() / 2.0, Color32::from_white_alpha((70.0 * alpha) as u8));
        let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * frac.clamp(0.0, 1.0), track.max.y));
        p.rect_filled(fill, track.height() / 2.0, theme::ACCENT.gamma_multiply(alpha));
        if seek_focus {
            p.circle_filled(pos2(fill.right(), track.center().y), 11.0, white);
            p.circle_stroke(pos2(fill.right(), track.center().y), 13.0, Stroke::new(2.0, theme::ACCENT.gamma_multiply(alpha)));
        }
        let time_font = FontId::monospace(17.0);
        p.text(pos2(track.left() - 16.0, y), Align2::RIGHT_CENTER, format_time(shown_ms), time_font.clone(), white);
        p.text(pos2(track.right() + 16.0, y), Align2::LEFT_CENTER, length.map_or("--:--".to_owned(), format_time), time_font, dim);
        if let Some(s) = &tv.seek {
            let d = s.target - self.player.time();
            let sign = if d < 0 { "−" } else { "+" };
            let x = fill.right().clamp(track.left() + 40.0, track.right() - 40.0);
            p.text(pos2(x, y - 26.0), Align2::CENTER_CENTER, format!("{sign}{}", format_time(d.abs())), FontId::proportional(17.0), white);
        }

        // Buttons.
        let size = 48.0;
        let gap = 14.0;
        let n = BUTTONS.len() as f32;
        let row_w = n * size + (n - 1.0) * gap + 30.0;
        let mut x = screen.center().x - row_w / 2.0;
        let cy = screen.bottom() - 58.0;
        for (i, b) in BUTTONS.iter().enumerate() {
            if i == 3 {
                x += 30.0; // playback apart from the settings
            }
            let r = Rect::from_center_size(pos2(x + size / 2.0, cy), vec2(size, size));
            let focused = tv.panel.is_none() && tv.bar_focus == BarFocus::Button(i);
            let on = match b {
                Button::Picture => self.crop != Crop::None || self.android.view.zoom != Zoom::Fit || self.android.view.cut_bars,
                Button::Playlist => tv.panel == Some(Panel::Playlist),
                _ => tv.panel.is_some_and(|p| b.panel() == Some(p)),
            };
            let (bg, fg) = if focused {
                (Color32::WHITE.gamma_multiply(alpha), theme::BG)
            } else {
                (Color32::from_white_alpha((26.0 * alpha) as u8), if on { theme::ACCENT.gamma_multiply(alpha) } else { white })
            };
            p.circle_filled(r.center(), size / 2.0, bg);
            let icon = b.icon(playing);
            let dx = if icon == Icon::Play { 1.5 } else { 0.0 };
            theme::paint_icon(p, Rect::from_center_size(r.center() + vec2(dx, 0.0), vec2(22.0, 22.0)), icon, fg);
            if focused {
                p.text(pos2(r.center().x, cy + size / 2.0 + 14.0), Align2::CENTER_CENTER, b.label(playing), FontId::proportional(14.0), white);
            }
            x += size + gap;
        }
    }

    /// While seeking with the bar hidden: just the time and a thin line.
    fn paint_seek_osd(&self, p: &egui::Painter, screen: Rect, target: i64) {
        let Some(len) = self.player.length() else { return };
        let line = Rect::from_min_max(pos2(screen.left(), screen.bottom() - 6.0), screen.max);
        p.rect_filled(line, 0.0, Color32::from_white_alpha(60));
        let f = (target as f32 / len as f32).clamp(0.0, 1.0);
        p.rect_filled(Rect::from_min_max(line.min, pos2(line.left() + line.width() * f, line.bottom())), 0.0, theme::ACCENT);
        let d = target - self.player.time();
        let sign = if d < 0 { "−" } else { "+" };
        let text = format!("{}   {sign}{}   / {}", format_time(target), format_time(d.abs()), format_time(len));
        let g = p.layout_no_wrap(text, FontId::proportional(20.0), Color32::WHITE);
        let bubble = Rect::from_center_size(pos2(screen.center().x, screen.bottom() - 52.0), g.size() + vec2(36.0, 18.0));
        p.rect_filled(bubble, 12.0, Color32::from_black_alpha(170));
        p.galley(bubble.min + vec2(18.0, 9.0), g, Color32::WHITE);
    }

    fn paint_panel(&mut self, p: &egui::Painter, screen: Rect, panel: Panel) {
        let rect = Rect::from_min_max(pos2(screen.right() - 400.0, screen.top()), screen.max);
        p.rect_filled(rect, 0.0, theme::SURFACE);
        p.text(rect.left_top() + vec2(28.0, 28.0), Align2::LEFT_TOP, panel.title(), FontId::proportional(24.0), theme::TEXT);
        let rows = self.panel_rows(panel);
        let area = Rect::from_min_max(rect.left_top() + vec2(16.0, 80.0), rect.right_bottom() - vec2(16.0, 48.0));
        let step = tv::ROW_H + 4.0;
        let visible = ((area.height() + 4.0) / step).floor().max(1.0) as usize;
        let mut nav = self.android.tv.panel_nav;
        nav.clamp(rows.len(), visible);
        // A header right above the first visible row comes along with it.
        if nav.top > 0 && matches!(rows[nav.top - 1], PanelRow::Header(_)) && nav.focus < nav.top + visible - 1 {
            nav.top -= 1;
        }
        self.android.tv.panel_nav = nav;
        self.android.tv.panel_rows_visible = visible;
        let end = (nav.top + visible).min(rows.len());
        for (slot, i) in (nav.top..end).enumerate() {
            let r = Rect::from_min_size(pos2(area.left(), area.top() + slot as f32 * step), vec2(area.width(), tv::ROW_H));
            self.paint_panel_row(p, r, &rows[i], nav.focus == i);
        }
        tv::scroll_hints(p, area, nav.top > 0, end < rows.len());
        let hint = match rows.get(nav.focus) {
            Some(PanelRow::AudioDelay | PanelRow::SubDelay) => "Влево и вправо — изменить     Назад — закрыть",
            _ => "OK — выбрать     Назад — закрыть",
        };
        p.text(pos2(rect.left() + 28.0, rect.bottom() - 22.0), Align2::LEFT_CENTER, hint, FontId::proportional(14.0), theme::TEXT_DIM);
    }

    fn paint_panel_row(&self, p: &egui::Painter, r: Rect, row: &PanelRow, focused: bool) {
        let current = self.row_is_current(row);
        let delay = |ms: i64| if ms == 0 { "0 мс".to_owned() } else { format!("{ms:+} мс") };
        match row {
            PanelRow::Header(t) => {
                p.text(pos2(r.left() + 12.0, r.bottom() - 10.0), Align2::LEFT_BOTTOM, *t, FontId::proportional(15.0), theme::TEXT_DIM);
            }
            PanelRow::AudioTrack(_, name) | PanelRow::SubTrack(_, name) => {
                tv::row(p, r, None, name, Trail::Check(current), focused, false)
            }
            PanelRow::AudioDelay => {
                tv::row(p, r, None, "Задержка звука", Trail::Adjust(&delay(self.audio_delay_ms)), focused, false)
            }
            PanelRow::SubDelay => {
                tv::row(p, r, None, "Задержка субтитров", Trail::Adjust(&delay(self.subtitle_delay_ms)), focused, false)
            }
            PanelRow::SubFile => tv::row(p, r, Some((Icon::Folder, theme::ACCENT)), "Загрузить из файла", Trail::None, focused, false),
            PanelRow::Zoom(z) => tv::row(p, r, None, z.label(), Trail::Check(current), focused, false),
            PanelRow::CutBars => tv::row(p, r, None, "Убирать чёрные полосы", Trail::Switch(self.android.view.cut_bars), focused, false),
            PanelRow::Hdr => tv::row(p, r, None, "HDR → SDR", Trail::Value(&self.hdr_label()), focused, false),
            PanelRow::Output => tv::row(p, r, None, "Вывод видео", Trail::Value(&self.output_label()), focused, false),
            PanelRow::Crop(c) => {
                let label = if *c == Crop::None { "Без кадрирования".to_owned() } else { c.label() };
                tv::row(p, r, None, &label, Trail::Check(self.crop == *c), focused, false)
            }
            PanelRow::Speed(s) => {
                let label = if *s == 1.0 { "Обычная".to_owned() } else { format!("{}×", trim_float(*s)) };
                tv::row(p, r, None, &label, Trail::Check(current), focused, false)
            }
            PanelRow::Repeat => {
                let (icon, v) = match self.playlist.repeat {
                    Repeat::Off => (Icon::Repeat, "выкл"),
                    Repeat::All => (Icon::Repeat, "весь список"),
                    Repeat::One => (Icon::RepeatOne, "один файл"),
                };
                tv::row(p, r, Some((icon, theme::TEXT)), "Повтор", Trail::Value(v), focused, false)
            }
            PanelRow::Shuffle => {
                tv::row(p, r, Some((Icon::Shuffle, theme::TEXT)), "Перемешать", Trail::Switch(self.playlist.shuffle), focused, false)
            }
            PanelRow::Item(i) => {
                let Some(item) = self.playlist.items.get(*i) else { return };
                let d = item.duration.map(format_time);
                let trail = d.as_deref().map_or(Trail::None, Trail::Value);
                let icon = if current { Some((Icon::Play, theme::ACCENT)) } else { None };
                tv::row(p, r, icon, &item.title, trail, focused, current)
            }
            PanelRow::AddFiles => tv::row(p, r, Some((Icon::Plus, theme::ACCENT)), "Добавить файлы", Trail::None, focused, false),
            PanelRow::Save => tv::row(p, r, Some((Icon::Playlist, theme::ACCENT)), "Сохранить как M3U", Trail::None, focused, false),
            PanelRow::Clear => tv::row(p, r, Some((Icon::Close, theme::TEXT_DIM)), "Очистить плейлист", Trail::None, focused, false),
        }
    }

    /// Short message (track switched, file added...) over everything.
    pub(super) fn tv_status(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        let Some((text, at)) = &self.status else { return };
        let age = at.elapsed().as_secs_f32();
        if age >= 2.5 {
            self.status = None;
            return;
        }
        let alpha = (1.0 - (age - 2.0).max(0.0) * 2.0).clamp(0.0, 1.0);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("tv_status")));
        let galley = p.layout_no_wrap(text.clone(), FontId::proportional(18.0), Color32::WHITE);
        let over_video = self.is_active() && self.android.browser.is_none();
        let top = if over_video && self.bar_visible() { 96.0 } else { 24.0 };
        let left = if over_video { 40.0 } else { screen.width() - 40.0 - galley.size().x - 32.0 };
        let bubble = Rect::from_min_size(screen.min + vec2(left, top), galley.size() + vec2(32.0, 16.0));
        p.rect_filled(bubble, 10.0, Color32::from_black_alpha((190.0 * alpha) as u8));
        p.galley(bubble.min + vec2(16.0, 8.0), galley, Color32::WHITE.gamma_multiply(alpha));
        ctx.request_repaint();
    }

    /// The browser over the whole screen while it is open.
    pub(super) fn tv_browser(&mut self, ctx: &egui::Context) {
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
        let has_access = self.android.storage_ok;
        let rect = ctx.content_rect();
        egui::Area::new(egui::Id::unique("file_browser")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.set_min_size(rect.size());
            if let Some(b) = &mut self.android.browser {
                b.show(ui, rect, has_access, &self.store);
            }
        });
    }

    /// The ring around the widget egui's focus is on, in the text dialogs.
    /// Whether HDR video is being turned into SDR, as the panel says it.
    /// Shows the video layer under the window: the whole picture placed so
    /// that its `uv` part fills `target`, with the window left clear over
    /// `target` only and black around it.
    pub(super) fn place_layer(&mut self, p: &egui::Painter, screen: Rect, target: Rect, uv: Rect, ppp: f32) {
        let shown = target.intersect(screen);
        for bar in [
            Rect::from_min_max(screen.min, pos2(screen.max.x, shown.min.y)),
            Rect::from_min_max(pos2(screen.min.x, shown.max.y), screen.max),
            Rect::from_min_max(pos2(screen.min.x, shown.min.y), pos2(shown.min.x, shown.max.y)),
            Rect::from_min_max(pos2(shown.max.x, shown.min.y), pos2(screen.max.x, shown.max.y)),
        ] {
            if bar.is_positive() {
                p.rect_filled(bar, 0.0, Color32::BLACK);
            }
        }
        let size = target.size() / uv.size();
        let min = target.min - uv.min.to_vec2() * size;
        let px = |v: f32| (v * ppp).round() as i32;
        if let Some(g) = &mut self.android.gpu {
            g.place_layer([px(min.x), px(min.y), px(size.x), px(size.y)]);
        }
    }

    fn output_label(&self) -> String {
        match self.android.view.output {
            Output::Layer => "Видеослой (HDR)",
            Output::Texture => "Видеокарта",
            Output::Cpu => "Процессор",
        }
        .into()
    }

    fn hdr_label(&self) -> String {
        match self.android.view.hdr_to_sdr {
            Some(true) => "Вкл".into(),
            Some(false) => "Выкл".into(),
            None => format!("Авто ({})", if self.android.hdr == Some(true) { "вкл" } else { "выкл" }),
        }
    }

    /// Whether the current video looks like HDR10: 10-bit HEVC, VP9 or AV1
    /// at 4K-ish sizes (SDR 10-bit encodes are mostly smaller).
    fn looks_hdr(&self) -> bool {
        let Some(v) = self.player.video_info() else { return false };
        let codec = matches!(&v.codec, b"hevc" | b"VP90" | b"av01");
        let height = v.height.max(self.frame_size.1 as u32);
        codec && if v.profile == 2 { height >= 1440 } else { height >= 2000 }
    }

    /// Takes the decoder's newest frames into the GPU textures (see
    /// [`crate::surface`]).
    pub(super) fn tv_gpu_frame(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(at) = self.android.exit_at {
            if Instant::now() >= at {
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
            if self.video_id.is_some() && self.texture.is_none() {
                self.video_id = None;
            }
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
        self.android.layer_picture = gpu.is_layer() && size.is_some();
        if shown.reattached {
            self.player.restart_video();
        }
        if let Some((w, h)) = size.filter(|_| self.android.layer_picture) {
            self.frame_size = (w as f32, h as f32);
        }
        self.android.gpu_picture = shown.picture;
        self.android.gpu_subtitles = shown.subtitles;
        if let Some((id, s)) = shown.picture {
            self.video_id = Some(id);
            self.frame_size = s;
        }
        // No callback says a frame is ready: look for one every frame.
        if self.player.is_playing() {
            ctx.request_repaint();
        }
    }

    /// Opens the on-screen keyboard whenever the focus moves onto a text
    /// field, and draws it.
    pub(super) fn tv_keyboard(&mut self, ctx: &egui::Context) {
        let now = ctx.text_edit_focused().then(|| ctx.memory(|m| m.focused())).flatten();
        if now.is_some() && now != self.android.tv.text_focus {
            self.android.tv.osk.show();
        }
        if now.is_none() {
            self.android.tv.osk.open = false;
        }
        self.android.tv.text_focus = now;
        self.android.tv.osk.paint(ctx);
    }

    pub(super) fn paint_egui_focus(&self, ctx: &egui::Context) {
        if !self.tv_typing() {
            return;
        }
        let Some(r) = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)) else { return };
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::unique("tv_focus")));
        p.rect_stroke(r.rect.expand(3.0), 8.0, Stroke::new(2.5, tv::FOCUS), egui::StrokeKind::Outside);
    }

    /// The default egui widgets (dialogs) a size that reads on a TV.
    pub(super) fn tv_style(ctx: &egui::Context) {
        ctx.style_mut_of(egui::Theme::Dark, |s| {
            for font in s.text_styles.values_mut() {
                font.size *= 1.25;
            }
            s.spacing.button_padding = vec2(14.0, 8.0);
            s.spacing.interact_size.y = 34.0;
        });
    }
}
