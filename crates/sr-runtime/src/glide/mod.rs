//! 3dfx Glide 2.x, implemented natively.
//!
//! The game imports 130 Glide entry points by name through 5-byte thunks; the profile
//! points every thunk at a hook whose argument is the import index. This module decodes the
//! stdcall arguments, keeps the Voodoo Graphics state (combine units, blending, depth,
//! fog, texture memory, frame buffer) and hands finished frames to a [`Backend`].
//!
//! Constants and structures follow the public Glide 2.x headers (`glide.h`, `sst1vid.h`).

pub mod consts;
pub mod state;
pub mod tex;


use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;
pub use state::{GlideState, Vertex};

/// Everything a backend needs to draw with the current Glide state.
pub struct DrawContext<'a> {
    pub st: &'a GlideState,
    pub tmu: &'a tex::TexMemory,
    pub palette: &'a [u32; 256],
    /// Hash of the palette's contents (cache key for paletted textures).
    pub palette_gen: u64,
    pub ncc: &'a [[u8; 48]; 2],
    /// The game's screen size (`grSstWinOpen` resolution).
    pub screen: (u32, u32),
    /// Id of the 3D model part these polygons belong to (1..127, bit 7 set for cars and
    /// their wheels); 0 = none (2D).
    pub part: u8,
    /// The kind of object the part belongs to.
    pub kind: crate::depth::Kind,
}

/// What a rendering backend receives from the Glide layer.
pub trait Backend: Send {
    /// `grSstWinOpen`: the game's (virtual) screen size.
    fn open(&mut self, width: u32, height: u32) -> Result<(), String>;
    fn close(&mut self) {}
    /// Draw triangles in the game's screen space with the current state.
    fn triangles(&mut self, ctx: &DrawContext, verts: &[Vertex]);
    /// `grBufferClear`.
    fn clear(&mut self, ctx: &DrawContext, color: u32, alpha: u8, depth: u16);
    /// Pixels written through the linear frame buffer since the last call: the screen
    /// rectangle `rect` = (x, y, w, h) as RGB565 with `mask` false = untouched pixel, drawn
    /// as `parts` (screen rectangles inside it, each anchored to the centre or one side of
    /// a widescreen view: HUD elements). `slot` = (n, generation) for images that will be
    /// drawn again unchanged (the HUD on interpolated frames): the backend may keep image n
    /// and skip the upload when it is called again with the same generation.
    /// `sprites`: the game's pictures blitted in this time, drawn from their own pixels
    /// first, in order (`crate::ui2d`; their pixels are not in `mask`). `pixels` is empty
    /// when every written pixel is a sprite's.
    fn lfb_write(&mut self, rect: (u32, u32, u32, u32), pixels: &[u16], mask: &[bool], parts: &[LfbPart], slot: Option<(u32, u64)>, sprites: &[Sprite2D]);
    /// `grBufferSwap`: show the frame. `interval` is the game's requested swap interval.
    fn swap(&mut self, interval: u32);
    /// The game starts drawing a 3D view (`set_view`): the main view, the rear-view
    /// mirror, a menu scene. Backends with their own depth buffer clear it.
    fn begin_view(&mut self) {}
    /// The texture at a TMU address changed (download); backends drop cached copies.
    fn invalidate_textures(&mut self, start: u32, end: u32);
    /// Called after a buffer swap, repeatedly while it returns true: the backend wants the
    /// game held at this frame (settings overlay open) and shows a frame per call.
    fn hold(&mut self) -> bool {
        false
    }
    /// Wait until a frame can be shown without the present blocking (VSync on: the display
    /// has taken the previous one), at most `timeout`. False = not before the timeout.
    fn wait_ready(&mut self, _timeout: std::time::Duration) -> bool {
        true
    }

    // ---- native renderer (docs/04-native-renderer.md, docs/re/native/integration.md) ----
    // Backends without one keep these defaults: the scene is refused, so the runtime stays
    // on the Glide path.

    /// Upload the scene of a race (once per race; replaces the previous one).
    fn native_scene(&mut self, _scene: &crate::native::scene::SceneData) -> Result<(), String> {
        Err("this backend has no native renderer".into())
    }
    /// The data of the views drawn next (transforms, fades, animation): before `native_view`.
    fn native_frame(&mut self, _frame: &crate::native::scene::FrameData) -> Result<(), String> {
        Ok(())
    }
    /// Draw view `view` of `frame` now, where the game draws its world for that view (after
    /// `begin_view`, before anything 2D); what the Glide path draws afterwards is depth
    /// tested against it.
    fn native_view(&mut self, _frame: &crate::native::scene::FrameData, _view: usize) -> Result<NativeViewStats, String> {
        Ok(NativeViewStats::default())
    }
    /// The backing test's results finished since the last call, oldest first
    /// (`FrameData::static_backing`, `docs/re/native/visibility.md`); `wait`: block until
    /// every view drawn so far has its results.
    fn native_backing(&mut self, _wait: bool) -> Vec<crate::native::scene::BackingResults> {
        Vec::new()
    }
}

/// What drawing a native view did (diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeViewStats {
    pub draws: u32,
    pub triangles: u32,
}

/// The rectangle `x0..x1` x `y0..y1` of an LFB image and where it goes in a widescreen
/// view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfbPart {
    pub x0: u32,
    pub x1: u32,
    pub y0: u32,
    pub y1: u32,
    /// -1: move to the left edge, 0: stay centred, 1: move to the right edge,
    /// 2: stretch across the whole view (full-width bars).
    pub anchor: i32,
}

/// One of the game's 2D pictures with a palette applied, at its own resolution (before
/// the 3dfx build's stretch to 480 lines; `crate::ui2d`).
pub struct Picture {
    /// Identifies the pixels (texture cache key).
    pub key: u64,
    pub w: u32,
    pub h: u32,
    /// RGBA8, R in the low byte, premultiplied (index 0 of a transparent picture: 0).
    pub rgba: Vec<u32>,
}

/// A blit of a picture: `pic` fills the box `dst` = (x, y, width, height) of the game's
/// 640 x 480 screen (the box the game filled with its stretched copy), cut to `clip`
/// (x0, y0, x1, y1), moved like the HUD element it is in (`anchor`, as `LfbPart`).
#[derive(Clone)]
pub struct Sprite2D {
    pub pic: std::sync::Arc<Picture>,
    pub dst: [f32; 4],
    pub clip: [i32; 4],
    pub anchor: i32,
}

/// A backend that draws nothing (tests, headless runs).
pub struct NullBackend;

