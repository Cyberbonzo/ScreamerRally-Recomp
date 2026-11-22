//! The editor's state and behaviour: what the window's input (or the test script) does, the
//! shared edits it changes (`sr_runtime::visedit`), and what the renderer draws (layout,
//! sector states, shapes, panel, status line).

use std::sync::Arc;
use std::time::{Duration, Instant};

use sr_runtime::visedit::{self, Cell, Edits, GRID, Live, RoadReach, TrackMap};

use crate::action::Action;
use crate::canvas::{self, Canvas, Rgba};
use crate::panel::{self, FileState, HoverInfo, PanelInfo, SectorState};
use crate::scope;
use crate::shapes::Shapes;
use crate::view::{self, MapView, ZOOM_STEP};

/// Mouse buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    Left,
    Right,
    Middle,
}

/// Input, in window (client) pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Move(f64, f64),
    Leave,
    Down { btn: Btn, x: f64, y: f64, ctrl: bool },
    Up { btn: Btn, x: f64, y: f64 },
    /// Wheel notches (positive: away from the user, zoom in) at a window pixel.
    Wheel { notches: f64, x: f64, y: f64 },
    /// A key (Windows virtual-key code).
    Key { vk: u32, ctrl: bool, shift: bool, repeat: bool },
    /// A typed character (`+`, `-`, `<`, `>`).
    Char(char),
    Resize(u32, u32),
    Dpi(u32),
}

pub const VK_BACK: u32 = 0x08;
pub const VK_HOME: u32 = 0x24;

/// Sector states in the state texture (`map.hlsl`).
pub const S_NOT_DRAWN: u8 = 0;
pub const S_DRAWN: u8 = 1;
pub const S_ADDED: u8 = 2;
pub const S_HIDDEN: u8 = 3;
pub const S_ROAD: u8 = 4;

/// The road's line on the map (as the panel's legend shows it).
const ROAD_CYAN: [u8; 4] = [panel::ROAD_CYAN.0, panel::ROAD_CYAN.1, panel::ROAD_CYAN.2, 255];

/// A number of sectors as words ("1 sector", "1.5 sectors").
fn sectors(n: f64) -> String {
    if n == 1.0 { "1 sector".into() } else { format!("{n} sectors") }
}

/// A number of camera cells as words.
fn cams_text(n: usize) -> String {
    if n == 1 { "1 camera cell".into() } else { format!("{n} camera cells") }
}

/// The road extension in words.
fn road_text(r: RoadReach) -> String {
    if r.along == 0 {
        "off (the designers' lists)".into()
    } else {
        format!("{} further along the road than the designers' lists, {} each side", sectors(r.along as f64), sectors(r.width_half as f64 * 0.5))
    }
}

/// The edit area: a click changes the camera cells within this many sectors along the road
/// each way (`visedit::road_area`), by default and at most.
pub const DEFAULT_AREA: u32 = 4;
pub const MAX_AREA: u32 = 16;

/// Pixels the mouse may move with a button down and still click.
const CLICK_SLOP: f64 = 4.0;
/// Autosave this long after the last change.
const AUTOSAVE: Duration = Duration::from_secs(1);
/// After a failed save, try again this much later.
const SAVE_RETRY: Duration = Duration::from_secs(5);
/// A message stays in the status line this long (then the hints come back).
const MESSAGE_TIME: Duration = Duration::from_secs(5);
/// Grid lines once a cell is this many pixels.
const GRID_MIN_PX: f64 = 14.0;

/// Where things are in the window (pixels): x, y, w, h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: [u32; 2],
    /// Window pixels per dot of the panel and status line.
    pub scale: i32,
    pub map: [i32; 4],
    pub panel: [i32; 4],
    pub status: [i32; 4],
}

impl Layout {
    /// The layout of a `w` x `h` window at `dpi`, the panel's essential part `essential`
    /// dots tall: the panel's scale follows the DPI (2 at 100 %, 3 at 150 %, 4 at 200 %), less
    /// when the window is too small for it.
    pub fn new(w: u32, h: u32, dpi: u32, essential: i32) -> Layout {
        let want = ((2.0 * dpi.max(96) as f64 / 96.0).round() as i32).max(1);
        // the largest scale the essential part fits; a window a little short for scale 2 cuts
        // the end of the panel (the file section) rather than going down to scale 1 (tiny)
        let fits = |s: i32, part: i32| panel::PANEL_W * s * 2 <= w as i32 && (part + panel::STATUS_H) * s <= h as i32;
        let scale = (2..=want).rev().find(|&s| fits(s, essential)).or_else(|| (2..=want).rev().find(|&s| fits(s, essential * 3 / 4))).unwrap_or(1);
        let (w, h) = (w as i32, h as i32);
        let status_h = panel::STATUS_H * scale;
        let panel_w = (panel::PANEL_W * scale).min(w);
        let body = (h - status_h).max(0);
        Layout { size: [w as u32, h as u32], scale, map: [0, 0, (w - panel_w).max(0), body], panel: [w - panel_w, 0, panel_w, body], status: [0, body, w, status_h.min(h)] }
    }

    pub fn in_map(&self, x: f64, y: f64) -> bool {
        inside(self.map, x, y)
    }

    pub fn in_panel(&self, x: f64, y: f64) -> bool {
        inside(self.panel, x, y)
    }
}

fn inside(r: [i32; 4], x: f64, y: f64) -> bool {
    x >= r[0] as f64 && y >= r[1] as f64 && x < (r[0] + r[2]) as f64 && y < (r[1] + r[3]) as f64
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    /// Left (draw) or right (hide) button on the map: a click or a rectangle. `target`: the
    /// state the sectors take; `btn`: the button that started it.
    Select { start: [f64; 2], now: [f64; 2], first: Cell, target: bool, moved: bool, btn: Btn },
    /// Middle button: moving the map (a middle click when it did not move: back to default).
    Pan { start: [f64; 2], last: [f64; 2], moved: bool, right: bool },
}

/// The mouse pointer the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Arrow,
    Cross,
    Hand,
    Move,
}

/// The race's map and what the editor derives from it.
struct Race {
    map: Arc<TrackMap>,
    /// Statics per cell (`row * 256 + column`).
    objects: Vec<u32>,
}

pub struct Editor {
    race: Option<Race>,
    /// Tracks received so far.
    races: usize,
    edits: Edits,
    version: u64,
    live: Live,
    pub view: MapView,
    locked: Option<Cell>,
    /// The edit area (sectors along the road each way) and its camera cells (for the map).
    area: u32,
    area_cells: Vec<Cell>,
    follow: bool,
    blink: bool,
    grid: bool,
    textures: bool,
    dpi: u32,
    pub layout: Layout,
    mouse: Option<[f64; 2]>,
    hover: Option<Cell>,
    hot: Option<Action>,
    drag: Drag,
    last_edit: Option<Instant>,
    retry_save: Option<Instant>,
    file: FileState,
    message: Option<(String, Rgba, Instant)>,
    last_tick: Option<Instant>,
    /// The sector states for the camera cell (`S_*`, `row * 256 + column`), and for which
    /// (camera cell, edits version) they were made.
    states: Vec<u8>,
    states_for: Option<(Option<Cell>, u64)>,
    draws: usize,
    /// The camera cell has no list within reach and no edits: the game draws everything in
    /// view.
    all_in_view: bool,
    /// The road extension of the camera cell (what it adds, the stretches it follows).
    road_ext: Option<visedit::RoadExtension>,
    panel: Option<panel::Painted>,
    /// The renderer must rebuild the map's buffers / upload the states.
    pub map_changed: bool,
    pub states_changed: bool,
    redraw: bool,
}

