//! The on-screen keyboard for the remote. A NativeActivity has no Android
//! text field, and TV firmwares often don't show their keyboard for one
//! (VLC shows it for a Java `EditText`), so text fields get this keyboard:
//! its keys turn into the text and key events egui's `TextEdit` takes from a
//! real keyboard.

use egui::{pos2, vec2, Align2, Color32, FontId, Key, Painter, Rect, Stroke};

use crate::theme;
use crate::tv::{self, Nav, Press};

#[derive(Clone, Copy, PartialEq, Eq)]
enum K {
    Char(char),
    Shift,
    Lang,
    Symbols,
    Space,
    Left,
    Right,
    Erase,
    Done,
}

/// Addresses, logins and passwords are mostly Latin: English comes first.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    Russian,
    #[default]
    English,
    Symbols,
}

const RU: [&str; 3] = ["йцукенгшщзхъ", "фывапролджэ", "ячсмитьбю."];
const EN: [&str; 3] = ["qwertyuiop", "asdfghjkl:", "zxcvbnm/.-"];
const SYM: [&str; 3] = ["!@#$%^&*()_", "+=\\|;'\",<>", "?~`[]{}/.-"];

/// Key height and the gap between keys, in points.
const KEY_H: f32 = 38.0;
const GAP: f32 = 6.0;

#[derive(Default)]
pub struct Keyboard {
    pub open: bool,
    mode: Mode,
    /// The last letter layout, to return to from symbols.
    letters: Mode,
    shift: bool,
    row: usize,
    col: usize,
}

impl Keyboard {
    fn rows(&self) -> Vec<Vec<(K, f32)>> {
        let letters = match self.mode {
            Mode::Russian => RU,
            Mode::English => EN,
            Mode::Symbols => SYM,
        };
        let mut rows = vec!["1234567890".chars().map(|c| (K::Char(c), 1.0)).collect::<Vec<_>>()];
        for line in letters {
            let shift = self.shift && self.mode != Mode::Symbols;
            rows.push(
                line.chars()
                    .map(|c| (K::Char(if shift { c.to_uppercase().next().unwrap_or(c) } else { c }), 1.0))
                    .collect(),
            );
        }
        rows.push(vec![
            (K::Shift, 1.4),
            (K::Lang, 1.4),
            (K::Symbols, 1.4),
            (K::Space, 3.6),
            (K::Left, 1.0),
            (K::Right, 1.0),
            (K::Erase, 1.8),
            (K::Done, 1.8),
        ]);
        rows
    }

    fn label(&self, k: K) -> String {
        match k {
            K::Char(c) => c.to_string(),
            K::Shift => "Aa".into(),
            K::Lang => if self.letters == Mode::English { "РУС" } else { "ENG" }.into(),
            K::Symbols => if self.mode == Mode::Symbols { "АБВ" } else { "?123" }.into(),
            K::Space => "Пробел".into(),
            K::Left => "‹".into(),
            K::Right => "›".into(),
            K::Erase => "Стереть".into(),
            K::Done => "Готово".into(),
        }
    }

    /// Opens the keyboard on its first letter row.
    pub fn show(&mut self) {
        if !self.open {
            self.open = true;
            self.row = 1;
            self.col = 0;
        }
    }

    /// Handles the remote's presses while the keyboard is open, adding
    /// what the keys type to `events`. Returns the presses it doesn't use.
    pub fn handle(&mut self, presses: Vec<Press>, events: &mut Vec<egui::Event>) -> Vec<Press> {
        let mut rest = Vec::new();
        for press in presses {
            let rows = self.rows();
            self.row = self.row.min(rows.len() - 1);
            self.col = self.col.min(rows[self.row].len() - 1);
            match press.nav {
                Nav::Left => self.col = self.col.saturating_sub(1),
                Nav::Right => self.col = (self.col + 1).min(rows[self.row].len() - 1),
                Nav::Up | Nav::Down => {
                    let to = if press.nav == Nav::Up { self.row.checked_sub(1) } else { Some(self.row + 1) };
                    if let Some(to) = to.filter(|&r| r < rows.len()) {
                        // The key under the middle of this one.
                        let x = center(&rows[self.row], self.col);
                        let row = &rows[to];
                        self.col = (0..row.len())
                            .min_by(|&a, &b| (center(row, a) - x).abs().total_cmp(&(center(row, b) - x).abs()))
                            .unwrap_or(0);
                        self.row = to;
                    }
                }
                Nav::Ok => self.press(rows[self.row][self.col].0, events),
                Nav::Back => self.open = false,
                _ => rest.push(press),
            }
        }
        rest
    }

