//! Settings overlay: a menu drawn over the game in the project's own pixel font.
//!
//! The menu model (`Menu`) is filled and acted on by the application; this module only
//! navigates it (keyboard scancodes, which the controller's menu buttons also produce) and
//! draws it. A tab is a list of rows: section headings, collapsible groups (rarely changed
//! options; collapsed at start, then as left for the session) and options, indented under
//! the option they depend on. The panel is painted in software at the font's resolution
//! (one "dot" per font pixel) and shown at an integer scale chosen for the window, nearest
//! neighbour, so it stays crisp at any size; its height fits the longest tab with every
//! group open, so nothing scrolls (except in windows under about 300 pixels). A tab can end
//! in a paged list (`Row::Pager`, the CRT tab's shader parameters): the rows after the pager
//! are shown a page at a time in the space the panel has left, the page following the
//! selection. While it is open the Glide layer holds the game at a frame boundary (see
//! `Backend::hold`), so the game is paused.

use std::sync::{Arc, Mutex};

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::pixel_font::{self as font, ADVANCE, CAP};

/// Opens and closes the overlay (F12; the controller's Back button sends it too).
pub const TOGGLE_SCANCODE: u8 = 0x58;

#[derive(Clone, Debug)]
pub enum Value {
    /// One of several named options.
    Choice { options: Vec<String>, index: usize },
    /// An integer in a range, shown with a suffix (e.g. "%").
    Number { value: i32, min: i32, max: i32, step: i32, suffix: String },
    /// A real number in a range (a shader parameter): Left / Right step, Enter / A goes
    /// back to `default`. `step` 0: ten steps over the range (as ShaderGlass's sliders).
    Float { value: f32, min: f32, max: f32, step: f32, default: f32 },
    /// A command (Enter / A, Left / Right do it); the text says what it does.
    Action(String),
    /// Text typed while the row is selected (letters, digits, space, - . / _; Backspace
    /// deletes, Delete clears). With a controller: Left / Right change the last letter
    /// (past the first: removed), Enter / A adds one.
    Text(String),
}

/// Characters a `Value::Text` takes, in the order a controller steps through them (the
/// first, nothing, removes the last character).
const TEXT_CHARS: &str = "\0abcdefghijklmnopqrstuvwxyz0123456789- ./_";
/// Longest `Value::Text`.
pub const TEXT_MAX: usize = 24;

/// Set-1 scancodes of the characters a `Value::Text` takes.
fn typed_char(code: u8) -> Option<char> {
    const ROW1: &[u8] = b"1234567890-";
    const ROWQ: &[u8] = b"qwertyuiop";
    const ROWA: &[u8] = b"asdfghjkl";
    const ROWZ: &[u8] = b"zxcvbnm";
    Some(match code {
        0x02..=0x0c => ROW1[(code - 0x02) as usize] as char,
        0x10..=0x19 => ROWQ[(code - 0x10) as usize] as char,
        0x1e..=0x26 => ROWA[(code - 0x1e) as usize] as char,
        0x2c..=0x32 => ROWZ[(code - 0x2c) as usize] as char,
        0x34 => '.',
        0x35 => '/',
        0x39 => ' ',
        _ => return None,
    })
}

/// Decimals to show a float with: enough for its step.
pub fn decimals(step: f32) -> usize {
    let s = step.abs();
    if s == 0.0 || s >= 1.0 {
        if s == 0.0 { 2 } else { 0 }
    } else {
        ((-s.log10()).ceil() as usize).clamp(1, 4)
    }
}

/// `v` as a `Value::Float` with step `step` shows it.
pub fn float_text(v: f32, step: f32) -> String {
    let t = format!("{:.*}", decimals(step), v);
    if t.starts_with('-') && t.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { t[1..].to_string() } else { t }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub value: Value,
    /// Shown under the list for the selected item (at most two lines).
    pub hint: String,
    /// Shown dimmed: no effect with the current values of the options it depends on (it
    /// can still be changed).
    pub inactive: bool,
}

#[derive(Clone, Debug)]
pub enum Row {
    /// A section heading (not selectable).
    Section(String),
    /// A collapsible group: the rows after it with a larger depth, up to the next section
    /// or row of its depth or less. Selectable: Enter / A, Left / Right close and open it.
    Group { title: String, hint: String, depth: u8, expanded: bool },
    /// An option: depth 0 under its section, 1 when it depends on the option above it or
    /// is in a group.
    Item { item: Item, depth: u8 },
    /// The start of a paged list: every row after it is shown a page at a time (as many as
    /// the panel has room for). Selectable: Left / Right turn the pages; it shows "PAGE
    /// 2 / 7" and `label` (e.g. how many rows there are).
    Pager { label: String, hint: String },
}

#[derive(Clone, Debug, Default)]
pub struct Tab {
    pub title: String,
    pub rows: Vec<Row>,
    /// The page of the paged list shown (`Row::Pager`), when the selection is not in it.
    pub page: usize,
}

#[derive(Default, Debug)]
pub struct Menu {
    pub open: bool,
    pub tab: usize,
    /// The selected row of the tab (an option or a group, shown).
    pub row: usize,
    pub tabs: Vec<Tab>,
    /// Ids of items changed since the application last looked.
    pub changed: Vec<String>,
    /// Status line (e.g. "saved").
    pub status: String,
    /// Hint shown for a few seconds after start ("F12: settings").
    pub toast_until: Option<std::time::Instant>,
    ext: bool,
    /// Rows of a paged list that fit the panel, as last painted (`Row::Pager`).
    pub page_rows: std::sync::atomic::AtomicUsize,
}

pub type SharedMenu = Arc<Mutex<Menu>>;

impl Value {
    pub fn text(&self) -> String {
        match self {
            Value::Choice { options, index } => options.get(*index).cloned().unwrap_or_default(),
            Value::Number { value, suffix, .. } => format!("{value}{suffix}"),
            Value::Float { value, step, .. } => float_text(*value, *step),
            Value::Action(t) => t.clone(),
            Value::Text(t) => format!("{t}_"),
        }
    }

