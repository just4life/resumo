//! A touch-friendly file browser drawn with egui, used instead of the system
//! file dialogs on Android (where `rfd` has nothing to show). When opening
//! media it also browses the local network (see `lan.rs`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{vec2, Align, Color32, FontId, Layout, Rect, RichText, Sense, Ui};

use crate::lan::{self, Lan, ListState, Listing, NetItem};
use crate::playlist::{self, has_extension};
use crate::theme::{self, Icon};
use crate::vlc::Instance;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    /// Pick media or playlist files; `play` starts the first one.
    Open { play: bool },
    /// Add a whole folder to the playlist.
    Folder,
    /// Load a subtitle file into the current video.
    Subtitle,
    /// Choose where to save the playlist as M3U.
    SaveM3u,
}

pub enum Outcome {
    Files(Vec<PathBuf>, bool),
    Folder(PathBuf),
    Subtitle(PathBuf),
    Save(PathBuf),
    /// Files on the network, and whether to play the first.
    Network(Vec<NetItem>, bool),
}

struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

pub struct Browser {
    pub purpose: Purpose,
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Picking several files (after "Выделить" or a long press); otherwise a
    /// tap on a file opens it straight away.
    selecting: bool,
    selected: BTreeSet<PathBuf>,
    file_name: String,
    error: Option<String>,
    roots: Vec<(String, PathBuf)>,
    /// libVLC, for the network section (only when opening media).
    inst: Option<Instance>,
    /// Discovery keeps running while the browser is open, so switching back
    /// to "Сеть" shows what was already found.
    lan: Option<Lan>,
    net: Option<NetView>,
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
    selected: BTreeSet<String>,
    address: String,
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
            selected: BTreeSet::new(),
            address: "smb://".to_owned(),
        }
    }

    fn open(&mut self, item: NetItem) {
        self.stack.push(item);
        self.load();
    }

    fn load(&mut self) {
        self.listing = self.stack.last().map(|f| self.lan.open(&f.mrl));
        self.items.clear();
        self.selected.clear();
        self.state = NetState::Loading;
        self.loaded_at = None;
    }

    /// One level up; false at the server list.
    fn up(&mut self) -> bool {
        if self.stack.pop().is_none() {
            return false;
        }
        if self.stack.is_empty() {
            self.listing = None;
            self.items.clear();
            self.selected.clear();
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
}

impl Browser {
    pub fn new(purpose: Purpose, dir: PathBuf, roots: Vec<(String, PathBuf)>, inst: &Instance) -> Self {
        let mut b = Self {
            purpose,
            dir: PathBuf::new(),
            entries: Vec::new(),
            selecting: false,
            selected: BTreeSet::new(),
            file_name: "playlist.m3u".to_owned(),
            error: None,
            roots,
            inst: matches!(purpose, Purpose::Open { .. }).then(|| inst.clone()),
            lan: None,
            net: None,
        };
        b.enter(dir);
        b
    }

    /// Whether the network section is showing (discovery needs multicast).
    pub fn on_network(&self) -> bool {
        self.net.is_some()
    }

    fn enter_network(&mut self) {
        let Some(inst) = &self.inst else { return };
        let lan = self.lan.get_or_insert_with(|| Lan::start(inst)).clone();
        self.net = Some(NetView::new(lan));
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Re-reads the folder (after storage access was granted).
    pub fn refresh(&mut self) {
        let d = self.dir.clone();
        self.enter(d);
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
        self.selected.clear();
        self.selecting = false;
    }

    /// "Back": stops picking files, or goes one folder up; returns false at a
    /// storage root.
    pub fn go_up(&mut self) -> bool {
        if self.selecting {
            self.selecting = false;
            self.selected.clear();
            if let Some(net) = &mut self.net {
                net.selected.clear();
            }
            return true;
        }
        if let Some(net) = &mut self.net {
            return net.up();
        }
        if self.roots.iter().any(|(_, r)| *r == self.dir) {
            return false;
        }
        match self.dir.parent() {
            Some(p) if p != Path::new("/") && p != Path::new("/storage") && p != Path::new("/storage/emulated") => {
                let p = p.to_path_buf();
                self.enter(p);
                true
            }
            _ => false,
        }
    }

    fn title(&self) -> &'static str {
        match self.purpose {
            Purpose::Open { play: true } => "Открыть файлы",
            Purpose::Open { play: false } => "Добавить файлы",
            Purpose::Folder => "Добавить папку",
            Purpose::Subtitle => "Загрузить субтитры",
            Purpose::SaveM3u => "Сохранить плейлист",
        }
    }

    /// Draws the browser over `rect`. `Some(None)` means "closed without a
    /// choice", `Some(Some(_))` is the choice, `None` keeps it open.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, has_access: bool, ask_access: &mut bool) -> Option<Option<Outcome>> {
        let mut result = None;
        ui.painter().rect_filled(rect, 0.0, theme::BG);
        // Swallow touches so nothing behind the browser reacts.
        ui.interact(rect, ui.scope_id().with("browser_bg"), Sense::click_and_drag());
        let inner = rect.shrink2(vec2(12.0, 8.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)), |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                if theme::icon_button(ui, Icon::Close, "Закрыть", false).clicked() {
                    result = Some(None);
                }
                ui.label(RichText::new(self.title()).strong().size(17.0).color(theme::TEXT));
            });
            ui.horizontal_wrapped(|ui| {
                let roots = self.roots.clone();
                for (name, path) in roots {
                    let on = self.net.is_none() && self.dir.starts_with(&path);
                    if ui.add(egui::Button::selectable(on, name).min_size(vec2(0.0, 34.0))).clicked() {
                        self.net = None;
                        self.enter(path);
                    }
                }
                if self.inst.is_some()
                    && ui.add(egui::Button::selectable(self.net.is_some(), "Сеть").min_size(vec2(0.0, 34.0))).clicked()
                    && self.net.is_none()
                {
                    self.enter_network();
                }
            });
            ui.horizontal(|ui| {
                let can_up = match &self.net {
                    Some(net) => !net.stack.is_empty(),
                    None => !self.roots.iter().any(|(_, r)| *r == self.dir),
                };
                if ui.add_enabled(can_up, egui::Button::new("‹ Вверх").min_size(vec2(72.0, 34.0))).clicked() {
                    self.go_up();
                }
                let path = match &self.net {
                    Some(net) => net.path_label(),
                    None => self.dir.to_string_lossy().into_owned(),
                };
                ui.add(egui::Label::new(RichText::new(path).color(theme::TEXT_DIM).size(12.5)).truncate());
            });

            if let Some(net) = &mut self.net {
                let play = matches!(self.purpose, Purpose::Open { play: true });
                result = network_section(ui, net, play, &mut self.selecting);
                return;
            }

            if !has_access {
                egui::Frame::new().fill(theme::SURFACE_HI).corner_radius(10).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Нет доступа к файлам").strong().color(theme::TEXT));
                    ui.label(
                        RichText::new("Чтобы видеть видео и субтитры в папках, разрешите VPlayer доступ ко всем файлам.")
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(4.0);
                    if theme::pill(ui, None, "Разрешить доступ").clicked() {
                        *ask_access = true;
                    }
                });
            }

            // Bottom action bar first, so the list gets the rest of the height.
            let bar_h = if self.purpose == Purpose::SaveM3u { 96.0 } else { 52.0 };
            let list_h = (ui.available_height() - bar_h).max(60.0);
            let mut chosen_file: Option<PathBuf> = None;
            let mut long_pressed = false;
            let mut enter: Option<PathBuf> = None;
            let picking = self.selecting && matches!(self.purpose, Purpose::Open { .. });
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                if let Some(e) = &self.error {
                    ui.add_space(12.0);
                    ui.label(RichText::new(e).color(theme::TEXT_DIM));
                } else if self.entries.is_empty() {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Здесь нет подходящих файлов").color(theme::TEXT_DIM));
                }
                for e in &self.entries {
                    let (icon, color) = if e.is_dir { (Icon::Folder, theme::ACCENT) } else { file_icon(&e.path) };
                    let mark = (picking && !e.is_dir).then(|| self.selected.contains(&e.path));
                    let row = entry_row(ui, &e.name, icon, color, mark);
                    if row.clicked() {
                        if e.is_dir {
                            enter = Some(e.path.clone());
                        } else {
                            chosen_file = Some(e.path.clone());
                        }
                    } else if row.long_touched() && !e.is_dir {
                        chosen_file = Some(e.path.clone());
                        long_pressed = true;
                    }
                }
            });
            if let Some(d) = enter {
                self.enter(d);
            }
            if let Some(f) = chosen_file {
                match self.purpose {
                    Purpose::Open { play } => {
                        if long_pressed {
                            self.selecting = true;
                        }
                        if !self.selecting {
                            result = Some(Some(Outcome::Files(vec![f], play)));
                        } else if !self.selected.remove(&f) {
                            self.selected.insert(f);
                        }
                    }
                    Purpose::Subtitle => result = Some(Some(Outcome::Subtitle(f))),
                    Purpose::SaveM3u => {
                        self.file_name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    }
                    Purpose::Folder => {}
                }
            }

            ui.add_space(4.0);
            match self.purpose {
                Purpose::Open { play } => {
                    ui.horizontal(|ui| {
                        let any_files = self.entries.iter().any(|e| !e.is_dir);
                        if select_button(ui, &mut self.selecting, self.selected.len(), any_files) {
                            let files: Vec<PathBuf> = self.selected.iter().cloned().collect();
                            result = Some(Some(Outcome::Files(files, play)));
                        }
                        let any_media = self.entries.iter().any(|e| !e.is_dir && has_extension(&e.path, playlist::MEDIA_EXTENSIONS));
                        if ui.add_enabled(any_media, egui::Button::new("Вся папка").min_size(vec2(110.0, 40.0))).clicked() {
                            result = Some(Some(Outcome::Files(vec![self.dir.clone()], play)));
                        }
                    });
                }
                Purpose::Folder => {
                    if ui.add(egui::Button::new("Добавить эту папку").min_size(vec2(180.0, 40.0))).clicked() {
                        result = Some(Some(Outcome::Folder(self.dir.clone())));
                    }
                }
                Purpose::Subtitle => {
                    ui.label(RichText::new("Нажмите на файл субтитров").color(theme::TEXT_DIM));
                }
                Purpose::SaveM3u => {
                    ui.horizontal(|ui| {
                        ui.label("Имя файла");
                        ui.add(egui::TextEdit::singleline(&mut self.file_name).desired_width(ui.available_width()));
                    });
                    let name = self.file_name.trim();
                    if ui.add_enabled(!name.is_empty(), egui::Button::new("Сохранить сюда").min_size(vec2(180.0, 40.0))).clicked() {
                        let mut f = self.dir.join(name);
                        if !has_extension(&f, playlist::PLAYLIST_EXTENSIONS) {
                            f.set_extension("m3u");
                        }
                        result = Some(Some(Outcome::Save(f)));
                    }
                }
            }
        });
        result
    }
}

