//! The file browser for Android TV, driven by the remote: sources on the
//! left (device storage, USB drives, the local network), the folder on the
//! right. OK on a video plays its folder from that video on. Used instead of
//! the system file dialogs, which Android TV mostly doesn't have. When
//! opening media it also browses the local network (see `lan.rs`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, RichText, Ui};

use crate::lan::{self, Lan, ListState, Listing, NetItem};
use crate::playlist::{self, has_extension};
use crate::store::Store;
use crate::theme::{self, Icon};
use crate::tv::{self, ListNav, Nav, Press, Trail};
use crate::vlc::Instance;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    /// Pick media or playlist files; `play` replaces the playlist with the
    /// folder and plays the chosen file, otherwise the file is added.
    Open { play: bool },
    /// Add a whole folder to the playlist.
    Folder,
    /// Load a subtitle file into the current video.
    Subtitle,
    /// Choose where to save the playlist as M3U.
    SaveM3u,
}

pub enum Outcome {
    /// Files to add; with `Some(i)` the playlist is replaced and file `i` plays.
    Files(Vec<PathBuf>, Option<usize>),
    Folder(PathBuf),
    Subtitle(PathBuf),
    Save(PathBuf),
    /// Files on the network, the same way as `Files`.
    Network(Vec<NetItem>, Option<usize>),
}

/// What the app should do after a key press in the browser.
pub enum Action {
    None,
    Close,
    /// Do this and close the browser.
    Done(Outcome),
    /// Do this and keep browsing (adding files one by one).
    Keep(Outcome),
    AskAccess,
}

struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

/// A text field on top of the browser.
#[derive(Clone, Copy, PartialEq)]
enum Editing {
    Address,
    FileName,
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Sources,
    List,
}

/// A row of the list on the right.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Access,
    /// Play (or add) every video in the folder.
    WholeFolder,
    FileName,
    SaveHere,
    Entry(usize),
    Address,
    Server(usize),
    NetEntry(usize),
}

pub struct Browser {
    pub purpose: Purpose,
    dir: PathBuf,
    entries: Vec<Entry>,
    file_name: String,
    error: Option<String>,
    roots: Vec<(String, PathBuf)>,
    /// libVLC, for the network section (only when opening media).
    inst: Option<Instance>,
    /// Discovery keeps running while the browser is open, so switching back
    /// to "Сеть" shows what was already found.
    lan: Option<Lan>,
    net: Option<NetView>,
    zone: Zone,
    sources: ListNav,
    list: ListNav,
    editing: Option<(Editing, String)>,
    /// Rows that fit on screen, from the last frame.
    visible_rows: usize,
}

/// The "Сеть" section: servers found on the network, or one of their folders.
struct NetView {
    lan: Lan,
    /// Folders opened from the server list, outermost first.
    stack: Vec<NetItem>,
    listing: Option<Listing>,
    items: Vec<NetItem>,
    state: NetState,
    /// When the folder finished loading; libVLC can still add entries for a
    /// moment after that, so it is re-read for a few seconds.
    loaded_at: Option<Instant>,
    polled_at: Instant,
    servers: Vec<NetItem>,
    address: String,
    /// After going up: the folder we came from, to put the focus back on it.
    came_from: Option<String>,
}

#[derive(PartialEq)]
enum NetState {
    Loading,
    Ready,
    Failed,
}

const NET_REPOLL: Duration = Duration::from_millis(400);

impl NetView {
    fn new(lan: Lan) -> Self {
        let servers = lan.servers();
        Self {
            lan,
            stack: Vec::new(),
            listing: None,
            items: Vec::new(),
            state: NetState::Ready,
            loaded_at: None,
            polled_at: Instant::now(),
            servers,
            address: "smb://".to_owned(),
            came_from: None,
        }
    }

    fn open(&mut self, item: NetItem) {
        self.stack.push(item);
        self.came_from = None;
        self.load();
    }

