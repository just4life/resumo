//! Playing from the local network through libVLC: media servers (UPnP/DLNA)
//! and shared folders (SMB) are found by libVLC's discovery modules, a folder
//! is listed by letting libVLC parse its address, and the login libVLC asks
//! for when a share has a password is shown as a dialog in the player.
//! Logins are remembered per server in `network-logins.tsv` in the app's
//! private folder and handed to libVLC as options (`:smb-user=…`) next time.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::rc::Rc;
use std::sync::Mutex;

use egui::{vec2, Align2, RichText};

use crate::theme;
use crate::vlc::{libvlc_instance_t, libvlc_media_t, Instance};

#[repr(C)]
pub struct libvlc_media_discoverer_t {
    _p: [u8; 0],
}
#[repr(C)]
pub struct libvlc_media_list_t {
    _p: [u8; 0],
}
#[repr(C)]
pub struct libvlc_dialog_id {
    _p: [u8; 0],
}

#[repr(C)]
struct libvlc_media_discoverer_description_t {
    psz_name: *mut c_char,
    psz_longname: *mut c_char,
    i_cat: c_int,
}

type Text = *const c_char;
type Id = *mut libvlc_dialog_id;

#[repr(C)]
struct libvlc_dialog_cbs {
    pf_display_error: Option<unsafe extern "C" fn(*mut c_void, Text, Text)>,
    pf_display_login: Option<unsafe extern "C" fn(*mut c_void, Id, Text, Text, Text, bool)>,
    pf_display_question: Option<unsafe extern "C" fn(*mut c_void, Id, Text, Text, c_int, Text, Text, Text)>,
    pf_display_progress: Option<unsafe extern "C" fn(*mut c_void, Id, Text, Text, bool, f32, Text)>,
    pf_cancel: Option<unsafe extern "C" fn(*mut c_void, Id)>,
    pf_update_progress: Option<unsafe extern "C" fn(*mut c_void, Id, f32, Text)>,
}

const CATEGORY_LAN: c_int = 1;
const PARSE_NETWORK: c_int = 0x01;
const DO_INTERACT: c_int = 0x08;
const PARSED_DONE: c_int = 4;
const TYPE_DIRECTORY: c_int = 2;
const META_TITLE: c_int = 0;
/// No time limit on listing a folder: the time includes waiting for the
/// user to type a password, and an unreachable server fails on its own.
const LIST_TIMEOUT_MS: c_int = 0;

#[cfg_attr(windows, link(name = "libvlc"))]
#[cfg_attr(not(windows), link(name = "vlc"))]
extern "C" {
    fn libvlc_free(ptr: *mut c_void);

    fn libvlc_media_discoverer_list_get(
        inst: *mut libvlc_instance_t,
        cat: c_int,
        services: *mut *mut *mut libvlc_media_discoverer_description_t,
    ) -> usize;
    fn libvlc_media_discoverer_list_release(services: *mut *mut libvlc_media_discoverer_description_t, count: usize);
    fn libvlc_media_discoverer_new(inst: *mut libvlc_instance_t, name: *const c_char) -> *mut libvlc_media_discoverer_t;
    fn libvlc_media_discoverer_start(md: *mut libvlc_media_discoverer_t) -> c_int;
    fn libvlc_media_discoverer_stop(md: *mut libvlc_media_discoverer_t);
    fn libvlc_media_discoverer_release(md: *mut libvlc_media_discoverer_t);
    fn libvlc_media_discoverer_media_list(md: *mut libvlc_media_discoverer_t) -> *mut libvlc_media_list_t;

    fn libvlc_media_list_release(ml: *mut libvlc_media_list_t);
    fn libvlc_media_list_lock(ml: *mut libvlc_media_list_t);
    fn libvlc_media_list_unlock(ml: *mut libvlc_media_list_t);
    fn libvlc_media_list_count(ml: *mut libvlc_media_list_t) -> c_int;
    fn libvlc_media_list_item_at_index(ml: *mut libvlc_media_list_t, i: c_int) -> *mut libvlc_media_t;

    fn libvlc_media_new_location(inst: *mut libvlc_instance_t, mrl: *const c_char) -> *mut libvlc_media_t;
    fn libvlc_media_release(md: *mut libvlc_media_t);
    fn libvlc_media_add_option(md: *mut libvlc_media_t, option: *const c_char);
    fn libvlc_media_get_mrl(md: *mut libvlc_media_t) -> *mut c_char;
    fn libvlc_media_get_meta(md: *mut libvlc_media_t, meta: c_int) -> *mut c_char;
    fn libvlc_media_get_type(md: *mut libvlc_media_t) -> c_int;
    fn libvlc_media_parse_with_options(md: *mut libvlc_media_t, flags: c_int, timeout_ms: c_int) -> c_int;
    fn libvlc_media_parse_stop(md: *mut libvlc_media_t);
    fn libvlc_media_get_parsed_status(md: *mut libvlc_media_t) -> c_int;
    fn libvlc_media_subitems(md: *mut libvlc_media_t) -> *mut libvlc_media_list_t;

    fn libvlc_dialog_set_callbacks(inst: *mut libvlc_instance_t, cbs: *const libvlc_dialog_cbs, data: *mut c_void);
    fn libvlc_dialog_post_login(id: Id, user: *const c_char, pass: *const c_char, store: bool) -> c_int;
    fn libvlc_dialog_post_action(id: Id, action: c_int) -> c_int;
    fn libvlc_dialog_dismiss(id: Id) -> c_int;
}

