//! The panel on the right of the map (state and buttons) and the status line, painted in
//! the overlay's pixel font. Everything is laid out in dots (font pixels) and shown at an
//! integer scale. The painting takes plain data ([`PanelInfo`]) so it can be tested.

use sr_render::pixel_font::{self as font, ADVANCE, CAP};
use sr_runtime::visedit::Cell;

use crate::action::Action;
use crate::canvas::*;

/// Characters per line of the panel.
pub const COLS: i32 = 28;
const CONTENT_W: i32 = COLS * ADVANCE - 1;
const PAD: i32 = 8;
/// The panel's width in dots.
pub const PANEL_W: i32 = CONTENT_W + 2 * PAD;
const ROW: i32 = 11;
const BTN_H: i32 = 13;
const BTN_PITCH: i32 = 15;
const BTN_GAP: i32 = 3;
const SECTION_GAP: i32 = 5;
/// The status line's height in dots.
pub const STATUS_H: i32 = CAP + 6;

/// Yellow of the camera cell and the scope's outlines (the map draws them in this colour).
pub const CAMERA_YELLOW: Rgba = Rgba(255, 214, 64, 255);
/// The map's tints, as the legend shows them.
pub const ADDED_GREEN: Rgba = Rgba(92, 224, 108, 255);
pub const HIDDEN_RED: Rgba = Rgba(236, 72, 60, 255);
/// The road's line ("Extend along the road").
pub const ROAD_CYAN: Rgba = Rgba(90, 220, 255, 255);
/// Messages about the road extension.
pub const ROAD_TEXT: Rgba = Rgba(130, 205, 250, 255);
const DRAWN_SAMPLE: Rgba = Rgba(118, 178, 104, 255);
const NOT_DRAWN_SAMPLE: Rgba = Rgba(150, 70, 58, 255);
const ROAD_SAMPLE: Rgba = Rgba(96, 170, 214, 255);

/// A sector's state as seen from the editing camera cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectorState {
    /// In the designers' list, not hidden.
    DrawnByDefault,
    /// Added by the road extension, not hidden.
    ByTheRoad,
    /// Added by the user.
    Added,
    /// In the designers' list, hidden by the user.
    Hidden,
    /// Not in the list.
    NotDrawn,
}