    fn step(&mut self, dir: i32) -> bool {
        match self {
            Value::Choice { options, index } => {
                let n = options.len() as i32;
                if n < 2 {
                    return false;
                }
                *index = ((*index as i32 + dir).rem_euclid(n)) as usize;
                true
            }
            Value::Number { value, min, max, step, .. } => {
                let v = (*value + dir * *step).clamp(*min, *max);
                let changed = v != *value;
                *value = v;
                changed
            }
            Value::Float { value, min, max, step, .. } => {
                // ShaderGlass's sliders: positions min + k * step, k = 0 .. (max - min) / step
                let range = *max - *min;
                if range <= 0.0 {
                    return false;
                }
                let steps = if *step > 0.0 { (range / *step).round().max(1.0) } else { 10.0 };
                let k = ((*value - *min) / range * steps).round() + dir as f32;
                let v = (*min + range * k.clamp(0.0, steps) / steps).clamp(*min, *max);
                let changed = v != *value;
                *value = v;
                changed
            }
            Value::Action(_) => true,
            Value::Text(t) => {
                // a controller: the last character steps through TEXT_CHARS
                let chars: Vec<char> = TEXT_CHARS.chars().collect();
                let Some(last) = t.pop() else { return false };
                let i = chars.iter().position(|&c| c == last).unwrap_or(0) as i32;
                let c = chars[(i + dir).rem_euclid(chars.len() as i32) as usize];
                if c != '\0' {
                    t.push(c);
                }
                true
            }
        }
    }

    /// Enter / A: the next value; a float goes back to its default, a text gets a letter.
    fn enter(&mut self) -> bool {
        match self {
            Value::Float { value, default, .. } => {
                let changed = *value != *default;
                *value = *default;
                changed
            }
            Value::Text(t) if t.chars().count() < TEXT_MAX => {
                t.push('a');
                true
            }
            Value::Text(_) => false,
            _ => self.step(1),
        }
    }

    /// Characters of the longest text the value can show.
    fn widest(&self) -> usize {
        match self {
            Value::Choice { options, .. } => options.iter().map(|o| o.chars().count()).max().unwrap_or(0),
            Value::Number { min, max, suffix, .. } => format!("{min}{suffix}").chars().count().max(format!("{max}{suffix}").chars().count()),
            Value::Float { min, max, step, default, .. } => [*min, *max, *default, -(max.abs().max(min.abs()))].iter().map(|v| float_text(*v, *step).chars().count()).max().unwrap_or(0),
            Value::Action(t) => t.chars().count(),
            Value::Text(_) => TEXT_MAX + 1,
        }
    }
}

impl Row {
    fn depth(&self) -> Option<u8> {
        match self {
            Row::Section(_) => None,
            Row::Group { depth, .. } | Row::Item { depth, .. } => Some(*depth),
            Row::Pager { .. } => Some(0),
        }
    }
}

/// Rows of a paged list shown at least (the panel is made this tall for them).
const PAGE_MIN: usize = 2;
/// Rows the panel grows by at most for a paged list, where the window has the room.
const PAGE_GROW: i32 = 10;

/// Rows with a little space above them: section headings and the pager.
fn gap_above(r: &Row) -> bool {
    matches!(r, Row::Section(_) | Row::Pager { .. })
}

impl Tab {
    /// Whether each row is shown (every group around it expanded); `all`: as if all were.
    fn shown_with(&self, all: bool) -> Vec<bool> {
        let mut around: Vec<(u8, bool)> = Vec::new(); // groups the row is in: depth, expanded
        self.rows
            .iter()
            .map(|r| {
                let Some(d) = r.depth() else {
                    around.clear();
                    return true;
                };
                while around.last().is_some_and(|g| g.0 >= d) {
                    around.pop();
                }
                let shown = all || around.iter().all(|g| g.1);
                if let Row::Group { expanded, .. } = r {
                    around.push((d, *expanded));
                }
                shown
            })
            .collect()
    }

    pub fn shown(&self) -> Vec<bool> {
        self.shown_with(false)
    }

    /// The rows that can be selected: shown options and groups.
    fn selectable(&self) -> Vec<usize> {
        let shown = self.shown();
        (0..self.rows.len()).filter(|&i| shown[i] && self.rows[i].depth().is_some()).collect()
    }

    /// Options in the group at row `g`.
    fn group_len(&self, g: usize) -> usize {
        let Some(Row::Group { depth, .. }) = self.rows.get(g) else { return 0 };
        self.rows[g + 1..].iter().take_while(|r| r.depth().is_some_and(|d| d > *depth)).filter(|r| matches!(r, Row::Item { .. })).count()
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.rows.iter().filter_map(|r| if let Row::Item { item, .. } = r { Some(item) } else { None })
    }

    fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.rows.iter_mut().filter_map(|r| if let Row::Item { item, .. } = r { Some(item) } else { None })
    }

    /// The row of the pager, if the tab ends in a paged list.
    pub fn pager(&self) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Pager { .. }))
    }

    /// Height of the rows shown in dots (all of them with `all`), up to and with the pager.
    fn fixed_height(&self, all: bool) -> i32 {
        let end = self.pager().map_or(self.rows.len(), |p| p + 1);
        let mut h = 0;
        for (r, shown) in self.rows[..end].iter().zip(self.shown_with(all)) {
            if shown {
                if h > 0 && gap_above(r) {
                    h += SECTION_GAP;
                }
                h += ROW;
            }
        }
        h
    }

    /// Rows of the paged list.
    fn paged_len(&self) -> usize {
        self.pager().map_or(0, |p| self.rows.len() - p - 1)
    }

    /// Height the panel needs for this tab with every group open: the paged list at its
    /// least (`want`: all of it).
    fn list_height(&self, want: bool) -> i32 {
        let paged = self.paged_len();
        let rows = if want { paged } else { paged.min(PAGE_MIN) };
        self.fixed_height(true) + rows as i32 * ROW
    }
}

impl Menu {
    pub fn new(tabs: Vec<Tab>) -> Menu {
        let mut m = Menu { tabs, ..Default::default() };
        m.row = m.first_row();
        m
    }

    fn first_row(&self) -> usize {
        self.tabs.get(self.tab).and_then(|t| t.selectable().first().copied()).unwrap_or(0)
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.tabs.iter().flat_map(|t| t.items()).find(|i| i.id == id)
    }

