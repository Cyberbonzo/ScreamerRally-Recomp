//! The visibility editor (the "occluder tool", `docs/05-visibility-editor.md`): what the game
//! and the editor window share.
//!
//! The game's world is a 256 x 256 grid of cells of 8 192 units ([`Cell`] = (row, column):
//! row from z, column from x). The designers gave many cells a **visibility list**: the cells
//! drawn while the camera is in that cell (the result of Milestone's own tool in 1997). The
//! editor lets the user change those lists per camera cell: a cell of the list hidden, a cell
//! added. The game then draws, for the camera's cell, exactly its **effective list**: the
//! default list ([`default_list`]: the designers' list, or for a camera cell without one the
//! list of the nearest cell with one), plus the track's **road extension** ([`RoadReach`],
//! [`road_extension`]: the road followed further than the designers' list goes, the same rule
//! for every camera cell, so it applies by itself wherever the camera is), minus the cells the
//! user hid from that camera cell, plus the ones the user added to it. The user's edits of a
//! camera cell apply to that camera cell only.
//!
//! Who does what:
//! * the native renderer publishes the race's [`TrackMap`] at the race's start
//!   ([`publish_map`], which also loads the track's saved edits) and the camera and car every
//!   frame ([`publish_live`]); it asks [`effective_list`] for the camera's cell while
//!   [`edits_active`];
//! * the editor window reads [`map`] and [`live`], changes the edits with [`apply`] /
//!   [`reset`] (one undo step each), [`undo`] / [`redo`], [`save`], and marks the cell under
//!   the mouse with [`set_highlight`] (blinking in the game's view);
//! * the app turns the tool on ([`set_tool_mode`]) and tells where edits are saved
//!   ([`set_dir`]).
//!
//! Every change bumps [`version`], so readers can tell when to look again. Everything here
//! is behind one mutex; calls are short.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// A world cell: (row, column) of the game's 256 x 256 grid; row from z, column from x.
pub type Cell = (u16, u16);
/// Cells per side.
pub const GRID: usize = 256;
/// A cell's side in world units.
pub const CELL_SIZE: f64 = 8192.0;
/// World coordinate of the grid's first cell's edge (x and z).
pub const WORLD_MIN: f64 = -1_048_576.0;
/// How far (cells, Chebyshev) [`default_list`] looks for a cell with a list around a camera
/// cell without one (as the renderer's `NOLIST_REACH`).
pub const NOLIST_REACH: usize = 8;

/// The cell holding world position (x, z).
pub fn cell_of(x: f64, z: f64) -> Cell {
    let k = |v: f64| ((v - WORLD_MIN) / CELL_SIZE).floor().clamp(0.0, (GRID - 1) as f64) as u16;
    (k(z), k(x))
}

/// A cell's world rectangle: x0, z0, x1, z1.
pub fn cell_rect(c: Cell) -> [f64; 4] {
    let x0 = WORLD_MIN + c.1 as f64 * CELL_SIZE;
    let z0 = WORLD_MIN + c.0 as f64 * CELL_SIZE;
    [x0, z0, x0 + CELL_SIZE, z0 + CELL_SIZE]
}

/// A cell as a visibility list entry (`row << 16 | column`, the game's form).
pub fn entry(c: Cell) -> u32 {
    (c.0 as u32) << 16 | c.1 as u32
}

/// A visibility list entry as a cell.
pub fn cell_of_entry(e: u32) -> Cell {
    ((e >> 16) as u16, (e & 0xffff) as u16)
}

// ---------------------------------------------------------------------------------------
// The map of a race (published once per race)

/// A texture expanded to RGBA (`u32` per texel, bytes r, g, b, a in memory order).
#[derive(Clone, Debug, Default)]
pub struct MapTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u32>,
}

/// How a map triangle is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapMaterial {
    /// Index into [`TrackMap::textures`]; `None`: vertex colour only.
    pub texture: Option<u32>,
    /// Texels of this colour (`0xRRGGBB`) are not drawn (the game's chroma key).
    pub key: Option<u32>,
    /// Blended with what is under it (glass, water, shadows).
    pub blended: bool,
}

/// One triangle of a static, in world coordinates (y up).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapTri {
    pub pos: [[f32; 3]; 3],
    pub uv: [[f32; 2]; 3],
    pub color: [[u8; 4]; 3],
    pub material: u32,
    /// Index into [`TrackMap::statics`].
    pub statik: u32,
}

/// A static object (track piece, building, tree, terrain).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapStatic {
    pub cell: Cell,
    pub center: [f32; 3],
    pub radius: f32,
}

/// Everything the editor shows of a race's world.
#[derive(Clone, Debug, Default)]
pub struct TrackMap {
    /// Stable identity of the track (a hash of its visibility lists and statics): names the
    /// saved edits.
    pub id: String,
    /// The track's name if known (else "track").
    pub name: String,
    pub textures: Vec<MapTexture>,
    pub materials: Vec<MapMaterial>,
    pub tris: Vec<MapTri>,
    pub statics: Vec<MapStatic>,
    /// The designers' visibility list of every cell (`row * 256 + column`; empty: none).
    pub lists: Vec<Vec<u32>>,
    /// World rectangle of all statics: x0, z0, x1, z1.
    pub bounds: [f32; 4],
    /// The road's line (`crate::road`: the computer's cars' line; `None`: not known).
    pub road: Option<crate::road::Road>,
}