/// The section of the browser showing the network: servers and shares, or
/// one of their folders, with the same selection and "open" bar as files.
fn network_section(ui: &mut Ui, net: &mut NetView, play: bool, selecting: &mut bool) -> Option<Option<Outcome>> {
    net.tick();
    ui.ctx().request_repaint_after(NET_REPOLL);
    let mut result = None;
    let list_h = (ui.available_height() - 52.0).max(60.0);
    let mut open: Option<NetItem> = None;
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
        if net.stack.is_empty() {
            ui.label(RichText::new("Адрес вручную").color(theme::TEXT_DIM).size(12.5));
            ui.horizontal(|ui| {
                let w = ui.available_width() - 100.0;
                let r = ui.add(egui::TextEdit::singleline(&mut net.address).desired_width(w).hint_text("smb://192.168.1.10/Видео"));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let addr = net.address.trim().to_owned();
                let valid = addr.contains("://") && !addr.ends_with("://");
                if (ui.add_enabled(valid, egui::Button::new("Открыть").min_size(vec2(88.0, 34.0))).clicked() || enter) && valid {
                    open = Some(NetItem { name: lan::name_from_mrl(&addr), mrl: addr, is_dir: true });
                }
            });
            ui.add_space(8.0);
            if net.servers.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Ищу медиасерверы и общие папки…").color(theme::TEXT_DIM));
                });
                ui.label(
                    RichText::new("Телефон должен быть в той же сети Wi-Fi, что и компьютер или NAS.")
                        .color(theme::TEXT_DIM)
                        .size(12.5),
                );
            }
            for s in &net.servers {
                if entry_row(ui, &s.name, Icon::Folder, theme::ACCENT, None).clicked() {
                    open = Some(s.clone());
                }
            }
            let saved = lan::saved_logins();
            if !saved.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new("Сохранённые пароли").color(theme::TEXT_DIM).size(12.5));
                for server in saved {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&server).color(theme::TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new("Забыть").min_size(vec2(88.0, 34.0))).clicked() {
                                lan::forget_login(&server);
                            }
                        });
                    });
                }
            }
            return;
        }
        match net.state {
            NetState::Loading => {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Загрузка…").color(theme::TEXT_DIM));
                });
            }
            NetState::Failed => {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Не удалось открыть. Проверьте адрес, а для общей папки — имя пользователя и пароль.")
                        .color(theme::TEXT_DIM),
                );
            }
            NetState::Ready if net.items.is_empty() => {
                ui.add_space(12.0);
                ui.label(RichText::new("Здесь нет видео").color(theme::TEXT_DIM));
            }
            NetState::Ready => {}
        }
        let mut chosen: Option<NetItem> = None;
        for item in &net.items {
            let (icon, color) = if item.is_dir {
                (Icon::Folder, theme::ACCENT)
            } else {
                file_icon(Path::new(&item.name))
            };
            let mark = (*selecting && !item.is_dir).then(|| net.selected.contains(&item.mrl));
            let row = entry_row(ui, &item.name, icon, color, mark);
            if row.clicked() {
                if item.is_dir {
                    open = Some(item.clone());
                } else {
                    chosen = Some(item.clone());
                }
            } else if row.long_touched() && !item.is_dir {
                *selecting = true;
                chosen = Some(item.clone());
            }
        }
        if let Some(item) = chosen {
            if !*selecting {
                result = Some(Some(Outcome::Network(vec![item], play)));
            } else if !net.selected.remove(&item.mrl) {
                net.selected.insert(item.mrl);
            }
        }
    });
    if let Some(item) = open {
        *selecting = false;
        net.open(item);
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let any_files = net.items.iter().any(|i| !i.is_dir);
        if select_button(ui, selecting, net.selected.len(), any_files) {
            let files = net.items.iter().filter(|i| net.selected.contains(&i.mrl)).cloned().collect();
            result = Some(Some(Outcome::Network(files, play)));
        }
        let files: Vec<NetItem> = net.items.iter().filter(|i| !i.is_dir).cloned().collect();
        if ui.add_enabled(!files.is_empty(), egui::Button::new("Вся папка").min_size(vec2(110.0, 40.0))).clicked() {
            result = Some(Some(Outcome::Network(files, play)));
        }
    });
    result
}