    pub fn item_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.tabs.iter_mut().flat_map(|t| t.items_mut()).find(|i| i.id == id)
    }

    /// The selected row.
    pub fn selected(&self) -> Option<&Row> {
        self.tabs.get(self.tab)?.rows.get(self.row)
    }

    /// One scancode byte from the keyboard (set 1, E0 prefixes as separate bytes).
    /// Returns true if the overlay took it (it must not reach the game).
    pub fn key(&mut self, sc: u8) -> bool {
        if sc == 0xe0 {
            self.ext = true;
            return self.open;
        }
        if self.open && self.text_key(sc) {
            return true;
        }
        let ext = std::mem::take(&mut self.ext);
        let make = sc & 0x80 == 0;
        let code = sc & 0x7f;
        if code == TOGGLE_SCANCODE && !ext {
            if make {
                self.open = !self.open;
                self.status.clear();
                if self.open && !self.tabs.get(self.tab).is_some_and(|t| t.selectable().contains(&self.row)) {
                    self.row = self.first_row();
                }
            }
            return true;
        }
        if !self.open {
            return false;
        }
        if !make {
            return true;
        }
        match (code, ext) {
            (0x01, false) => self.open = false,                 // Esc (B)
            (0x48, true) => self.move_row(-1),                  // Up
            (0x50, true) => self.move_row(1),                   // Down
            (0x4b, true) => self.left_right(-1),                // Left
            (0x4d, true) => self.left_right(1),                 // Right
            (0x1c, false) => self.enter(),                      // Enter (A)
            (0x49, true) => self.switch_tab(-1),                // PgUp (LB)
            (0x51, true) | (0x0f, false) => self.switch_tab(1), // PgDn (RB) / Tab
            _ => {}
        }
        true
    }

    /// Typing into a selected `Value::Text` (keyboard): characters, Backspace, Delete.
    fn text_key(&mut self, sc: u8) -> bool {
        let ext = self.ext;
        let code = sc & 0x7f;
        let (tab, row) = (self.tab, self.row);
        let Some(Row::Item { item, .. }) = self.tabs.get_mut(tab).and_then(|t| t.rows.get_mut(row)) else { return false };
        let Value::Text(t) = &mut item.value else { return false };
        let typing = matches!((code, ext), (0x0e, false) | (0x53, true)) || (!ext && typed_char(code).is_some());
        if !typing {
            return false;
        }
        self.ext = false;
        if sc & 0x80 != 0 {
            return true;
        }
        let changed = match (code, ext) {
            (0x0e, false) => t.pop().is_some(),
            (0x53, true) => !std::mem::take(t).is_empty(),
            (c, _) => match typed_char(c) {
                Some(ch) if t.chars().count() < TEXT_MAX => {
                    t.push(ch);
                    true
                }
                _ => false,
            },
        };
        if changed {
            let id = item.id.clone();
            if !self.changed.contains(&id) {
                self.changed.push(id);
            }
        }
        true
    }

    /// Up / Down: the previous / next option or group shown (headings are passed over),
    /// round from the last to the first.
    fn move_row(&mut self, dir: i32) {
        let Some(tab) = self.tabs.get(self.tab) else { return };
        let sel = tab.selectable();
        if sel.is_empty() {
            return;
        }
        let n = sel.len() as i32;
        let pos = match sel.iter().position(|&r| r == self.row) {
            Some(p) => (p as i32 + dir).rem_euclid(n) as usize,
            None if dir > 0 => sel.iter().position(|&r| r > self.row).unwrap_or(0),
            None => sel.iter().rposition(|&r| r < self.row).unwrap_or(sel.len() - 1),
        };
        self.row = sel[pos];
        self.follow_page();
    }

    /// The paged list's page follows a selection in it.
    fn follow_page(&mut self) {
        let k = self.page_rows.load(std::sync::atomic::Ordering::Relaxed).max(1);
        let row = self.row;
        if let Some(tab) = self.tabs.get_mut(self.tab) {
            if let Some(p) = tab.pager() {
                if row > p {
                    tab.page = (row - p - 1) / k;
                }
            }
        }
    }

    /// Turn the paged list's page (the pager row): `dir` pages on, round.
    fn turn_page(&mut self, dir: i32) {
        let k = self.page_rows.load(std::sync::atomic::Ordering::Relaxed).max(1);
        if let Some(tab) = self.tabs.get_mut(self.tab) {
            let pages = tab.paged_len().div_ceil(k).max(1) as i32;
            tab.page = (tab.page as i32 + dir).rem_euclid(pages) as usize;
        }
    }

    /// Left / Right: change the option, or close / open the group.
    fn left_right(&mut self, dir: i32) {
        let (tab, row) = (self.tab, self.row);
        match self.tabs.get_mut(tab).and_then(|t| t.rows.get_mut(row)) {
            Some(Row::Group { expanded, .. }) => *expanded = dir > 0,
            Some(Row::Item { .. }) => self.change(dir),
            Some(Row::Pager { .. }) => self.turn_page(dir),
            _ => {}
        }
    }

    /// Enter / A: next value of the option, or open / close the group.
    fn enter(&mut self) {
        let (tab, row) = (self.tab, self.row);
        match self.tabs.get_mut(tab).and_then(|t| t.rows.get_mut(row)) {
            Some(Row::Group { expanded, .. }) => *expanded = !*expanded,
            Some(Row::Item { item, .. }) => {
                if item.value.enter() {
                    let id = item.id.clone();
                    if !self.changed.contains(&id) {
                        self.changed.push(id);
                    }
                }
            }
            Some(Row::Pager { .. }) => self.turn_page(1),
            _ => {}
        }
    }

    fn switch_tab(&mut self, dir: i32) {
        let n = self.tabs.len() as i32;
        if n > 0 {
            self.tab = ((self.tab as i32 + dir).rem_euclid(n)) as usize;
            self.row = self.first_row();
        }
    }

    fn change(&mut self, dir: i32) {
        let (tab, row) = (self.tab, self.row);
        if let Some(Row::Item { item, .. }) = self.tabs.get_mut(tab).and_then(|t| t.rows.get_mut(row)) {
            if item.value.step(dir) {
                let id = item.id.clone();
                if !self.changed.contains(&id) {
                    self.changed.push(id);
                }
            }
        }
    }

    /// Text that does not fit the overlay's layout (for tests): rows wider than the panel,
    /// hints longer than two lines, characters the font does not have.
    pub fn layout_problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        let mut texts: Vec<&str> = Vec::new();
        let hint_lines = |what: &str, hint: &str, p: &mut Vec<String>| {
            let n = font::wrap(hint, COLS as usize).len();
            if n > 2 {
                p.push(format!("{what}: the hint takes {n} lines"));
            }
        };
        let titles: i32 = self.tabs.iter().map(|t| font::width(&t.title) + 2 * TAB_PAD).sum();
        if titles > CONTENT_W {
            p.push(format!("tabs: the titles take {titles} pixels, wider than the panel's {CONTENT_W}"));
        }
        for t in &self.tabs {
            texts.push(&t.title);
            for r in &t.rows {
                match r {
                    Row::Section(s) => {
                        texts.push(s);
                        if s.chars().count() as i32 > COLS - 4 {
                            p.push(format!("section {s}: too long"));
                        }
                    }
                    Row::Group { title, hint, depth, .. } => {
                        texts.extend([title.as_str(), hint.as_str()]);
                        // "+ TITLE", a space, then "NN SETTINGS" right-aligned like a value
                        let n = indent(*depth) + 2 + title.chars().count() as i32 + 1 + 11 + 2;
                        if n > COLS {
                            p.push(format!("group {title}: {n} columns, the panel has {COLS}"));
                        }
                        hint_lines(title, hint, &mut p);
                    }
                    Row::Pager { label, hint } => {
                        texts.extend([label.as_str(), hint.as_str()]);
                        hint_lines("pager", hint, &mut p);
                    }
                    Row::Item { item, depth } => {
                        texts.extend([item.label.as_str(), item.hint.as_str()]);
                        if let Value::Action(t) = &item.value {
                            texts.push(t);
                        }
                        if let Value::Choice { options, .. } = &item.value {
                            texts.extend(options.iter().map(String::as_str));
                        }
                        // label, a space, then the value between its arrows: "< VALUE  >"
                        let n = indent(*depth) + item.label.chars().count() as i32 + 1 + item.value.widest() as i32 + 4;
                        if n > COLS {
                            p.push(format!("{}: {n} columns, the panel has {COLS}", item.id));
                        }
                        hint_lines(&item.id, &item.hint, &mut p);
                    }
                }
            }
        }
        for s in texts {
            if let Some(c) = s.chars().find(|&c| font::find(c).is_none()) {
                p.push(format!("{s:?}: no glyph for {c:?}"));
            }
        }
        p
    }
}