impl TrackMap {
    /// The map of a scene (`scene.statics[i]` described by `statics[i]`) with the grid's
    /// visibility lists `lists`. `full`: with the triangles and textures (the editor's
    /// picture); without, only what the game needs to draw saved edits.
    pub fn from_scene(scene: &sr_scene::SceneData, statics: &[crate::native::extract::StaticMeta], lists: &[Vec<u32>], name: &str, full: bool) -> TrackMap {
        let textures = if full {
            scene.textures.iter().map(|t| MapTexture { width: t.width, height: t.height, rgba: expand_rgba(&t.format, &t.pixels, t.width, t.height) }).collect()
        } else {
            Vec::new()
        };
        let materials = scene
            .materials
            .iter()
            .map(|m| MapMaterial {
                texture: m.texture,
                key: match m.transparency {
                    // the colour bits only, as the renderer compares them
                    sr_scene::Transparency::ChromaKey { key } => Some(key & 0xff_ffff),
                    _ => None,
                },
                blended: matches!(m.transparency, sr_scene::Transparency::Blended(_)),
            })
            .collect();
        let mut tris = Vec::new();
        let mut map_statics = Vec::with_capacity(statics.len());
        let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for (i, inst) in scene.statics.iter().enumerate() {
            let meta = statics.get(i).copied().unwrap_or_default();
            map_statics.push(MapStatic { cell: meta.cell, center: meta.center.map(|v| v as f32), radius: meta.radius });
            let Some(mesh) = scene.meshes.get(inst.mesh as usize).filter(|_| full) else { continue };
            let t = &inst.transform;
            let world = |p: [f32; 3]| -> [f32; 3] {
                let b = &t.basis;
                [0, 1, 2].map(|k| (b[k * 3] as f64 * p[0] as f64 + b[k * 3 + 1] as f64 * p[1] as f64 + b[k * 3 + 2] as f64 * p[2] as f64 + t.origin[k]) as f32)
            };
            for r in &mesh.ranges {
                for tri in mesh.indices[r.indices.start as usize..r.indices.end as usize].chunks_exact(3) {
                    let v = [0, 1, 2].map(|k| &mesh.vertices[tri[k] as usize]);
                    let pos = [0, 1, 2].map(|k| world(v[k].pos));
                    for p in &pos {
                        bounds[0] = bounds[0].min(p[0]);
                        bounds[1] = bounds[1].min(p[2]);
                        bounds[2] = bounds[2].max(p[0]);
                        bounds[3] = bounds[3].max(p[2]);
                    }
                    tris.push(MapTri { pos, uv: [0, 1, 2].map(|k| v[k].uv), color: [0, 1, 2].map(|k| v[k].color), material: r.material, statik: i as u32 });
                }
            }
        }
        if tris.is_empty() {
            // without triangles: the statics' spheres
            for st in &map_statics {
                bounds[0] = bounds[0].min(st.center[0] - st.radius);
                bounds[1] = bounds[1].min(st.center[2] - st.radius);
                bounds[2] = bounds[2].max(st.center[0] + st.radius);
                bounds[3] = bounds[3].max(st.center[2] + st.radius);
            }
            if map_statics.is_empty() {
                bounds = [0.0; 4];
            }
        }
        let mut lists = lists.to_vec();
        lists.resize(GRID * GRID, Vec::new());
        TrackMap { id: track_id(&lists, statics.len()), name: name.to_string(), textures, materials, tris, statics: map_statics, lists, bounds, road: None }
    }

    /// The cells holding objects.
    pub fn occupied(&self) -> BTreeSet<Cell> {
        self.statics.iter().map(|s| s.cell).collect()
    }

    /// The designers' list of a cell (`None`: it has none).
    pub fn list(&self, c: Cell) -> Option<&[u32]> {
        self.lists.get(c.0 as usize * GRID + c.1 as usize).filter(|l| !l.is_empty()).map(|l| l.as_slice())
    }

    /// The cells that have a designers' list (the track's camera cells), by row then column.
    pub fn listed_cells(&self) -> Vec<Cell> {
        (0..GRID * GRID).filter(|&k| !self.lists[k].is_empty()).map(|k| ((k / GRID) as u16, (k % GRID) as u16)).collect()
    }

    /// Statics per cell (indices into [`TrackMap::statics`]).
    pub fn statics_by_cell(&self) -> BTreeMap<Cell, Vec<u32>> {
        let mut m: BTreeMap<Cell, Vec<u32>> = BTreeMap::new();
        for (i, s) in self.statics.iter().enumerate() {
            m.entry(s.cell).or_default().push(i as u32);
        }
        m
    }

    /// The map's size in memory (bytes, its vectors' contents).
    pub fn bytes(&self) -> usize {
        use std::mem::size_of;
        self.textures.iter().map(|t| t.rgba.len() * 4 + size_of::<MapTexture>()).sum::<usize>()
            + self.materials.len() * size_of::<MapMaterial>()
            + self.tris.len() * size_of::<MapTri>()
            + self.statics.len() * size_of::<MapStatic>()
            + self.lists.iter().map(|l| l.len() * 4 + size_of::<Vec<u32>>()).sum::<usize>()
    }

    /// The triangles against their statics' bounding spheres (a check of the export: the
    /// triangles in world coordinates). Returns (vertices outside their static's sphere by more
    /// than 1 % of its radius plus one unit, vertices, the largest distance from a static's
    /// centre in radii).
    pub fn check_spheres(&self) -> (usize, usize, f32) {
        let (mut outside, mut n, mut worst) = (0, 0, 0f32);
        for t in &self.tris {
            let Some(s) = self.statics.get(t.statik as usize) else { continue };
            for p in &t.pos {
                let d = ((p[0] - s.center[0]).powi(2) + (p[1] - s.center[1]).powi(2) + (p[2] - s.center[2]).powi(2)).sqrt();
                n += 1;
                worst = worst.max(d / s.radius.max(1.0));
                if d > s.radius * 1.01 + 1.0 {
                    outside += 1;
                }
            }
        }
        (outside, n, worst)
    }
}

/// A stable name for a track: FNV-1a over its lists (entries in order) and its number of
/// statics, 16 hex digits.
pub fn track_id(lists: &[Vec<u32>], statics: usize) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u32| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(statics as u32);
    for (k, l) in lists.iter().enumerate() {
        if !l.is_empty() {
            eat(k as u32);
            eat(l.len() as u32);
            for &e in l {
                eat(e);
            }
        }
    }
    format!("{h:016x}")
}

fn x5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}
fn x6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}
fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    r | g << 8 | b << 16 | a << 24
}