impl Editor {
    pub fn new(w: u32, h: u32, dpi: u32) -> Editor {
        let mut e = Editor {
            race: None,
            races: 0,
            edits: Edits::default(),
            version: u64::MAX,
            live: Live::default(),
            view: MapView::default(),
            locked: None,
            area: DEFAULT_AREA,
            area_cells: Vec::new(),
            follow: true,
            blink: true,
            grid: true,
            textures: true,
            dpi,
            layout: Layout::new(w, h, dpi, 340),
            mouse: None,
            hover: None,
            hot: None,
            drag: Drag::None,
            last_edit: None,
            retry_save: None,
            file: FileState::None,
            message: None,
            last_tick: None,
            states: vec![S_DRAWN; GRID * GRID],
            states_for: None,
            draws: 0,
            all_in_view: false,
            road_ext: None,
            panel: None,
            map_changed: false,
            states_changed: true,
            redraw: true,
        };
        e.relayout(w, h);
        e
    }

    /// How many tracks the editor has received.
    pub fn races(&self) -> usize {
        self.races
    }

    pub fn map(&self) -> Option<&Arc<TrackMap>> {
        self.race.as_ref().map(|r| &r.map)
    }

    pub fn textures(&self) -> bool {
        self.textures
    }

    pub fn states(&self) -> &[u8] {
        &self.states
    }

    /// The cell under the mouse (for the map's shader: brighter).
    pub fn hover(&self) -> Option<Cell> {
        self.hover
    }

    /// Whether the sectors are tinted (there is an editing camera cell).
    pub fn tinted(&self) -> bool {
        self.cam().is_some()
    }

    /// Something changed since the last frame drawn (cleared by [`Editor::drawn`]).
    pub fn needs_redraw(&self) -> bool {
        self.redraw || self.map_changed || self.states_changed
    }

    pub fn drawn(&mut self) {
        self.redraw = false;
    }

    /// Something moves (a drag, the car followed): look again soon.
    pub fn busy(&self) -> bool {
        self.drag != Drag::None || (self.follow && self.live.frame > 0)
    }

    pub fn mouse_inside(&self) -> bool {
        self.mouse.is_some()
    }