impl Backend for NullBackend {
    fn open(&mut self, _: u32, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn triangles(&mut self, _: &DrawContext, _: &[Vertex]) {}
    fn clear(&mut self, _: &DrawContext, _: u32, _: u8, _: u16) {}
    fn lfb_write(&mut self, _: (u32, u32, u32, u32), _: &[u16], _: &[bool], _: &[LfbPart], _: Option<(u32, u64)>, _: &[Sprite2D]) {}
    fn swap(&mut self, _: u32) {}
    fn invalidate_textures(&mut self, _: u32, _: u32) {}
}

pub struct Glide {
    pub names: Vec<String>,
    pub arg_bytes: Vec<u16>,
    pub calls: Vec<u64>,
    pub st: GlideState,
    pub tmu: tex::TexMemory,
    pub backend: Box<dyn Backend>,
    pub width: u32,
    pub height: u32,
    /// Texture palette and NCC tables (TMU state, not part of `GrState`).
    pub palette: [u32; 256],
    pub palette_gen: u64,
    /// Hash and sampled entries of each palette table seen, by address.
    palette_tables: std::collections::HashMap<u32, (u64, [u32; 4])>,
    pub ncc: [[u8; 48]; 2],
    /// Emulated linear frame buffer (RGB565, 1024-pixel stride like Voodoo Graphics).
    lfb_locked_write: Option<u32>,
    lfb_addr: u32,
    pub frames: u64,
    pub tris: u64,
    verts: Vec<Vertex>,
    trace_budget: Vec<u32>,
    /// Import names without decoration (`_grDrawTriangle@12` -> `GRDRAWTRIANGLE`).
    funcs: Vec<&'static str>,
    /// The calls made per polygon, by import index (fast path, no name matching).
    hot: Vec<Hot>,
    /// `grHints(GR_HINT_STWHINT, mask)`.
    stw_hint: u32,
    vertex_samples: u32,
    /// Log every call (with vertices) during every n-th frame (`SR_TRACE_EVERY`).
    pub trace_frame: Option<u64>,
    /// Widescreen extension the game currently renders with (game pixels each side).
    pub view_ext: f32,
    /// Widescreen extension of the main view (the HUD anchors to its edges).
    pub hud_ext: f32,
    /// Link address the current Glide call returns to.
    caller: u32,
    /// A race view is being drawn this frame (the sky was drawn): HUD elements anchor to
    /// the sides of a widescreen view.
    pub race_frame: bool,
    fps_mark: Option<std::time::Instant>,
    /// Frames drawn by the runtime between the game's own (diagnostics).
    pub extra_frames: u64,
    /// Triangles with a vertex off the screen (broken geometry; diagnostics).
    pub bad_tris: u64,
    /// Depth of the flat-colour polygon being built (see `crate::depth`).
    pub flat_depth: crate::depth::FlatDepth,
    /// Id of the 3D model part being drawn (`crate::depth`), 1..127 cycling (bit 7: car).
    pub part: u8,
    /// Kind of the object being drawn (set per object, `crate::depth`).
    pub part_kind: crate::depth::Kind,
    part_seq: u8,
    /// Car objects: object number (0..7) and part within it (0..15), see `next_part`.
    car_object: u8,
    car_part: u8,
    /// Exact screen positions of the vertices of the object part being drawn
    /// (`crate::subpixel`).
    pub subpixel: crate::subpixel::SubPixelMap,
    /// The current call comes from the vertex builder building a flat-colour polygon.
    flat_poly: bool,
    /// Rendering mode of the polygon the builder is drawing (u32::MAX: other callers).
    builder_mode: u32,
    /// Depth buffer on and a builder polygon: the sign of a front face's screen area
    /// (0 = not applicable).
    front_sign: f32,
    /// The game culls this object's back faces (crate::depth::CULL_ON).
    cull_flag: bool,
    /// The batched triangles are back faces (drawn without writing depth).
    back_batch: bool,
    /// Back faces culled (diagnostics).
    pub culled_back: u64,
    /// Recording what is drawn after the main view's world (HUD, mirror) for extra frames.
    pub hud_recording: bool,
    hud_ops: Vec<HudOp>,
    last_hud: Vec<HudOp>,
    /// Where the rear-view mirror's drawing starts and ends in `hud_ops`, and in `last_hud`
    /// (extra frames draw the mirror again instead of showing the recorded one).
    mirror_start: Option<usize>,
    mirror_end: Option<usize>,
    last_mirror: Option<(usize, usize)>,
    /// This frame's swap shows nothing: interpolated frames are shown instead.
    pub suppress_present: bool,
    /// LFB pixels written and not yet sent to the backend.
    lfb_pending: bool,
    /// Time stamp counter cycles in Glide calls (statistics).
    host_time: u64,
    swap_time: u64,
    /// Host time per Glide function (diagnostics).
    pub func_time: Vec<u64>,
    /// The game's pictures blitted since the last flush (`crate::ui2d`), and for each
    /// screen pixel the blit that wrote it last (index + 1, 0 = none) and the value it
    /// wrote; `ui_box` bounds the marked pixels.
    ui_ops: Vec<Sprite2D>,
    ui_owner: Vec<u16>,
    ui_expect: Vec<u16>,
    ui_box: Option<[i32; 4]>,
}

/// Something drawn over the world (HUD, mirror), kept to be drawn again on extra frames.
enum HudOp {
    Tris { st: GlideState, palette: Box<[u32; 256]>, palette_gen: u64, ncc: [[u8; 48]; 2], verts: Vec<Vertex> },
    Lfb { rect: (u32, u32, u32, u32), pixels: Vec<u16>, mask: Vec<bool>, parts: Vec<LfbPart>, slot: (u32, u64), sprites: Vec<Sprite2D> },
}

/// Return addresses of the sky / background quads (cb800 ff.): their texture coordinates
/// are extended to the widened quad.
const SKY_CALLERS: &[u32] = &[0xcb99f, 0xcb9b3, 0xcba39, 0xcba4d, 0xcbb77, 0xcbb8b];
/// Return address of `grDrawLine` in the game's line routine (c0dca): the HUD needle.
const LINE_CALLER: u32 = 0xc0ea0;
/// The game's frame count (buffer swaps; diagnostics: screenshot names).
pub static FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Emulated texture memory: the Voodoo Graphics' 4 MB (the game's texture manager keeps
/// its own, smaller budget anyway).
const TMU_BYTES: u32 = 4 << 20;

/// `grDrawTriangle` call in the 3D vertex builder (cc5e8).
const BUILDER_CALLER: u32 = 0xcc6f3;

/// 64-bit FNV-1a hash of a texture palette (identifies its contents for texture caching).
fn palette_hash(p: &[u32; 256]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &c in p {
        h = (h ^ c as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Runs of `true` in `used` (as half-open ranges), joined across gaps shorter than `gap`.
fn runs(used: &[bool], gap: u32) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, &u) in used.iter().enumerate() {
        if !u {
            continue;
        }
        let i = i as u32;
        match out.last_mut() {
            Some(last) if i - last.1 < gap => last.1 = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

/// Columns of empty space that separate two LFB elements on the same rows. Words of a line
/// of text stay one element (the intro's credits: gaps of about 16 columns, a line split
/// into words put a word in the outer quarter at the screen's edge); the race HUD's
/// elements on the same rows are 168 or more apart.
const LFB_ELEMENT_GAP: u32 = 64;

/// Which side a HUD element spanning game columns `x0..=x1` belongs to: elements in the
/// outer quarters of the screen follow its edges (lap, times, position, speedometer);
/// the rest stays centred (progress bar and its markers, messages).
fn hud_anchor(x0: f32, x1: f32) -> i32 {
    let c = (x0 + x1) / 2.0;
    if c < 160.0 {
        -1
    } else if c > 480.0 {
        1
    } else {
        0
    }
}

const GR_HINT_STWHINT: u32 = 0;
const GR_STWHINT_W_DIFF_TMU0: u32 = 0x02;

pub const LFB_STRIDE_PIXELS: u32 = 1024;
const LFB_HEIGHT: u32 = 1024;
/// Untouched LFB pixels hold this value so writes can be told apart from untouched pixels.
const LFB_EMPTY: u16 = 0x0821;

/// Undecorated import names, kept for the life of the process (a few dozen short strings).
fn names_for_dispatch(names: &[String]) -> Vec<&'static str> {
    names
        .iter()
        .map(|n| &*Box::leak(n.trim_start_matches('_').split('@').next().unwrap_or("").to_string().into_boxed_str()))
        .collect()
}

impl Glide {
    pub fn new(names: Vec<String>, backend: Box<dyn Backend>) -> Glide {
        let arg_bytes = names.iter().map(|n| n.rsplit_once('@').and_then(|(_, b)| b.parse().ok()).unwrap_or(0)).collect();
        let n = names.len();
        let names_copy = names.clone();
        let lfb_bytes = LFB_STRIDE_PIXELS * LFB_HEIGHT * 2;
        // the game keeps its address (deterministic in deterministic mode)
        let lfb_addr = mem::alloc_guest(lfb_bytes).expect("cannot allocate the Glide frame buffer");
        Glide {
            names,
            arg_bytes,
            calls: vec![0; n],
            st: GlideState::default(),
            tmu: tex::TexMemory::new(TMU_BYTES),
            backend,
            width: 640,
            height: 480,
            palette: [0; 256],
            palette_gen: 0,
            palette_tables: Default::default(),
            ncc: [[0; 48]; 2],
            lfb_locked_write: None,
            lfb_addr,
            frames: 0,
            tris: 0,
            verts: Vec::with_capacity(4096),
            trace_budget: vec![8; n],
            funcs: names_for_dispatch(&names_copy),
            hot: names_for_dispatch(&names_copy).iter().map(|n| Hot::of(n)).collect(),
            stw_hint: 0,
            vertex_samples: 0,
            trace_frame: std::env::var("SR_TRACE_EVERY").ok().and_then(|v| v.parse().ok()),
            view_ext: 0.0,
            hud_ext: 0.0,
            caller: 0,
            race_frame: false,
            fps_mark: None,
            extra_frames: 0,
            bad_tris: 0,
            flat_depth: Default::default(),
            part: 0,
            part_kind: Default::default(),
            part_seq: 0,
            car_object: 0,
            car_part: 0,
            subpixel: Default::default(),
            flat_poly: false,
            builder_mode: u32::MAX,
            front_sign: 0.0,
            cull_flag: false,
            back_batch: false,
            culled_back: 0,
            hud_recording: false,
            hud_ops: Vec::new(),
            last_hud: Vec::new(),
            mirror_start: None,
            mirror_end: None,
            last_mirror: None,
            suppress_present: false,
            lfb_pending: false,
            host_time: Default::default(),
            swap_time: Default::default(),
            func_time: vec![0; n],
            ui_ops: Vec::new(),
            ui_owner: Vec::new(),
            ui_expect: Vec::new(),
            ui_box: None,
        }
    }

    /// A write lock on the frame buffer is held (the game is drawing 2D).
    pub fn lfb_locked(&self) -> bool {
        self.lfb_locked_write.is_some()
    }

    /// A picture blit (`crate::ui2d`): returns its number for `ui_mark`.
    pub fn ui_op(&mut self, s: Sprite2D) -> u16 {
        if self.ui_ops.len() >= u16::MAX as usize - 1 {
            // (never seen: a flush per frame at least) the oldest stay in the image
            self.ui_ops.clear();
            self.clear_ui_marks();
        }
        self.ui_ops.push(s);
        self.ui_ops.len() as u16
    }

    /// Blit `n` writes RGB565 `v` at screen pixel (x, y).
    #[inline]
    pub fn ui_mark(&mut self, n: u16, x: i32, y: i32, v: u16) {
        let (w, h) = (self.width as i32, self.height as i32);
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        if self.ui_owner.len() != (w * h) as usize {
            self.ui_owner = vec![0; (w * h) as usize];
            self.ui_expect = vec![0; (w * h) as usize];
        }
        let i = (y * w + x) as usize;
        self.ui_owner[i] = n;
        self.ui_expect[i] = v;
        let b = self.ui_box.get_or_insert([x, y, x + 1, y + 1]);
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x + 1);
        b[3] = b[3].max(y + 1);
    }

    fn clear_ui_marks(&mut self) {
        if let Some([x0, y0, x1, y1]) = self.ui_box.take() {
            let w = self.width as i32;
            for y in y0..y1 {
                self.ui_owner[(y * w + x0) as usize..(y * w + x1) as usize].fill(0);
            }
        }
    }

    fn lfb_slice(&self) -> &'static mut [u16] {
        unsafe { std::slice::from_raw_parts_mut(mem::ptr(self.lfb_addr) as *mut u16, (LFB_STRIDE_PIXELS * LFB_HEIGHT) as usize) }
    }

    /// Send pixels the game wrote through the LFB to the backend (in draw order, after any
    /// triangles submitted before) and reset the buffer. Writes are collected until
    /// something else is drawn, so a frame's HUD elements usually arrive in one call; only
    /// the rectangle that holds written pixels is converted and uploaded.
    fn flush_lfb(&mut self) {
        self.flush_tris();
        self.lfb_pending = false;
        let (w, h) = (self.width as usize, self.height as usize);
        let stride = LFB_STRIDE_PIXELS as usize;
        let buf = self.lfb_slice();
        let empty4 = (LFB_EMPTY as u64) * 0x0001_0001_0001_0001;
        let (mut x0, mut x1, mut y0, mut y1) = (w, 0, h, 0);
        for y in 0..h {
            let row = &buf[y * stride..][..w];
            let (pre, mid, post) = unsafe { row.align_to::<u64>() };
            if pre.iter().all(|&p| p == LFB_EMPTY) && mid.iter().all(|&q| q == empty4) && post.iter().all(|&p| p == LFB_EMPTY) {
                continue;
            }
            let first = row.iter().position(|&p| p != LFB_EMPTY).unwrap_or(0);
            let last = row.iter().rposition(|&p| p != LFB_EMPTY).unwrap_or(0);
            x0 = x0.min(first);
            x1 = x1.max(last + 1);
            y0 = y0.min(y);
            y1 = y + 1;
        }
        if y0 >= y1 {
            self.ui_ops.clear();
            self.clear_ui_marks();
            return;
        }
        let (bw, bh) = (x1 - x0, y1 - y0);
        let mut pixels = Vec::with_capacity(bw * bh);
        let mut mask = Vec::with_capacity(bw * bh);
        for y in y0..y1 {
            for p in buf[y * stride + x0..][..bw].iter_mut() {
                pixels.push(*p);
                mask.push(*p != LFB_EMPTY);
                *p = LFB_EMPTY;
            }
        }
        let rect = (x0 as u32, y0 as u32, bw as u32, bh as u32);
        // the HUD elements as the game's pixels make them (the pictures' stretched pixels
        // included: the same anchors as with the original scaling)
        let parts = self.lfb_parts(rect, &mask);
        let (pixels, mask, sprites) = if self.ui_ops.is_empty() { (pixels, mask, Vec::new()) } else { self.take_sprites(rect, pixels, mask, &parts) };
        if self.hud_recording {
            let n = self.hud_ops.iter().filter(|o| matches!(o, HudOp::Lfb { .. })).count() as u32;
            let slot = (n, self.frames);
            self.backend.lfb_write(rect, &pixels, &mask, &parts, Some(slot), &sprites);
            self.hud_ops.push(HudOp::Lfb { rect, pixels, mask, parts, slot, sprites });
        } else {
            self.backend.lfb_write(rect, &pixels, &mask, &parts, None, &sprites);
        }
    }

    /// The picture blits since the last flush (`crate::ui2d`): each takes the anchor of the
    /// HUD element it lies in, and the pixels that are still the ones a blit wrote last
    /// leave the image (the renderer draws them from the picture). `pixels` / `mask` of the
    /// flushed rectangle; returns them without those pixels (empty when none is left).
    fn take_sprites(&mut self, rect: (u32, u32, u32, u32), pixels: Vec<u16>, mut mask: Vec<bool>, parts: &[LfbPart]) -> (Vec<u16>, Vec<bool>, Vec<Sprite2D>) {
        let mut sprites = std::mem::take(&mut self.ui_ops);
        let (rx, ry, rw, rh) = rect;
        let w = self.width as usize;
        let mut left = 0usize;
        for y in 0..rh as usize {
            let row = (ry as usize + y) * w + rx as usize;
            for x in 0..rw as usize {
                let i = y * rw as usize + x;
                if mask[i] && self.ui_owner.get(row + x).is_some_and(|&n| n != 0) && self.ui_expect[row + x] == pixels[i] {
                    mask[i] = false;
                } else if mask[i] {
                    left += 1;
                }
            }
        }
        self.clear_ui_marks();
        let race = self.race_frame && self.hud_ext > 0.0;
        for s in &mut sprites {
            // the element with the largest share of the blit's rectangle
            let [cx0, cy0, cx1, cy1] = s.clip;
            let overlap = |p: &LfbPart| {
                let ox = (cx1.min(p.x1 as i32) - cx0.max(p.x0 as i32)).max(0);
                let oy = (cy1.min(p.y1 as i32) - cy0.max(p.y0 as i32)).max(0);
                ox as i64 * oy as i64
            };
            s.anchor = match parts.iter().filter(|p| overlap(p) > 0).max_by_key(|p| overlap(p)) {
                Some(p) => p.anchor,
                None if race => hud_anchor(cx0 as f32, (cx1 - 1) as f32),
                None => 0,
            };
        }
        if left == 0 {
            return (Vec::new(), Vec::new(), sprites);
        }
        (pixels, mask, sprites)
    }

    /// Split an LFB image (the rectangle `rect` of the screen) into HUD elements (bands of
    /// rows, then runs of columns, separated by empty space) and anchor each to the side it
    /// is on; outside a widescreen race view the image stays whole. Parts are in screen
    /// coordinates.
    fn lfb_parts(&self, rect: (u32, u32, u32, u32), mask: &[bool]) -> Vec<LfbPart> {
        let (rx, ry, w, h) = rect;
        let whole = vec![LfbPart { x0: rx, x1: rx + w, y0: ry, y1: ry + h, anchor: 0 }];
        if !(self.race_frame && self.hud_ext > 0.0) {
            return whole;
        }
        let row_used: Vec<bool> = (0..h).map(|y| mask[(y * w) as usize..][..w as usize].iter().any(|&m| m)).collect();
        let mut parts = Vec::new();
        for (y0, y1) in runs(&row_used, 4) {
            let col_used: Vec<bool> = (0..w).map(|x| (y0..y1).any(|y| mask[(y * w + x) as usize])).collect();
            for (x0, x1) in runs(&col_used, LFB_ELEMENT_GAP) {
                let (x0, x1, y0, y1) = (x0 + rx, x1 + rx, y0 + ry, y1 + ry);
                let anchor = if x0 == 0 && x1 == self.width { 2 } else { hud_anchor(x0 as f32, (x1 - 1) as f32) };
                parts.push(LfbPart { x0, x1, y0, y1, anchor });
            }
        }
        if self.vertex_samples > 0 {
            crate::info!("    lfb parts {parts:?}");
        }
        parts
    }

    fn download_table(&mut self, ty: u32, data: u32, start: u32, end: u32) {
        match ty {
            consts::GR_TEXTABLE_PALETTE => {
                let (lo, hi) = (start.min(255) as usize, end.min(255) as usize);
                if lo > hi {
                    return;
                }
                if (lo, hi) == (0, 255) {
                    // The game downloads one of its texture palettes before almost every
                    // texture, mostly one of the few it last used: each table's hash is kept
                    // by its address (a few sampled entries tell when a table was rewritten
                    // in place), and the palette is copied only when it changes.
                    let sample = unsafe { [mem::rd32(data), mem::rd32(data + 4 * 37), mem::rd32(data + 4 * 128), mem::rd32(data + 4 * 255)] };
                    let table = unsafe { std::slice::from_raw_parts(mem::ptr(data) as *const u32, 256) };
                    let phash = match self.palette_tables.get(&data) {
                        Some(&(h, smp)) if smp == sample => h,
                        _ => {
                            let mut t = [0u32; 256];
                            t.copy_from_slice(table);
                            let h = palette_hash(&t);
                            if self.palette_tables.len() > 4096 {
                                self.palette_tables.clear();
                            }
                            self.palette_tables.insert(data, (h, sample));
                            h
                        }
                    };
                    // same contents as the current palette: nothing changes
                    if phash != self.palette_gen {
                        self.state_change();
                        self.palette.copy_from_slice(table);
                        self.palette_gen = phash;
                    }
                    return;
                }
                // a partial download: onto the full palette
                let new = unsafe { std::slice::from_raw_parts(mem::ptr(data) as *const u32, hi - lo + 1) };
                if self.palette[lo..=hi] != *new {
                    self.state_change();
                    self.palette[lo..=hi].copy_from_slice(new);
                    self.palette_gen = palette_hash(&self.palette);
                }
            }
            consts::GR_TEXTABLE_NCC0 | consts::GR_TEXTABLE_NCC1 => unsafe {
                let new = mem::slice(data, 48);
                if self.ncc[ty as usize] != *new {
                    self.state_change();
                    self.ncc[ty as usize].copy_from_slice(new);
                }
            },
            _ => {}
        }
    }

    fn reset_lfb(&mut self) {
        self.lfb_slice().fill(LFB_EMPTY);
        self.ui_ops.clear();
        self.ui_owner.clear();
        self.ui_expect.clear();
        self.ui_box = None;
    }

    fn flush_tris(&mut self) {
        if !self.verts.is_empty() {
            let ctx = DrawContext {
                st: &self.st,
                tmu: &self.tmu,
                palette: &self.palette,
                palette_gen: self.palette_gen,
                ncc: &self.ncc,
                screen: (self.width, self.height),
                part: self.part,
                kind: self.part_kind,
            };
            self.backend.triangles(&ctx, &self.verts);
            if self.hud_recording {
                self.hud_ops.push(HudOp::Tris { st: self.st, palette: Box::new(self.palette), palette_gen: self.palette_gen, ncc: self.ncc, verts: self.verts.clone() });
            }
            self.verts.clear();
        }
    }

    /// Finish an extra (interpolated) frame: flush its world, then draw the last real
    /// frame's HUD and mirror over it.
    pub fn end_extra_frame(&mut self) {
        self.extra_hud(0, usize::MAX);
        self.race_frame = false;
    }

    /// An extra frame whose mirror was drawn again between `extra_hud` calls is done.
    pub fn finish_extra_frame(&mut self) {
        self.flush_tris();
        self.race_frame = false;
    }

    /// Extra frames: draw the last real frame's recorded ops `from..to` (HUD, mirror) over
    /// what is drawn so far.
    pub fn extra_hud(&mut self, from: usize, to: usize) {
        self.flush_tris();
        if self.lfb_pending {
            self.flush_lfb();
        }
        let ops = std::mem::take(&mut self.last_hud);
        for op in ops.iter().take(to).skip(from) {
            match op {
                HudOp::Tris { st, palette, palette_gen, ncc, verts } => {
                    let ctx = DrawContext { st, tmu: &self.tmu, palette, palette_gen: *palette_gen, ncc, screen: (self.width, self.height), part: 0, kind: Default::default() };
                    self.backend.triangles(&ctx, verts);
                }
                HudOp::Lfb { rect, pixels, mask, parts, slot, sprites } => self.backend.lfb_write(*rect, pixels, mask, parts, Some(*slot), sprites),
            }
        }
        self.last_hud = ops;
    }

    /// The rear-view mirror's view starts (`crate::widescreen`): mark it in the recording.
    pub fn mirror_begin(&mut self) {
        if self.hud_recording && self.mirror_start.is_none() {
            self.flush_tris();
            if self.lfb_pending {
                self.flush_lfb();
            }
            self.mirror_start = Some(self.hud_ops.len());
        }
    }

    /// The game's end-of-frame drawing starts (4b6c4, after the mirror): the mirror's end.
    pub fn mirror_end(&mut self) {
        if self.hud_recording && self.mirror_start.is_some() && self.mirror_end.is_none() {
            self.flush_tris();
            if self.lfb_pending {
                self.flush_lfb();
            }
            self.mirror_end = Some(self.hud_ops.len());
        }
    }

    /// The recorded ops of the last real frame that drew the mirror (start, end).
    pub fn last_mirror(&self) -> Option<(usize, usize)> {
        self.last_mirror
    }

    /// Ops recorded so far in this frame (the menus' extra frames mark their native views
    /// with it, `native::menus`).
    pub fn recorded_ops(&self) -> usize {
        self.hud_ops.len()
    }

    /// A view starts (`set_view`): what is batched is drawn first, then the backend forgets
    /// the depths (the mirror's polygons must not be hidden by the main view's).
    pub fn begin_view(&mut self) {
        self.flush_tris();
        if self.lfb_pending {
            self.flush_lfb();
        }
        self.backend.begin_view();
    }

    /// Draw what is batched (triangles, LFB pixels) now: before the runtime draws into the
    /// frame itself (the native renderer's views).
    pub fn flush(&mut self) {
        self.flush_tris();
        if self.lfb_pending {
            self.flush_lfb();
        }
    }

    /// Texture downloads so far (diagnostics).
    pub fn download_count(&self) -> u64 {
        self.funcs.iter().zip(&self.calls).filter(|(f, _)| f.starts_with("GRTEXDOWNLOAD") && **f != "GRTEXDOWNLOADTABLE").map(|(_, c)| *c).sum()
    }

    /// A new 3D object of kind `kind` starts (`crate::depth`). The previous object's
    /// triangles still batched are drawn first: the renderer picks their depth rules by
    /// the kind (a car's last polygons drawn as the smoke puff after it lost their depth
    /// window; the Shield's rear overlay flickered).
    pub fn next_object(&mut self, kind: crate::depth::Kind) {
        self.flush_tris();
        self.part_kind = kind;
        self.car_object = (self.car_object + 1) & 7;
        self.car_part = 0;
    }

    /// The engine starts drawing another model part (`crate::depth`).
    pub fn next_part(&mut self) {
        self.flush_tris();
        // Cars (and everything in the menus' 3D scenes: cars and their props, which no
        // scene table names) get ids 80h | object << 4 | part, so that the renderer can
        // tell a car's own part, its other parts and everything else apart (its glass keeps
        // the game's order over its own part, a narrow depth window over the rest of the
        // car, and none over walls and other cars); the rest 1..127.
        let car = matches!(self.part_kind, crate::depth::Kind::Car | crate::depth::Kind::Wheel) || !self.race_frame;
        if car {
            self.car_part = (self.car_part + 1) & 15;
            self.part = 0x80 | self.car_object << 4 | self.car_part;
        } else {
            self.part_seq = if self.part_seq >= 127 { 1 } else { self.part_seq + 1 };
            self.part = self.part_seq;
        }
    }

    /// Any state change must flush batched triangles first.
    fn state_change(&mut self) {
        self.flush_tris();
    }

    fn push_tri(&mut self, a: u32, b: u32, c: u32) {
        if self.lfb_pending {
            self.flush_lfb();
        }
        let (mut va, mut vb, mut vc) = unsafe { (Vertex::read(a), Vertex::read(b), Vertex::read(c)) };
        // Glide uses the vertex 1/w for texturing unless GR_STWHINT_W_DIFF_TMU0 says the
        // TMU has its own (tmuvtx[0].oow is ignored otherwise).
        if self.stw_hint & GR_STWHINT_W_DIFF_TMU0 == 0 {
            for v in [&mut va, &mut vb, &mut vc] {
                v.tmu_oow = v.oow;
            }
        }
        // 3D vertices: exact screen positions instead of the engine's whole pixels
        if self.builder_mode != u32::MAX {
            for v in [&mut va, &mut vb, &mut vc] {
                if let Some((x, y, r)) = self.subpixel.get(v.x, v.y) {
                    v.x = x;
                    v.y = y;
                    v.oow *= r;
                    v.sow *= r;
                    v.tow *= r;
                    v.tmu_oow *= r;
                }
            }
        }
        if self.flat_poly {
            for v in [&mut va, &mut vb, &mut vc] {
                v.oow = self.flat_depth.oow(v.x, v.y);
            }
        }
        if self.vertex_samples > 0 {
            let p = |v: &Vertex| format!("({:.2},{:.2} oow={:.6e} ooz={:.0} rgba={:.0},{:.0},{:.0},{:.0} st={:.1},{:.1})", v.x, v.y, v.oow, v.ooz, v.r, v.g, v.b, v.a, v.sow / v.tmu_oow, v.tow / v.tmu_oow);
            crate::info!("    tri {} {} {} caller {:#x} mode {} part {:#x} kind {:?}", p(&va), p(&vb), p(&vc), self.caller, self.builder_mode as i32, self.part, self.part_kind);
        }
        // The engine's 2D clipper keeps every vertex on screen: one outside is broken
        // geometry (diagnostics: counted, the first few logged)
        let (lo, hi) = (-self.view_ext - 2.0, self.width as f32 + self.view_ext + 2.0);
        if [&va, &vb, &vc].iter().any(|v| !(v.x >= lo && v.x <= hi && v.y >= -2.0 && v.y <= self.height as f32 + 2.0 && v.oow.is_finite())) {
            self.bad_tris += 1;
            if self.bad_tris <= 20 {
                crate::warn!(
                    "Glide: off-screen triangle in frame {} (caller {:#x}): ({:.1},{:.1}) ({:.1},{:.1}) ({:.1},{:.1})",
                    self.frames, self.caller, va.x, va.y, vb.x, vb.y, vc.x, vc.y
                );
            }
        }
        if self.view_ext > 0.0 && SKY_CALLERS.contains(&self.caller) {
            widen_sky(&mut [&mut va, &mut vb, &mut vc], self.view_ext);
        }
        // the frame-end fade (a semi-transparent black quad over the game's 640 x 480)
        // covers the whole screen, its last row and column included (`cover_screen`), and
        // the whole widened view
        if FADE_CALLERS.contains(&self.caller) {
            cover_screen(&mut [&mut va, &mut vb, &mut vc], self.width as f32, self.height as f32);
            if self.hud_ext > 0.0 {
                widen_full_rect(&mut [&mut va, &mut vb, &mut vc]);
            }
        }
        // Only the 3D vertex builder's triangles are 3D (depth buffer); everything else
        // (sky and ground quads, fades, 2D shapes; their 1/w may be stale builder values)
        // is marked 2D with a negative 1/z, which the game never uses.
        if self.builder_mode == u32::MAX {
            for v in [&mut va, &mut vb, &mut vc] {
                v.ooz = -2.0;
            }
        }
        if self.hud_ext > 0.0 && RECT_CALLERS.contains(&self.caller) {
            if self.race_frame {
                anchor_rect(&mut [&mut va, &mut vb, &mut vc]);
            } else {
                // menus (widened 3D scenes): full-width rectangles (fades) cover the whole
                // view; boxes stay in the middle with the menu's 2D pictures
                widen_full_rect(&mut [&mut va, &mut vb, &mut vc]);
            }
        }
        if self.st.cull != consts::GR_CULL_DISABLE {
            let area = (vb.x - va.x) * (vc.y - va.y) - (vc.x - va.x) * (vb.y - va.y);
            let sign_neg = area < 0.0;
            // Glide: GR_CULL_NEGATIVE culls triangles with negative signed area (y down)
            if (self.st.cull == consts::GR_CULL_NEGATIVE && sign_neg) || (self.st.cull == consts::GR_CULL_POSITIVE && !sign_neg) {
                return;
            }
        }
        // Depth buffer: back faces. The game culls them per object (crate::depth::CULL_ON)
        // but only for polygons entirely on screen; clipped ones are culled here as the
        // game intends. Objects it does not cull rely on its drawing order to hide their
        // back faces (a closed model's are behind its own front faces anyway): those are
        // depth tested but write no depth, so whatever the game draws over them later
        // still covers them. Same tolerance as the game's test (cross product < -20 in
        // its 640 x 400 space; y is 1.2x here).
        let mut back = false;
        if self.front_sign != 0.0 {
            let area = (vb.x - va.x) * (vc.y - va.y) - (vc.x - va.x) * (vb.y - va.y);
            if area * self.front_sign < -24.0 {
                if self.cull_flag {
                    self.culled_back += 1;
                    return;
                }
                back = true;
                for v in [&mut va, &mut vb, &mut vc] {
                    v.ooz = -1.0;
                }
            }
        }
        if back != self.back_batch {
            self.flush_tris();
            self.back_batch = back;
        }
        self.verts.extend_from_slice(&[va, vb, vc]);
        self.tris += 1;
    }

    /// `grDrawLine`: Voodoo lines are one pixel thick, drawn as a thin quad offset by one
    /// pixel along the minor axis.
    fn push_line(&mut self, a: u32, b: u32) {
        if self.lfb_pending {
            self.flush_lfb();
        }
        let (mut va, mut vb) = unsafe { (Vertex::read(a), Vertex::read(b)) };
        if self.stw_hint & GR_STWHINT_W_DIFF_TMU0 == 0 {
            va.tmu_oow = va.oow;
            vb.tmu_oow = vb.oow;
        }
        if self.race_frame && self.hud_ext > 0.0 && self.caller == LINE_CALLER {
            let shift = hud_anchor(va.x.min(vb.x), va.x.max(vb.x)) as f32 * crate::view::ext();
            va.x += shift;
            vb.x += shift;
        }
        if self.vertex_samples > 0 {
            crate::info!("    line ({:.1},{:.1}) ({:.1},{:.1}) caller {:#x}", va.x, va.y, vb.x, vb.y, self.caller);
        }
        let (dx, dy) = if (vb.x - va.x).abs() >= (vb.y - va.y).abs() { (0.0, 1.0) } else { (1.0, 0.0) };
        let mut va2 = va;
        let mut vb2 = vb;
        va2.x += dx;
        va2.y += dy;
        vb2.x += dx;
        vb2.y += dy;
        self.verts.extend_from_slice(&[va, vb, vb2, va, vb2, va2]);
        self.tris += 2;
    }
}

/// Return addresses of the frame-end fade's two triangles (54b4c: constant colour
/// 88000000h over 0..639.8 x 0..479).
const FADE_CALLERS: &[u32] = &[0x54cc7, 0x54cde];

/// Return addresses of the game's 2D fill-rectangle routine (c0cxx: two triangles): the
/// pause screen's darkening, menu boxes.
const RECT_CALLERS: &[u32] = &[0xc0cbf, 0xc0cd3];

/// A 2D rectangle over a widescreen race view: full-width rectangles cover the whole view,
/// others follow the side they are on, like the HUD's LFB elements. (Both triangles of a
/// rectangle span its full width, so each is treated the same way.)
fn anchor_rect(v: &mut [&mut Vertex; 3]) {
    let x0 = v.iter().map(|p| p.x).fold(f32::MAX, f32::min);
    let x1 = v.iter().map(|p| p.x).fold(f32::MIN, f32::max);
    let ext = crate::view::ext();
    if x0 <= 0.0 && x1 >= 639.5 {
        for p in v.iter_mut() {
            if p.x <= 0.0 {
                p.x -= ext;
            } else if p.x >= 639.5 {
                p.x += ext;
            }
        }
    } else {
        let shift = hud_anchor(x0, x1) as f32 * ext;
        for p in v.iter_mut() {
            p.x += shift;
        }
    }
}

/// The fade's quad (0..639.8 x 0..479, constant colour) put on the screen's edges
/// (`width` x `height`). The game ends it at its last row and column: at 640 x 480 the row
/// below is left as it was (on the 3dfx too), and scaled up that is one or more rows plus,
/// at 2.5 times and more, the last column. After the name entry the screen is cleared
/// white and faded to black with this quad: those rows stayed white. Only the edges move:
/// the colour is the same everywhere on the quad.
fn cover_screen(v: &mut [&mut Vertex; 3], width: f32, height: f32) {
    let full = |a: f32, b: f32, size: f32| a <= 0.0 && b >= size - 1.0;
    let (x0, x1) = (v.iter().map(|p| p.x).fold(f32::MAX, f32::min), v.iter().map(|p| p.x).fold(f32::MIN, f32::max));
    let (y0, y1) = (v.iter().map(|p| p.y).fold(f32::MAX, f32::min), v.iter().map(|p| p.y).fold(f32::MIN, f32::max));
    if !(full(x0, x1, width) && full(y0, y1, height)) {
        return;
    }
    for p in v.iter_mut() {
        if p.x >= width - 1.0 {
            p.x = width;
        }
        if p.y >= height - 1.0 {
            p.y = height;
        }
    }
}

/// A rectangle spanning the game's whole width spans the whole widened view.
fn widen_full_rect(v: &mut [&mut Vertex; 3]) {
    let x0 = v.iter().map(|p| p.x).fold(f32::MAX, f32::min);
    let x1 = v.iter().map(|p| p.x).fold(f32::MIN, f32::max);
    if x0 <= 0.0 && x1 >= 639.5 {
        let ext = crate::view::ext();
        for p in v.iter_mut() {
            if p.x <= 0.0 {
                p.x -= ext;
            } else if p.x >= 639.5 {
                p.x += ext;
            }
        }
    }
}

/// A sky quad spans the widened view (x from the shifted table); the game's texture
/// coordinates are for its 4:3 span (x 0..639.8), so extend them linearly to the new
/// edges instead of stretching the picture. On these screen-aligned quads s depends only
/// on x (and t only on y).
fn widen_sky(v: &mut [&mut Vertex; 3], ext: f32) {
    let left = |x: f32| x <= -ext + 0.01;
    let right = |x: f32| x >= 639.8 + ext - 0.01;
    let l = v.iter().find(|p| left(p.x)).map(|p| p.sow / p.tmu_oow);
    let r = v.iter().find(|p| right(p.x)).map(|p| p.sow / p.tmu_oow);
    let (Some(l), Some(r)) = (l, r) else {
        return;
    };
    let ds = (r - l) * ext / 639.8;
    for p in v.iter_mut() {
        if left(p.x) {
            p.sow -= ds * p.tmu_oow;
        } else if right(p.x) {
            p.sow += ds * p.tmu_oow;
        }
    }
}

fn f32_arg(f: &HookFrame, n: u32) -> f32 {
    f32::from_bits(f.stack(n))
}

/// Read the import name table of the 3dfx build and bind every thunk to `call`.
pub fn bind_imports(m: &mut Machine, thunks_link: u32, names_link: u32, count: u32, resolver_link: u32) -> Result<(), String> {
    let backend = m.pending_backend.take().unwrap_or_else(|| Box::new(NullBackend));
    let mut names = Vec::new();
    for i in 0..count {
        let p = unsafe { mem::rd32(m.img.link(names_link + i * 4)) };
        names.push(String::from_utf8_lossy(&unsafe { mem::cstr(p, 64) }).to_string());
    }
    for i in 0..count {
        let at = thunks_link + i * 5;
        let rel = resolver_link.wrapping_sub(at + 5).to_le_bytes();
        let expect = [0xe8, rel[0], rel[1], rel[2], rel[3]];
        m.install_hook_arg("glide import", at, &expect, call, i)?;
    }
    crate::info!("Glide: bound {} imports to the native implementation", names.len());
    m.glide = Some(Glide::new(names, backend));
    Ok(())
}

/// Entry point for every Glide call made by the game.
pub fn call(m: &mut Machine, f: &mut HookFrame, index: u32) -> HookResult {
    let caller = m.img.to_link(f.stack(0)).unwrap_or(0);
    // the vertex builder (cc5e8) drawing a flat-colour polygon: its 1/w is not set by the
    // game (crate::depth)
    // engine addresses read for every builder triangle (resolved once)
    static ENGINE: std::sync::OnceLock<[u32; 4]> = std::sync::OnceLock::new();
    let [mode_addr, cull_on_addr, cull_off_addr, xt_addr] = *ENGINE.get_or_init(|| {
        [crate::depth::BUILDER_MODE, crate::depth::CULL_ON, crate::depth::CULL_OFF_FLAGS, 0xaa6c4].map(|a| m.img.link(a))
    });
    let builder_mode = if caller == BUILDER_CALLER { unsafe { mem::rd32(mode_addr) } } else { u32::MAX };
    let flat_poly = builder_mode == crate::depth::MODE_FLAT;
    // depth buffer on: which way front faces wind on screen (the x table's direction; a
    // mirrored view reverses it), and whether the game culls this object's back faces
    // (crate::depth::CULL_ON)
    let (front_sign, cull_flag) = if builder_mode != u32::MAX && crate::depth::ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        let on = unsafe { mem::rd32(cull_on_addr) != 0 && mem::rd32(cull_off_addr) & 1 == 0 };
        let xt = unsafe { mem::rd32(xt_addr) };
        let dir = if m.img.contains(xt.wrapping_add(4 * 300)) && m.img.contains(xt.wrapping_add(4 * 302)) {
            unsafe { f32::from_bits(mem::rd32(xt + 4 * 301)) - f32::from_bits(mem::rd32(xt + 4 * 300)) }
        } else {
            1.0
        };
        (if dir < 0.0 { -1.0 } else { 1.0 }, on)
    } else {
        (0.0, false)
    };
    let ext = m.widescreen.applied as f32;
    let hud_ext = m.widescreen.main_ext as f32;
    let g = m.glide.as_mut().expect("glide not bound");
    let i = index as usize;
    g.calls[i] += 1;
    let func = g.funcs[i];
    let hot = g.hot[i];
    let is_swap = hot == Hot::BufferSwap;
    let bytes = g.arg_bytes[i];
    g.view_ext = ext;
    g.hud_ext = hud_ext;
    g.caller = caller;
    g.flat_poly = flat_poly;
    g.front_sign = front_sign;
    g.cull_flag = cull_flag;
    g.builder_mode = builder_mode;
    let tracing = g.trace_frame.is_some_and(|n| n > 0 && g.frames % n == 0 && g.frames > 0);
    g.vertex_samples = if tracing { u32::MAX } else { 0 };
    let budget = &mut g.trace_budget[i];
    if *budget > 0 || tracing {
        *budget = budget.saturating_sub(1);
        let name = &g.names[i];
        let args: Vec<String> = (1..=bytes as u32 / 4).map(|k| format!("{:#x}", f.stack(k))).collect();
        if tracing {
            // code addresses on the stack: the call chain (diagnostics)
            let chain: Vec<String> =
                (0..32).map(|k| f.stack(k)).filter(|&v| m.img.is_code(v)).filter_map(|v| m.img.to_link(v)).take(5).map(|l| format!("{l:x}")).collect();
            crate::info!("[frame {}] {name}({}) <- {}", g.frames, args.join(", "), chain.join(" "));
        } else {
            crate::debug!("glide {name}({})", args.join(", "));
        }
    }
    let t0 = crate::time::cycles();
    let was_race = g.race_frame;
    // a menu frame shown as interpolated frames (native renderer, `native::menus`)
    let mut menu_fill = false;
    if is_swap {
        let interpolate = crate::interp::active(m, was_race);
        menu_fill = !interpolate && crate::native::menus::extra_frames(m);
        m.glide.as_mut().unwrap().suppress_present = interpolate || menu_fill;
    }
    let g = m.glide.as_mut().expect("glide not bound");
    let ret = match dispatch_hot(g, f, hot) {
        Some(r) => r,
        None => dispatch(g, f, func),
    };
    // SR_DUMP_TEXTURES: what the game binds and downloads against its textures in RAM
    if crate::native::textures::checking() {
        if hot == Hot::TexSource {
            crate::native::textures::check_bind(g, caller, f.stack(2), f.stack(3), f.stack(4));
        } else if hot == Hot::ChromakeyValue {
            crate::native::textures::check_chroma(caller, f.stack(1));
        } else if func == "GRTEXDOWNLOADMIPMAP" {
            crate::native::textures::check_download(g, f.stack(2), f.stack(4));
        }
    }
    let dt = crate::time::cycles().wrapping_sub(t0);
    g.host_time += dt;
    g.func_time[i] += dt;
    if is_swap {
        g.swap_time += dt;
    }
    if let Some(v) = ret {
        f.eax = v;
    }
    if is_swap {
        crate::trace_state(m);
        if m.glide.as_ref().is_some_and(|g| g.suppress_present) {
            if menu_fill { crate::native::menus::fill(m) } else { crate::interp::fill(m) }
        }
        hold(m);
        if crate::time::cooperative_timer() {
            if let Some(scheduler) = m.sched.as_ref() {
                scheduler.cooperative_frame();
            }
        }
        crate::unlock::tick(m);
        let frame = m.glide.as_ref().map_or(0, |g| g.frames);
        crate::debug_peek(m, frame);
        crate::stop_point(m);
    }
    HookResult::Return(bytes)
}

/// Pause the game at a frame boundary while the backend asks for it (settings overlay):
/// no timer ticks, no sound, the same frame shown with the overlay.
fn hold(m: &mut Machine) {
    if !m.glide.as_mut().is_some_and(|g| g.backend.hold()) {
        return;
    }
    crate::info!("game paused (settings)");
    crate::events::PAUSED.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(a) = &m.audio {
        a.set_paused(true);
    }
    while m.glide.as_mut().is_some_and(|g| g.backend.hold()) {}
    if let Some(a) = &m.audio {
        a.set_paused(false);
    }
    crate::events::PAUSED.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::info!("game resumed");
}

/// Set a state field; the pending triangles are drawn first only if it changes (the game
/// sets the same state before almost every polygon).
macro_rules! set_state {
    ($g:ident, $field:ident, $val:expr) => {{
        let v = $val;
        if $g.st.$field != v {
            $g.state_change();
            $g.st.$field = v;
        }
    }};
}

/// The Glide calls the game makes for (almost) every polygon, recognised by import index
/// instead of by name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hot {
    DrawTriangle,
    TexSource,
    TexDownloadTable,
    AlphaBlendFunction,
    AlphaCombine,
    ColorCombine,
    ChromakeyMode,
    ChromakeyValue,
    TexClampMode,
    ConstantColorValue,
    BufferSwap,
    Other,
}