/// A server, share, folder or file on the network.
#[derive(Clone, Debug)]
pub struct NetItem {
    pub mrl: String,
    pub name: String,
    pub is_dir: bool,
}

/// Running network discovery. Cheap to clone; discovery stops when the last
/// clone is dropped.
#[derive(Clone)]
pub struct Lan(Rc<Discovery>);

struct Discovery {
    inst: Instance,
    discoverers: Vec<*mut libvlc_media_discoverer_t>,
}

impl Lan {
    /// Starts every LAN discovery module libVLC has (UPnP, SMB/NetBIOS, mDNS…).
    pub fn start(inst: &Instance) -> Self {
        let inst = inst.clone();
        let mut discoverers = Vec::new();
        unsafe {
            let mut list = std::ptr::null_mut();
            let n = libvlc_media_discoverer_list_get(inst.raw, CATEGORY_LAN, &mut list);
            for i in 0..n {
                let desc = &**list.add(i);
                let md = libvlc_media_discoverer_new(inst.raw, desc.psz_name);
                if md.is_null() {
                    continue;
                }
                if libvlc_media_discoverer_start(md) == 0 {
                    discoverers.push(md);
                } else {
                    libvlc_media_discoverer_release(md);
                }
            }
            if n > 0 {
                libvlc_media_discoverer_list_release(list, n);
            }
        }
        Self(Rc::new(Discovery { inst, discoverers }))
    }

    /// Servers and shares found so far.
    pub fn servers(&self) -> Vec<NetItem> {
        let mut out: Vec<NetItem> = Vec::new();
        for &md in &self.0.discoverers {
            unsafe {
                let ml = libvlc_media_discoverer_media_list(md);
                if ml.is_null() {
                    continue;
                }
                for mut item in list_items(ml) {
                    item.is_dir = true;
                    if !out.iter().any(|o| o.mrl == item.mrl) {
                        out.push(item);
                    }
                }
                libvlc_media_list_release(ml);
            }
        }
        out.sort_by_key(|i| i.name.to_lowercase());
        out
    }

    /// Starts listing a server, share or folder.
    pub fn open(&self, mrl: &str) -> Listing {
        let md = CString::new(mrl).map_or(std::ptr::null_mut(), |c| unsafe {
            libvlc_media_new_location(self.0.inst.raw, c.as_ptr())
        });
        if !md.is_null() {
            unsafe {
                for o in media_options(mrl) {
                    if let Ok(o) = CString::new(o) {
                        libvlc_media_add_option(md, o.as_ptr());
                    }
                }
                libvlc_media_parse_with_options(md, PARSE_NETWORK | DO_INTERACT, LIST_TIMEOUT_MS);
            }
        }
        Listing { md }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        for &md in &self.discoverers {
            unsafe {
                libvlc_media_discoverer_stop(md);
                libvlc_media_discoverer_release(md);
            }
        }
    }
}