/// Expand texels to RGBA (as the renderer's `prepare::expand_level`, without its colour-key
/// fill).
pub fn expand_rgba(format: &sr_scene::TexFormat, px: &[u8], w: u32, h: u32) -> Vec<u32> {
    use sr_scene::TexFormat as F;
    let b8 = |i: usize| *px.get(i).unwrap_or(&0) as u32;
    let b16 = |i: usize| b8(i * 2) | b8(i * 2 + 1) << 8;
    let pal = |p: &[u32; 256], i: u32| {
        let c = p[(i & 0xff) as usize];
        rgba((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff, 255)
    };
    (0..(w * h) as usize)
        .map(|i| match format {
            F::Indexed8 { palette } => pal(palette, b8(i)),
            F::IndexedAlpha88 { palette } => {
                let v = b16(i);
                (pal(palette, v) & 0x00ff_ffff) | (v >> 8) << 24
            }
            F::Rgb565 => {
                let v = b16(i);
                rgba(x5(v >> 11), x6((v >> 5) & 63), x5(v & 31), 255)
            }
            F::Argb1555 => {
                let v = b16(i);
                rgba(x5((v >> 10) & 31), x5((v >> 5) & 31), x5(v & 31), if v & 0x8000 != 0 { 255 } else { 0 })
            }
            F::Argb4444 => {
                let v = b16(i);
                rgba(((v >> 8) & 15) * 17, ((v >> 4) & 15) * 17, (v & 15) * 17, (v >> 12) * 17)
            }
            F::Rgb332 => {
                let v = b8(i);
                rgba(((v >> 5) & 7) * 255 / 7, ((v >> 2) & 7) * 255 / 7, (v & 3) * 255 / 3, 255)
            }
            F::Alpha8 => rgba(255, 255, 255, b8(i)),
            F::Intensity8 => {
                let l = b8(i);
                rgba(l, l, l, 255)
            }
            F::AlphaIntensity44 => {
                let v = b8(i);
                let l = (v & 15) * 17;
                rgba(l, l, l, (v >> 4) * 17)
            }
            F::AlphaIntensity88 => {
                let v = b16(i);
                let l = v & 0xff;
                rgba(l, l, l, v >> 8)
            }
            F::Rgba8 => b8(i * 4) | b8(i * 4 + 1) << 8 | b8(i * 4 + 2) << 16 | b8(i * 4 + 3) << 24,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// The camera and the car (published every frame)

/// The main view's camera and the player's car, as last drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// The game's frame counter at publication.
    pub frame: u64,
    pub cam_pos: [f32; 3],
    /// The camera's viewing direction on the ground (x, z), unit length.
    pub cam_forward: [f32; 2],
    /// The camera's cell (the one whose effective list is drawn).
    pub cam_cell: Cell,
    pub car_pos: [f32; 3],
    /// The car's heading on the ground (x, z), unit length.
    pub car_forward: [f32; 2],
    /// Statics drawn in the main view this frame (any fade above 0), sorted.
    pub drawn: Vec<u32>,
}

// ---------------------------------------------------------------------------------------
// Edits

/// The changes to one camera cell's default list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellEdit {
    /// Cells drawn that the default list does not name.
    pub add: BTreeSet<Cell>,
    /// Cells of the default list not drawn.
    pub remove: BTreeSet<Cell>,
}

impl CellEdit {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

/// The track's road extension (the editor's Extend): every camera cell also draws the road
/// followed `along` sectors further, forward and back, than its designers' list goes, and
/// whatever stands within `width_half` half sectors of the road's line there
/// ([`road_extension`]). `along` 0: the designers' lists as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoadReach {
    pub along: u32,
    pub width_half: u32,
}

/// Each Extend reaches this many sectors further along the road, at most this far; the width
/// (half sectors) by default and at most.
pub const ALONG_STEP: u32 = 2;
pub const MAX_ALONG: u32 = 80;
pub const DEFAULT_WIDTH_HALF: u32 = 2;
pub const MAX_WIDTH_HALF: u32 = 16;

impl Default for RoadReach {
    fn default() -> Self {
        RoadReach { along: 0, width_half: DEFAULT_WIDTH_HALF }
    }
}

impl RoadReach {
    /// Along and width in world units.
    pub fn units(self) -> (f64, f64) {
        (self.along as f64 * CELL_SIZE, self.width_half as f64 * 0.5 * CELL_SIZE)
    }
}

/// All changes of a track: the road extension, and per camera cell the user's own edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edits {
    pub road: RoadReach,
    pub cams: BTreeMap<Cell, CellEdit>,
}

/// The default list of camera cell `cam`: its designers' list, or for a cell without one the
/// list of the nearest cell with one (cell centres, Chebyshev reach [`NOLIST_REACH`]; ties:
/// the first by row then column). `None`: none within reach.
pub fn default_list(lists: &[Vec<u32>], cam: Cell) -> Option<Vec<u32>> {
    let at = |r: usize, c: usize| lists.get(r * GRID + c).filter(|l| !l.is_empty());
    let (r0, c0) = (cam.0 as usize, cam.1 as usize);
    if let Some(l) = at(r0, c0) {
        return Some(l.clone());
    }
    let mut best: Option<(usize, &Vec<u32>)> = None;
    for r in r0.saturating_sub(NOLIST_REACH)..=(r0 + NOLIST_REACH).min(GRID - 1) {
        for c in c0.saturating_sub(NOLIST_REACH)..=(c0 + NOLIST_REACH).min(GRID - 1) {
            if let Some(l) = at(r, c) {
                let d = r.abs_diff(r0).pow(2) + c.abs_diff(c0).pow(2);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, l));
                }
            }
        }
    }
    best.map(|b| b.1.clone())
}

/// What camera cell `cam` draws by default, as the game does: its default list
/// ([`default_list`]), or for a cell without a list within reach every cell holding objects:
/// without a list the engine draws every cell of the view's box (`7a640`), and so does the
/// native renderer while no list has been seen (Stadium has no list at all). Sorted entries.
pub fn default_cells(map: &TrackMap, cam: Cell) -> Vec<u32> {
    default_list(&map.lists, cam).unwrap_or_else(|| map.occupied().into_iter().map(entry).collect())
}

/// Camera cell `cam` has no list within reach: the game draws every cell in view.
pub fn draws_all_in_view(map: &TrackMap, cam: Cell) -> bool {
    default_list(&map.lists, cam).is_none()
}

/// The cell whose designers' list camera cell `cam` starts from: itself when it has a list,
/// else the nearest cell with one (as [`default_list`]). `None`: none within reach.
pub fn source_cell(lists: &[Vec<u32>], cam: Cell) -> Option<Cell> {
    let has = |r: usize, c: usize| lists.get(r * GRID + c).is_some_and(|l| !l.is_empty());
    let (r0, c0) = (cam.0 as usize, cam.1 as usize);
    if has(r0, c0) {
        return Some(cam);
    }
    let mut best: Option<(usize, Cell)> = None;
    for r in r0.saturating_sub(NOLIST_REACH)..=(r0 + NOLIST_REACH).min(GRID - 1) {
        for c in c0.saturating_sub(NOLIST_REACH)..=(c0 + NOLIST_REACH).min(GRID - 1) {
            if has(r, c) {
                let d = r.abs_diff(r0).pow(2) + c.abs_diff(c0).pow(2);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, (r as u16, c as u16)));
                }
            }
        }
    }
    best.map(|b| b.1)
}