/// `s` with the characters the font has no glyph for replaced by `?`.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if font::find(c).is_some() { c } else { '?' }).collect()
}

/// `s` cut (with an ellipsis) to `cols` characters.
pub fn fit(s: &str, cols: usize) -> String {
    font::fit(s, cols)
}

/// Columns a value has on a row with `label` at `depth` ("< VALUE  >" after the label).
pub fn value_columns(label: &str, depth: u8) -> usize {
    (COLS - indent(depth) - label.chars().count() as i32 - 1 - 4).max(4) as usize
}

// ---- layout (in dots: pixels of the font, before scaling) ----

/// Characters per line of the panel's content.
const COLS: i32 = 56;
const CONTENT_W: i32 = COLS * ADVANCE - 1;
/// Space between the panel's edge and its content.
const PAD: i32 = 10;
const PANEL_W: i32 = CONTENT_W + 2 * PAD;
/// Pitch of the list's rows.
const ROW: i32 = 11;
/// Extra space above a section heading.
const SECTION_GAP: i32 = 4;
const TITLE_Y: i32 = 7;
const TABS_Y: i32 = 19;
const TAB_H: i32 = 13;
/// Space on each side of a tab's title in its box (room for six tabs).
const TAB_PAD: i32 = 4;
const LIST_Y: i32 = TABS_Y + TAB_H + 5;

/// The tabs' title positions for titles `widths` pixels wide: the panel's width left over
/// shared evenly, half a share before the first title and after the last, so the gaps
/// between titles are equal whatever their lengths.
fn tab_positions(widths: &[i32]) -> Vec<i32> {
    let n = widths.len().max(1) as i32;
    let spare = CONTENT_W - widths.iter().sum::<i32>();
    let mut before = 0;
    widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let x = PAD + before + spare * (2 * i as i32 + 1) / (2 * n);
            before += w;
            x
        })
        .collect()
}
/// From the end of the list to the bottom edge: separator, two hint lines, key help.
const FOOTER: i32 = 3 + 5 + 23 + CAP + 6;
/// Kept free around the panel.
const MARGIN: i32 = 4;
/// Drop shadow below and right of the panel.
const SHADOW: i32 = 2;

/// Columns from the content's left edge to an option of this depth.
fn indent(depth: u8) -> i32 {
    2 + 2 * depth as i32
}

/// The pixel scale wanted for a window this tall: 2 at 720 to 1080 lines, 3 at 1440, 4 at
/// 2160 (text 14, 21, 28 pixels tall); 1 below 600 lines.
pub fn ui_scale(height: u32) -> u32 {
    ((height as f32 / 540.0).round() as u32).max(if height >= 600 { 2 } else { 1 })
}

/// Where and how large the menu is drawn in a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Window pixels per dot.
    pub scale: u32,
    /// Panel size in dots.
    pub width: i32,
    pub height: i32,
    /// Height of the list area in dots.
    list: i32,
    /// The list area cannot hold every row of the longest tab (tiny windows): it scrolls.
    pub scrolls: bool,
}

impl Layout {
    pub fn new(menu: &Menu, w: u32, h: u32) -> Layout {
        // the longest tab with every group open: the panel keeps its size whatever is open
        // (a paged list at its least; it takes the room the window has at that scale, up to
        // its whole length and at most PAGE_GROW rows more, so that the other tabs' panel
        // stays compact)
        let list = menu.tabs.iter().map(|t| t.list_height(false)).max().unwrap_or(ROW).max(ROW);
        let want = menu.tabs.iter().map(|t| t.list_height(true)).max().unwrap_or(ROW).max(list);
        let need_w = (PANEL_W + SHADOW + 2 * MARGIN) as u32;
        let need_h = (LIST_Y + list + FOOTER + SHADOW + 2 * MARGIN) as u32;
        match (1..=ui_scale(h)).rev().find(|s| need_w * s <= w && need_h * s <= h) {
            Some(scale) => {
                let room = h as i32 / scale as i32 - (LIST_Y + FOOTER + SHADOW + 2 * MARGIN);
                let list = want.min(room).min(list + PAGE_GROW * ROW).max(list);
                Layout { scale, width: PANEL_W, height: LIST_Y + list + FOOTER, list, scrolls: false }
            }
            None => {
                let fit = ((h as i32 - 2 * MARGIN - SHADOW - LIST_Y - FOOTER) / ROW).max(3) * ROW;
                let l = fit.min(list);
                Layout { scale: 1, width: PANEL_W, height: LIST_Y + l + FOOTER, list: l, scrolls: l < list }
            }
        }
    }
}

// ---- software painting ----

/// Straight (not premultiplied) colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgba(u8, u8, u8, u8);