/// A folder being listed in the background.
pub struct Listing {
    md: *mut libvlc_media_t,
}

pub enum ListState {
    Loading,
    Done(Vec<NetItem>),
    Failed,
}

impl Listing {
    pub fn poll(&self) -> ListState {
        if self.md.is_null() {
            return ListState::Failed;
        }
        unsafe {
            match libvlc_media_get_parsed_status(self.md) {
                0 => ListState::Loading,
                PARSED_DONE => {
                    let ml = libvlc_media_subitems(self.md);
                    if ml.is_null() {
                        return ListState::Done(Vec::new());
                    }
                    let items = list_items(ml);
                    libvlc_media_list_release(ml);
                    ListState::Done(items)
                }
                _ => ListState::Failed,
            }
        }
    }
}

impl Drop for Listing {
    fn drop(&mut self) {
        if !self.md.is_null() {
            unsafe {
                libvlc_media_parse_stop(self.md);
                libvlc_media_release(self.md);
            }
        }
    }
}

unsafe fn list_items(ml: *mut libvlc_media_list_t) -> Vec<NetItem> {
    let mut out = Vec::new();
    libvlc_media_list_lock(ml);
    for i in 0..libvlc_media_list_count(ml) {
        let md = libvlc_media_list_item_at_index(ml, i);
        if md.is_null() {
            continue;
        }
        if let Some(mrl) = take_string(libvlc_media_get_mrl(md)) {
            let name = take_string(libvlc_media_get_meta(md, META_TITLE))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| name_from_mrl(&mrl));
            out.push(NetItem { mrl, name, is_dir: libvlc_media_get_type(md) == TYPE_DIRECTORY });
        }
        libvlc_media_release(md);
    }
    libvlc_media_list_unlock(ml);
    out
}

/// Copies and frees a string libVLC allocated.
unsafe fn take_string(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    libvlc_free(p.cast());
    Some(s)
}