impl SectorState {
    pub fn text(self) -> &'static str {
        match self {
            SectorState::DrawnByDefault => "DRAWN",
            SectorState::ByTheRoad => "DRAWN (ROAD EXTENSION)",
            SectorState::Added => "DRAWN (ADDED BY YOU)",
            SectorState::Hidden => "NOT DRAWN (YOU HID IT)",
            SectorState::NotDrawn => "NOT DRAWN",
        }
    }

    pub fn drawn(self) -> bool {
        matches!(self, SectorState::DrawnByDefault | SectorState::ByTheRoad | SectorState::Added)
    }

    fn colour(self) -> Rgba {
        match self {
            SectorState::DrawnByDefault => DRAWN_SAMPLE,
            SectorState::ByTheRoad => ROAD_SAMPLE,
            SectorState::Added => ADDED_GREEN,
            SectorState::Hidden => HIDDEN_RED,
            SectorState::NotDrawn => NOT_DRAWN_SAMPLE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverInfo {
    pub cell: Cell,
    pub objects: usize,
    /// `None`: no editing camera cell yet.
    pub state: Option<SectorState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FileState {
    /// Nothing saved in this session (and nothing to save).
    #[default]
    None,
    Saved(String),
    Error(String),
}

/// Everything the panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelInfo {
    /// Track name and id; `None`: no race yet.
    pub track: Option<(String, String)>,
    pub cam: Option<Cell>,
    pub locked: bool,
    /// Sectors the camera cell draws, and how many of them the road extension adds.
    pub draws: usize,
    pub by_road: usize,
    /// The camera cell has no list within reach and no edits: the game draws everything in
    /// view (the engine's rule without a list).
    pub all_in_view: bool,
    /// The edit area (sectors along the road each way) and how many camera cells it holds.
    pub area: u32,
    pub area_cams: usize,
    /// The track has a road line.
    pub road_known: bool,
    /// The track has visibility lists (Stadium has none: the road extension, which reaches
    /// beyond the lists, has nothing to do).
    pub track_lists: bool,
    /// The road extension: sectors further than the designers' lists, width in half sectors.
    pub along: u32,
    pub width_half: u32,
    pub hover: Option<HoverInfo>,
    pub edited_cams: usize,
    pub dirty: bool,
    pub file: FileState,
    pub follow: bool,
    pub blink: bool,
    pub grid: bool,
    pub textures: bool,
    /// The button under the mouse.
    pub hot: Option<Action>,
}

impl Default for PanelInfo {
    fn default() -> Self {
        PanelInfo {
            track: None,
            cam: None,
            locked: false,
            draws: 0,
            by_road: 0,
            all_in_view: false,
            area: crate::editor::DEFAULT_AREA,
            area_cams: 1,
            road_known: false,
            track_lists: true,
            along: 0,
            width_half: sr_runtime::visedit::DEFAULT_WIDTH_HALF,
            hover: None,
            edited_cams: 0,
            dirty: false,
            file: FileState::None,
            follow: true,
            blink: true,
            grid: true,
            textures: true,
            hot: None,
        }
    }
}

/// A clickable rectangle of the panel (dots): x, y, w, h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Button {
    pub rect: [i32; 4],
    pub action: Action,
}

pub struct Painted {
    pub canvas: Canvas,
    pub buttons: Vec<Button>,
    /// Height the essential part takes (without the legend and the key list).
    pub essential: i32,
    /// Height everything takes (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub full: i32,
}

impl Painted {
    /// The button at panel dot (x, y).
    pub fn button_at(&self, x: i32, y: i32) -> Option<Action> {
        self.buttons.iter().find(|b| x >= b.rect[0] && y >= b.rect[1] && x < b.rect[0] + b.rect[2] && y < b.rect[1] + b.rect[3]).map(|b| b.action)
    }

    pub fn button(&self, a: Action) -> Option<Button> {
        self.buttons.iter().copied().find(|b| b.action == a)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Normal,
    On,
    Disabled,
}

struct Painter<'a> {
    c: Canvas,
    y: i32,
    buttons: Vec<Button>,
    info: &'a PanelInfo,
}

impl Painter<'_> {
    fn section(&mut self, title: &str) {
        self.y += SECTION_GAP;
        self.c.text(PAD, self.y + 2, title, SECTION);
        let lx = PAD + font::width(title) + 5;
        self.c.fill(lx, self.y + 5, PAD + CONTENT_W - lx, 1, SECTION_LINE);
        self.y += ROW;
    }

    fn text(&mut self, left: &str, lc: Rgba, right: &str, rc: Rgba) {
        let right = font::fit(&printable(right), COLS as usize);
        let room = (COLS as usize).saturating_sub(right.chars().count() + if right.is_empty() { 0 } else { 1 });
        self.c.text(PAD, self.y + 2, &font::fit(&printable(left), room), lc);
        self.c.text_right(PAD + CONTENT_W, self.y + 2, &right, rc);
        self.y += ROW;
    }

    fn wrapped(&mut self, s: &str, c: Rgba, max_lines: usize) {
        let lines = font::wrap(&printable(s), COLS as usize);
        for (k, line) in lines.iter().take(max_lines).enumerate() {
            let line = if k + 1 == max_lines && lines.len() > max_lines { font::fit(&lines[k..].join(" "), COLS as usize) } else { line.clone() };
            self.c.text(PAD, self.y + 2, &line, c);
            self.y += ROW;
        }
    }

    /// A row of equal buttons: (label, key, action, on).
    fn buttons(&mut self, row: &[(&str, &str, Action, bool)], enabled: bool) {
        let n = row.len() as i32;
        let w = (CONTENT_W - BTN_GAP * (n - 1)) / n;
        for (i, (label, key, action, on)) in row.iter().enumerate() {
            let x = PAD + i as i32 * (w + BTN_GAP);
            let bw = if i as i32 == n - 1 { PAD + CONTENT_W - x } else { w };
            let look = if !enabled { Look::Disabled } else if *on { Look::On } else { Look::Normal };
            self.button(x, self.y, bw, label, key, *action, look);
        }
        self.y += BTN_PITCH;
    }

    fn button(&mut self, x: i32, y: i32, w: i32, label: &str, key: &str, action: Action, look: Look) {
        let hot = look != Look::Disabled && self.info.hot == Some(action);
        let (frame, text, keyc) = match look {
            Look::Disabled => (SEPARATOR, LABEL_OFF, LABEL_OFF),
            Look::On => (ACCENT, WHITE, VALUE),
            Look::Normal => (if hot { ACCENT } else { BORDER }, if hot { WHITE } else { LABEL }, MUTED),
        };
        if look == Look::On {
            self.c.fill(x, y, w, BTN_H, TAB_BOX);
        }
        if hot {
            self.c.fill(x, y, w, BTN_H, BAR);
        }
        self.c.frame(x, y, w, BTN_H, frame);
        if key.is_empty() {
            self.c.text_centred(x + w / 2, y + 3, label, text);
        } else {
            self.c.text(x + 4, y + 3, label, text);
            self.c.text_right(x + w - 4, y + 3, key, keyc);
        }
        // a button cut by a short panel is not clickable
        if look != Look::Disabled && y + BTN_H <= self.c.h {
            self.buttons.push(Button { rect: [x, y, w, BTN_H], action });
        }
    }

    /// A legend entry at column `col` (0 or 14): a swatch and a word.
    fn swatch(&mut self, col: i32, kind: u8, word: &str) {
        let x = PAD + col * ADVANCE;
        let y = self.y + 1;
        match kind {
            0 => self.c.fill(x, y, 9, 9, DRAWN_SAMPLE),
            1 => self.c.fill(x, y, 9, 9, NOT_DRAWN_SAMPLE),
            2 => self.c.fill(x, y, 9, 9, HIDDEN_RED),
            3 => self.c.fill(x, y, 9, 9, ADDED_GREEN),
            4 => {
                self.c.frame(x, y, 9, 9, CAMERA_YELLOW);
                self.c.frame(x + 1, y + 1, 7, 7, CAMERA_YELLOW);
            }
            6 => self.c.fill(x, y + 3, 9, 3, ROAD_CYAN),
            7 => self.c.fill(x, y, 9, 9, ROAD_SAMPLE),
            _ => self.c.frame(x, y, 9, 9, Rgba(255, 214, 64, 170)),
        }
        self.c.text(x + 13, self.y + 2, word, LABEL);
    }
}