    fn load(&mut self) {
        self.listing = self.stack.last().map(|f| self.lan.open(&f.mrl));
        self.items.clear();
        self.state = NetState::Loading;
        self.loaded_at = None;
    }

    /// One level up; false at the server list.
    fn up(&mut self) -> bool {
        let Some(left) = self.stack.pop() else { return false };
        self.came_from = Some(left.mrl);
        if self.stack.is_empty() {
            self.listing = None;
            self.items.clear();
            self.state = NetState::Ready;
        } else {
            self.load();
        }
        true
    }

    fn tick(&mut self) {
        if self.polled_at.elapsed() < NET_REPOLL && self.state != NetState::Loading {
            return;
        }
        self.polled_at = Instant::now();
        let Some(listing) = &self.listing else {
            self.servers = self.lan.servers();
            return;
        };
        if self.loaded_at.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
            return;
        }
        match listing.poll() {
            ListState::Loading => {}
            ListState::Done(items) => {
                let mut items: Vec<NetItem> = items.into_iter().filter(lan::is_openable).collect();
                items.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name))));
                self.items = items;
                self.state = NetState::Ready;
                self.loaded_at.get_or_insert_with(Instant::now);
            }
            ListState::Failed => {
                if self.state == NetState::Loading {
                    self.state = NetState::Failed;
                }
            }
        }
    }

    fn path_label(&self) -> String {
        std::iter::once("Сеть").chain(self.stack.iter().map(|f| f.name.as_str())).collect::<Vec<_>>().join(" › ")
    }

    fn files(&self) -> Vec<NetItem> {
        self.items.iter().filter(|i| !i.is_dir).cloned().collect()
    }
}

impl Browser {
    pub fn new(purpose: Purpose, dir: PathBuf, roots: Vec<(String, PathBuf)>, inst: &Instance) -> Self {
        let mut b = Self {
            purpose,
            dir: PathBuf::new(),
            entries: Vec::new(),
            file_name: "playlist.m3u".to_owned(),
            error: None,
            roots,
            inst: matches!(purpose, Purpose::Open { .. }).then(|| inst.clone()),
            lan: None,
            net: None,
            zone: Zone::List,
            sources: ListNav::default(),
            list: ListNav::default(),
            editing: None,
            visible_rows: 8,
        };
        b.enter(dir);
        b
    }

    /// Opens straight on the network section.
    pub fn show_network(&mut self) {
        self.enter_network();
        self.sources.focus = self.roots.len();
    }

    /// Whether the network section is showing (discovery needs multicast).
    pub fn on_network(&self) -> bool {
        self.net.is_some()
    }

    /// A text field is open (the remote's arrows then belong to it).
    pub fn wants_text(&self) -> bool {
        self.editing.is_some()
    }