/// "smb://nas/Movies/My%20Film.mkv" → "My Film.mkv".
pub fn name_from_mrl(mrl: &str) -> String {
    let last = mrl.trim_end_matches('/').rsplit('/').next().unwrap_or(mrl);
    let last = if last.is_empty() { mrl } else { last };
    percent_decode(last)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ dialogs

/// A question libVLC asks while opening something on the network.
pub enum Dialog {
    /// `retry`: asked again right after a login, so the last one was wrong.
    Login { id: usize, retry: bool, user: String, pass: String },
    Question { id: usize, title: String, text: String, cancel: String, actions: Vec<(c_int, String)> },
}

impl Dialog {
    fn id(&self) -> usize {
        match self {
            Dialog::Login { id, .. } | Dialog::Question { id, .. } => *id,
        }
    }
}

struct Pending {
    queue: Vec<Dialog>,
    cancelled: Vec<usize>,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending { queue: Vec::new(), cancelled: Vec::new() });
static LAST_LOGIN: Mutex<Option<std::time::Instant>> = Mutex::new(None);

static CALLBACKS: libvlc_dialog_cbs = libvlc_dialog_cbs {
    pf_display_error: Some(on_error),
    pf_display_login: Some(on_login),
    pf_display_question: Some(on_question),
    pf_display_progress: None,
    pf_cancel: Some(on_cancel),
    pf_update_progress: None,
};

unsafe fn text(p: Text) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn on_error(_: *mut c_void, _title: Text, _text: Text) {
    // Failures show up where they happen (the folder list, the player), in Russian.
}

unsafe extern "C" fn on_login(_: *mut c_void, id: Id, _title: Text, _msg: Text, user: Text, _store: bool) {
    // libVLC's own text is English; the dialog says it in Russian instead.
    // Asked again right after a login, or although a saved login was passed
    // along: that login was wrong.
    let saved = OPENING.lock().unwrap().as_deref().and_then(saved_login);
    let retry = saved.is_some() || LAST_LOGIN.lock().unwrap().is_some_and(|t| t.elapsed().as_secs() < 30);
    let user = saved.map(|l| l.user).unwrap_or_else(|| text(user));
    let d = Dialog::Login { id: id as usize, retry, user, pass: String::new() };
    PENDING.lock().unwrap().queue.push(d);
}

unsafe extern "C" fn on_question(
    _: *mut c_void,
    id: Id,
    title: Text,
    msg: Text,
    _kind: c_int,
    cancel: Text,
    action1: Text,
    action2: Text,
) {
    let actions = [(1, action1), (2, action2)]
        .into_iter()
        .filter(|(_, a)| !a.is_null())
        .map(|(n, a)| (n, text(a)))
        .collect();
    let d = Dialog::Question { id: id as usize, title: text(title), text: text(msg), cancel: text(cancel), actions };
    PENDING.lock().unwrap().queue.push(d);
}

unsafe extern "C" fn on_cancel(_: *mut c_void, id: Id) {
    // libVLC holds its dialog lock here, so dismiss from the UI thread instead.
    PENDING.lock().unwrap().cancelled.push(id as usize);
}

/// Lets libVLC ask for a login and password (and the rare yes/no question)
/// through [`dialog_window`].
pub fn install_dialogs(inst: &Instance) {
    unsafe { libvlc_dialog_set_callbacks(inst.raw, &CALLBACKS, std::ptr::null_mut()) };
}

/// Shows libVLC's pending login or question, one at a time. `current` holds
/// the one on screen between frames.
/// `cancel` (Back on the remote) answers "Cancel".
pub fn dialog_window(ctx: &egui::Context, current: &mut Option<Dialog>, cancel: bool) {
    {
        let mut p = PENDING.lock().unwrap();
        for id in std::mem::take(&mut p.cancelled) {
            p.queue.retain(|d| d.id() != id);
            if current.as_ref().is_some_and(|d| d.id() == id) {
                *current = None;
            }
            unsafe { libvlc_dialog_dismiss(id as Id) };
        }
        if current.is_none() && !p.queue.is_empty() {
            *current = Some(p.queue.remove(0));
        }
    }
    let Some(dialog) = current else { return };
    let dialog_id = dialog.id();
    ctx.request_repaint_after(std::time::Duration::from_millis(300));
    let width = (ctx.content_rect().width() - 80.0).min(380.0);
    let mut answer: Option<Option<c_int>> = None; // Some(None) = cancel, Some(Some(0)) = login, n = action
    if cancel {
        answer = Some(None);
    }
    let title = match dialog {
        Dialog::Login { .. } => "Вход в сетевую папку".to_owned(),
        Dialog::Question { title, .. } => title.clone(),
    };
    egui::Window::new(title)
        .id(egui::Id::unique("lan_dialog"))
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Tooltip)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 24.0))
        .show(ctx, |ui| {
            ui.set_width(width);
            match dialog {
                Dialog::Login { retry, user, pass, .. } => {
                    let note = if *retry {
                        "Имя пользователя или пароль не подошли. Попробуйте ещё раз."
                    } else {
                        "Папка защищена паролем. Введите имя пользователя и пароль от компьютера или NAS."
                    };
                    ui.label(RichText::new(note).color(theme::TEXT_DIM));
                    ui.label("Пользователь");
                    let u = ui.add(egui::TextEdit::singleline(user).desired_width(width));
                    // The remote starts on the user name (and the keyboard opens),
                    // whatever had the focus before the dialog came up.
                    let shown = egui::Id::unique("lan_dialog_focused");
                    let this = dialog_id as u64;
                    if ctx.data(|d| d.get_temp::<u64>(shown)) != Some(this) || ctx.memory(|m| m.focused().is_none()) {
                        ctx.data_mut(|d| d.insert_temp(shown, this));
                        u.request_focus();
                    }
                    ui.label("Пароль");
                    let r = ui.add(egui::TextEdit::singleline(pass).password(true).desired_width(width));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if theme::pill(ui, None, "Войти").clicked() || enter {
                            answer = Some(Some(0));
                        }
                        if ui.button("Отмена").clicked() {
                            answer = Some(None);
                        }
                    });
                }
                Dialog::Question { text, cancel, actions, .. } => {
                    ui.label(text.as_str());
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for (n, label) in actions.iter() {
                            if ui.button(label.as_str()).clicked() {
                                answer = Some(Some(*n));
                            }
                        }
                        let cancel = if cancel.is_empty() { "Отмена" } else { cancel.as_str() };
                        if ui.button(cancel).clicked() {
                            answer = Some(None);
                        }
                    });
                }
            }
        });
    let Some(answer) = answer else { return };
    let dialog = current.take().unwrap();
    let id = dialog.id() as Id;
    unsafe {
        match (dialog, answer) {
            (Dialog::Login { user, pass, .. }, Some(0)) => {
                let u = CString::new(user).unwrap_or_default();
                let p = CString::new(pass).unwrap_or_default();
                *LAST_LOGIN.lock().unwrap() = Some(std::time::Instant::now());
                libvlc_dialog_post_login(id, u.as_ptr(), p.as_ptr(), false);
                let opening = OPENING.lock().unwrap().clone();
                if let Some(server) = opening.as_deref().and_then(server_key) {
                    save_login(server, Login { user: user_of(&u), pass: user_of(&p) });
                }
            }
            (Dialog::Question { .. }, Some(n)) => {
                libvlc_dialog_post_action(id, n);
            }
            _ => {
                libvlc_dialog_dismiss(id);
            }
        }
    }
}