/// The bar's left button. "Выделить" starts picking several files; while
/// picking it reads "Открыть (n)", or "Отмена" (stop picking) with none ticked.
/// Returns true when the picked files should be opened.
fn select_button(ui: &mut Ui, selecting: &mut bool, n: usize, has_files: bool) -> bool {
    let (label, enabled) = match (*selecting, n) {
        (false, _) => ("Выделить".to_owned(), has_files),
        (true, 0) => ("Отмена".to_owned(), true),
        (true, n) => (format!("Открыть ({n})"), true),
    };
    if !ui.add_enabled(enabled, egui::Button::new(label).min_size(vec2(150.0, 40.0))).clicked() {
        return false;
    }
    match (*selecting, n) {
        (false, _) => *selecting = true,
        (true, 0) => *selecting = false,
        _ => return true,
    }
    false
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

/// A file or folder row. `mark` is the tick circle shown while picking
/// files: `Some(true)` ticked, `Some(false)` empty.
fn entry_row(ui: &mut Ui, name: &str, icon: Icon, color: Color32, mark: Option<bool>) -> egui::Response {
    let selected = mark == Some(true);
    let height = 48.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let p = ui.painter_at(rect);
    let bg = if selected {
        theme::ACCENT.gamma_multiply(0.22)
    } else if resp.is_pointer_button_down_on() {
        theme::SURFACE_HI
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 8.0, bg);
    let icon_rect = Rect::from_center_size(rect.left_center() + vec2(20.0, 0.0), vec2(20.0, 20.0));
    theme::paint_icon(&p, icon_rect, icon, color);
    let width = rect.width() - 90.0;
    let galley = ui.painter().layout(name.to_owned(), FontId::proportional(14.5), theme::TEXT, width);
    let y = rect.center().y - galley.size().y.min(height - 6.0) / 2.0;
    p.galley(egui::pos2(rect.left() + 44.0, y), galley, theme::TEXT);
    if mark == Some(false) {
        p.circle_stroke(rect.right_center() - vec2(20.0, 0.0), 10.0, egui::Stroke::new(1.5, theme::TEXT_DIM));
    }
    if selected {
        let c = rect.right_center() - vec2(20.0, 0.0);
        p.circle_filled(c, 11.0, theme::ACCENT);
        let tick = vec![c + vec2(-5.0, 0.0), c + vec2(-1.5, 4.0), c + vec2(5.0, -4.0)];
        p.add(egui::Shape::line(tick, egui::Stroke::new(2.2, Color32::WHITE)));
    }
    resp
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