/// Paint the panel `h` dots tall. The legend and the key list are left out when there is no
/// room for them.
pub fn paint(info: &PanelInfo, h: i32) -> Painted {
    let mut p = Painter { c: Canvas::new(PANEL_W, h), y: 6, buttons: Vec::new(), info };
    p.c.fill(0, 0, PANEL_W, h, PANEL);
    p.c.fill(0, 0, 1, h, BORDER);
    p.c.text(PAD, p.y, "VISIBILITY EDITOR", WHITE);
    p.y += ROW + 2;
    let Some((name, id)) = &info.track else {
        p.wrapped("No track yet.", HINT, 1);
        p.wrapped("Start a race in the game window: the track appears here.", LABEL, 3);
        let essential = p.y + 4;
        keys(&mut p, h);
        let full = p.y + 4;
        return Painted { canvas: p.c, buttons: p.buttons, essential, full };
    };
    // the name, and the id (shortened when the name is long)
    let id = if name.chars().count() + 1 + id.len() <= COLS as usize { id.clone() } else { id.chars().take(8).collect() };
    p.text(name, VALUE, &id, MUTED);

    // the camera cell whose list is edited
    p.section("CAMERA CELL");
    match info.cam {
        Some(c) => {
            let how = if info.locked { ("LOCKED", HINT) } else { ("FOLLOWING", SAVED) };
            p.text(&format!("ROW {}  COL {}", c.0, c.1), WHITE, how.0, how.1);
            if info.all_in_view {
                p.text("DRAWS ALL IN VIEW", LABEL, "NO LIST", MUTED);
            } else {
                let road = if info.by_road > 0 { format!("{} BY THE ROAD", info.by_road) } else { String::new() };
                p.text(&format!("DRAWS {} SECTORS", info.draws), LABEL, &road, ROAD_SAMPLE);
            }
        }
        None => {
            p.text("NOT KNOWN YET", MUTED, "", MUTED);
            p.text("CTRL+CLICK PICKS ONE", MUTED, "", MUTED);
        }
    }
    p.buttons(&[("LOCK CAMERA CELL", "L", Action::ToggleLock, info.locked)], true);

    // the road extension: the whole track, wherever the camera is
    p.section("ROAD EXTENSION: WHOLE TRACK");
    let extendable = info.road_known && info.track_lists;
    p.buttons(&[("EXTEND", "E", Action::Extend, false), ("LESS", "SHIFT+E", Action::Less, false)], extendable);
    // ALONG +n (sectors further than the designers' lists)   WIDTH [-] w [+]
    {
        let y = p.y;
        let (label, value, look) = if extendable { (LABEL, VALUE, Look::Normal) } else { (LABEL_OFF, LABEL_OFF, Look::Disabled) };
        let width = if info.width_half % 2 == 0 { (info.width_half / 2).to_string() } else { format!("{}.5", info.width_half / 2) };
        p.c.text(PAD, y + 3, "ALONG", label);
        p.c.text(PAD + 36, y + 3, &if info.along == 0 { "OFF".to_string() } else { format!("+{}", info.along) }, value);
        p.c.text(PAD + 88, y + 3, "WIDTH", label);
        p.button(PAD + 120, y, 13, "-", "", Action::WidthDown, look);
        p.c.text_centred(PAD + 144, y + 3, &width, value);
        p.button(PAD + CONTENT_W - 13, y, 13, "+", "", Action::WidthUp, look);
        p.y += BTN_PITCH;
    }
    if !info.track_lists {
        p.text("NO LISTS: ALL IN VIEW DRAWN", MUTED, "", MUTED);
    } else if !info.road_known {
        p.text("NO ROAD LINE ON THIS TRACK", MUTED, "", MUTED);
    }

    // clicks on the map change the camera cells of the area around the camera
    let title = if info.area_cams == 1 { "CLICKS: 1 CAMERA CELL".to_string() } else { format!("CLICKS: {} CAMERA CELLS", info.area_cams) };
    p.section(&title);
    // AREA [-] n [+]   EACH WAY (0: this camera cell only)
    {
        let y = p.y;
        p.c.text(PAD, y + 3, "AREA", LABEL);
        p.button(PAD + 32, y, 13, "-", "", Action::AreaDown, Look::Normal);
        p.c.text_centred(PAD + 56, y + 3, &info.area.to_string(), VALUE);
        p.button(PAD + 67, y, 13, "+", "", Action::AreaUp, Look::Normal);
        p.c.text_right(PAD + CONTENT_W, y + 3, if info.area == 0 { "THIS CELL" } else { "EACH WAY" }, MUTED);
        p.y += BTN_PITCH;
    }
    p.buttons(&[("RESET AREA", "", Action::ResetArea, false), ("SAVE", "", Action::Save, false)], info.cam.is_some());
    p.buttons(&[("UNDO", "CTRL+Z", Action::Undo, false), ("REDO", "CTRL+Y", Action::Redo, false)], true);

    p.section("VIEW");
    p.buttons(&[("FIT", "HOME", Action::Fit, false), ("FOLLOW CAR", "F", Action::ToggleFollow, info.follow)], true);
    p.buttons(&[("GRID", "G", Action::ToggleGrid, info.grid), ("TEXTURES", "T", Action::ToggleTextures, info.textures)], true);

    p.section("SECTOR UNDER THE MOUSE");
    match &info.hover {
        Some(hv) => {
            let objects = if hv.objects == 1 { "1 OBJECT".to_string() } else { format!("{} OBJECTS", hv.objects) };
            p.text(&format!("ROW {}  COL {}", hv.cell.0, hv.cell.1), WHITE, &objects, LABEL);
            match hv.state {
                Some(s) => p.text(s.text(), s.colour(), "", WHITE),
                None => p.text("NO CAMERA CELL YET", MUTED, "", MUTED),
            }
        }
        None => {
            p.text("POINT AT THE MAP", MUTED, "", MUTED);
            p.text("", MUTED, "", MUTED);
        }
    }
    p.buttons(&[("BLINK IN THE GAME", "B", Action::ToggleBlink, info.blink)], true);

    p.section("FILE");
    let edited = if info.edited_cams == 1 { "1 CAMERA CELL EDITED".to_string() } else { format!("{} CAMERA CELLS EDITED", info.edited_cams) };
    match (&info.file, info.dirty) {
        (FileState::Error(_), _) => p.text("NOT SAVED", FAILED, "", FAILED),
        (_, true) => p.text("UNSAVED CHANGES", HINT, "", HINT),
        (FileState::Saved(_), false) => p.text("SAVED", SAVED, "", SAVED),
        (FileState::None, false) => p.text("NOTHING TO SAVE YET", MUTED, "", MUTED),
    }
    p.text(&edited, LABEL, "", LABEL);
    match &info.file {
        FileState::Error(e) => p.text(e, FAILED, "", FAILED),
        FileState::Saved(path) => {
            // the file's name (the folder is <data>\visibility; the status line shows all)
            let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
            p.text(name, MUTED, "", MUTED)
        }
        FileState::None => p.text("", MUTED, "", MUTED),
    }
    let essential = p.y + 4;
    // the legend, then the keys, when they fit
    if p.y + SECTION_GAP + ROW * 5 + 4 <= h {
        p.section("ON THE MAP");
        p.swatch(0, 0, "DESIGNERS");
        p.swatch(14, 7, "BY THE ROAD");
        p.y += ROW;
        p.swatch(0, 3, "YOU ADDED");
        p.swatch(14, 2, "YOU HID");
        p.y += ROW;
        p.swatch(0, 1, "NOT DRAWN");
        p.swatch(14, 4, "CAMERA CELL");
        p.y += ROW;
        p.swatch(0, 6, "ROAD'S LINE");
        p.swatch(14, 5, "CLICK AREA");
        p.y += ROW;
    }
    keys(&mut p, h);
    let full = p.y + 4;
    Painted { canvas: p.c, buttons: p.buttons, essential, full }
}