impl Edits {
    /// What camera cell `cam` draws before the user's own edits of it: its default cells
    /// ([`default_cells`]) and the road extension ([`road_extension`]).
    pub fn base(&self, map: &TrackMap, cam: Cell) -> BTreeSet<u32> {
        let mut set: BTreeSet<u32> = default_cells(map, cam).into_iter().collect();
        if let Some(r) = road_extension(map, cam, self.road) {
            set.extend(r.cells.iter().map(|&c| entry(c)));
        }
        set
    }

    /// The effective list of camera cell `cam` (sorted entries): its base ([`Edits::base`])
    /// minus its removed cells plus its added ones. `None`: a camera cell without a list
    /// within reach and not edited keeps the game's own rule (every cell in view).
    pub fn effective(&self, map: &TrackMap, cam: Cell) -> Option<Vec<u32>> {
        let e = self.cams.get(&cam);
        if e.is_none() && draws_all_in_view(map, cam) {
            return None;
        }
        let mut set = self.base(map, cam);
        if let Some(e) = e {
            for c in &e.remove {
                set.remove(&entry(*c));
            }
            for c in &e.add {
                set.insert(entry(*c));
            }
        }
        Some(set.into_iter().collect())
    }

    /// Whether `cell` is drawn from camera cell `cam`.
    pub fn visible(&self, map: &TrackMap, cam: Cell, cell: Cell) -> bool {
        match self.edit_state(cam, cell) {
            Some(v) => v,
            None => self.base(map, cam).contains(&entry(cell)),
        }
    }

    /// The user's own edit of `cell` from camera cell `cam`: `Some(true)` added,
    /// `Some(false)` hidden, `None` as its base has it.
    pub fn edit_state(&self, cam: Cell, cell: Cell) -> Option<bool> {
        self.cams.get(&cam).and_then(|e| if e.add.contains(&cell) { Some(true) } else if e.remove.contains(&cell) { Some(false) } else { None })
    }

    /// Draw (`visible`) or hide `cell` from camera cell `cam` (that camera cell only). An edit
    /// that brings the cell back to its base's state is dropped.
    pub fn set(&mut self, map: &TrackMap, cam: Cell, cell: Cell, visible: bool) {
        let default = self.base(map, cam).contains(&entry(cell));
        let e = self.cams.entry(cam).or_default();
        e.add.remove(&cell);
        e.remove.remove(&cell);
        if visible && !default {
            e.add.insert(cell);
        } else if !visible && default {
            e.remove.insert(cell);
        }
        if e.is_empty() {
            self.cams.remove(&cam);
        }
    }

    /// No change at all: no road extension, no camera cell edited.
    pub fn is_empty(&self) -> bool {
        self.road.along == 0 && self.cams.is_empty()
    }

    /// Back to the default list's state for `cell` from `cam`.
    pub fn reset(&mut self, cam: Cell, cell: Cell) {
        if let Some(e) = self.cams.get_mut(&cam) {
            e.add.remove(&cell);
            e.remove.remove(&cell);
            if e.is_empty() {
                self.cams.remove(&cam);
            }
        }
    }

    /// The saved form (`docs/05-visibility-editor.md`, "File").
    pub fn to_text(&self, id: &str, name: &str) -> String {
        let mut s = String::from("# Screamer Rally visibility edits (docs/05-visibility-editor.md)\n");
        s += &format!("track {id} {name}\n");
        if self.road != RoadReach::default() {
            s += &format!("road {} {}\n", self.road.along, self.road.width_half);
        }
        for (cam, e) in &self.cams {
            s += &format!("cam {} {}", cam.0, cam.1);
            for c in &e.add {
                s += &format!(" +{},{}", c.0, c.1);
            }
            for c in &e.remove {
                s += &format!(" -{},{}", c.0, c.1);
            }
            s += "\n";
        }
        s
    }

    /// Read the saved form; returns the edits and the track id written in it.
    pub fn from_text(text: &str) -> Result<(Edits, String), String> {
        let mut edits = Edits::default();
        let mut id = String::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut w = line.split_whitespace();
            let bad = |what: &str| format!("line {}: {what}", n + 1);
            match w.next() {
                Some("track") => id = w.next().unwrap_or_default().to_string(),
                Some("road") => {
                    let num = |s: Option<&str>| s.and_then(|s| s.parse::<u32>().ok());
                    edits.road = RoadReach { along: num(w.next()).ok_or_else(|| bad("road along"))?.min(MAX_ALONG), width_half: num(w.next()).ok_or_else(|| bad("road width"))?.min(MAX_WIDTH_HALF) };
                }
                Some("cam") => {
                    let num = |s: Option<&str>| s.and_then(|s| s.parse::<u16>().ok()).filter(|&v| (v as usize) < GRID);
                    let cam = (num(w.next()).ok_or_else(|| bad("camera row"))?, num(w.next()).ok_or_else(|| bad("camera column"))?);
                    let e = edits.cams.entry(cam).or_default();
                    for t in w {
                        let (sign, rest) = t.split_at(1);
                        let (r, c) = rest.split_once(',').ok_or_else(|| bad("cell"))?;
                        let cell = (num(Some(r)).ok_or_else(|| bad("cell row"))?, num(Some(c)).ok_or_else(|| bad("cell column"))?);
                        match sign {
                            "+" => e.add.insert(cell),
                            "-" => e.remove.insert(cell),
                            _ => return Err(bad("cell sign")),
                        };
                    }
                    if e.is_empty() {
                        edits.cams.remove(&cam);
                    }
                }
                _ => return Err(bad("unknown line")),
            }
        }
        Ok((edits, id))
    }
}

/// How far from a camera cell's centre the road may pass for the road extension to start
/// from it (a camera beside the road; world units).
pub const ROAD_REACH: f64 = 2.0 * CELL_SIZE;

/// Where the road extension starts for camera cell `cam`: the road's places in it (the
/// nearest one within [`ROAD_REACH`] when the road misses the cell).
pub fn road_sources(road: &crate::road::Road, cam: Cell) -> Vec<f64> {
    road.passes(cell_rect(cam), ROAD_REACH)
}