    /// The file last saved in this session.
    pub fn saved_file(&self) -> Option<String> {
        match &self.file {
            FileState::Saved(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// The state in the log (the test script's `log`).
    pub fn log_state(&self) {
        let e = &self.edits;
        let cams: Vec<String> = e.cams.iter().take(12).map(|(c, ce)| format!("{},{}: +{} -{}", c.0, c.1, ce.add.len(), ce.remove.len())).collect();
        sr_runtime::info!(
            "visibility editor: track {:?}, camera cell {:?} ({}), edit area {} sectors ({} camera cells), road extension {}, draws {} ({} by the road), view centre ({:.0}, {:.0}) {:.1} px per sector, {} camera cells edited [{}{}], unsaved {}, live frame {}",
            self.race.as_ref().map(|r| r.map.name.as_str()),
            self.cam(),
            if self.locked.is_some() { "locked" } else { "following" },
            self.area,
            self.area_cells.len(),
            road_text(self.edits.road),
            self.draws,
            self.road_ext.as_ref().map_or(0, |r| r.cells.len()),
            self.view.center[0],
            self.view.center[1],
            self.view.cell_px(),
            e.cams.len(),
            cams.join("; "),
            if e.cams.len() > 12 { "; ..." } else { "" },
            visedit::dirty(),
            self.live.frame
        );
        if let (Some(r), Some(cam)) = (self.race.as_ref(), self.cam()) {
            match self.edits.effective(&r.map, cam) {
                Some(l) => {
                    let eff: Vec<String> = l.iter().map(|&e| visedit::cell_of_entry(e)).map(|c| format!("{},{}", c.0, c.1)).collect();
                    sr_runtime::info!("visibility editor: camera cell {},{} draws [{}]", cam.0, cam.1, eff.join(" "));
                }
                None => sr_runtime::info!("visibility editor: camera cell {},{} draws all in view (no list)", cam.0, cam.1),
            }
            let near: Vec<String> = (cam.0.saturating_sub(6)..=(cam.0 + 6).min(255))
                .flat_map(|r| (cam.1.saturating_sub(6)..=(cam.1 + 6).min(255)).map(move |c| (r, c)))
                .filter(|&c| self.objects(c) > 0)
                .map(|c| format!("{},{}:{}", c.0, c.1, self.objects(c)))
                .collect();
            sr_runtime::info!("visibility editor: sectors with objects within 6 of it [{}]", near.join(" "));
        }
        let l = &self.live;
        sr_runtime::info!("visibility editor: live camera {:?} forward {:?}, car {:?} forward {:?}", l.cam_pos, l.cam_forward, l.car_pos, l.car_forward);
    }

    // ---- the camera cell and the scope ----

    /// The camera cell whose list is edited: the locked one, else the game's camera's.
    pub fn cam(&self) -> Option<Cell> {
        self.locked.or_else(|| (self.race.is_some() && self.live.frame > 0).then_some(self.live.cam_cell))
    }

    /// The camera cells a click changes: those within the edit area along the road of the
    /// editing camera cell (`visedit::road_area`).
    pub fn area_cams(&self) -> Vec<Cell> {
        match (self.race.as_ref(), self.cam()) {
            (Some(r), Some(cam)) => visedit::road_area(&r.map, cam, self.area as f64 * visedit::CELL_SIZE),
            _ => Vec::new(),
        }
    }

    fn objects(&self, c: Cell) -> u32 {
        self.race.as_ref().map_or(0, |r| r.objects[c.0 as usize * GRID + c.1 as usize])
    }

    /// A sector's state from the editing camera cell.
    pub fn state(&self, c: Cell) -> Option<SectorState> {
        let r = self.race.as_ref()?;
        let cam = self.cam()?;
        Some(match self.edits.edit_state(cam, c) {
            Some(true) => SectorState::Added,
            Some(false) => SectorState::Hidden,
            None if visedit::default_cells(&r.map, cam).contains(&visedit::entry(c)) => SectorState::DrawnByDefault,
            None if visedit::road_extension(&r.map, cam, self.edits.road).is_some_and(|x| x.cells.contains(&c)) => SectorState::ByTheRoad,
            None => SectorState::NotDrawn,
        })
    }

    fn say(&mut self, msg: impl Into<String>, colour: Rgba) {
        let msg = msg.into();
        sr_runtime::info!("visibility editor: {msg}");
        self.message = Some((msg, colour, Instant::now()));
        self.redraw = true;
    }

    // ---- input ----

    pub fn handle(&mut self, ev: Event) {
        self.redraw = true;
        match ev {
            Event::Resize(w, h) => self.relayout(w, h),
            Event::Dpi(d) => {
                self.dpi = d;
                let [w, h] = self.layout.size;
                self.relayout(w, h);
            }
            Event::Move(x, y) => self.mouse_move(x, y),
            Event::Leave => {
                self.mouse = None;
                self.hot = None;
                self.set_hover(None);
            }
            Event::Down { btn, x, y, ctrl } => self.mouse_down(btn, x, y, ctrl),
            Event::Up { btn, x, y } => self.mouse_up(btn, x, y),
            Event::Wheel { notches, x, y } => {
                if self.layout.in_map(x, y) && self.race.is_some() {
                    let (mx, my) = self.to_map(x, y);
                    self.view.zoom_at(mx, my, ZOOM_STEP.powf(notches));
                    self.mouse_move(x, y);
                }
            }
            Event::Key { vk, ctrl, shift, repeat } => self.key(vk, ctrl, shift, repeat),
            Event::Char(c) => match c {
                '+' | '=' => self.act(Action::ZoomIn),
                '-' | '_' => self.act(Action::ZoomOut),
                '<' => self.act(Action::WidthDown),
                '[' => self.act(Action::AreaDown),
                ']' => self.act(Action::AreaUp),
                '>' => self.act(Action::WidthUp),
                _ => {}
            },
        }
    }

    fn key(&mut self, vk: u32, ctrl: bool, shift: bool, repeat: bool) {
        let action = match (vk, ctrl) {
            (0x5a, true) if shift => Some(Action::Redo), // Ctrl+Shift+Z
            (0x5a, true) => Some(Action::Undo),
            (0x59, true) => Some(Action::Redo),
            (0x53, true) => Some(Action::Save),
            (_, true) => None,
            (0x4c, _) => Some(Action::ToggleLock),
            (0x46, _) => Some(Action::ToggleFollow),
            (0x45, _) if shift => Some(Action::Less),
            (0x45, _) => Some(Action::Extend),
            (0x42, _) => Some(Action::ToggleBlink),
            (0x47, _) => Some(Action::ToggleGrid),
            (0x54, _) => Some(Action::ToggleTextures),
            (VK_HOME, _) => Some(Action::Fit),
            (VK_BACK, _) => Some(Action::ResetArea),
            (0x25..=0x28, _) => {
                // arrow keys: move the map by a fifth of its size (repeats while held)
                let (w, h) = (self.layout.map[2] as f64 * 0.2, self.layout.map[3] as f64 * 0.2);
                let (dx, dy) = match vk {
                    0x25 => (w, 0.0),
                    0x27 => (-w, 0.0),
                    0x26 => (0.0, h),
                    _ => (0.0, -h),
                };
                self.view.pan(dx, dy);
                if self.follow {
                    self.follow = false;
                    self.say("Follow car off (F to follow again)", canvas::MUTED);
                }
                self.redraw = true;
                None
            }
            _ => None,
        };
        if let Some(a) = action {
            // held keys repeat only what makes sense repeated
            if !repeat || matches!(a, Action::Undo | Action::Redo) {
                self.act(a);
            }
        }
    }

    fn to_map(&self, x: f64, y: f64) -> (f64, f64) {
        (x - self.layout.map[0] as f64, y - self.layout.map[1] as f64)
    }

    fn cell_at(&self, x: f64, y: f64) -> Option<Cell> {
        if !self.layout.in_map(x, y) || self.race.is_none() {
            return None;
        }
        let (mx, my) = self.to_map(x, y);
        self.view.cell_at(mx, my)
    }

    fn panel_button(&self, x: f64, y: f64) -> Option<Action> {
        if !self.layout.in_panel(x, y) {
            return None;
        }
        let s = self.layout.scale as f64;
        let p = self.panel.as_ref()?;
        p.button_at(((x - self.layout.panel[0] as f64) / s).floor() as i32, ((y - self.layout.panel[1] as f64) / s).floor() as i32)
    }

    /// The centre of a panel button in window pixels (the test script clicks it).
    pub fn button_center(&self, a: Action) -> Option<[f64; 2]> {
        let b = self.panel.as_ref()?.button(a)?;
        let s = self.layout.scale as f64;
        Some([self.layout.panel[0] as f64 + (b.rect[0] as f64 + b.rect[2] as f64 * 0.5) * s, self.layout.panel[1] as f64 + (b.rect[1] as f64 + b.rect[3] as f64 * 0.5) * s])
    }

    /// Centre the map on a cell, `px` pixels per sector (0: the zoom as it is); follow car off
    /// (the test script).
    pub fn center_on(&mut self, c: Cell, px: f64) {
        self.view.center = view::cell_center(c);
        if px > 0.0 {
            self.view.scale = (px / visedit::CELL_SIZE).clamp(view::MIN_SCALE, view::MAX_SCALE);
        }
        self.follow = false;
        self.redraw = true;
    }

    /// Window pixel of a world point (the test script points at the map with it).
    pub fn world_to_window(&self, x: f64, z: f64) -> [f64; 2] {
        let p = self.view.to_screen(x, z);
        [p[0] + self.layout.map[0] as f64, p[1] + self.layout.map[1] as f64]
    }

    fn set_hover(&mut self, c: Option<Cell>) {
        if self.hover != c {
            self.hover = c;
            visedit::set_highlight(c, self.blink);
        }
    }

    fn mouse_move(&mut self, x: f64, y: f64) {
        self.mouse = Some([x, y]);
        match &mut self.drag {
            Drag::Select { start, now, moved, .. } => {
                *now = [x, y];
                if (x - start[0]).hypot(y - start[1]) > CLICK_SLOP {
                    *moved = true;
                }
            }
            Drag::Pan { start, last, moved, .. } => {
                if !*moved && (x - start[0]).hypot(y - start[1]) > CLICK_SLOP {
                    *moved = true;
                    // a drag: the map follows from where the button went down
                    *last = *start;
                }
                if *moved {
                    let (dx, dy) = (x - last[0], y - last[1]);
                    *last = [x, y];
                    self.view.pan(dx, dy);
                    if self.follow {
                        self.follow = false;
                        self.say("Follow car off (F to follow again)", canvas::MUTED);
                    }
                }
            }
            Drag::None => {}
        }
        self.hot = self.panel_button(x, y);
        let c = self.cell_at(x, y);
        self.set_hover(c);
    }

    fn mouse_down(&mut self, btn: Btn, x: f64, y: f64, ctrl: bool) {
        self.mouse_move(x, y);
        if btn == Btn::Left {
            if let Some(a) = self.panel_button(x, y) {
                self.act(a);
                return;
            }
        }
        if !self.layout.in_map(x, y) || self.race.is_none() {
            return;
        }
        match btn {
            // left: draw, right: hide (a click: the sector; a drag: a rectangle)
            Btn::Left | Btn::Right => {
                let Some(cell) = self.cell_at(x, y) else { return };
                if ctrl && btn == Btn::Left {
                    self.locked = Some(cell);
                    self.say(format!("Editing the list of camera cell {}, {} (locked; L follows the game's camera again)", cell.0, cell.1), canvas::HINT);
                    return;
                }
                match self.state(cell) {
                    Some(_) => self.drag = Drag::Select { start: [x, y], now: [x, y], first: cell, target: btn == Btn::Left, moved: false, btn },
                    None => self.say("No camera cell yet: drive in the game, or Ctrl+click a sector to pick one", canvas::HINT),
                }
            }
            Btn::Middle => self.drag = Drag::Pan { start: [x, y], last: [x, y], moved: false, right: false },
        }
    }

    fn mouse_up(&mut self, btn: Btn, x: f64, y: f64) {
        self.mouse_move(x, y);
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        match drag {
            Drag::Select { start, now, first, target, moved, btn: b } if btn == b => {
                if !moved {
                    if self.objects(first) == 0 {
                        self.say(format!("Sector {}, {} has no objects", first.0, first.1), canvas::MUTED);
                    } else {
                        self.set_cells(&[first], target);
                    }
                } else {
                    let (ax, ay) = self.to_map(start[0], start[1]);
                    let (bx, by) = self.to_map(now[0], now[1]);
                    let cells: Vec<Cell> = scope::cells_in_rect(self.view.to_world(ax, ay), self.view.to_world(bx, by)).into_iter().filter(|&c| self.objects(c) > 0).collect();
                    self.set_cells(&cells, target);
                }
            }
            Drag::Pan { moved, .. } if btn == Btn::Middle => {
                // a middle click (no drag): the sector back to its default
                if !moved {
                    if let Some(c) = self.cell_at(x, y) {
                        self.reset_sector(c);
                    }
                }
            }
            // another button let go: the drag goes on
            other => self.drag = other,
        }
    }

    // ---- edits ----

    fn set_cells(&mut self, cells: &[Cell], drawn: bool) {
        let cams = self.area_cams();
        if cams.is_empty() {
            return;
        }
        if cells.is_empty() {
            self.say("No sector with objects in the rectangle", canvas::MUTED);
            return;
        }
        let changed = visedit::apply(&cams, cells, drawn);
        self.sync();
        let what = if cells.len() == 1 { format!("Sector {}, {}", cells[0].0, cells[0].1) } else { format!("{} sectors", cells.len()) };
        let how = if drawn { "drawn" } else { "hidden" };
        if changed {
            self.say(format!("{what} {how} from {} (the road {} each way around you)", cams_text(cams.len()), sectors(self.area as f64)), if drawn { panel::ADDED_GREEN } else { panel::HIDDEN_RED });
        } else {
            self.say(format!("{what} already {how}"), canvas::MUTED);
        }
    }

    fn reset_sector(&mut self, cell: Cell) {
        let cams = self.area_cams();
        if cams.is_empty() {
            return;
        }
        if visedit::reset(&cams, &[cell]) {
            self.sync();
            self.say(format!("Sector {}, {} back to the designers' lists and the road, from {} around you", cell.0, cell.1, cams_text(cams.len())), canvas::WHITE);
        } else {
            self.say(format!("Sector {}, {} is at its default", cell.0, cell.1), canvas::MUTED);
        }
    }

    pub fn act(&mut self, a: Action) {
        self.redraw = true;
        let center = [self.layout.map[2] as f64 * 0.5, self.layout.map[3] as f64 * 0.5];
        match a {
            Action::ToggleLock => match self.locked {
                Some(_) => {
                    self.locked = None;
                    self.say("Following the game's camera cell", canvas::SAVED);
                }
                None => match self.cam() {
                    Some(c) => {
                        self.locked = Some(c);
                        self.say(format!("Camera cell {}, {} locked", c.0, c.1), canvas::HINT);
                    }
                    None => self.say("No camera cell to lock yet: Ctrl+click a sector to pick one", canvas::HINT),
                },
            },
            Action::ToggleFollow => {
                self.follow = !self.follow;
                self.say(if self.follow { "Following the car" } else { "Follow car off" }, canvas::WHITE);
            }
            Action::Fit => self.fit(),
            Action::ZoomIn => self.view.zoom_at(center[0], center[1], ZOOM_STEP),
            Action::ZoomOut => self.view.zoom_at(center[0], center[1], 1.0 / ZOOM_STEP),
            Action::Extend | Action::Less => {
                let mut r = self.edits.road;
                r.along = if a == Action::Extend { (r.along + visedit::ALONG_STEP).min(visedit::MAX_ALONG) } else { r.along.saturating_sub(visedit::ALONG_STEP) };
                self.set_road(r);
            }
            Action::WidthDown | Action::WidthUp => {
                let mut r = self.edits.road;
                r.width_half = if a == Action::WidthUp { (r.width_half + 1).min(visedit::MAX_WIDTH_HALF) } else { r.width_half.saturating_sub(1) };
                self.set_road(r);
            }
            Action::AreaDown | Action::AreaUp => {
                self.area = if a == Action::AreaUp { (self.area + 1).min(MAX_AREA) } else { self.area.saturating_sub(1) };
                self.area_cells = self.area_cams();
                let msg = if self.area == 0 { "Clicks change this camera cell only".to_string() } else { format!("Clicks change the road {} each way around you ({})", sectors(self.area as f64), cams_text(self.area_cells.len())) };
                self.say(msg, canvas::WHITE);
            }
            Action::ResetArea => {
                let cams = self.area_cams();
                if cams.is_empty() {
                    self.say("No camera cell yet", canvas::MUTED);
                } else if visedit::reset_cameras(&cams) {
                    self.sync();
                    self.say(format!("Your clicks in {} around you dropped: the designers' lists and the road again", cams_text(cams.len())), canvas::WHITE);
                } else {
                    self.say("No clicks of yours in the area around you", canvas::MUTED);
                }
            }
            Action::Undo => {
                if visedit::undo() {
                    self.sync();
                    self.say("Undone", canvas::WHITE);
                } else {
                    self.say("Nothing to undo", canvas::MUTED);
                }
            }
            Action::Redo => {
                if visedit::redo() {
                    self.sync();
                    self.say("Redone", canvas::WHITE);
                } else {
                    self.say("Nothing to redo", canvas::MUTED);
                }
            }
            Action::Save => self.save(),
            Action::ToggleBlink => {
                self.blink = !self.blink;
                visedit::set_highlight(self.hover, self.blink);
                self.say(if self.blink { "The sector under the mouse blinks in the game" } else { "No blinking in the game" }, canvas::WHITE);
            }
            Action::ToggleGrid => self.grid = !self.grid,
            Action::ToggleTextures => {
                self.textures = !self.textures;
                self.say(if self.textures { "Textures" } else { "Flat colours" }, canvas::WHITE);
            }
        }
    }

    /// The road extension changed (whole track, one undo step): what it does now, said with
    /// what the camera cell gains.
    fn set_road(&mut self, r: RoadReach) {
        if self.race.as_ref().is_none_or(|x| x.map.road.is_none()) {
            self.say("This track's road line is not known: no road extension", canvas::MUTED);
            return;
        }
        let before = self.edits.road;
        if r == before {
            let msg = if r.along == 0 && before.along == 0 { "Already the designers' lists (E: draw further along the road)" } else { "That is as far as it goes" };
            self.say(msg, canvas::MUTED);
            return;
        }
        visedit::set_road(r);
        self.sync();
        self.compute_states();
        let here = self.road_ext.as_ref().map_or(0, |x| x.cells.len());
        let msg = if r.along == 0 {
            "Road extension off: the designers' lists everywhere".to_string()
        } else {
            format!("Road extension: {} (this camera cell: {here} sectors more; everywhere on the track as you drive)", road_text(r))
        };
        self.say(msg, panel::ROAD_TEXT);
    }

    fn save(&mut self) {
        if self.race.is_none() {
            self.say("No track to save", canvas::MUTED);
            return;
        }
        match visedit::save() {
            Ok(p) => {
                // the file's name first (the status line may cut the end)
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
                self.say(format!("Saved {name} in {dir}"), canvas::SAVED);
                self.file = FileState::Saved(p.display().to_string());
                self.retry_save = None;
            }
            Err(e) => {
                self.say(format!("Cannot save: {e}"), canvas::FAILED);
                self.file = FileState::Error(e);
                self.retry_save = Some(Instant::now() + SAVE_RETRY);
            }
        }
        self.sync();
    }

    /// Save now if there are unsaved changes (the window is closing). Returns an error to
    /// show.
    pub fn save_before_closing(&mut self) -> Result<(), String> {
        if self.race.is_some() && visedit::dirty() {
            self.save();
            if let FileState::Error(e) = &self.file {
                return Err(e.clone());
            }
        }
        Ok(())
    }

    fn fit(&mut self) {
        if let Some(r) = &self.race {
            let b = r.map.bounds;
            if b[2] > b[0] {
                self.view.fit([b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64]);
            }
        }
    }

    /// Read the shared edits again.
    fn sync(&mut self) {
        let v = visedit::version();
        let e = visedit::edits();
        if e != self.edits {
            self.last_edit = Some(Instant::now());
            self.edits = e;
        }
        self.version = v;
        self.redraw = true;
    }

    fn relayout(&mut self, w: u32, h: u32) {
        let essential = panel::paint(&self.panel_info(), 2000).essential;
        self.layout = Layout::new(w.max(1), h.max(1), self.dpi, essential);
        let m = self.layout.map;
        // the world point at the map's centre stays there
        self.view.size = [m[2].max(1) as f64, m[3].max(1) as f64];
        self.panel = None;
        self.redraw = true;
    }

    // ---- once per loop ----

    /// Follow the shared state: a new race, edits changed elsewhere, the camera and the car;
    /// keep the car in view; save a second after the last change.
    pub fn tick(&mut self, now: Instant) {
        let dt = self.last_tick.map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last_tick = Some(now);
        if let Some(m) = visedit::map() {
            if self.race.as_ref().is_none_or(|r| !Arc::ptr_eq(&r.map, &m)) {
                self.new_race(m);
            }
        }
        if visedit::version() != self.version {
            self.sync();
        }
        let live = visedit::live();
        if live != self.live {
            self.live = live;
            self.redraw = true;
        }
        if self.follow && self.race.is_some() && self.live.frame > 0 && self.drag == Drag::None && self.view.follow([self.live.car_pos[0] as f64, self.live.car_pos[2] as f64], dt.min(0.2)) {
            self.redraw = true;
            if let Some([x, y]) = self.mouse {
                let c = self.cell_at(x, y);
                self.set_hover(c);
            }
        }
        // the sector states for the camera cell
        let key = (self.cam(), self.version);
        if self.states_for != Some(key) {
            self.states_for = Some(key);
            self.compute_states();
        }
        // autosave
        if self.race.is_some() && visedit::dirty() {
            let due = self.last_edit.is_none_or(|t| now.duration_since(t) >= AUTOSAVE);
            if due && self.retry_save.is_none_or(|t| now >= t) {
                self.save();
            }
        }
        if self.message.as_ref().is_some_and(|m| now.duration_since(m.2) > MESSAGE_TIME) {
            self.message = None;
            self.redraw = true;
        }
    }

    fn new_race(&mut self, m: Arc<TrackMap>) {
        let mut objects = vec![0u32; GRID * GRID];
        for s in &m.statics {
            objects[s.cell.0 as usize * GRID + s.cell.1 as usize] += 1;
        }
        sr_runtime::info!("visibility editor: track {} ({}): {} triangles, {} textures, {} statics, {} camera cells with a list", m.id, m.name, m.tris.len(), m.textures.len(), m.statics.len(), m.listed_cells().len());
        if let Some(r) = &m.road {
            sr_runtime::info!("visibility editor: road line of {} points, {:.0} units ({:.0} sectors)", r.points().len(), r.length(), r.length() / visedit::CELL_SIZE);
        }
        let name = m.name.clone();
        self.race = Some(Race { map: m, objects });
        self.races += 1;
        self.locked = None;
        self.hover = None;
        self.drag = Drag::None;
        self.file = FileState::None;
        self.last_edit = None;
        self.version = u64::MAX;
        self.sync();
        self.last_edit = None;
        self.fit();
        self.map_changed = true;
        self.states_for = None;
        let [w, h] = self.layout.size;
        self.relayout(w, h);
        self.say(format!("Track loaded: {name}"), canvas::WHITE);
    }

    fn compute_states(&mut self) {
        self.states_changed = true;
        self.redraw = true;
        self.area_cells = self.area_cams();
        let (Some(r), Some(cam)) = (self.race.as_ref(), self.cam()) else {
            self.states.fill(S_DRAWN);
            self.draws = 0;
            self.all_in_view = false;
            self.road_ext = None;
            return;
        };
        self.states.fill(S_NOT_DRAWN);
        let at = |c: Cell| c.0 as usize * GRID + c.1 as usize;
        // the designers' list (without one within reach: every sector, as the game draws
        // everything in view), the road extension, then the user's own clicks
        for e in visedit::default_cells(&r.map, cam) {
            self.states[at(visedit::cell_of_entry(e))] = S_DRAWN;
        }
        self.road_ext = visedit::road_extension(&r.map, cam, self.edits.road);
        if let Some(x) = &self.road_ext {
            for &c in &x.cells {
                self.states[at(c)] = S_ROAD;
            }
        }
        if let Some(ce) = self.edits.cams.get(&cam) {
            for &c in &ce.add {
                self.states[at(c)] = S_ADDED;
            }
            for &c in &ce.remove {
                self.states[at(c)] = S_HIDDEN;
            }
        }
        self.draws = self.edits.effective(&r.map, cam).map_or(0, |l| l.len());
        self.all_in_view = visedit::draws_all_in_view(&r.map, cam) && !self.edits.cams.contains_key(&cam);
    }

    // ---- what is drawn ----

    pub fn panel_info(&self) -> PanelInfo {
        let r = self.race.as_ref();
        PanelInfo {
            track: r.map(|r| (r.map.name.clone(), r.map.id.clone())),
            cam: self.cam(),
            locked: self.locked.is_some(),
            draws: self.draws,
            all_in_view: self.all_in_view,
            by_road: self.road_ext.as_ref().map_or(0, |x| x.cells.len()),
            area: self.area,
            area_cams: self.area_cells.len(),
            road_known: r.is_some_and(|r| r.map.road.is_some()),
            track_lists: r.is_some_and(|r| r.map.lists.iter().any(|l| !l.is_empty())),
            along: self.edits.road.along,
            width_half: self.edits.road.width_half,
            hover: self.hover.map(|c| HoverInfo { cell: c, objects: self.objects(c) as usize, state: self.state(c) }),
            edited_cams: self.edits.cams.len(),
            dirty: r.is_some() && visedit::dirty(),
            file: self.file.clone(),
            follow: self.follow,
            blink: self.blink,
            grid: self.grid,
            textures: self.textures,
            hot: self.hot,
        }
    }

    /// The panel's picture (dots).
    pub fn paint_panel(&mut self) -> &Canvas {
        let h = self.layout.panel[3] / self.layout.scale;
        let p = panel::paint(&self.panel_info(), h.max(1));
        self.panel = Some(p);
        &self.panel.as_ref().unwrap().canvas
    }

    /// The status line's picture (dots).
    pub fn paint_status(&self) -> Canvas {
        let w = self.layout.status[2] / self.layout.scale;
        let (left, colour) = self.status_text();
        panel::paint_status(w.max(1), &left, colour, &self.status_right())
    }

    fn status_text(&self) -> (String, Rgba) {
        if let Some(a) = self.hot {
            return (a.help().to_string(), canvas::WHITE);
        }
        if let Some((m, c, _)) = &self.message {
            return (m.clone(), *c);
        }
        if self.race.is_none() {
            return ("Start a race in the game window".into(), canvas::HINT);
        }
        match (self.hover, self.cam()) {
            (Some(c), Some(_)) => {
                let _ = c;
                ("Left click / drag: draw  Right click / drag: hide  Middle click: default  Middle drag: move".to_string(), canvas::MUTED)
            }
            (Some(_), None) => ("Ctrl+click: edit the list of this camera cell (or drive in the game)".into(), canvas::HINT),
            _ => ("Wheel: zoom  Middle drag or arrow keys: move the map  Home: whole track".into(), canvas::MUTED),
        }
    }

    fn status_right(&self) -> String {
        match (self.mouse, self.hover) {
            (Some([x, y]), Some(c)) if self.layout.in_map(x, y) => format!("SECTOR {},{}", c.0, c.1),
            _ => String::new(),
        }
    }

    /// A message over the map (no track yet), or none.
    pub fn map_message(&self) -> Option<Canvas> {
        self.race.is_none().then(|| canvas::label(&[("START A RACE IN THE GAME WINDOW", canvas::HINT), ("THE TRACK APPEARS HERE", canvas::LABEL)]))
    }

    pub fn title(&self) -> String {
        let mut t = String::from("Screamer Rally - Visibility Editor");
        if let Some(r) = &self.race {
            t += &format!(" - {}", r.map.name);
            if visedit::dirty() {
                t += " *";
            }
        }
        t
    }

    pub fn pointer(&self) -> Pointer {
        match (self.drag, self.mouse) {
            (Drag::Pan { moved: true, .. }, _) => Pointer::Move,
            _ if self.hot.is_some() => Pointer::Hand,
            (_, Some([x, y])) if self.layout.in_map(x, y) && self.race.is_some() => Pointer::Cross,
            _ => Pointer::Arrow,
        }
    }

    /// The shapes over the map, in window pixels (clipped to the map by the renderer).
    pub fn shapes(&self) -> Shapes {
        let mut s = Shapes::default();
        if self.race.is_none() {
            return s;
        }
        let ox = self.layout.map[0] as f64;
        let oy = self.layout.map[1] as f64;
        let rect = |c: Cell| {
            let r = self.view.cell_rect(c);
            [r[0] + ox, r[1] + oy, r[2] + ox, r[3] + oy]
        };
        let [r0, c0, r1, c1] = self.view.visible_cells();
        let shown = |c: Cell| c.0 >= r0 && c.0 <= r1 && c.1 >= c0 && c.1 <= c1;
        let px = self.view.cell_px();
        // grid
        if self.grid && px >= GRID_MIN_PX {
            let a = ((px - GRID_MIN_PX) / 40.0).clamp(0.0, 1.0);
            let col = [255, 255, 255, (22.0 + 26.0 * a) as u8];
            let top = rect((r1, c0))[1];
            let bottom = rect((r0, c0))[3];
            let left = rect((r0, c0))[0];
            let right = rect((r0, c1))[2];
            for c in c0..=c1 {
                let x = rect((r0, c))[0].round();
                s.rect([x, top, x + 1.0, bottom], col);
            }
            for r in r0..=r1 {
                let y = rect((r, c0))[3].round();
                s.rect([left, y, right, y + 1.0], col);
            }
        }
        let yellow = |a: u8| [panel::CAMERA_YELLOW.0, panel::CAMERA_YELLOW.1, panel::CAMERA_YELLOW.2, a];
        // line widths and the car's size follow the screen's scale (1 at 100 %)
        let k = (self.dpi as f64 / 96.0).max(1.0);
        let cam = self.cam();
        // the edit area's other camera cells
        let thin = (if px > 48.0 { 2.0 } else { 1.0 } * k).round();
        for &c in &self.area_cells {
            if Some(c) != cam && shown(c) {
                s.frame(rect(c), thin.min(px * 0.25).max(1.0), yellow(170));
            }
        }
        // the rectangle being dragged: the sectors it takes, in the colour they will have
        if let Drag::Select { start, now, target, moved: true, .. } = self.drag {
            let (ax, ay) = self.to_map(start[0], start[1]);
            let (bx, by) = self.to_map(now[0], now[1]);
            let fill = if target { [92, 224, 108, 80] } else { [236, 72, 60, 90] };
            for c in scope::cells_in_rect(self.view.to_world(ax, ay), self.view.to_world(bx, by)) {
                if self.objects(c) > 0 && shown(c) {
                    s.rect(rect(c), fill);
                }
            }
            let r = [start[0].min(now[0]), start[1].min(now[1]), start[0].max(now[0]), start[1].max(now[1])];
            s.rect(r, [255, 255, 255, 24]);
            s.frame(r, k.round(), [255, 255, 255, 230]);
        }
        // the road's line thin; for the camera cell the stretches the road extension follows
        // beyond its designers' list (bright) with the width each side (the band's edges)
        if let Some(road) = self.race.as_ref().and_then(|r| r.map.road.as_ref()) {
            let to = |p: [f64; 2]| self.world_to_window(p[0], p[1]);
            let thin = k.round().max(1.0);
            for i in 0..road.points().len() {
                let (a, b) = road.segment(i);
                s.line(to(a), to(b), thin, [ROAD_CYAN[0], ROAD_CYAN[1], ROAD_CYAN[2], 120]);
            }
            if let Some(x) = &self.road_ext {
                let (_, width) = self.edits.road.units();
                for &i in &x.stretch.segments {
                    let (a, b) = road.segment(i);
                    s.line(to(a), to(b), (3.0 * k).round(), ROAD_CYAN);
                    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                    let len = dx.hypot(dz);
                    if width > 0.0 && len > 0.0 {
                        let (nx, nz) = (-dz / len * width, dx / len * width);
                        for side in [1.0, -1.0] {
                            s.line(to([a[0] + side * nx, a[1] + side * nz]), to([b[0] + side * nx, b[1] + side * nz]), thin, [ROAD_CYAN[0], ROAD_CYAN[1], ROAD_CYAN[2], 190]);
                        }
                    }
                }
            }
        }
        // the camera cell: thick, with a dark edge outside
        if let Some(c) = cam {
            let r = rect(c);
            let w = (if px > 40.0 { 4.0 } else { 3.0 } * k).round();
            let e = k.round();
            s.frame([r[0] - e, r[1] - e, r[2] + e, r[3] + e], e, [0, 0, 0, 200]);
            s.frame(r, w.min(px * 0.5).max(1.0), yellow(255));
        }
        // the sector under the mouse
        if let Some(c) = self.hover {
            let r = rect(c);
            let e = k.round();
            s.frame([r[0] - e, r[1] - e, r[2] + e, r[3] + e], e, [0, 0, 0, 160]);
            s.frame(r, (2.0 * k).round().min(px * 0.5).max(1.0), [255, 255, 255, 255]);
        }
        // the game's camera (a wedge: where it looks) and the car (an arrow: where it heads)
        if self.live.frame > 0 {
            let l = &self.live;
            let cp = self.world_to_window(l.cam_pos[0] as f64, l.cam_pos[2] as f64);
            // world (x, z) directions on the screen: z up
            let cdir = [l.cam_forward[0] as f64, -l.cam_forward[1] as f64];
            s.wedge(cp, cdir, 36.0 * k, 0.55, [120, 186, 255, 70], [120, 186, 255, 230]);
            let p = self.world_to_window(l.car_pos[0] as f64, l.car_pos[2] as f64);
            s.arrow(p, [l.car_forward[0] as f64, -l.car_forward[1] as f64], 22.0 * k, [255, 255, 255, 255], [0, 0, 0, 230]);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_follows_dpi_and_size() {
        let l = Layout::new(1200, 900, 96, 338);
        assert_eq!(l.scale, 2);
        assert_eq!(l.panel[2], panel::PANEL_W * 2);
        assert_eq!(l.map[2] + l.panel[2], 1200);
        assert_eq!(l.status[1] + l.status[3], 900);
        assert!(l.in_map(10.0, 10.0) && !l.in_panel(10.0, 10.0) && l.in_panel(1199.0, 10.0));
        // 150 % and 200 %
        assert_eq!(Layout::new(1800, 1350, 144, 338).scale, 3);
        assert_eq!(Layout::new(2400, 1800, 192, 338).scale, 4);
        // a little short for scale 2: the panel's end is cut; much too short: scale 1
        assert_eq!(Layout::new(1200, 600, 96, 338).scale, 2);
        assert_eq!(Layout::new(1200, 500, 96, 338).scale, 1);
        // 150 %, a window not tall enough for scale 3: 2, everything shown
        assert_eq!(Layout::new(1378, 844, 144, 338).scale, 2);
        // a 720-line window keeps scale 2
        assert_eq!(Layout::new(1100, 720, 96, 338).scale, 2);
    }

    /// One test drives the shared state (`visedit` is a process-wide singleton).
    #[test]
    fn clicks_drags_and_keys_change_the_shared_edits() {
        use sr_runtime::visedit::{MapStatic, entry};
        let mut lists = vec![Vec::new(); GRID * GRID];
        // camera cell (128, 128) sees itself and (128, 129); (128, 130) sees (128, 131)
        lists[128 * GRID + 128] = vec![entry((128, 128)), entry((128, 129))];
        lists[128 * GRID + 130] = vec![entry((128, 130)), entry((128, 131))];
        let statics: Vec<MapStatic> = [(128, 128), (128, 129), (128, 130), (128, 131), (129, 128), (129, 129)].iter().map(|&c| MapStatic { cell: c, center: [0.0; 3], radius: 1.0 }).collect();
        // a road loop: east along row 128 from column 124 to 136, up to row 132, back west, down
        let centre = |c: Cell| view::cell_center(c);
        let corners = [centre((128, 124)), centre((128, 136)), centre((132, 136)), centre((132, 124))];
        let mut pts = Vec::new();
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            for k in 0..((b[0] - a[0]).abs().max((b[1] - a[1]).abs()) / 1000.0).ceil() as usize {
                let t = k as f64 * 1000.0 / (b[0] - a[0]).abs().max((b[1] - a[1]).abs());
                pts.push([(a[0] + t * (b[0] - a[0])) as f32, (a[1] + t * (b[1] - a[1])) as f32]);
            }
        }
        let statics: Vec<MapStatic> = statics.into_iter().map(|mut st| {
            let c = centre(st.cell);
            st.center = [c[0] as f32, 0.0, c[1] as f32];
            st
        }).collect();
        let map = TrackMap { id: "test".into(), name: "Test".into(), statics, lists, bounds: [0.0, 0.0, 4.0 * 8192.0, 2.0 * 8192.0], road: sr_runtime::road::Road::new(&pts), ..Default::default() };
        let dir = std::env::temp_dir().join(format!("sr-viseditor-test-{}", std::process::id()));
        visedit::set_dir(dir.clone());
        visedit::publish_map(map);
        let mut e = Editor::new(1200, 900, 96);
        let t0 = Instant::now();
        e.tick(t0);
        assert!(e.map().is_some() && e.map_changed);
        assert_eq!(e.cam(), None, "no camera published, none picked");
        let _ = e.paint_panel();
        // pixel of a cell's centre
        let at = |e: &Editor, c: Cell| {
            let p = view::cell_center(c);
            e.world_to_window(p[0], p[1])
        };
        // a click without a camera cell does nothing; Ctrl+click picks (128, 128)
        let p = at(&e, (128, 129));
        e.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Left, x: p[0], y: p[1] });
        assert!(visedit::edits().cams.is_empty());
        let p = at(&e, (128, 128));
        e.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: true });
        e.handle(Event::Up { btn: Btn::Left, x: p[0], y: p[1] });
        assert_eq!(e.cam(), Some((128, 128)));
        assert_eq!(e.hover(), Some((128, 128)));
        // the edit area down to 0 for now: clicks change this camera cell only
        for _ in 0..DEFAULT_AREA {
            e.handle(Event::Char('['));
        }
        assert_eq!(e.area_cams(), vec![(128, 128)]);
        assert_eq!(visedit::highlight(), (Some((128, 128)), true));
        // right click (128, 129): drawn by default -> hidden; a left click on a drawn sector
        // changes nothing
        let p = at(&e, (128, 129));
        e.handle(Event::Move(p[0], p[1]));
        assert_eq!(e.state((128, 129)), Some(SectorState::DrawnByDefault));
        e.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Left, x: p[0], y: p[1] });
        assert!(visedit::visible((128, 128), (128, 129)) && visedit::edits().cams.is_empty());
        e.handle(Event::Down { btn: Btn::Right, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Right, x: p[0] + 2.0, y: p[1] });
        assert!(!visedit::visible((128, 128), (128, 129)));
        assert_eq!(e.state((128, 129)), Some(SectorState::Hidden));
        // a left click: drawn again
        e.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Left, x: p[0], y: p[1] });
        assert!(visedit::visible((128, 128), (128, 129)));
        assert!(visedit::edits().cams.is_empty());
        // drag from (129, 128) (not drawn) over (128, 131): all six sectors drawn, one step
        let a = at(&e, (129, 128));
        let b = at(&e, (128, 131));
        e.handle(Event::Down { btn: Btn::Left, x: a[0], y: a[1], ctrl: false });
        e.handle(Event::Move((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0));
        assert!(!e.shapes().v.is_empty());
        e.handle(Event::Move(b[0], b[1]));
        e.handle(Event::Up { btn: Btn::Left, x: b[0], y: b[1] });
        let eff = visedit::effective_list((128, 128)).unwrap();
        assert_eq!(eff.len(), 6, "{eff:?}");
        assert_eq!(e.state((129, 129)), Some(SectorState::Added));
        // undo takes the whole rectangle back
        e.handle(Event::Key { vk: 0x5a, ctrl: true, shift: false, repeat: false });
        assert_eq!(visedit::effective_list((128, 128)).unwrap().len(), 2);
        e.handle(Event::Key { vk: 0x59, ctrl: true, shift: false, repeat: false });
        assert_eq!(visedit::effective_list((128, 128)).unwrap().len(), 6);
        // middle click: back to the default
        let p = at(&e, (129, 129));
        e.handle(Event::Down { btn: Btn::Middle, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Middle, x: p[0], y: p[1] });
        assert_eq!(e.state((129, 129)), Some(SectorState::NotDrawn));
        // right drag: a rectangle hidden, one step ((129, 128) was added, (129, 129) is not
        // drawn by default)
        let a = at(&e, (129, 128));
        e.handle(Event::Down { btn: Btn::Right, x: a[0], y: a[1], ctrl: false });
        e.handle(Event::Move(p[0], p[1]));
        e.handle(Event::Up { btn: Btn::Right, x: p[0], y: p[1] });
        assert_eq!(visedit::effective_list((128, 128)).unwrap().len(), 4);
        assert_eq!(e.state((129, 128)), Some(SectorState::NotDrawn));
        e.handle(Event::Key { vk: 0x5a, ctrl: true, shift: false, repeat: false });
        assert_eq!(visedit::effective_list((128, 128)).unwrap().len(), 5);
        // middle drag: the map moves, nothing is reset
        let before = e.view.center;
        e.handle(Event::Down { btn: Btn::Middle, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Move(p[0] + 50.0, p[1] + 20.0));
        e.handle(Event::Up { btn: Btn::Middle, x: p[0] + 50.0, y: p[1] + 20.0 });
        assert!(e.view.center != before);
        assert_eq!(visedit::effective_list((128, 128)).unwrap().len(), 5);
        // arrow keys move the map too
        let before = e.view.center;
        e.handle(Event::Key { vk: 0x27, ctrl: false, shift: false, repeat: false });
        assert!(e.view.center != before);
        // wheel: zoom in around the mouse
        let s0 = e.view.scale;
        e.handle(Event::Wheel { notches: 2.0, x: 300.0, y: 300.0 });
        assert!(e.view.scale > s0);
        // backspace drops the camera cell's own edits
        e.handle(Event::Key { vk: VK_BACK, ctrl: false, shift: false, repeat: false });
        assert!(visedit::edits().cams.is_empty());
        // Extend: the road extension, for every camera cell. (128, 128)'s list stops at
        // (128, 129): the road beyond it, (128, 130) and (128, 131), is drawn by the road;
        // (129, 129) is not beside the road there
        e.tick(Instant::now());
        e.act(Action::Extend);
        assert_eq!(visedit::edits().road.along, visedit::ALONG_STEP);
        assert!(visedit::visible((128, 128), (128, 130)) && visedit::visible((128, 128), (128, 131)));
        assert!(!visedit::visible((128, 128), (129, 129)));
        assert_eq!(e.state((128, 130)), Some(SectorState::ByTheRoad));
        // a sector the road added, hidden by hand: from this camera cell only
        let p = at(&e, (128, 131));
        e.handle(Event::Down { btn: Btn::Right, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Right, x: p[0], y: p[1] });
        assert!(!visedit::visible((128, 128), (128, 131)));
        assert_eq!(e.state((128, 131)), Some(SectorState::Hidden));
        assert_eq!(visedit::edits().cams.keys().copied().collect::<Vec<_>>(), vec![(128, 128)]);
        assert!(visedit::visible((128, 130), (128, 131)), "another camera cell keeps it");
        // undo the click, then Extend (one step each); redo
        e.act(Action::Undo);
        assert!(visedit::visible((128, 128), (128, 131)));
        e.act(Action::Undo);
        assert!(!visedit::visible((128, 128), (128, 130)), "Extend is one undo step");
        e.act(Action::Redo);
        assert!(visedit::visible((128, 128), (128, 130)));
        // Shift+E: less far, down to the designers' lists; the panel's buttons too
        e.handle(Event::Key { vk: 0x45, ctrl: false, shift: true, repeat: false });
        assert_eq!(visedit::edits().road.along, 0);
        e.handle(Event::Key { vk: 0x45, ctrl: false, shift: false, repeat: false });
        let _ = e.paint_panel();
        let p = e.button_center(Action::Less).unwrap();
        e.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: false });
        assert_eq!(visedit::edits().road.along, 0);
        let p = e.button_center(Action::Extend).unwrap();
        e.handle(Event::Down { btn: Btn::Left, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Char('>'));
        assert_eq!(visedit::edits().road, RoadReach { along: visedit::ALONG_STEP, width_half: visedit::DEFAULT_WIDTH_HALF + 1 });
        // the edit area back to 4 sectors along the road: a click from (128, 128) changes the
        // camera cells along the road around it, not (132, 127) on the loop's far side (4
        // sectors away in a straight line, 11 along the road)
        for _ in 0..DEFAULT_AREA {
            e.handle(Event::Char(']'));
        }
        let area = e.area_cams();
        assert!(area.contains(&(128, 127)) && area.contains(&(128, 131)) && !area.contains(&(132, 127)), "{area:?}");
        let p = at(&e, (128, 129));
        e.handle(Event::Down { btn: Btn::Right, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Right, x: p[0], y: p[1] });
        assert!(!visedit::visible((128, 128), (128, 129)) && !visedit::visible((128, 127), (128, 129)));
        assert!(visedit::visible((132, 127), (128, 129)), "beyond the area");
        // Backspace: the area's clicks dropped
        e.handle(Event::Key { vk: VK_BACK, ctrl: false, shift: false, repeat: false });
        assert!(visedit::visible((128, 127), (128, 129)) && visedit::edits().cams.is_empty());
        e.handle(Event::Down { btn: Btn::Right, x: p[0], y: p[1], ctrl: false });
        e.handle(Event::Up { btn: Btn::Right, x: p[0], y: p[1] });
        // autosave a second after the last change
        assert!(visedit::dirty());
        e.tick(Instant::now() + Duration::from_millis(1500));
        assert!(!visedit::dirty());
        let file = dir.join("visibility").join("test.vis");
        let (saved, id) = Edits::from_text(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!((saved, id.as_str()), (visedit::edits(), "test"));
        assert!(e.title().ends_with("Test"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