// ------------------------------------------------------------ saved logins

/// The network address most recently opened (listed or played): a login
/// libVLC asks for belongs to its server.
static OPENING: Mutex<Option<String>> = Mutex::new(None);
static LOGINS: Mutex<Option<Vec<(String, Login)>>> = Mutex::new(None);

#[derive(Clone)]
struct Login {
    user: String,
    pass: String,
}

fn user_of(c: &CString) -> String {
    c.to_string_lossy().into_owned()
}

/// "smb://NAS/Movies/a.mkv" → "smb://nas". Only schemes whose libVLC
/// modules take a login through options.
fn server_key(mrl: &str) -> Option<String> {
    let (scheme, rest) = mrl.split_once("://")?;
    option_prefix(scheme)?;
    let host = rest.split('/').next()?.rsplit('@').next()?;
    (!host.is_empty()).then(|| format!("{}://{}", scheme.to_lowercase(), host.to_lowercase()))
}

fn option_prefix(scheme: &str) -> Option<&'static str> {
    match scheme.to_lowercase().as_str() {
        "smb" => Some("smb"),
        "ftp" | "ftps" => Some("ftp"),
        "sftp" => Some("sftp"),
        "http" | "https" => Some("http"),
        _ => None,
    }
}

fn logins_file() -> Option<std::path::PathBuf> {
    crate::store::config_dir().map(|d| d.join("network-logins.tsv"))
}

fn with_logins<R>(f: impl FnOnce(&mut Vec<(String, Login)>) -> R) -> R {
    let mut guard = LOGINS.lock().unwrap();
    let logins = guard.get_or_insert_with(|| {
        let text = logins_file().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        text.lines()
            .filter_map(|l| {
                let mut parts = l.splitn(3, '\t');
                let server = parts.next()?.to_owned();
                let user = unescape(parts.next()?);
                let pass = unescape(parts.next()?);
                Some((server, Login { user, pass }))
            })
            .collect()
    });
    f(logins)
}

fn saved_login(mrl: &str) -> Option<Login> {
    let key = server_key(mrl)?;
    with_logins(|l| l.iter().find(|(s, _)| *s == key).map(|(_, login)| login.clone()))
}