/// The edit area of camera cell `cam`: the camera cells within `along` (world units) of it
/// along the road, both ways round the lap (cells whose road places, [`road_sources`], lie
/// within `along` of one of `cam`'s), so that a click holds while the car drives through that
/// stretch, and not on another stretch of road that passes near (a hairpin's other leg).
/// `cam` itself always. Without a road line, or for a camera cell the road does not pass
/// near: the cells within `along` in a straight line (rows and columns).
pub fn road_area(map: &TrackMap, cam: Cell, along: f64) -> Vec<Cell> {
    let mut out = BTreeSet::from([cam]);
    let near = |reach: i32| {
        let (r0, c0) = (cam.0 as i32, cam.1 as i32);
        (r0 - reach..=r0 + reach).flat_map(move |r| (c0 - reach..=c0 + reach).map(move |c| (r, c))).filter(|&(r, c)| r >= 0 && c >= 0 && (r as usize) < GRID && (c as usize) < GRID).map(|(r, c)| (r as u16, c as u16))
    };
    let from = map.road.as_ref().map(|r| road_sources(r, cam)).unwrap_or_default();
    match map.road.as_ref() {
        Some(road) if !from.is_empty() => {
            let l = road.length();
            let reach = (along / CELL_SIZE).ceil() as i32 + (ROAD_REACH / CELL_SIZE).ceil() as i32 + 1;
            for c in near(reach) {
                let within = road_sources(road, c).iter().any(|&p| {
                    from.iter().any(|&s| {
                        let d = (p - s).rem_euclid(l);
                        d.min(l - d) <= along
                    })
                });
                if within {
                    out.insert(c);
                }
            }
        }
        _ => out.extend(near((along / CELL_SIZE).round() as i32)),
    }
    out.into_iter().collect()
}

/// What the road extension adds to a camera cell.
#[derive(Clone, Debug)]
pub struct RoadExtension {
    /// The stretches of road followed beyond the designers' list.
    pub stretch: crate::road::Stretch,
    /// The cells with objects within the width of them, not in the designers' list.
    pub cells: Vec<Cell>,
}

/// The road extension of camera cell `cam` (`None`: none: `along` 0, no road line, no list,
/// or the road does not pass near). Like water poured where the road passes the camera cell:
/// it follows the road forward and back as far as the designers' list covers it (the road's
/// points in cells of the list, or in cells without objects), then `along` sectors further,
/// and takes every cell with an object whose bounding circle on the ground comes within the
/// width of those further stretches. Parts of the map near in a straight line only (the other
/// leg of a hairpin, another loop of the track) stay out unless within the width.
pub fn road_extension(map: &TrackMap, cam: Cell, reach: RoadReach) -> Option<RoadExtension> {
    if reach.along == 0 {
        return None;
    }
    let road = map.road.as_ref()?;
    let designers: BTreeSet<Cell> = default_list(&map.lists, cam)?.into_iter().map(cell_of_entry).collect();
    let from = road_sources(road, cam);
    if from.is_empty() {
        return None;
    }
    let occupied = map.occupied();
    let covered = |p: [f64; 2]| {
        let c = cell_of(p[0], p[1]);
        designers.contains(&c) || !occupied.contains(&c)
    };
    let (along, width) = reach.units();
    let mut ranges = Vec::new();
    for &s in &from {
        let ahead = road.walk(s, true, covered);
        let behind = road.walk(s, false, covered);
        ranges.push((s + ahead, s + ahead + along));
        ranges.push((s - behind - along, s - behind));
    }
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    ranges.dedup_by(|a, b| (a.0 - b.0).abs() < 1.0 && (a.1 - b.1).abs() < 1.0);
    let stretch = road.stretch_ranges(&ranges);
    let mut cells = BTreeSet::new();
    for st in &map.statics {
        if cells.contains(&st.cell) || designers.contains(&st.cell) {
            continue;
        }
        if road.distance(&stretch, [st.center[0] as f64, st.center[2] as f64], width + st.radius as f64).is_some() {
            cells.insert(st.cell);
        }
    }
    Some(RoadExtension { stretch, cells: cells.into_iter().collect() })
}

// ---------------------------------------------------------------------------------------
// Shared state

#[derive(Default)]
struct Shared {
    map: Option<Arc<TrackMap>>,
    live: Live,
    edits: Edits,
    /// The edits as last saved or loaded (for [`dirty`]).
    saved: Edits,
    /// The track has a saved file (or edits were made): the game draws the effective lists.
    have_file: bool,
    undo: Vec<Edits>,
    redo: Vec<Edits>,
    highlight: Option<Cell>,
    blink: bool,
    dir: Option<PathBuf>,
    status: String,
}

static TOOL: AtomicBool = AtomicBool::new(false);
static VERSION: AtomicU64 = AtomicU64::new(0);
static SHARED: Mutex<Option<Shared>> = Mutex::new(None);
const UNDO_LIMIT: usize = 200;

fn shared() -> MutexGuard<'static, Option<Shared>> {
    let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Shared::default());
    }
    g
}

fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    f(shared().as_mut().unwrap())
}