impl Hot {
    fn of(name: &str) -> Hot {
        match name {
            "GRDRAWTRIANGLE" => Hot::DrawTriangle,
            "GRTEXSOURCE" => Hot::TexSource,
            "GRTEXDOWNLOADTABLE" => Hot::TexDownloadTable,
            "GRALPHABLENDFUNCTION" => Hot::AlphaBlendFunction,
            "GRALPHACOMBINE" => Hot::AlphaCombine,
            "GRCOLORCOMBINE" => Hot::ColorCombine,
            "GRCHROMAKEYMODE" => Hot::ChromakeyMode,
            "GRCHROMAKEYVALUE" => Hot::ChromakeyValue,
            "GRTEXCLAMPMODE" => Hot::TexClampMode,
            "GRCONSTANTCOLORVALUE" => Hot::ConstantColorValue,
            "GRBUFFERSWAP" => Hot::BufferSwap,
            _ => Hot::Other,
        }
    }
}

/// The per-polygon calls (same behaviour as their arms in `dispatch`); None: not one of
/// them, or one with diagnostics on (handled by `dispatch`).
#[inline]
fn dispatch_hot(g: &mut Glide, f: &HookFrame, hot: Hot) -> Option<Option<u32>> {
    let a = |n: u32| f.stack(n);
    if g.vertex_samples > 0 {
        return None;
    }
    Some(match hot {
        Hot::DrawTriangle => {
            if SKY_CALLERS.contains(&g.caller) {
                g.race_frame = true;
                // diagnostics: SR_DEBUG_BG=1 paints the sky magenta too (holes show)
                static DEBUG_BG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                if *DEBUG_BG.get_or_init(|| std::env::var_os("SR_DEBUG_BG").is_some()) {
                    let saved = (g.st.color_combine, g.st.constant_color);
                    set_state!(g, color_combine, [3, 8, 1, 2, 0]);
                    set_state!(g, constant_color, 0xffff00ff);
                    g.push_tri(a(1), a(2), a(3));
                    set_state!(g, color_combine, saved.0);
                    set_state!(g, constant_color, saved.1);
                    return Some(None);
                }
            }
            g.push_tri(a(1), a(2), a(3));
            None
        }
        Hot::TexSource => {
            let info = unsafe { tex::TexInfo::read(a(4)) };
            set_state!(g, tex, state::TexBinding { valid: 1, start: a(2), even_odd: a(3), info: info.layout() });
            None
        }
        Hot::TexDownloadTable => {
            g.download_table(a(2), a(3), 0, 255);
            None
        }
        Hot::AlphaBlendFunction => {
            set_state!(g, blend, [a(1), a(2), a(3), a(4)]);
            None
        }
        Hot::AlphaCombine => {
            set_state!(g, alpha_combine, [a(1), a(2), a(3), a(4), a(5)]);
            None
        }
        Hot::ColorCombine => {
            set_state!(g, color_combine, [a(1), a(2), a(3), a(4), a(5)]);
            None
        }
        Hot::ChromakeyMode => {
            set_state!(g, chromakey, (a(1) != 0) as u8);
            None
        }
        Hot::ChromakeyValue => {
            set_state!(g, chromakey_value, a(1));
            None
        }
        Hot::TexClampMode => {
            set_state!(g, clamp, [a(2), a(3)]);
            None
        }
        Hot::ConstantColorValue | Hot::BufferSwap | Hot::Other => return None,
    })
}