fn save_login(server: String, login: Login) {
    with_logins(|logins| {
        logins.retain(|(s, _)| *s != server);
        logins.push((server, login));
        write_logins(logins);
    });
}

/// The servers a login is remembered for ("smb://nas").
pub fn saved_logins() -> Vec<String> {
    with_logins(|logins| logins.iter().map(|(s, _)| s.clone()).collect())
}

/// Forgets the login remembered for `server` (as [`saved_logins`] gives it).
pub fn forget_login(server: &str) {
    with_logins(|logins| {
        logins.retain(|(s, _)| s != server);
        write_logins(logins);
    });
}

fn write_logins(logins: &[(String, Login)]) {
    let Some(path) = logins_file() else { return };
    if logins.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let text: String =
        logins.iter().map(|(s, l)| format!("{s}\t{}\t{}\n", escape(&l.user), escape(&l.pass))).collect();
    let _ = std::fs::write(path, text);
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// libVLC options for opening `mrl`: the saved login for its server, if any.
/// Also notes `mrl` as what is being opened, so a login typed for it is saved.
pub fn media_options(mrl: &str) -> Vec<String> {
    if server_key(mrl).is_none() {
        return Vec::new();
    }
    *OPENING.lock().unwrap() = Some(mrl.to_owned());
    let Some(login) = saved_login(mrl) else { return Vec::new() };
    let prefix = mrl.split_once("://").and_then(|(s, _)| option_prefix(s)).unwrap_or("smb");
    let mut user = login.user.as_str();
    let mut options = Vec::new();
    // Windows accounts may be typed as "DOMAIN;user" or "DOMAIN\user".
    if prefix == "smb" {
        if let Some((domain, u)) = user.split_once(';').or_else(|| user.split_once('\\')) {
            options.push(format!(":smb-domain={domain}"));
            user = u;
        }
    }
    options.push(format!(":{prefix}-user={user}"));
    options.push(format!(":{prefix}-pwd={}", login.pass));
    options
}

/// Whether a network listing entry is worth showing when opening media:
/// folders, media and playlist files, and items without an extension (media
/// servers usually name items by title).
pub fn is_openable(item: &NetItem) -> bool {
    if item.is_dir {
        return true;
    }
    let path = std::path::Path::new(&item.name);
    match path.extension().and_then(|e| e.to_str()) {
        None => true,
        Some(e) if e.len() > 4 || e.contains(' ') => true,
        Some(_) => {
            crate::playlist::has_extension(path, crate::playlist::MEDIA_EXTENSIONS)
                || crate::playlist::has_extension(path, crate::playlist::PLAYLIST_EXTENSIONS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_addresses() {
        assert_eq!(name_from_mrl("smb://nas/Movies/My%20Film.mkv"), "My Film.mkv");
        assert_eq!(name_from_mrl("smb://nas/Movies/"), "Movies");
        assert_eq!(name_from_mrl("smb://nas/%D0%9A%D0%B8%D0%BD%D0%BE"), "Кино");
        assert_eq!(name_from_mrl("smb://nas/100%"), "100%");
    }

    #[test]
    fn login_servers() {
        assert_eq!(server_key("smb://NAS/Movies/a.mkv").as_deref(), Some("smb://nas"));
        assert_eq!(server_key("smb://rick@10.0.2.2/private").as_deref(), Some("smb://10.0.2.2"));
        assert_eq!(server_key("upnp://http://192.168.1.2:8200/x"), None);
        assert_eq!(server_key("/storage/emulated/0/a.mkv"), None);
        assert_eq!(unescape(&escape("a\tb\\c\nd")), "a\tb\\c\nd");
    }

    #[test]
    fn openable_items() {
        let item = |name: &str| NetItem { mrl: String::new(), name: name.to_owned(), is_dir: false };
        assert!(is_openable(&item("Film.mkv")));
        assert!(is_openable(&item("Film (2020)")));
        assert!(is_openable(&item("Mr. Robot S01E01")));
        assert!(!is_openable(&item("cover.jpg")));
        assert!(!is_openable(&item("Film.srt")));
    }
}