fn bump() {
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// The tool is on (`--vis-editor`): the game runs as the tool's game window.
pub fn tool_mode() -> bool {
    TOOL.load(Ordering::Relaxed)
}

pub fn set_tool_mode(on: bool) {
    TOOL.store(on, Ordering::Relaxed);
}

/// Where edits are saved: `<dir>\visibility\<track id>.vis`.
pub fn set_dir(dir: PathBuf) {
    with(|s| s.dir = Some(dir));
}

fn file_for(s: &Shared, id: &str) -> Option<PathBuf> {
    s.dir.as_ref().map(|d| d.join("visibility").join(format!("{id}.vis")))
}

/// Changes on any change of the map, the edits or the highlight.
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

/// A race's map (at its start): replaces the previous one and loads the track's saved edits.
pub fn publish_map(map: TrackMap) {
    with(|s| {
        let path = file_for(s, &map.id);
        let loaded = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
        let (edits, have_file) = match loaded.map(|t| Edits::from_text(&t)) {
            Some(Ok((e, id))) if id == map.id || id.is_empty() => (e, true),
            Some(Ok((_, id))) => {
                crate::warn!("visibility edits: {:?} is for track {id}, not {}; ignored", path, map.id);
                (Edits::default(), false)
            }
            Some(Err(e)) => {
                crate::warn!("visibility edits: {:?} unreadable ({e}); ignored", path);
                (Edits::default(), false)
            }
            None => (Edits::default(), false),
        };
        crate::info!(
            "visibility edits: track {} ({}), {} cells with a list, road extension {} sectors ({} half sectors wide), {} camera cells edited{}; map {:.1} MB",
            map.id,
            map.name,
            map.listed_cells().len(),
            edits.road.along,
            edits.road.width_half,
            edits.cams.len(),
            if have_file { format!(" (loaded {:?})", path.unwrap_or_default()) } else { String::new() },
            map.bytes() as f64 / (1024.0 * 1024.0)
        );
        if std::env::var("SR_VISEDIT_MAPCHECK").is_ok_and(|v| v.trim() == "1") {
            let (outside, n, worst) = map.check_spheres();
            crate::info!("visibility editor: map check: {outside} of {n} triangle vertices outside their static's bounding sphere; farthest {worst:.3} radii from its centre; bounds {:?}", map.bounds);
        }
        s.saved = edits.clone();
        s.edits = edits;
        s.have_file = have_file;
        s.undo.clear();
        s.redo.clear();
        s.highlight = None;
        s.map = Some(Arc::new(map));
    });
    bump();
}

/// The race's map (`None` before the first race of the session or outside races).
pub fn map() -> Option<Arc<TrackMap>> {
    with(|s| s.map.clone())
}

/// The camera and the car, every drawn frame (main view).
pub fn publish_live(live: Live) {
    with(|s| s.live = live);
}

pub fn live() -> Live {
    with(|s| s.live.clone())
}

/// The game draws the effective lists (instead of its automatic rule): in the tool, or for
/// a track with saved edits.
pub fn edits_active() -> bool {
    tool_mode() || with(|s| s.have_file && s.map.is_some())
}

/// The effective list of camera cell `cam` (sorted entries; `None`: no map, or no list).
pub fn effective_list(cam: Cell) -> Option<Vec<u32>> {
    with(|s| s.map.as_ref().and_then(|m| s.edits.effective(m, cam)))
}

/// Whether `cell` is drawn from camera cell `cam`.
pub fn visible(cam: Cell, cell: Cell) -> bool {
    with(|s| s.map.as_ref().is_some_and(|m| s.edits.visible(m, cam, cell)))
}

/// A copy of the current edits.
pub fn edits() -> Edits {
    with(|s| s.edits.clone())
}

fn change(s: &mut Shared, f: impl FnOnce(&mut Edits, &TrackMap)) -> bool {
    let Some(map) = s.map.clone() else { return false };
    let before = s.edits.clone();
    f(&mut s.edits, &map);
    if s.edits == before {
        return false;
    }
    s.undo.push(before);
    if s.undo.len() > UNDO_LIMIT {
        s.undo.remove(0);
    }
    s.redo.clear();
    true
}

/// Draw (`visible`) or hide `cells` from every camera cell of `cams` (one undo step).
/// Returns whether anything changed.
pub fn apply(cams: &[Cell], cells: &[Cell], visible: bool) -> bool {
    let changed = with(|s| {
        change(s, |e, map| {
            for &cam in cams {
                for &c in cells {
                    e.set(map, cam, c, visible);
                }
            }
        })
    });
    if changed {
        bump();
    }
    changed
}

/// Back to the default lists' state for `cells` from every camera cell of `cams` (one undo
/// step).
pub fn reset(cams: &[Cell], cells: &[Cell]) -> bool {
    let changed = with(|s| {
        change(s, |e, _| {
            for &cam in cams {
                for &c in cells {
                    e.reset(cam, c);
                }
            }
        })
    });
    if changed {
        bump();
    }
    changed
}

/// The track's road extension (one undo step). Returns whether it changed.
pub fn set_road(reach: RoadReach) -> bool {
    let changed = with(|s| change(s, |e, _| e.road = reach));
    if changed {
        bump();
    }
    changed
}

/// Every edit of camera cells `cams` dropped (one undo step).
pub fn reset_cameras(cams: &[Cell]) -> bool {
    let changed = with(|s| {
        change(s, |e, _| {
            for c in cams {
                e.cams.remove(c);
            }
        })
    });
    if changed {
        bump();
    }
    changed
}

pub fn undo() -> bool {
    let done = with(|s| match s.undo.pop() {
        Some(prev) => {
            let cur = std::mem::replace(&mut s.edits, prev);
            s.redo.push(cur);
            true
        }
        None => false,
    });
    if done {
        bump();
    }
    done
}

pub fn redo() -> bool {
    let done = with(|s| match s.redo.pop() {
        Some(next) => {
            let cur = std::mem::replace(&mut s.edits, next);
            s.undo.push(cur);
            true
        }
        None => false,
    });
    if done {
        bump();
    }
    done
}

/// Unsaved changes.
pub fn dirty() -> bool {
    with(|s| s.edits != s.saved)
}

/// Save the edits of the current track; returns the file.
pub fn save() -> Result<PathBuf, String> {
    let r = with(|s| {
        let map = s.map.clone().ok_or("no track loaded")?;
        let path = file_for(s, &map.id).ok_or("no folder for the edits")?;
        if s.edits.is_empty() {
            // nothing edited (everything undone or reset): no file, so that normal play keeps
            // the automatic rule for this track
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            s.saved = s.edits.clone();
            s.have_file = false;
            return Ok(path);
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        std::fs::write(&path, s.edits.to_text(&map.id, &map.name)).map_err(|e| format!("{}: {e}", path.display()))?;
        s.saved = s.edits.clone();
        s.have_file = true;
        Ok(path)
    });
    bump();
    r
}

/// The cell under the editor's mouse (`blink`: blinking in the game's view), or none.
pub fn set_highlight(cell: Option<Cell>, blink: bool) {
    let changed = with(|s| {
        let c = s.highlight != cell || s.blink != blink;
        s.highlight = cell;
        s.blink = blink;
        c
    });
    if changed {
        bump();
    }
}

pub fn highlight() -> (Option<Cell>, bool) {
    with(|s| (s.highlight, s.blink))
}

/// A short message for the editor's status line (from the game: "saved", errors).
pub fn set_status(msg: impl Into<String>) {
    with(|s| s.status = msg.into());
    bump();
}

pub fn status() -> String {
    with(|s| s.status.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists() -> Vec<Vec<u32>> {
        let mut l = vec![Vec::new(); GRID * GRID];
        // camera cell (10, 10) sees (10, 10), (10, 11), (11, 11)
        l[10 * GRID + 10] = vec![entry((10, 10)), entry((10, 11)), entry((11, 11))];
        // (10, 12) sees (10, 12), (9, 13)
        l[10 * GRID + 12] = vec![entry((10, 12)), entry((9, 13))];
        l
    }

    #[test]
    fn cells_and_entries() {
        assert_eq!(cell_of(WORLD_MIN, WORLD_MIN), (0, 0));
        assert_eq!(cell_of(0.0, 0.0), (128, 128));
        assert_eq!(cell_of(-1.0, 8192.0), (129, 127));
        assert_eq!(cell_of_entry(entry((200, 3))), (200, 3));
        let r = cell_rect((128, 128));
        assert_eq!(r, [0.0, 0.0, 8192.0, 8192.0]);
    }

    #[test]
    fn defaults_and_effective_lists() {
        let l = lists();
        let mut e = Edits::default();
        assert_eq!(default_list(&l, (10, 10)).unwrap().len(), 3);
        // a cell without a list takes the nearest's: (10, 11) is as near to (10, 10) as to
        // (10, 12): the first by row then column wins
        assert_eq!(default_list(&l, (10, 11)), default_list(&l, (10, 10)));
        assert_eq!(default_list(&l, (100, 100)), None);
        let m = TrackMap { lists: l.clone(), ..Default::default() };
        e.set(&m, (10, 10), (10, 11), false);
        e.set(&m, (10, 10), (12, 12), true);
        assert!(!e.visible(&m, (10, 10), (10, 11)));
        assert!(e.visible(&m, (10, 10), (12, 12)));
        assert_eq!(e.effective(&m, (10, 10)).unwrap(), vec![entry((10, 10)), entry((11, 11)), entry((12, 12))]);
        // back to the default: the edit goes
        e.set(&m, (10, 10), (10, 11), true);
        e.reset((10, 10), (12, 12));
        assert!(e.cams.is_empty() && e.is_empty());
        // a camera cell with no list anywhere near (Stadium: none at all): the game draws
        // every cell in view, i.e. every cell holding objects; not edited, it keeps the game's
        // own rule (no list)
        let s = |cell: Cell| MapStatic { cell, center: [0.0; 3], radius: 1.0 };
        let m = TrackMap { lists: l.clone(), statics: vec![s((100, 101)), s((101, 100)), s((10, 10))], ..Default::default() };
        assert!(draws_all_in_view(&m, (100, 100)) && !draws_all_in_view(&m, (10, 11)));
        assert_eq!(default_cells(&m, (100, 100)), vec![entry((10, 10)), entry((100, 101)), entry((101, 100))]);
        assert_eq!(e.effective(&m, (100, 100)), None);
        assert!(e.visible(&m, (100, 100), (101, 100)) && !e.visible(&m, (100, 100), (100, 100)));
        // drawing what is drawn already changes nothing; hiding one: every other cell
        e.set(&m, (100, 100), (100, 101), true);
        assert!(e.is_empty());
        e.set(&m, (100, 100), (100, 101), false);
        assert_eq!(e.effective(&m, (100, 100)).unwrap(), vec![entry((10, 10)), entry((101, 100))]);
        // a sector without objects drawn too
        e.set(&m, (100, 100), (100, 102), true);
        assert_eq!(e.effective(&m, (100, 100)).unwrap(), vec![entry((10, 10)), entry((100, 102)), entry((101, 100))]);
    }

    #[test]
    fn edits_stay_with_their_camera_cell() {
        let l = lists();
        let m = TrackMap { lists: l.clone(), ..Default::default() };
        let mut e = Edits::default();
        // (10, 9) has no list: it draws (10, 10)'s
        assert_eq!(source_cell(&l, (10, 9)), Some((10, 10)));
        assert!(e.visible(&m, (10, 9), (10, 11)));
        // hiding (10, 11) from (10, 10) hides it from (10, 10) only
        e.set(&m, (10, 10), (10, 11), false);
        assert!(!e.visible(&m, (10, 10), (10, 11)));
        assert!(e.visible(&m, (10, 9), (10, 11)));
        assert_eq!(e.edit_state((10, 9), (10, 11)), None);
        assert_eq!(e.edit_state((10, 10), (10, 11)), Some(false));
        // (10, 9)'s own edit
        e.set(&m, (10, 9), (10, 11), false);
        assert!(!e.visible(&m, (10, 9), (10, 11)));
        e.set(&m, (10, 9), (10, 11), true);
        assert!(!e.cams.contains_key(&(10, 9)));
    }

    #[test]
    fn text_round_trip() {
        let m = TrackMap { lists: lists(), ..Default::default() };
        let mut e = Edits::default();
        e.set(&m, (10, 10), (10, 11), false);
        e.set(&m, (10, 10), (30, 2), true);
        e.set(&m, (10, 12), (9, 13), false);
        let t = e.to_text("00ff", "Rally Test");
        assert!(!t.contains("road"), "no road line while the extension is off");
        let (back, id) = Edits::from_text(&t).unwrap();
        assert_eq!(back, e);
        assert_eq!(id, "00ff");
        e.road = RoadReach { along: 6, width_half: 3 };
        let t = e.to_text("00ff", "Rally Test");
        assert!(t.contains("\nroad 6 3\n"), "{t}");
        assert_eq!(Edits::from_text(&t).unwrap().0, e);
        assert!(Edits::from_text("road 6").is_err());
        assert!(Edits::from_text("cam 1").is_err());
        assert!(Edits::from_text("cam 1 2 *3,4").is_err());
    }

    #[test]
    fn the_map_has_world_triangles_inside_their_spheres() {
        use crate::native::extract::StaticMeta;
        use sr_scene::{Instance, Material, Mesh, MeshRange, SceneData, TexFormat, TextureData, Transform, Vertex};
        let v = |p: [f32; 3], uv: [f32; 2]| Vertex { pos: p, normal: [0.0; 3], uv, color: [255, 255, 255, 255] };
        let mesh = Mesh {
            vertices: vec![v([0.0, 0.0, 0.0], [0.0, 0.0]), v([100.0, 0.0, 0.0], [1.0, 0.0]), v([0.0, 50.0, -200.0], [0.0, 1.0])],
            indices: vec![0, 1, 2],
            ranges: vec![MeshRange { material: 0, indices: 0..3 }],
        };
        // a quarter turn about y (model x -> world -z), at (8192, 10, -16384)
        let transform = Transform { basis: [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0], origin: [8192.0, 10.0, -16384.0] };
        let scene = SceneData {
            // 2 x 1 texels 5:6:5: white, magenta
            textures: vec![TextureData { width: 2, height: 1, format: TexFormat::Rgb565, pixels: vec![0xff, 0xff, 0x1f, 0xf8], mipmaps: Default::default() }],
            materials: vec![Material::chroma_key(0, 0xff_ff00ff)],
            meshes: vec![mesh],
            statics: vec![Instance::new(0, transform)],
            ..Default::default()
        };
        // the sphere around the world triangle (8192, 10, -16384), (8192, 10, -16484), (7992, 60, -16384)
        let meta = vec![StaticMeta { cell: cell_of(8100.0, -16400.0), grounded: false, center: [8092.0, 35.0, -16434.0], radius: 120.0 }];
        let full = TrackMap::from_scene(&scene, &meta, &lists(), "Rally Test", true);
        assert_eq!(full.tris.len(), 1);
        let t = &full.tris[0];
        assert_eq!(t.pos, [[8192.0, 10.0, -16384.0], [8192.0, 10.0, -16484.0], [7992.0, 60.0, -16384.0]]);
        assert_eq!((t.uv[1], t.material, t.statik), ([1.0, 0.0], 0, 0));
        assert_eq!(full.check_spheres(), (0, 3, full.check_spheres().2));
        assert!(full.check_spheres().2 <= 1.0);
        assert_eq!(full.bounds, [7992.0, -16484.0, 8192.0, -16384.0]);
        // the chroma key as the renderer compares it (colour bits only), the texels in RGBA
        assert_eq!(full.materials[0], MapMaterial { texture: Some(0), key: Some(0xff00ff), blended: false });
        assert_eq!(full.textures[0].rgba, vec![0xffff_ffff, 0xffff_00ff]);
        assert_eq!(full.statics[0].cell, (125, 128));
        assert_eq!(full.list((10, 10)).unwrap().len(), 3);
        assert!(full.bytes() >= 8 + std::mem::size_of::<MapTri>() + 6 * 4);
        // without the picture: the statics and the lists only, bounds from the spheres, the
        // same track id
        let lean = TrackMap::from_scene(&scene, &meta, &lists(), "Rally Test", false);
        assert!(lean.tris.is_empty() && lean.textures.is_empty());
        assert_eq!(lean.bounds, [7972.0, -16554.0, 8212.0, -16314.0]);
        assert_eq!(lean.id, full.id);
        // a triangle outside its static's sphere is found
        let mut bad = full.clone();
        bad.tris[0].pos[2] = [0.0; 3];
        assert_eq!(bad.check_spheres().0, 1);
    }

    #[test]
    fn the_road_extension_follows_the_road_beyond_the_designers_list() {
        // a long loop: a bottom leg along row 128 from column 120 to 140 and a top leg two
        // rows up, back west; an object at the centre of each of their cells
        let centre = |c: Cell| {
            let r = cell_rect(c);
            [(r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5]
        };
        let (west, east) = (centre((128, 120)), centre((130, 140)));
        let mut pts = Vec::new();
        let mut walk = |a: [f64; 2], b: [f64; 2]| {
            let n = ((b[0] - a[0]).abs().max((b[1] - a[1]).abs()) / 1000.0).ceil() as usize;
            for i in 0..n {
                let t = i as f64 / n as f64;
                pts.push([(a[0] + t * (b[0] - a[0])) as f32, (a[1] + t * (b[1] - a[1])) as f32]);
            }
        };
        walk(west, [east[0], west[1]]);
        walk([east[0], west[1]], east);
        walk(east, [west[0], east[1]]);
        walk([west[0], east[1]], west);
        let road = crate::road::Road::new(&pts).unwrap();
        let mut statics = Vec::new();
        for col in 120..=140 {
            for row in [128, 130] {
                let p = centre((row, col));
                statics.push(MapStatic { cell: (row, col), center: [p[0] as f32, 0.0, p[1] as f32], radius: 100.0 });
            }
        }
        let cam = (128, 130);
        // the designers' list of the camera cell: two sectors of road each way
        let mut lists = vec![Vec::new(); GRID * GRID];
        lists[cam.0 as usize * GRID + cam.1 as usize] = (128..=132).map(|c| entry((128, c))).collect();
        let mut map = TrackMap { statics, lists, ..Default::default() };
        let mut e = Edits::default();
        // no extension: the designers' list; no road line: nothing either
        assert!(road_extension(&map, cam, e.road).is_none());
        assert!(road_extension(&map, cam, RoadReach { along: 4, width_half: 2 }).is_none());
        map.road = Some(road);
        // 4 sectors further each way than the list goes, 1 sector each side: columns 123 to
        // 127 and 133 to 137 (the ends reach half a sector further); the top leg, two
        // sectors away in a straight line, is not beside the road there
        e.road = RoadReach { along: 4, width_half: 2 };
        let got = road_extension(&map, cam, e.road).unwrap().cells;
        let want: Vec<Cell> = (123..=127).chain(133..=137).map(|c| (128, c)).collect();
        assert_eq!(got, want);
        assert_eq!(e.effective(&map, cam).unwrap().len(), 5 + 10);
        // a sector hidden by the user stays hidden, from this camera cell only
        e.set(&map, cam, (128, 135), false);
        assert!(!e.visible(&map, cam, (128, 135)) && e.visible(&map, cam, (128, 134)));
        // wider than the gap: the top leg beside those stretches comes too
        let wide = road_extension(&map, cam, RoadReach { along: 4, width_half: 5 }).unwrap().cells;
        assert!(wide.contains(&(130, 135)) && !wide.contains(&(130, 120)), "{wide:?}");
        // the edit area: 4 sectors along the road each way from (128, 130); the top leg
        // passes as near as (128, 134) in a straight line, but is far along the road
        let area = road_area(&map, cam, 4.0 * CELL_SIZE);
        assert!(area.contains(&cam) && area.contains(&(128, 126)) && area.contains(&(128, 134)), "{area:?}");
        assert!(!area.contains(&(128, 137)) && !area.contains(&(130, 130)) && !area.contains(&(130, 126)), "{area:?}");
        assert_eq!(road_area(&map, (100, 100), 2.0 * CELL_SIZE).len(), 25, "off the road: a square");
        // a camera cell far from the road: nothing to follow
        assert!(road_sources(map.road.as_ref().unwrap(), (100, 100)).is_empty());
        assert!(road_extension(&map, (100, 100), e.road).is_none());
    }
}