    fn press(&mut self, k: K, events: &mut Vec<egui::Event>) {
        let key = |events: &mut Vec<egui::Event>, key: Key, modifiers: egui::Modifiers| {
            for pressed in [true, false] {
                events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
            }
        };
        match k {
            K::Char(c) => {
                events.push(egui::Event::Text(c.to_string()));
                self.shift = false;
            }
            K::Space => events.push(egui::Event::Text(" ".into())),
            K::Shift => self.shift = !self.shift,
            K::Lang => {
                self.letters = if self.letters == Mode::English { Mode::Russian } else { Mode::English };
                self.mode = self.letters;
            }
            K::Symbols => self.mode = if self.mode == Mode::Symbols { self.letters } else { Mode::Symbols },
            K::Left => key(events, Key::ArrowLeft, Default::default()),
            K::Right => key(events, Key::ArrowRight, Default::default()),
            K::Erase => key(events, Key::Backspace, Default::default()),
            // On to the next field or the dialog's button.
            K::Done => {
                key(events, Key::Tab, Default::default());
                self.open = false;
            }
        }
    }

    /// Height of the keyboard on screen, in points.
    pub fn height(&self) -> f32 {
        5.0 * KEY_H + 4.0 * GAP + 2.0 * 14.0 + 22.0
    }

    pub fn paint(&self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let screen = ctx.content_rect();
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::unique("osk")));
        let panel = Rect::from_min_max(pos2(screen.left(), screen.bottom() - self.height()), screen.max);
        p.rect_filled(panel, 0.0, theme::SURFACE);
        p.line_segment([panel.left_top(), panel.right_top()], Stroke::new(1.0, theme::TRACK));
        let rows = self.rows();
        let units = rows.iter().map(|r| r.iter().map(|k| k.1).sum::<f32>()).fold(0.0, f32::max);
        let unit = ((screen.width() - 80.0 + GAP) / units).min(58.0);
        let mut y = panel.top() + 14.0;
        for (r, row) in rows.iter().enumerate() {
            let width: f32 = row.iter().map(|k| k.1 * unit).sum::<f32>() - GAP;
            let mut x = screen.center().x - width / 2.0;
            for (c, &(k, w)) in row.iter().enumerate() {
                let rect = Rect::from_min_size(pos2(x, y), vec2(w * unit - GAP, KEY_H));
                paint_key(&p, rect, &self.label(k), r == self.row && c == self.col, k == K::Shift && self.shift);
                x += w * unit;
            }
            y += KEY_H + GAP;
        }
        p.text(
            pos2(screen.center().x, panel.bottom() - 14.0),
            Align2::CENTER_CENTER,
            "OK — ввести     Назад — скрыть клавиатуру     «Готово» — дальше",
            FontId::proportional(13.0),
            theme::TEXT_DIM,
        );
    }
}

/// The middle of key `i` of `row`, in key units from the row's middle.
fn center(row: &[(K, f32)], i: usize) -> f32 {
    let total: f32 = row.iter().map(|k| k.1).sum();
    let before: f32 = row[..i].iter().map(|k| k.1).sum();
    before + row[i].1 / 2.0 - total / 2.0
}

fn paint_key(p: &Painter, rect: Rect, label: &str, focused: bool, on: bool) {
    let fill = if focused { theme::ACCENT } else if on { theme::ACCENT.gamma_multiply(0.35) } else { theme::SURFACE_HI };
    p.rect_filled(rect, 8.0, fill);
    if focused {
        tv::focus_ring(p, rect, 8.0);
    }
    let color = if focused { Color32::WHITE } else { theme::TEXT };
    let size = if label.chars().count() > 1 { 15.0 } else { 19.0 };
    p.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(size), color);
}