fn dispatch(g: &mut Glide, f: &HookFrame, name: &str) -> Option<u32> {
    use consts::*;
    let a = |n: u32| f.stack(n);
    match name {
        // ---- initialisation and hardware queries ----
        "GRGLIDEINIT" => {
            g.st = GlideState::default();
            None
        }
        "GRGLIDESHUTDOWN" => {
            g.backend.close();
            None
        }
        "GRGLIDEGETVERSION" => {
            let s = b"Glide Version 2.43\0";
            unsafe { mem::slice_mut(a(1), s.len() as u32).copy_from_slice(s) };
            None
        }
        "GRSSTQUERYHARDWARE" | "GRSSTQUERYBOARDS" => {
            // GrHwConfiguration: one Voodoo Graphics with 2 MB frame buffer, one 4 MB TMU
            let p = a(1);
            unsafe {
                mem::wr32(p, 1); // num_sst
                mem::wr32(p + 4, GR_SSTTYPE_VOODOO);
                mem::wr32(p + 8, 2); // fbRam
                mem::wr32(p + 12, 2); // fbiRev
                mem::wr32(p + 16, 1); // nTexelfx
                mem::wr32(p + 20, 0); // sliDetect
                mem::wr32(p + 24, 1); // tmuConfig[0].tmuRev
                mem::wr32(p + 28, TMU_BYTES >> 20); // tmuConfig[0].tmuRam (MB)
            }
            Some(1)
        }
        "GRHINTS" => {
            if a(1) == GR_HINT_STWHINT {
                g.state_change();
                g.stw_hint = a(2);
            }
            None
        }
        "GRSSTSELECT" | "GRSSTORIGIN" | "GRGLIDESHAMELESSPLUG" | "GRERRORSETCALLBACK" | "GRSSTIDLE"
        | "GRRESETTRISTATS" | "GRSSTRESETPERFSTATS" | "GRSSTCONFIGPIPELINE" | "GRTEXMULTIBASE" | "GRDITHERMODE"
        | "GRTEXLODBIASVALUE" | "GRTEXDETAILCONTROL" | "GRTEXNCCTABLE" | "GRALPHACONTROLSITRGBLIGHTING" => {
            if name == "GRSSTORIGIN" {
                g.st.origin = a(1);
            }
            None
        }
        "GRSSTWINOPEN" => {
            let (w, h) = consts::resolution(a(2)).unwrap_or((640, 480));
            g.width = w;
            g.height = h;
            g.st.origin = a(5);
            crate::info!("Glide: grSstWinOpen {w}x{h}, refresh {}, colour format {}, origin {}, {} colour / {} aux buffers", a(3), a(4), a(5), a(6), a(7));
            g.reset_lfb();
            match g.backend.open(w, h) {
                Ok(()) => Some(1),
                Err(e) => {
                    crate::error!("cannot open the display: {e}");
                    Some(0)
                }
            }
        }
        "GRSSTWINCLOSE" => {
            g.backend.close();
            None
        }
        "GRSSTSCREENWIDTH" => Some(g.width),
        "GRSSTSCREENHEIGHT" => Some(g.height),
        "GRSSTISBUSY" => Some(0),
        "GRSSTVRETRACEON" => Some(1),
        "GRSSTVIDEOLINE" => Some(0),
        "GRSSTSTATUS" => Some(0x0fff_f03f),
        "GRSSTCONTROL" => Some(1),
        "GRBUFFERNUMPENDING" => Some(0),
        "GRTRISTATS" => {
            unsafe {
                mem::wr32(a(1), g.tris as u32);
                mem::wr32(a(2), g.tris as u32);
            }
            None
        }
        "GRSSTPERFSTATS" => None,

        // ---- frame ----
        "GRBUFFERCLEAR" => {
            if g.lfb_pending {
                g.flush_lfb();
            }
            g.flush_tris();
            let ctx = DrawContext {
                st: &g.st,
                tmu: &g.tmu,
                palette: &g.palette,
                palette_gen: g.palette_gen,
                ncc: &g.ncc,
                screen: (g.width, g.height),
                part: 0,
                kind: Default::default(),
            };
            g.backend.clear(&ctx, a(1), a(2) as u8, a(3) as u16);
            None
        }
        "GRBUFFERSWAP" => {
            g.flush_tris();
            if g.lfb_pending {
                g.flush_lfb();
            }
            if !g.suppress_present {
                g.backend.swap(a(1));
            }
            g.hud_recording = false;
            g.last_hud = std::mem::take(&mut g.hud_ops);
            g.last_mirror = g.mirror_start.take().map(|s| (s, g.mirror_end.take().unwrap_or(g.last_hud.len())));
            g.mirror_end = None;
            g.race_frame = false;
            g.frames += 1;
            FRAME.store(g.frames, std::sync::atomic::Ordering::Relaxed);
            if g.frames <= 3 || g.frames % 500 == 0 {
                crate::info!("Glide: frame {} ({} triangles so far)", g.frames, g.tris);
            }
            if g.frames % 100 == 0 {
                let now = std::time::Instant::now();
                if let Some(t) = g.fps_mark {
                    let total = (now - t).as_secs_f64();
                    crate::debug!(
                        "Glide: {:.1} fps over the last 100 frames; per frame {:.2} ms, of which Glide {:.2} ms (swap {:.2} ms)",
                        100.0 / total,
                        total * 10.0,
                        crate::time::cycles_to_duration(g.host_time).as_secs_f64() * 10.0,
                        crate::time::cycles_to_duration(g.swap_time).as_secs_f64() * 10.0
                    );
                }
                g.host_time = Default::default();
                g.swap_time = Default::default();
                g.fps_mark = Some(now);
            }
            None
        }
        "GRRENDERBUFFER" => {
            set_state!(g, render_buffer, a(1));
            None
        }

        // ---- drawing ----
        "GRDRAWTRIANGLE" | "GUDRAWTRIANGLEWITHCLIP" | "GRAADRAWTRIANGLE" | "GUAADRAWTRIANGLEWITHCLIP" | "GUMPDRAWTRIANGLE" => {
            if SKY_CALLERS.contains(&g.caller) {
                g.race_frame = true;
            }
            g.push_tri(a(1), a(2), a(3));
            None
        }
        "GRDRAWPOLYGON" | "GRDRAWPLANARPOLYGON" | "GRAADRAWPOLYGON" => {
            let (n, ilist, vlist) = (a(1), a(2), a(3));
            for k in 1..n.saturating_sub(1) {
                let idx = |j: u32| unsafe { mem::rd32(ilist + j * 4) };
                g.push_tri(vlist + idx(0) * Vertex::SIZE, vlist + idx(k) * Vertex::SIZE, vlist + idx(k + 1) * Vertex::SIZE);
            }
            None
        }
        "GRDRAWPOLYGONVERTEXLIST" | "GRDRAWPLANARPOLYGONVERTEXLIST" | "GRAADRAWPOLYGONVERTEXLIST" | "GUDRAWPOLYGONVERTEXLISTWITHCLIP" => {
            let (n, vlist) = (a(1), a(2));
            for k in 1..n.saturating_sub(1) {
                g.push_tri(vlist, vlist + k * Vertex::SIZE, vlist + (k + 1) * Vertex::SIZE);
            }
            None
        }
        "GRDRAWLINE" | "GRAADRAWLINE" => {
            g.push_line(a(1), a(2));
            None
        }
        "GRDRAWPOINT" | "GRAADRAWPOINT" => None,

        // ---- state ----
        "GRCOLORCOMBINE" => {
            set_state!(g, color_combine, [a(1), a(2), a(3), a(4), a(5)]);
            None
        }
        "GRALPHACOMBINE" => {
            set_state!(g, alpha_combine, [a(1), a(2), a(3), a(4), a(5)]);
            None
        }
        "GUCOLORCOMBINEFUNCTION" => {
            set_state!(g, color_combine, consts::gu_color_combine(a(1)));
            None
        }
        "GUALPHASOURCE" => {
            set_state!(g, alpha_combine, consts::gu_alpha_source(a(1)));
            None
        }
        "GRALPHABLENDFUNCTION" => {
            set_state!(g, blend, [a(1), a(2), a(3), a(4)]);
            None
        }
        "GRALPHATESTFUNCTION" => {
            set_state!(g, alpha_test, a(1));
            None
        }
        "GRALPHATESTREFERENCEVALUE" => {
            set_state!(g, alpha_ref, a(1) as u8);
            None
        }
        "GRCONSTANTCOLORVALUE" => {
            // diagnostics: SR_DEBUG_BG=1 paints the ground background quad (set at cba25)
            // magenta, so holes in the world show
            static DEBUG_BG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if g.caller == 0xcba25 && *DEBUG_BG.get_or_init(|| std::env::var_os("SR_DEBUG_BG").is_some()) {
                set_state!(g, constant_color, 0xffff00ff);
                return None;
            }
            set_state!(g, constant_color, a(1));
            None
        }
        "GRCONSTANTCOLORVALUE4" => {
            g.state_change();
            let c = |v: f32| v.clamp(0.0, 255.0) as u32;
            g.st.constant_color = c(f32_arg(f, 1)) << 24 | c(f32_arg(f, 2)) << 16 | c(f32_arg(f, 3)) << 8 | c(f32_arg(f, 4));
            None
        }
        "GRCOLORMASK" => {
            set_state!(g, color_mask, (a(1) != 0) as u8);
            g.st.alpha_mask = (a(2) != 0) as u8;
            None
        }
        "GRCULLMODE" => {
            g.st.cull = a(1);
            None
        }
        "GRDEPTHBUFFERMODE" => {
            set_state!(g, depth_mode, a(1));
            None
        }
        "GRDEPTHBUFFERFUNCTION" => {
            set_state!(g, depth_func, a(1));
            None
        }
        "GRDEPTHMASK" => {
            set_state!(g, depth_mask, (a(1) != 0) as u8);
            None
        }
        "GRDEPTHBIASLEVEL" => {
            set_state!(g, depth_bias, a(1) as i16);
            None
        }
        "GRCHROMAKEYMODE" => {
            set_state!(g, chromakey, (a(1) != 0) as u8);
            None
        }
        "GRCHROMAKEYVALUE" => {
            set_state!(g, chromakey_value, a(1));
            None
        }
        "GRCLIPWINDOW" => {
            set_state!(g, clip, [a(1), a(2), a(3), a(4)]);
            None
        }
        "GRFOGMODE" => {
            set_state!(g, fog_mode, a(1));
            None
        }
        "GRFOGCOLORVALUE" => {
            set_state!(g, fog_color, a(1));
            None
        }
        "GRFOGTABLE" => {
            g.state_change();
            unsafe { g.st.fog_table.copy_from_slice(mem::slice(a(1), 64)) };
            None
        }
        "GUFOGGENERATEEXP" | "GUFOGGENERATEEXP2" | "GUFOGGENERATELINEAR" => {
            consts::gu_fog_generate(name, a(1), f32_arg(f, 2), f32_arg(f, 3));
            None
        }
        "GUFOGTABLEINDEXTOW" => Some(consts::fog_index_to_w(a(1) as i32).to_bits()),
        "GRGAMMACORRECTIONVALUE" => {
            g.st.gamma_bits = f.stack(1);
            None
        }
        "GRDISABLEALLEFFECTS" => {
            g.state_change();
            g.st.disable_all_effects();
            None
        }
        "GRGLIDEGETSTATE" => {
            unsafe { g.st.save(a(1)) };
            None
        }
        "GRGLIDESETSTATE" => {
            g.state_change();
            unsafe { g.st.load(a(1)) };
            None
        }

        // ---- texturing ----
        "GRTEXMINADDRESS" => Some(0),
        "GRTEXMAXADDRESS" => Some(g.tmu.size() - 8),
        "GRTEXCALCMEMREQUIRED" => Some(tex::mem_required(a(1), a(2), a(3), a(4), 3)),
        "GRTEXTEXTUREMEMREQUIRED" => {
            let info = unsafe { tex::TexInfo::read(a(2)) };
            Some(tex::mem_required(info.small_lod, info.large_lod, info.aspect, info.format, a(1)))
        }
        "GRTEXSOURCE" => {
            let info = unsafe { tex::TexInfo::read(a(4)) };
            if g.vertex_samples > 0 {
                crate::info!("    texture {:#x}: {:?} {:?}", a(2), info, tex::level_size(info.large_lod, info.aspect));
            }
            set_state!(g, tex, state::TexBinding { valid: 1, start: a(2), even_odd: a(3), info: info.layout() });
            None
        }
        "GRTEXDOWNLOADMIPMAP" => {
            let info = unsafe { tex::TexInfo::read(a(4)) };
            let (s, e) = g.tmu.download_mipmap(a(2), a(3), &info);
            g.backend.invalidate_textures(s, e);
            None
        }
        "GRTEXDOWNLOADMIPMAPLEVEL" => {
            let (s, e) = g.tmu.download_level(a(2), a(3), a(4), a(5), a(6), a(7), a(8), 0, u32::MAX);
            g.backend.invalidate_textures(s, e);
            None
        }
        "GRTEXDOWNLOADMIPMAPLEVELPARTIAL" => {
            let (s, e) = g.tmu.download_level(a(2), a(3), a(4), a(5), a(6), a(7), a(8), a(9), a(10));
            g.backend.invalidate_textures(s, e);
            None
        }
        "GRTEXDOWNLOADTABLE" | "GRTEXDOWNLOADTABLEPARTIAL" => {
            let (start, end) = if name == "GRTEXDOWNLOADTABLE" { (0, 255) } else { (a(4), a(5)) };
            g.download_table(a(2), a(3), start, end);
            None
        }
        "GRTEXFILTERMODE" => {
            set_state!(g, filter, [a(2), a(3)]);
            None
        }
        "GRTEXCLAMPMODE" => {
            set_state!(g, clamp, [a(2), a(3)]);
            None
        }
        "GRTEXMIPMAPMODE" => {
            set_state!(g, mipmap, a(2));
            None
        }
        "GRTEXCOMBINE" => {
            set_state!(g, tex_combine, [a(2), a(3), a(4), a(5), a(6), a(7)]);
            None
        }
        "GRTEXCOMBINEFUNCTION" | "GUTEXCOMBINEFUNCTION" => {
            set_state!(g, tex_combine, consts::tex_combine_function(a(2)));
            None
        }

        // ---- linear frame buffer ----
        "GRLFBLOCK" => {
            let (ty, buffer, write_mode, origin, info) = (a(1), a(2), a(3), a(4), a(6));
            if ty & 1 == 1 {
                g.lfb_locked_write = Some(write_mode);
            }
            unsafe {
                mem::wr32(info + 4, g.lfb_addr);
                mem::wr32(info + 8, LFB_STRIDE_PIXELS * 2);
                mem::wr32(info + 12, if write_mode == GR_LFBWRITEMODE_ANY { GR_LFBWRITEMODE_565 } else { write_mode });
                mem::wr32(info + 16, if origin == GR_ORIGIN_ANY { 0 } else { origin });
            }
            let _ = buffer;
            Some(1)
        }
        "GRLFBUNLOCK" => {
            if a(1) & 1 == 1 {
                g.lfb_pending = true;
            }
            g.lfb_locked_write = None;
            Some(1)
        }
        "GRLFBWRITEREGION" => {
            let (x, y, fmt, w, h, stride, src) = (a(2), a(3), a(4), a(5), a(6), a(7), a(8));
            let buf = g.lfb_slice();
            for row in 0..h {
                for col in 0..w {
                    let (dx, dy) = (x + col, y + row);
                    if dx >= LFB_STRIDE_PIXELS || dy >= LFB_HEIGHT {
                        continue;
                    }
                    let px = unsafe {
                        match fmt {
                            GR_LFB_SRC_FMT_565 | GR_LFB_SRC_FMT_555 | GR_LFB_SRC_FMT_1555 => mem::rd16(src + row * stride + col * 2),
                            _ => {
                                let v = mem::rd32(src + row * stride + col * 4);
                                (((v >> 19) & 0x1f) << 11 | ((v >> 10) & 0x3f) << 5 | ((v >> 3) & 0x1f)) as u16
                            }
                        }
                    };
                    buf[(dy * LFB_STRIDE_PIXELS + dx) as usize] = if px == LFB_EMPTY { LFB_EMPTY ^ 1 } else { px };
                }
            }
            g.lfb_pending = true;
            Some(1)
        }
        "GRLFBREADREGION" => {
            let (w, h, stride, dst) = (a(4), a(5), a(6), a(7));
            for row in 0..h {
                for col in 0..w {
                    unsafe { mem::wr16(dst + row * stride + col * 2, 0) };
                }
            }
            Some(1)
        }
        "GRLFBWRITECOLORFORMAT" | "GRLFBWRITECOLORSWIZZLE" | "GRLFBCONSTANTALPHA" | "GRLFBCONSTANTDEPTH" => None,
        "GRSPLASH" => None,

        _ => {
            crate::warn!("glide {name} not implemented");
            Some(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri(pts: [(f32, f32); 3]) -> [Vertex; 3] {
        pts.map(|(x, y)| Vertex { x, y, ..Default::default() })
    }

    fn cover(t: &mut [Vertex; 3]) {
        let [a, b, c] = t;
        cover_screen(&mut [a, b, c], 640.0, 480.0);
    }

    #[test]
    fn the_fade_covers_the_last_row_and_column() {
        // the game's two triangles (54cc7, 54cde)
        for pts in [[(0.0, 0.0), (639.8, 0.0), (639.8, 479.0)], [(0.0, 0.0), (639.8, 479.0), (0.0, 479.0)]] {
            let mut t = tri(pts);
            cover(&mut t);
            for (v, (x, y)) in t.iter().zip(pts) {
                assert_eq!((v.x, v.y), (if x > 0.0 { 640.0 } else { 0.0 }, if y > 0.0 { 480.0 } else { 0.0 }));
            }
        }
        // a quad smaller than the screen stays as it is
        let pts = [(0.0, 0.0), (639.8, 0.0), (639.8, 400.0)];
        let mut t = tri(pts);
        cover(&mut t);
        assert!(t.iter().zip(pts).all(|(v, (x, y))| (v.x, v.y) == (x, y)));
    }
}