/// The mouse and key help, as many lines as fit (all or nothing for the mouse part).
fn keys(p: &mut Painter, h: i32) {
    const KEYS: [(&str, &str); 12] = [
        ("LEFT CLICK/DRAG", "DRAW"),
        ("RIGHT CLICK/DRAG", "HIDE"),
        ("MIDDLE CLICK", "DEFAULT"),
        ("MIDDLE DRAG  ARROWS", "MOVE MAP"),
        ("WHEEL  + -", "ZOOM"),
        ("CTRL+CLICK", "PICK CAMERA CELL"),
        ("CTRL+Z  CTRL+Y", "UNDO  REDO"),
        ("CTRL+S", "SAVE"),
        ("E  SHIFT+E", "ROAD: FURTHER  LESS"),
        ("<  >", "ROAD: WIDTH"),
        ("[  ]", "CLICK AREA"),
        ("BACKSPACE", "RESET THE AREA"),
    ];
    if p.y + SECTION_GAP + ROW * (1 + KEYS.len() as i32) + 4 > h {
        return;
    }
    p.section("MOUSE AND KEYS");
    for (k, what) in KEYS {
        p.c.text(PAD, p.y + 2, k, VALUE);
        p.c.text_right(PAD + CONTENT_W, p.y + 2, what, LABEL);
        p.y += ROW;
    }
}