    fn enter_network(&mut self) {
        let Some(inst) = &self.inst else { return };
        let lan = self.lan.get_or_insert_with(|| Lan::start(inst)).clone();
        self.net = Some(NetView::new(lan));
        self.list = ListNav::default();
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Re-reads the folder (after storage access was granted).
    pub fn refresh(&mut self) {
        let d = self.dir.clone();
        let focus = self.list.focus;
        self.enter(d);
        self.list.focus = focus;
    }

    fn enter(&mut self, dir: PathBuf) {
        let wanted: &[&str] = match self.purpose {
            Purpose::Open { .. } => &[],
            Purpose::Folder => &[],
            Purpose::Subtitle => playlist::SUBTITLE_EXTENSIONS,
            Purpose::SaveM3u => playlist::PLAYLIST_EXTENSIONS,
        };
        let show_file = |p: &Path| match self.purpose {
            Purpose::Open { .. } => {
                has_extension(p, playlist::MEDIA_EXTENSIONS) || has_extension(p, playlist::PLAYLIST_EXTENSIONS)
            }
            Purpose::Folder => has_extension(p, playlist::MEDIA_EXTENSIONS),
            _ => has_extension(p, wanted),
        };
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                let mut entries: Vec<Entry> = rd
                    .flatten()
                    .filter_map(|e| {
                        let path = e.path();
                        let name = e.file_name().to_string_lossy().into_owned();
                        if name.starts_with('.') {
                            return None;
                        }
                        let is_dir = path.is_dir();
                        (is_dir || show_file(&path)).then_some(Entry { path, name, is_dir })
                    })
                    .collect();
                entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name))));
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(if e.kind() == std::io::ErrorKind::PermissionDenied {
                    "Нет доступа к этой папке".to_owned()
                } else {
                    format!("Не удалось открыть папку: {e}")
                });
            }
        }
        self.dir = dir;
        self.list = ListNav::default();
    }

    /// "Back": goes one folder up; returns false at a storage root.
    pub fn go_up(&mut self) -> bool {
        if let Some(net) = &mut self.net {
            return net.up();
        }
        if self.roots.iter().any(|(_, r)| *r == self.dir) {
            return false;
        }
        match self.dir.parent() {
            Some(p) if p != Path::new("/") && p != Path::new("/storage") && p != Path::new("/storage/emulated") => {
                let (p, child) = (p.to_path_buf(), self.dir.clone());
                self.enter(p);
                // Back on the folder we just left.
                let rows = self.rows(true);
                if let Some(i) = rows.iter().position(|r| matches!(r, Row::Entry(e) if self.entries[*e].path == child)) {
                    self.list.focus = i;
                }
                true
            }
            _ => false,
        }
    }

    fn title(&self) -> &'static str {
        match self.purpose {
            Purpose::Open { play: true } => "Открыть видео",
            Purpose::Open { play: false } => "Добавить в плейлист",
            Purpose::Folder => "Добавить папку",
            Purpose::Subtitle => "Загрузить субтитры",
            Purpose::SaveM3u => "Сохранить плейлист",
        }
    }

    /// The sources on the left: storage roots, then the network.
    fn source_names(&self) -> Vec<(String, Icon)> {
        let mut v: Vec<(String, Icon)> = self.roots.iter().map(|(n, _)| (n.clone(), Icon::Folder)).collect();
        if self.inst.is_some() {
            v.push(("Сеть".to_owned(), Icon::Network));
        }
        v
    }

    fn current_source(&self) -> usize {
        if self.net.is_some() {
            return self.roots.len();
        }
        self.roots.iter().position(|(_, r)| self.dir.starts_with(r)).unwrap_or(0)
    }

    fn media_files(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|e| !e.is_dir && has_extension(&e.path, playlist::MEDIA_EXTENSIONS))
            .map(|e| e.path.clone())
            .collect()
    }

    fn rows(&self, has_access: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(net) = &self.net {
            if net.stack.is_empty() {
                rows.push(Row::Address);
                rows.extend((0..net.servers.len()).map(Row::Server));
            } else {
                if net.items.iter().any(|i| !i.is_dir) {
                    rows.push(Row::WholeFolder);
                }
                rows.extend((0..net.items.len()).map(Row::NetEntry));
            }
            return rows;
        }
        if !has_access {
            rows.push(Row::Access);
        }
        match self.purpose {
            Purpose::Open { .. } if !self.media_files().is_empty() => rows.push(Row::WholeFolder),
            Purpose::Folder => rows.push(Row::WholeFolder),
            Purpose::SaveM3u => {
                rows.push(Row::FileName);
                rows.push(Row::SaveHere);
            }
            _ => {}
        }
        rows.extend((0..self.entries.len()).map(Row::Entry));
        rows
    }

    /// Handles one remote press.
    pub fn handle(&mut self, press: Press, has_access: bool) -> Action {
        if self.editing.is_some() {
            if press.nav == Nav::Back {
                self.editing = None;
            }
            return Action::None;
        }
        match self.zone {
            Zone::Sources => self.handle_sources(press),
            Zone::List => self.handle_list(press, has_access),
        }
    }

    fn handle_sources(&mut self, press: Press) -> Action {
        let n = self.source_names().len();
        match press.nav {
            Nav::Up => {
                self.sources.step(-1, n);
            }
            Nav::Down => {
                self.sources.step(1, n);
            }
            Nav::Ok | Nav::Right => {
                let i = self.sources.focus;
                if i != self.current_source() {
                    if let Some((_, path)) = self.roots.get(i).cloned() {
                        self.net = None;
                        self.enter(path);
                    } else {
                        self.enter_network();
                    }
                }
                self.zone = Zone::List;
            }
            Nav::Back => return Action::Close,
            _ => {}
        }
        Action::None
    }

    fn handle_list(&mut self, press: Press, has_access: bool) -> Action {
        let rows = self.rows(has_access);
        self.list.clamp(rows.len(), self.visible_rows);
        // An empty folder has nothing to be on: the arrows go to the sources.
        if rows.is_empty() && matches!(press.nav, Nav::Up | Nav::Down | Nav::Ok) {
            self.zone = Zone::Sources;
            self.sources.focus = self.current_source();
            if press.nav != Nav::Ok {
                return self.handle_sources(press);
            }
            return Action::None;
        }
        match press.nav {
            Nav::Up => {
                self.list.step(-1, rows.len());
            }
            Nav::Down => {
                self.list.step(1, rows.len());
            }
            Nav::Left => {
                self.zone = Zone::Sources;
                self.sources.focus = self.current_source();
            }
            Nav::Back => {
                if !self.go_up() {
                    return Action::Close;
                }
            }
            Nav::Ok => return rows.get(self.list.focus).map_or(Action::None, |r| self.activate(*r, false)),
            // Menu on a video adds it to the playlist without leaving.
            Nav::Menu => return rows.get(self.list.focus).map_or(Action::None, |r| self.activate(*r, true)),
            _ => {}
        }
        Action::None
    }

    fn activate(&mut self, row: Row, add_only: bool) -> Action {
        let play = matches!(self.purpose, Purpose::Open { play: true }) && !add_only;
        match row {
            Row::Access => return Action::AskAccess,
            Row::Address => {
                let addr = self.net.as_ref().map_or_else(String::new, |n| n.address.clone());
                self.editing = Some((Editing::Address, addr));
            }
            Row::FileName => self.editing = Some((Editing::FileName, self.file_name.clone())),
            Row::SaveHere => {
                let name = self.file_name.trim();
                if !name.is_empty() {
                    let mut f = self.dir.join(name);
                    if !has_extension(&f, playlist::PLAYLIST_EXTENSIONS) {
                        f.set_extension("m3u");
                    }
                    return Action::Done(Outcome::Save(f));
                }
            }
            Row::WholeFolder => {
                if let Some(net) = &self.net {
                    return Action::Done(Outcome::Network(net.files(), play.then_some(0)));
                }
                return match self.purpose {
                    Purpose::Folder => Action::Done(Outcome::Folder(self.dir.clone())),
                    _ => Action::Done(Outcome::Files(self.media_files(), play.then_some(0))),
                };
            }
            Row::Entry(i) => {
                let Some(e) = self.entries.get(i) else { return Action::None };
                if e.is_dir {
                    let p = e.path.clone();
                    self.enter(p);
                    return Action::None;
                }
                let f = e.path.clone();
                return match self.purpose {
                    Purpose::Subtitle => Action::Done(Outcome::Subtitle(f)),
                    Purpose::SaveM3u => {
                        self.file_name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        Action::None
                    }
                    Purpose::Folder => Action::None,
                    Purpose::Open { .. } if !play => Action::Keep(Outcome::Files(vec![f], None)),
                    Purpose::Open { .. } if has_extension(&f, playlist::PLAYLIST_EXTENSIONS) => {
                        Action::Done(Outcome::Files(vec![f], Some(0)))
                    }
                    Purpose::Open { .. } => {
                        let files = self.media_files();
                        let start = files.iter().position(|p| *p == f).unwrap_or(0);
                        Action::Done(Outcome::Files(files, Some(start)))
                    }
                };
            }
            Row::Server(i) => {
                if let Some(net) = &mut self.net {
                    if let Some(s) = net.servers.get(i).cloned() {
                        net.open(s);
                        self.list = ListNav::default();
                    }
                }
            }
            Row::NetEntry(i) => {
                let Some(net) = &mut self.net else { return Action::None };
                let Some(item) = net.items.get(i).cloned() else { return Action::None };
                if item.is_dir {
                    net.open(item);
                    self.list = ListNav::default();
                    return Action::None;
                }
                if !play {
                    return Action::Keep(Outcome::Network(vec![item], None));
                }
                let files = net.files();
                let start = files.iter().position(|f| f.mrl == item.mrl).unwrap_or(0);
                return Action::Done(Outcome::Network(files, Some(start)));
            }
        }
        Action::None
    }

    /// Draws the browser over `rect`; `store` says which videos were seen.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, has_access: bool, store: &Store) {
        if let Some(net) = &mut self.net {
            net.tick();
            ui.ctx().request_repaint_after(NET_REPOLL);
            // Back on the folder we came from, once the list has loaded.
            if net.state == NetState::Ready {
                if let Some(mrl) = net.came_from.take() {
                    let at = if net.stack.is_empty() {
                        net.servers.iter().position(|s| s.mrl == mrl).map(|i| i + 1)
                    } else {
                        let whole = usize::from(net.items.iter().any(|i| !i.is_dir));
                        net.items.iter().position(|s| s.mrl == mrl).map(|i| i + whole)
                    };
                    if let Some(i) = at {
                        self.list.focus = i;
                    }
                }
            }
        }
        let p = ui.painter().clone();
        p.rect_filled(rect, 0.0, theme::BG);
        let pad = vec2(40.0, 26.0);
        p.text(rect.min + pad, Align2::LEFT_TOP, self.title(), FontId::proportional(26.0), theme::TEXT);
        let path = match &self.net {
            Some(net) => net.path_label(),
            None => self.dir.to_string_lossy().into_owned(),
        };
        let mut job = egui::text::LayoutJob::simple(path, FontId::proportional(15.0), theme::TEXT_DIM, rect.width() - 120.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = p.layout_job(job);
        p.galley(rect.min + pad + vec2(0.0, 38.0), galley, theme::TEXT_DIM);

        let top = rect.top() + 100.0;
        let bottom = rect.bottom() - 48.0;
        let step = tv::ROW_H + 4.0;

        // Sources.
        let side = Rect::from_min_max(pos2(rect.left() + 32.0, top), pos2(rect.left() + 252.0, bottom));
        let current = self.current_source();
        for (i, (name, icon)) in self.source_names().into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(side.left(), side.top() + i as f32 * step), vec2(side.width(), tv::ROW_H));
            let focused = self.zone == Zone::Sources && self.sources.focus == i && self.editing.is_none();
            tv::row(&p, r, Some((icon, theme::ACCENT)), &name, Trail::None, focused, i == current);
        }

        // The folder.
        let list = Rect::from_min_max(pos2(side.right() + 24.0, top), pos2(rect.right() - 40.0, bottom));
        let rows = self.rows(has_access);
        self.visible_rows = ((list.height() + 4.0) / step).floor().max(1.0) as usize;
        self.list.clamp(rows.len(), self.visible_rows);
        let end = (self.list.top + self.visible_rows).min(rows.len());
        for (slot, i) in (self.list.top..end).enumerate() {
            let r = Rect::from_min_size(pos2(list.left(), list.top() + slot as f32 * step), vec2(list.width(), tv::ROW_H));
            let focused = self.zone == Zone::List && self.list.focus == i && self.editing.is_none();
            self.paint_row(&p, r, rows[i], focused, store);
        }
        tv::scroll_hints(&p, list, self.list.top > 0, end < rows.len());
        let below = list.top() + (end - self.list.top) as f32 * step + 14.0;
        if let Some(msg) = self.status_message(has_access) {
            let spinner = self.net.as_ref().is_some_and(|n| n.state == NetState::Loading || n.stack.is_empty());
            let mut x = list.left() + 16.0;
            if spinner {
                let c = pos2(x + 9.0, below + 10.0);
                let t = ui.input(|i| i.time) as f32;
                let pts: Vec<_> = (0..=10).map(|k| c + egui::Vec2::angled(t * 5.0 + k as f32 * 0.45) * 8.0).collect();
                p.add(egui::Shape::line(pts, egui::Stroke::new(2.5, theme::ACCENT)));
                ui.ctx().request_repaint();
                x += 30.0;
            }
            let mut job = egui::text::LayoutJob::simple(msg, FontId::proportional(16.0), theme::TEXT_DIM, list.right() - x);
            job.wrap.max_rows = 3;
            let g = p.layout_job(job);
            p.galley(pos2(x, below), g, theme::TEXT_DIM);
        }

        let hint = match (self.zone, self.purpose) {
            (Zone::Sources, _) => "OK — открыть     Вправо — к списку     Назад — закрыть",
            (Zone::List, Purpose::Open { play: true }) => {
                "OK — смотреть     Меню — добавить в плейлист     Влево — источники     Назад — вверх"
            }
            (Zone::List, Purpose::Open { play: false }) => "OK — добавить в плейлист     Влево — источники     Назад — вверх",
            (Zone::List, _) => "OK — выбрать     Влево — источники     Назад — вверх",
        };
        tv::hint(&p, rect, hint);
        self.text_window(ui.ctx());
    }

    fn paint_row(&self, p: &egui::Painter, r: Rect, row: Row, focused: bool, store: &Store) {
        let folder = (Icon::Folder, theme::ACCENT);
        match row {
            Row::Access => {
                tv::row(p, r, Some((Icon::Plus, theme::ACCENT)), "Разрешить доступ к файлам", Trail::None, focused, false)
            }
            Row::WholeFolder => {
                let label = match self.purpose {
                    Purpose::Open { play: true } => "Смотреть всю папку",
                    _ => "Добавить всю папку",
                };
                tv::row(p, r, Some((Icon::Playlist, theme::ACCENT)), label, Trail::None, focused, false)
            }
            Row::FileName => tv::row(p, r, None, "Имя файла", Trail::Value(&self.file_name), focused, false),
            Row::SaveHere => {
                tv::row(p, r, Some((Icon::Plus, theme::ACCENT)), "Сохранить в эту папку", Trail::None, focused, false)
            }
            Row::Entry(i) => {
                let e = &self.entries[i];
                let icon = if e.is_dir { folder } else { file_icon(&e.path) };
                if e.is_dir {
                    tv::row(p, r, Some(icon), &e.name, Trail::None, focused, false)
                } else {
                    tv::seen_row(p, r, Some(icon), &e.name, store.seen(&e.path.to_string_lossy()), focused)
                }
            }
            Row::Address => tv::row(
                p,
                r,
                Some((Icon::Link, theme::ACCENT)),
                "Ввести адрес вручную",
                Trail::Text("smb://…"),
                focused,
                false,
            ),
            Row::Server(i) => {
                let Some(net) = &self.net else { return };
                tv::row(p, r, Some((Icon::Network, theme::ACCENT)), &net.servers[i].name, Trail::None, focused, false)
            }
            Row::NetEntry(i) => {
                let Some(net) = &self.net else { return };
                let item = &net.items[i];
                let icon = if item.is_dir { folder } else { file_icon(Path::new(&item.name)) };
                if item.is_dir {
                    tv::row(p, r, Some(icon), &item.name, Trail::None, focused, false)
                } else {
                    // The same location the playlist keeps (`Playlist::add_location`).
                    tv::seen_row(p, r, Some(icon), &item.name, store.seen(item.mrl.trim()), focused)
                }
            }
        }
    }

    fn status_message(&self, has_access: bool) -> Option<String> {
        if let Some(net) = &self.net {
            if net.stack.is_empty() {
                return net.servers.is_empty().then(|| {
                    "Ищу медиасерверы и общие папки… Телевизор должен быть в той же сети, что и компьютер или NAS."
                        .to_owned()
                });
            }
            return match net.state {
                NetState::Loading => Some("Загрузка…".to_owned()),
                NetState::Failed => Some(
                    "Не удалось открыть. Проверьте адрес, а для общей папки — имя пользователя и пароль.".to_owned(),
                ),
                NetState::Ready if net.items.is_empty() => Some("Здесь нет видео".to_owned()),
                NetState::Ready => None,
            };
        }
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        if !has_access {
            return Some("Без доступа к файлам Android не покажет видео и субтитры в папках.".to_owned());
        }
        self.entries.is_empty().then(|| "Здесь нет подходящих файлов".to_owned())
    }

    /// The address or file name being typed, with the TV's on-screen keyboard.
    fn text_window(&mut self, ctx: &egui::Context) {
        let Some((kind, mut text)) = self.editing.take() else { return };
        let title = match kind {
            Editing::Address => "Адрес сетевой папки",
            Editing::FileName => "Имя файла плейлиста",
        };
        let mut done = false;
        let mut cancel = false;
        egui::Window::new(title)
            .id(egui::Id::unique("browser_text"))
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Tooltip)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 40.0))
            .show(ctx, |ui| {
                if kind == Editing::Address {
                    ui.label(RichText::new("Например smb://192.168.1.10/Видео").color(theme::TEXT_DIM));
                }
                let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(460.0).font(FontId::proportional(18.0)));
                if ctx.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let label = if kind == Editing::Address { "Открыть" } else { "Готово" };
                    if ui.add(egui::Button::new(label).min_size(vec2(120.0, 36.0))).clicked() {
                        done = true;
                    }
                    if ui.add(egui::Button::new("Отмена").min_size(vec2(120.0, 36.0))).clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            return;
        }
        if !done {
            self.editing = Some((kind, text));
            return;
        }
        let text = text.trim().to_owned();
        match kind {
            Editing::FileName => {
                if !text.is_empty() {
                    self.file_name = text;
                }
            }
            Editing::Address => {
                let valid = text.contains("://") && !text.ends_with("://");
                if let Some(net) = &mut self.net {
                    net.address = text.clone();
                    if valid {
                        net.open(NetItem { name: lan::name_from_mrl(&text), mrl: text, is_dir: true });
                        self.list = ListNav::default();
                    }
                }
            }
        }
    }
}

fn file_icon(path: &Path) -> (Icon, Color32) {
    if has_extension(path, playlist::SUBTITLE_EXTENSIONS) {
        (Icon::Subtitles, theme::TEXT_DIM)
    } else if has_extension(path, playlist::PLAYLIST_EXTENSIONS) {
        (Icon::Playlist, theme::TEXT_DIM)
    } else {
        (Icon::Play, theme::TEXT_DIM)
    }
}

/// Sort key that puts "Episode 2" before "Episode 10".
fn natural_key(name: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = 0u64;
            while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n = n.saturating_mul(10).saturating_add(d as u64 - '0' as u64);
                chars.next();
            }
            out.push((0, n, String::new()));
        } else {
            let mut s = String::new();
            while let Some(&d) = chars.peek().filter(|d| !d.is_ascii_digit()) {
                s.extend(d.to_lowercase());
                chars.next();
            }
            out.push((1, 0, s));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::natural_key;

    #[test]
    fn natural_sort() {
        let mut v = vec!["Ep 10.mkv", "ep 2.mkv", "Ep 1.mkv"];
        v.sort_by_key(|s| natural_key(s));
        assert_eq!(v, ["Ep 1.mkv", "ep 2.mkv", "Ep 10.mkv"]);
    }
}