const PANEL: Rgba = Rgba(8, 10, 16, 222);
const BORDER: Rgba = Rgba(66, 74, 92, 255);
const SHADE: Rgba = Rgba(0, 0, 0, 110);
const WHITE: Rgba = Rgba(255, 255, 255, 255);
const MUTED: Rgba = Rgba(120, 127, 140, 255);
const TAB_BOX: Rgba = Rgba(110, 170, 255, 48);
const ACCENT: Rgba = Rgba(120, 186, 255, 255);
const SEPARATOR: Rgba = Rgba(54, 60, 75, 255);
const SECTION: Rgba = Rgba(236, 239, 244, 255);
const SECTION_LINE: Rgba = Rgba(46, 52, 66, 255);
const LABEL: Rgba = Rgba(190, 196, 205, 255);
const LABEL_OFF: Rgba = Rgba(96, 102, 112, 255);
const VALUE: Rgba = Rgba(120, 186, 255, 255);
const VALUE_SEL: Rgba = Rgba(172, 216, 255, 255);
const VALUE_OFF: Rgba = Rgba(60, 90, 126, 255);
const BAR: Rgba = Rgba(120, 170, 255, 40);
const HINT: Rgba = Rgba(255, 212, 84, 255);
const SAVED: Rgba = Rgba(120, 228, 120, 255);
const FAILED: Rgba = Rgba(255, 128, 96, 255);
const FPS: Rgba = Rgba(140, 255, 140, 255);
const LABEL_BG: Rgba = Rgba(0, 0, 0, 160);

/// A premultiplied BGRA image painted in dots.
struct Canvas {
    w: i32,
    h: i32,
    px: Vec<[u8; 4]>,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Canvas {
        let (w, h) = (w.max(1), h.max(1));
        Canvas { w, h, px: vec![[0; 4]; (w * h) as usize] }
    }

    /// Paint one dot over what is there.
    fn dot(&mut self, x: i32, y: i32, c: Rgba) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let d = &mut self.px[(y * self.w + x) as usize];
        let (a, ia) = (c.3 as u32, 255 - c.3 as u32);
        let over = |s: u8, d: u8| ((s as u32 * a + d as u32 * ia + 127) / 255) as u8;
        *d = [over(c.2, d[0]), over(c.1, d[1]), over(c.0, d[2]), ((255 * a + d[3] as u32 * ia + 127) / 255) as u8];
    }

    fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgba) {
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                self.dot(xx, yy, c);
            }
        }
    }

    /// `s` with its top-left corner at (x, y).
    fn text(&mut self, x: i32, y: i32, s: &str, c: Rgba) {
        for (i, ch) in s.chars().enumerate() {
            let g = font::glyph(ch);
            let gx = x + i as i32 * ADVANCE;
            for (r, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        self.dot(gx + col, y + r as i32, c);
                    }
                }
            }
        }
    }

    /// `s` ending at x (right-aligned).
    fn text_right(&mut self, x: i32, y: i32, s: &str, c: Rgba) {
        self.text(x - font::width(s), y, s, c);
    }
}

/// Left edge of a value: right-aligned in the columns before its right arrow.
fn value_x(s: &str) -> i32 {
    PAD + (COLS - 2 - s.chars().count() as i32) * ADVANCE
}

/// The menu panel (with its drop shadow), in dots.
fn paint_menu(menu: &Menu, lay: &Layout) -> Canvas {
    let (w, h) = (lay.width, lay.height);
    let mut c = Canvas::new(w + SHADOW, h + SHADOW);
    c.fill(w, SHADOW, SHADOW, h, SHADE);
    c.fill(SHADOW, h, w - SHADOW, SHADOW, SHADE);
    c.fill(0, 0, w, h, PANEL);
    c.fill(0, 0, w, 1, BORDER);
    c.fill(0, h - 1, w, 1, BORDER);
    c.fill(0, 1, 1, h - 2, BORDER);
    c.fill(w - 1, 1, 1, h - 2, BORDER);
    let right = PAD + CONTENT_W;
    // title, and the key that opens the menu (or what was saved)
    c.text(PAD, TITLE_Y, "SCREAMER RALLY SETTINGS", WHITE);
    if menu.status.is_empty() {
        c.text_right(right, TITLE_Y, "F12 / BACK", MUTED);
    } else {
        let colour = if menu.status.starts_with("Saved") { SAVED } else { FAILED };
        c.text_right(right, TITLE_Y, &font::fit(&menu.status, (COLS - 26) as usize), colour);
    }
    // tabs
    let widths: Vec<i32> = menu.tabs.iter().map(|t| font::width(&t.title)).collect();
    for ((i, t), tx) in menu.tabs.iter().enumerate().zip(tab_positions(&widths)) {
        let tw = widths[i];
        let active = i == menu.tab;
        if active {
            c.fill(tx - TAB_PAD, TABS_Y, tw + 2 * TAB_PAD, TAB_H, TAB_BOX);
            c.fill(tx - TAB_PAD, TABS_Y + TAB_H - 1, tw + 2 * TAB_PAD, 1, ACCENT);
        }
        c.text(tx, TABS_Y + 3, &t.title, if active { WHITE } else { MUTED });
    }
    c.fill(PAD, TABS_Y + TAB_H, CONTENT_W, 1, SEPARATOR);
    if let Some(tab) = menu.tabs.get(menu.tab) {
        paint_list(&mut c, menu, tab, lay);
    }
    // the selected row's hint (two lines at most), the keys
    let sep = LIST_Y + lay.list + 3;
    c.fill(PAD, sep, CONTENT_W, 1, SEPARATOR);
    let hint = match menu.selected() {
        Some(Row::Item { item, .. }) => item.hint.as_str(),
        Some(Row::Group { hint, .. }) | Some(Row::Pager { hint, .. }) => hint.as_str(),
        _ => "",
    };
    let lines = font::wrap(hint, COLS as usize);
    for (k, line) in lines.iter().take(2).enumerate() {
        let line = if k == 1 && lines.len() > 2 { font::fit(&lines[1..].join(" "), COLS as usize) } else { line.clone() };
        c.text(PAD, sep + 5 + k as i32 * 10, &line, HINT);
    }
    let keys = match menu.selected() {
        Some(Row::Group { expanded: true, .. }) => "\u{2191}\u{2193} SELECT  ENTER/A HIDE  PGUP/PGDN LB/RB TAB  ESC CLOSE",
        Some(Row::Group { .. }) => "\u{2191}\u{2193} SELECT  ENTER/A SHOW  PGUP/PGDN LB/RB TAB  ESC CLOSE",
        Some(Row::Pager { .. }) => "\u{2191}\u{2193} SELECT  \u{2190}\u{2192} PAGE  PGUP/PGDN LB/RB TAB  ESC CLOSE",
        Some(Row::Item { item: Item { value: Value::Float { .. }, .. }, .. }) => "\u{2191}\u{2193} SELECT  \u{2190}\u{2192} CHANGE  ENTER/A DEFAULT  ESC CLOSE",
        Some(Row::Item { item: Item { value: Value::Action(_), .. }, .. }) => "\u{2191}\u{2193} SELECT  ENTER/A DO IT  PGUP/PGDN LB/RB TAB  ESC CLOSE",
        Some(Row::Item { item: Item { value: Value::Text(_), .. }, .. }) => "TYPE, BACKSPACE DELETES   PAD: \u{2190}\u{2192} LETTER, A ADDS ONE",
        _ => "\u{2191}\u{2193} SELECT  \u{2190}\u{2192} CHANGE  PGUP/PGDN LB/RB TAB  ESC CLOSE",
    };
    c.text(PAD, sep + 5 + 23, keys, MUTED);
    c
}