/// The status line, `w` dots wide: a message on the left, `right` on the right.
pub fn paint_status(w: i32, left: &str, left_colour: Rgba, right: &str) -> Canvas {
    let mut c = Canvas::new(w, STATUS_H);
    c.fill(0, 0, w, STATUS_H, PANEL);
    c.fill(0, 0, w, 1, BORDER);
    let cols = ((w - 2 * 6) / ADVANCE).max(1) as usize;
    let right = font::fit(&printable(right), cols / 2);
    let room = cols.saturating_sub(right.chars().count() + 2);
    c.text(6, 4, &font::fit(&printable(left), room), left_colour);
    c.text_right(w - 6, 4, &right, MUTED);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PanelInfo {
        PanelInfo {
            track: Some(("Rally China".into(), "1a2b3c4d5e6f7788".into())),
            cam: Some((120, 88)),
            draws: 143,
            by_road: 12,
            along: 6,
            road_known: true,
            hover: Some(HoverInfo { cell: (121, 88), objects: 14, state: Some(SectorState::Hidden) }),
            edited_cams: 3,
            file: FileState::Saved("D:\\data\\visibility\\1a2b3c4d5e6f7788.vis".into()),
            ..Default::default()
        }
    }

    #[test]
    fn every_action_has_a_button() {
        let p = paint(&info(), 1000);
        for a in Action::ALL {
            if matches!(a, Action::ZoomIn | Action::ZoomOut) {
                continue; // wheel and + / -
            }
            assert!(p.button(a).is_some(), "{a:?}");
        }
        // buttons do not overlap and stay inside the panel
        for (i, a) in p.buttons.iter().enumerate() {
            let r = a.rect;
            assert!(r[0] >= 0 && r[1] >= 0 && r[0] + r[2] <= PANEL_W && r[1] + r[3] <= p.full, "{a:?}");
            for b in &p.buttons[i + 1..] {
                let s = b.rect;
                let apart = r[0] + r[2] <= s[0] || s[0] + s[2] <= r[0] || r[1] + r[3] <= s[1] || s[1] + s[3] <= r[1];
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        // hit test: the centre of each button finds it
        for b in &p.buttons {
            assert_eq!(p.button_at(b.rect[0] + b.rect[2] / 2, b.rect[1] + b.rect[3] / 2), Some(b.action));
        }
        assert_eq!(p.button_at(-5, 3), None);
    }

    #[test]
    fn optional_parts_drop_out_when_short() {
        let tall = paint(&info(), 1000);
        assert!(tall.full > tall.essential + 60, "legend and keys shown");
        let short = paint(&info(), tall.essential + 2);
        assert!(short.full <= tall.essential + 6, "{} {}", short.full, tall.essential);
        // the essential part fits a 720-line window at scale 2 (with the status line)
        assert!(tall.essential + STATUS_H <= 360, "{}", tall.essential);
        // without a track: the message, the keys, no edit buttons
        let none = paint(&PanelInfo::default(), 600);
        assert!(none.buttons.is_empty());
        assert!(none.canvas.painted(PAD, 19, CONTENT_W, 30) > 50);
    }

    #[test]
    fn texts_fit_and_have_glyphs() {
        // every fixed text of the panel has glyphs (a missing one would show as a box)
        for s in ["VISIBILITY EDITOR", "CAMERA CELL", "ROAD EXTENSION: WHOLE TRACK", "CLICKS: 123 CAMERA CELLS", "NO ROAD LINE ON THIS TRACK", "THIS CELL", "EACH WAY", "CLICK AREA", "VIEW", "SECTOR UNDER THE MOUSE", "FILE", "ON THE MAP", "MOUSE AND KEYS", "LOCK CAMERA CELL", "BLINK IN THE GAME", "ROAD'S LINE", "CTRL+Z  CTRL+Y", "DRAW / HIDE", "E  SHIFT+E", "ROAD: FURTHER  LESS"] {
            assert!(s.chars().all(|c| font::find(c).is_some()), "{s}");
            assert!(s.chars().count() as i32 <= COLS, "{s}");
        }
        for a in Action::ALL {
            assert!(a.help().chars().all(|c| font::find(c).is_some()), "{}", a.help());
        }
        for s in [SectorState::DrawnByDefault, SectorState::ByTheRoad, SectorState::Added, SectorState::Hidden, SectorState::NotDrawn] {
            assert!(s.text().chars().count() as i32 <= COLS);
        }
        // the status line cuts what does not fit
        let c = paint_status(200, "a very long message that does not fit at all in the line", WHITE, "CELL 1,2");
        assert_eq!((c.w, c.h), (200, STATUS_H));
    }
}