/// The tab's rows shown; scrolled (with a line saying what is out of view) only when the
/// window is too small for them.
fn paint_list(c: &mut Canvas, menu: &Menu, tab: &Tab, lay: &Layout) {
    let shown = tab.shown();
    let mut rows: Vec<usize> = (0..tab.rows.len()).filter(|&i| shown[i]).collect();
    // a paged list: the page with the selection (or the one turned to) in the room left
    if let Some(p) = tab.pager() {
        let fixed = {
            let mut h = 0;
            for (k, &i) in rows.iter().enumerate().take_while(|(_, i)| **i <= p) {
                if k > 0 && gap_above(&tab.rows[i]) {
                    h += SECTION_GAP;
                }
                h += ROW;
            }
            h
        };
        let k = (((lay.list - fixed) / ROW).max(1) as usize).max(1);
        menu.page_rows.store(k, std::sync::atomic::Ordering::Relaxed);
        let n = tab.paged_len();
        let pages = n.div_ceil(k).max(1);
        let page = if menu.row > p { (menu.row - p - 1) / k } else { tab.page }.min(pages - 1);
        let first = p + 1 + page * k;
        rows.retain(|&i| i <= p || (i >= first && i < first + k));
        // the pager shows where the page is
        let text = if n == 0 { String::new() } else { format!("{}-{} OF {n}  PAGE {}/{pages}", page * k + 1, (page * k + k).min(n), page + 1) };
        PAGER_TEXT.with(|t| *t.borrow_mut() = Some(text));
    }
    let mut ys = Vec::with_capacity(rows.len());
    let mut y = 0;
    for (k, &i) in rows.iter().enumerate() {
        if k > 0 && gap_above(&tab.rows[i]) {
            y += SECTION_GAP;
        }
        ys.push(y);
        y += ROW;
    }
    let (first, count) = if y <= lay.list {
        (0, rows.len())
    } else {
        // even pitch; the last line tells how many rows are above and below
        let fit = ((lay.list / ROW - 1).max(1) as usize).min(rows.len());
        let sel = rows.iter().position(|&i| i == menu.row).unwrap_or(0);
        let first = (sel + 1).saturating_sub(fit).min(rows.len() - fit);
        for (k, y) in ys.iter_mut().enumerate() {
            *y = (k as i32 - first as i32) * ROW;
        }
        let (above, below) = (first, rows.len() - first - fit);
        let more = match (above, below) {
            (0, b) => format!("\u{25BC} {b} MORE BELOW"),
            (a, 0) => format!("\u{25B2} {a} MORE ABOVE"),
            (a, b) => format!("\u{25B2} {a} MORE ABOVE   \u{25BC} {b} MORE BELOW"),
        };
        c.text(PAD + indent(0) * ADVANCE, LIST_Y + fit as i32 * ROW + 2, &more, MUTED);
        (first, fit)
    };
    for k in first..first + count {
        paint_row(c, tab, rows[k], LIST_Y + ys[k], rows[k] == menu.row);
    }
}

thread_local! {
    /// The pager's position text for `paint_row` (set by `paint_list` just before).
    static PAGER_TEXT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn paint_row(c: &mut Canvas, tab: &Tab, i: usize, y: i32, selected: bool) {
    let col = |n: i32| PAD + n * ADVANCE;
    if selected {
        c.fill(PAD - 4, y, CONTENT_W + 8, ROW, BAR);
    }
    let ty = y + 2;
    match &tab.rows[i] {
        Row::Pager { label, .. } => {
            // drawn like a section heading, with the page on the right
            c.text(PAD, ty, label, if selected { WHITE } else { SECTION });
            let v = PAGER_TEXT.with(|t| t.borrow().clone()).unwrap_or_default();
            let lx = PAD + font::width(label) + 5;
            let rx = value_x(&v) - if selected { 3 * ADVANCE } else { ADVANCE };
            if rx > lx {
                c.fill(lx, ty + 3, rx - lx, 1, SECTION_LINE);
            }
            let vx = value_x(&v);
            c.text(vx, ty, &v, if selected { VALUE_SEL } else { VALUE });
            if selected && !v.is_empty() {
                c.text(vx - 2 * ADVANCE, ty, "\u{25C0}", VALUE_SEL);
                c.text(col(COLS - 1), ty, "\u{25B6}", VALUE_SEL);
            }
        }
        Row::Section(title) => {
            c.text(PAD, ty, title, SECTION);
            let lx = PAD + font::width(title) + 5;
            c.fill(lx, ty + 3, PAD + CONTENT_W - lx, 1, SECTION_LINE);
        }
        Row::Group { title, depth, expanded, .. } => {
            let marker = if *expanded { '-' } else { '+' };
            c.text(col(indent(*depth)), ty, &format!("{marker} {title}"), if selected { WHITE } else { LABEL });
            if !*expanded {
                let n = tab.group_len(i);
                let more = format!("{n} {}", if n == 1 { "SETTING" } else { "SETTINGS" });
                c.text(value_x(&more), ty, &more, MUTED);
            }
        }
        Row::Item { item, depth } => {
            let (label, value) = match (selected, item.inactive) {
                (true, false) => (WHITE, VALUE_SEL),
                (false, false) => (LABEL, VALUE),
                (true, true) => (LABEL, VALUE),
                (false, true) => (LABEL_OFF, VALUE_OFF),
            };
            c.text(col(indent(*depth)), ty, &item.label, label);
            let v = item.value.text();
            let vx = value_x(&v);
            c.text(vx, ty, &v, value);
            if selected {
                c.text(vx - 2 * ADVANCE, ty, "\u{25C0}", value);
                c.text(col(COLS - 1), ty, "\u{25B6}", value);
            }
        }
    }
}

/// A short text on a dark box (frame rate, start-up hint).
fn paint_label(text: &str, fg: Rgba) -> Canvas {
    let mut c = Canvas::new(font::width(text) + 8, CAP + 6);
    c.fill(0, 0, c.w, c.h, LABEL_BG);
    c.text(4, 3, text, fg);
    c
}

// ---- Direct2D ----

/// Direct2D resources for drawing the overlay.
pub struct Painter {
    d2d: ID2D1Factory,
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

impl Painter {
    pub fn new() -> Result<Painter, String> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(|e| e.to_string())?;
            Ok(Painter { d2d })
        }
    }

    fn target(&self, surface: &IDXGISurface) -> Option<ID2D1RenderTarget> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_R8G8B8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        unsafe { self.d2d.CreateDxgiSurfaceRenderTarget(surface, &props).ok() }
    }

    /// Draw the menu (and/or the start-up hint) onto the swap chain's back buffer.
    pub fn draw(&self, surface: &IDXGISurface, size: (u32, u32), menu: &Menu) {
        let toast = menu.toast_until.is_some_and(|t| std::time::Instant::now() < t);
        if !menu.open && !toast {
            return;
        }
        let Some(rt) = self.target(surface) else { return };
        let (w, h) = (size.0 as i32, size.1 as i32);
        unsafe {
            rt.BeginDraw();
            if menu.open {
                if let Ok(dim) = rt.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.45), None) {
                    rt.FillRectangle(&rect(0.0, 0.0, w as f32, h as f32), &dim);
                }
                let lay = Layout::new(menu, size.0, size.1);
                let c = paint_menu(menu, &lay);
                let s = lay.scale as i32;
                blit(&rt, &c, (w - lay.width * s) / 2, (h - lay.height * s) / 2, s);
            } else {
                let s = ui_scale(size.1) as i32;
                let c = paint_label("F12 / BACK: SETTINGS", WHITE);
                blit(&rt, &c, w - (c.w + 6) * s, h - (c.h + 6) * s, s);
            }
            let _ = rt.EndDraw(None, None);
        }
    }

    /// The frame rate counter in the bottom-left corner.
    pub fn draw_fps(&self, surface: &IDXGISurface, size: (u32, u32), label: &str) {
        let Some(rt) = self.target(surface) else { return };
        let s = ui_scale(size.1) as i32;
        let c = paint_label(label, FPS);
        unsafe {
            rt.BeginDraw();
            blit(&rt, &c, 6 * s, size.1 as i32 - (c.h + 6) * s, s);
            let _ = rt.EndDraw(None, None);
        }
    }
}

/// Show a canvas with its top-left corner at (x, y), each dot as an s x s square.
unsafe fn blit(rt: &ID2D1RenderTarget, c: &Canvas, x: i32, y: i32, s: i32) {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    let size = D2D_SIZE_U { width: c.w as u32, height: c.h as u32 };
    unsafe {
        if let Ok(bitmap) = rt.CreateBitmap(size, Some(c.px.as_ptr().cast()), c.w as u32 * 4, &props) {
            let dest = rect(x as f32, y as f32, (x + c.w * s) as f32, (y + c.h * s) as f32);
            rt.DrawBitmap(&bitmap, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        Item { id: id.into(), label: id.into(), value: Value::Choice { options: vec!["Off".into(), "On".into()], index: 0 }, hint: String::new(), inactive: false }
    }

    fn sample() -> Menu {
        let rows = vec![
            Row::Section("First".into()),
            Row::Item { item: item("a"), depth: 0 },
            Row::Item { item: item("a_sub"), depth: 1 },
            Row::Group { title: "More".into(), hint: String::new(), depth: 0, expanded: false },
            Row::Item { item: item("g1"), depth: 1 },
            Row::Item { item: item("g2"), depth: 1 },
            Row::Section("Second".into()),
            Row::Item { item: item("b"), depth: 0 },
        ];
        let other = vec![Row::Section("Only".into()), Row::Item { item: item("c"), depth: 0 }];
        Menu::new(vec![Tab { title: "One".into(), rows, page: 0 }, Tab { title: "Two".into(), rows: other, page: 0 }])
    }

    const UP: [u8; 2] = [0xe0, 0x48];
    const DOWN: [u8; 2] = [0xe0, 0x50];
    const RIGHT: [u8; 2] = [0xe0, 0x4d];
    const LEFT: [u8; 2] = [0xe0, 0x4b];

    fn press(m: &mut Menu, keys: &[u8]) {
        for &k in keys {
            m.key(k);
        }
    }

    fn selected_id(m: &Menu) -> String {
        match m.selected() {
            Some(Row::Item { item, .. }) => item.id.clone(),
            Some(Row::Group { title, .. }) => title.clone(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn groups_and_navigation() {
        let mut m = sample();
        assert_eq!(selected_id(&m), "a", "starts on the first option, not the heading");
        assert_eq!(m.tabs[0].shown(), [true, true, true, true, false, false, true, true]);
        press(&mut m, &[TOGGLE_SCANCODE]);
        assert!(m.open);
        press(&mut m, &DOWN);
        assert_eq!(selected_id(&m), "a_sub");
        press(&mut m, &DOWN);
        assert_eq!(selected_id(&m), "More");
        press(&mut m, &DOWN);
        assert_eq!(selected_id(&m), "b", "collapsed rows and headings are passed over");
        press(&mut m, &DOWN);
        assert_eq!(selected_id(&m), "a", "round to the first");
        press(&mut m, &UP);
        assert_eq!(selected_id(&m), "b");
        press(&mut m, &UP);
        press(&mut m, &[0x1c]); // Enter opens the group
        assert_eq!(m.tabs[0].shown(), [true; 8]);
        press(&mut m, &DOWN);
        assert_eq!(selected_id(&m), "g1");
        press(&mut m, &RIGHT);
        assert_eq!(m.changed, ["g1"]);
        press(&mut m, &UP);
        press(&mut m, &LEFT); // Left closes it
        assert_eq!(m.tabs[0].shown()[4..6], [false, false]);
        press(&mut m, &RIGHT); // Right opens it
        assert!(m.tabs[0].shown()[4]);
        // tabs: PgDn / PgUp; the group stays open (remembered)
        press(&mut m, &[0xe0, 0x51]);
        assert_eq!((m.tab, selected_id(&m).as_str()), (1, "c"));
        press(&mut m, &[0xe0, 0x49]);
        assert_eq!((m.tab, selected_id(&m).as_str()), (0, "a"));
        assert!(m.tabs[0].shown()[4]);
        assert_eq!(m.tabs[0].group_len(3), 2);
        press(&mut m, &[0x01]);
        assert!(!m.open);
    }

    #[test]
    fn layout_scales() {
        // a tab of 16 rows in 4 sections: the size of the game's longest (Graphics, all open)
        let mut rows = Vec::new();
        for s in 0..4 {
            rows.push(Row::Section(format!("S{s}")));
            for i in 0..3 {
                rows.push(Row::Item { item: item(&format!("{s}{i}")), depth: 0 });
            }
        }
        let m = Menu::new(vec![Tab { title: "T".into(), rows, page: 0 }]);
        let at = |w, h| {
            let l = Layout::new(&m, w, h);
            assert!(!l.scrolls, "{w} x {h}");
            assert!(((l.width + SHADOW) as u32) * l.scale <= w && ((l.height + SHADOW) as u32) * l.scale <= h, "{w} x {h}");
            l.scale
        };
        assert_eq!(at(1280, 720), 2);
        assert_eq!(at(960, 720), 2);
        assert_eq!(at(1920, 1080), 2);
        assert_eq!(at(1440, 1080), 2);
        assert_eq!(at(2560, 1440), 3);
        assert_eq!(at(3840, 2160), 4);
        assert_eq!(at(2880, 2160), 4);
        assert_eq!(at(640, 480), 1);
        let tiny = Layout::new(&m, 640, 200);
        assert!(tiny.scrolls && tiny.scale == 1);
    }

    #[test]
    fn painting() {
        let mut m = sample();
        m.open = true;
        let lay = Layout::new(&m, 1280, 720);
        let c = paint_menu(&m, &lay);
        assert_eq!((c.w, c.h), (lay.width + SHADOW, lay.height + SHADOW));
        // the selected row's value is drawn in the accent colour, right of the middle
        let lit = |c: &Canvas, x0: i32, x1: i32, y0: i32, y1: i32| (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| c.px[(y * c.w + x) as usize][3] == 255).count();
        assert!(lit(&c, PAD, PAD + CONTENT_W, TITLE_Y, TITLE_Y + CAP) > 50, "title");
        assert!(m.layout_problems().is_empty(), "{:?}", m.layout_problems());
    }

    #[test]
    fn floats_actions_text_and_pages() {
        let float = |id: &str| Item { id: id.into(), label: id.into(), value: Value::Float { value: 0.3, min: 0.1, max: 0.5, step: 0.05, default: 0.3 }, hint: String::new(), inactive: false };
        let mut rows = vec![
            Row::Section("S".into()),
            Row::Item { item: Item { id: "find".into(), label: "Find".into(), value: Value::Text(String::new()), hint: String::new(), inactive: false }, depth: 0 },
            Row::Item { item: Item { id: "go".into(), label: "Go".into(), value: Value::Action("ENTER".into()), hint: String::new(), inactive: false }, depth: 0 },
            Row::Pager { label: "Parameters".into(), hint: String::new() },
        ];
        for i in 0..30 {
            rows.push(Row::Item { item: float(&format!("p{i}")), depth: 0 });
        }
        let mut m = Menu::new(vec![Tab { title: "T".into(), rows, page: 0 }]);
        m.open = true;
        // typing into the text: letters, Backspace; the key releases do nothing
        press(&mut m, &[0x13, 0x93, 0x18, 0x98, 0x18, 0x0e, 0x8e]);
        assert_eq!(m.item("find").unwrap().value.text(), "ro_");
        // a controller: Right changes the last letter, Enter adds one
        press(&mut m, &RIGHT);
        press(&mut m, &[0x1c]);
        assert_eq!(m.item("find").unwrap().value.text(), "rpa_");
        assert_eq!(m.changed, ["find"]);
        // an action reports itself on Enter
        m.changed.clear();
        press(&mut m, &DOWN);
        press(&mut m, &[0x1c]);
        assert_eq!(m.changed, ["go"]);
        // floats step as ShaderGlass's sliders, Enter goes back to the default
        press(&mut m, &DOWN);
        press(&mut m, &DOWN);
        assert_eq!(selected_id(&m), "p0");
        press(&mut m, &RIGHT);
        press(&mut m, &RIGHT);
        assert_eq!(m.item("p0").unwrap().value.text(), "0.40");
        for _ in 0..5 {
            press(&mut m, &RIGHT);
        }
        assert_eq!(m.item("p0").unwrap().value.text(), "0.50", "clamped to the maximum");
        press(&mut m, &[0x1c]);
        assert_eq!(m.item("p0").unwrap().value.text(), "0.30");
        // paging: the panel at 720p shows part of the 30; the page follows the selection
        let lay = Layout::new(&m, 1280, 720);
        assert!(!lay.scrolls);
        let _ = paint_menu(&m, &lay);
        let k = m.page_rows.load(std::sync::atomic::Ordering::Relaxed);
        assert!(k >= 2 && k < 30, "{k} rows a page");
        for _ in 0..k {
            press(&mut m, &DOWN);
        }
        assert_eq!(m.tabs[0].page, 1);
        // the pager turns the pages round
        press(&mut m, &UP);
        while !matches!(m.selected(), Some(Row::Pager { .. })) {
            press(&mut m, &UP);
        }
        let pages = 30usize.div_ceil(k);
        press(&mut m, &LEFT);
        assert_eq!(m.tabs[0].page, pages - 1);
        assert!(m.layout_problems().is_empty(), "{:?}", m.layout_problems());
    }

    #[test]
    fn tab_titles_have_equal_gaps() {
        // the six tabs' titles, as the app names them
        let widths: Vec<i32> = ["Display", "Graphics", "Audio", "Controls", "Gameplay", "CRT"].iter().map(|t| font::width(t)).collect();
        let xs = tab_positions(&widths);
        let gaps: Vec<i32> = (1..xs.len()).map(|i| xs[i] - (xs[i - 1] + widths[i - 1])).collect();
        let (lo, hi) = (*gaps.iter().min().unwrap(), *gaps.iter().max().unwrap());
        assert!(hi - lo <= 1, "gaps {gaps:?}");
        // half a gap before the first title and after the last
        let first = xs[0] - PAD;
        let last = PAD + CONTENT_W - (xs[xs.len() - 1] + widths[widths.len() - 1]);
        assert!((2 * first - lo).abs() <= 2 && (2 * last - lo).abs() <= 2, "first {first}, last {last}, gaps {gaps:?}");
        // the active tab's box fits between its neighbours
        assert!(lo >= 2 * TAB_PAD, "gaps {gaps:?}");
    }
}
