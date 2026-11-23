//! Widescreen (Hor+) for STRT3FX: the game's 3D view is widened instead of stretched.
//!
//! The 3D pipeline works in a 640x400 integer screen space (y is scaled by 1.2 to 480 when
//! the Glide vertices are built). Its width appears in three forms (docs/re/strt3fx.md,
//! "Widescreen"):
//!
//! * `set_view(half_width, height, center_y)` (53300, called every frame) fills the view
//!   block at 81e0c: right edge, centres, the half-resolution centres used by the
//!   bounding-sphere cull at 82240, ...;
//! * immediates in the asm modules: the projection centre `+0x13f` (near-plane clippers,
//!   transform), the trivial accept/reject bounding-box tests `cmp r, 0x27e0000`, the 2D
//!   polygon clipper's right edge `0x27f` (df25a), the sky quad's right edge (cb971);
//! * the int -> float x table at ae6cc (float(i), with entry 639 = 639.8) through which
//!   every screen x reaches Glide.
//!
//! With an extension of `e` pixels on each side the game is given a view `640 + 2e` wide
//! (centre `319 + e`) and the x table maps `i -> i - e`, so Glide receives x from `-e` to
//! `639.8 + e` with the original 4:3 picture unchanged in the middle. The renderer shows
//! that range in a window of the matching aspect. Everything stays in the original code;
//! only constants change, between frames.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

/// `add r, 0x13f` (32-bit immediates): projection centre in the near-plane clippers.
const CENTER32: &[u32] = &[
    0x79d92, 0x79e71, 0x7b4d8, 0x7b851, 0x7b89c, 0xca2b2, 0xca37d, 0xcddd2, 0xcde9d, 0xce632, 0xce6fd, 0xcfc11, 0xcfc9c, 0xd2202,
    0xd22cd, 0xd2a62, 0xd2b2d, 0xd326d, 0xd32f8, 0xd5bac, 0xd5c57,
];
/// `add ax, 0x13f` (16-bit immediates).
const CENTER16: &[u32] = &[0xcef40, 0xcf079, 0xcf2d4, 0xcf410, 0xcfdfe, 0xcfe94, 0xd3438, 0xd34d0, 0xd4f92, 0xd509f, 0xd52b2, 0xd53c2];
/// `add edx, 0x13f0000`: centre in the packed (x << 16 | y) output of the transform.
const CENTER_HI: &[u32] = &[0x7ab1a, 0x7ac1e];
/// `0x27e0000`: packed right limit (x 638) of the polygon bounding-box tests and clamps.
const XMAX_HI: &[u32] = &[
    0xca5db, 0xca62a, 0xca821, 0xca840, 0xce0fb, 0xce14a, 0xce33f, 0xce35e, 0xce95b, 0xce9aa, 0xceb9f, 0xcebbe, 0xcfffe, 0xd004d,
    0xd0085, 0xd027d, 0xd029e, 0xd03d2, 0xd03f2, 0xd252b, 0xd257a, 0xd276f, 0xd278e, 0xd2d8b, 0xd2dda, 0xd2fcf, 0xd2fee, 0xd363a,
    0xd3689, 0xd36c1, 0xd386d, 0xd388e, 0xd3950, 0xd3972, 0xd55f7, 0xd5642, 0xd58fb, 0xd591a, 0xd5e9b, 0xd5eea, 0xd60df, 0xd60fe,
];
/// `sub eax, 0x27e`: the same limit unpacked.
const XMAX: &[u32] = &[0xd003d, 0xd0075, 0xd3679, 0xd36b1];
/// `mov ebx, 0x27f`: right edge of the 2D polygon clipper (ded50).
const CLIP_RIGHT: u32 = 0xdf25b;
/// `fld dword ptr [eax + 0x9fc]`: the sky / background quads' right x, entry 639 of the
/// x table.
const SKY_RIGHT: &[u32] = &[0xcb973, 0xcbac0];
/// x table: float per integer screen x (2048 entries).
const X_TABLE: u32 = 0xae6cc;
const X_TABLE_LEN: i32 = 2048;
/// `set_view(half_width = eax, height = edx, center_y = ebx)`.
const SET_VIEW: u32 = 0x53300;

/// Largest extension the x table allows (the view must stay below 2048 pixels).
pub const MAX_EXT: i32 = 700;

#[derive(Default)]
pub struct Widescreen {
    /// Constants verified at install; false = leave the game at 4:3.
    pub available: bool,
    /// Extension currently patched into the code.
    pub applied: i32,
    callers: std::collections::HashSet<u32>,
    last_logged: i32,
    /// Extension of the main (driver's) view this frame: the HUD follows it.
    pub main_ext: i32,
    /// The enlarged object lists (guest memory the drawing writes).
    pub object_lists: Vec<(u32, u32)>,
    /// set_view is being called by the runtime for an extra frame of the race view.
    pub force_race: bool,
    /// The runtime draws the rear-view mirror again (an extra frame).
    pub force_mirror: bool,
    /// Far distance scale in percent (100 = original).
    pub draw_distance: u32,
    /// The frustum rotation uses Q12 and corners / 4 (see `install_draw_distance`).
    frustum_q12: bool,
    /// Far corner z (negative) of the culling frustum, full units.
    far_z: i32,
}

struct Site {
    link: u32,
    size: u32,
    value: fn(i32) -> u32,
}

fn sites() -> Vec<Site> {
    let mut v = Vec::new();
    v.extend(CENTER32.iter().map(|&link| Site { link, size: 4, value: |e| (0x13f + e) as u32 }));
    v.extend(CENTER16.iter().map(|&link| Site { link, size: 2, value: |e| (0x13f + e) as u32 }));
    v.extend(CENTER_HI.iter().map(|&link| Site { link, size: 4, value: |e| ((0x13f + e) as u32) << 16 }));
    v.extend(XMAX_HI.iter().map(|&link| Site { link, size: 4, value: |e| ((0x27e + 2 * e) as u32) << 16 }));
    v.extend(XMAX.iter().map(|&link| Site { link, size: 4, value: |e| (0x27e + 2 * e) as u32 }));
    v.push(Site { link: CLIP_RIGHT, size: 4, value: |e| (0x27f + 2 * e) as u32 });
    v.extend(SKY_RIGHT.iter().map(|&link| Site { link, size: 4, value: |e| ((0x27f + 2 * e) * 4) as u32 }));
    v
}

/// Code bytes this module rewrites (must stay where they are).
pub fn protected(m: &Machine) -> Vec<(u32, u32)> {
    sites().iter().map(|s| (m.img.link(s.link), s.size)).collect()
}

fn read(m: &Machine, s: &Site) -> u32 {
    let a = m.img.link(s.link);
    unsafe { if s.size == 2 { mem::rd16(a) as u32 } else { mem::rd32(a) } }
}

fn write(m: &Machine, s: &Site, v: u32) {
    let a = m.img.link(s.link);
    unsafe {
        if s.size == 2 {
            mem::wr16(a, v as u16)
        } else {
            mem::wr32(a, v)
        }
    }
}

/// Check every constant and hook `set_view`. Missing pieces leave the game at 4:3.
pub fn install(m: &mut Machine) -> Result<(), String> {
    for s in sites() {
        let found = read(m, &s);
        if found != (s.value)(0) {
            crate::warn!("widescreen: unexpected constant {found:#x} at {:#x}; staying at 4:3", s.link);
            return Ok(());
        }
    }
    m.install_observer("widescreen set_view", SET_VIEW, set_view)?;
    m.widescreen.available = true;
    Ok(())
}

/// Extension requested by the renderer (whole pixels, covering the window).
fn requested() -> i32 {
    (crate::view::ext().ceil() as i32).clamp(0, MAX_EXT)
}

/// Make sure the main view's constants are applied (before drawing it from the runtime).
pub fn ensure_main(m: &mut Machine) {
    sync_applied(m);
    let e = m.widescreen.main_ext;
    if m.widescreen.available && m.widescreen.applied != e {
        apply(m, e);
    }
}

/// `applied` as guest memory has it: the extension in the projection centre's constant. The
/// interpolated frames put guest memory back after drawing (`crate::interp`), the code's
/// constants included, so after a step with the rear-view mirror memory holds the main
/// view's extension while the mirror's pass had recorded 0. A switch to 4:3 (extension 0)
/// then found nothing to change and the wide constants stayed: the mirror (its own x table,
/// half scale) drawn half the old extension to the right (2026-11-23).
fn sync_applied(m: &mut Machine) {
    if m.widescreen.available {
        let centre = read(m, &Site { link: CENTER_HI[0], size: 4, value: |_| 0 });
        m.widescreen.applied = (centre >> 16) as i32 - 0x13f;
    }
}

fn apply(m: &mut Machine, e: i32) {
    for s in sites() {
        write(m, &s, (s.value)(e));
    }
    // x table: i -> i - e; the right edge entry covers the last pixel column (.8, as the game)
    let t = m.img.link(X_TABLE);
    for i in 0..X_TABLE_LEN {
        unsafe { mem::wr32(t + i as u32 * 4, ((i - e) as f32).to_bits()) };
    }
    let right = 639 + 2 * e;
    unsafe { mem::wr32(t + right as u32 * 4, ((639.8f64 + e as f64) as f32).to_bits()) };
    m.widescreen.applied = e;
}

/// The menus' 3D scenes widen too (settings: display.wide_menus).
pub static MENUS_WIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Return address of the mirror view's camera set-up call (34c7e: call 236dc).
const MIRROR_RETURN: u32 = 0x34c83;
/// Return address of set_view in the race camera set-up (236dc).
const RACE_VIEW_RETURN: u32 = 0x238df;

/// Observer on `set_view`: widen the main view by the current extension.
fn set_view(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    if crate::log::enabled(crate::log::Level::Debug) {
        let caller = m.img.to_link(f.stack(0)).unwrap_or(0);
        if m.widescreen.callers.insert(caller) {
            let chain: Vec<String> = (0..48).map(|k| f.stack(k)).filter(|&v| m.img.is_code(v)).filter_map(|v| m.img.to_link(v)).map(|l| format!("{l:x}")).collect();
            crate::debug!("set_view({}, {}, {}) called from {caller:#x}; code addresses on the stack: {}", f.eax as i32, f.edx as i32, f.ebx as i32, chain.join(" "));
        }
    }
    // The rear-view mirror (toggled with M) is drawn after the main view through the same
    // set-up (34af4 -> 236dc -> set_view, returning to 34c83) at half resolution into a
    // fixed 4:3 frame: it keeps the original constants.
    let mirror = m.widescreen.force_mirror || (0..24).any(|k| m.img.to_link(f.stack(k)) == Some(MIRROR_RETURN));
    if mirror && !m.widescreen.force_mirror {
        // the game's mirror: its view for extra frames, and where it starts in the recording
        m.interp.mirror_view = (f.eax, f.edx, f.ebx, f.ecx);
        if let Some(g) = m.glide.as_mut() {
            g.mirror_begin();
        }
    }
    // Race views widen (camera set-up 236dc, also used by the attract mode and replays),
    // and so do the menus' 3D scenes (checkered floors, cars, the arcade cabinet) unless
    // MENUS_WIDE is off; their 2D titles and panels stay in the middle 4:3 area.
    let race_view = m.widescreen.force_race || m.img.to_link(f.stack(0)) == Some(RACE_VIEW_RETURN);
    if race_view && !mirror && !m.widescreen.force_race {
        m.interp.center_y = f.ebx;
    }
    let wide = race_view || MENUS_WIDE.load(std::sync::atomic::Ordering::Relaxed);
    let e = if mirror || !wide { 0 } else { requested() };
    // what memory holds, not what was last written (see `sync_applied`)
    sync_applied(m);
    if !mirror {
        m.widescreen.main_ext = e;
    }
    if e != m.widescreen.applied {
        if !mirror && e != m.widescreen.last_logged {
            crate::info!("widescreen: 3D view {} x 400 game pixels (extension {e} on each side)", 640 + 2 * e);
            m.widescreen.last_logged = e;
        }
        apply(m, e);
    }
    // the game only ever sets up the 640-wide view; leave anything else alone
    if f.eax == 320 {
        f.eax = (320 + m.widescreen.applied) as u32;
    }
    write_frustum(m);
    crate::lod::apply(m);
    if let Some(g) = m.glide.as_mut() {
        g.begin_view();
    }
    HookResult::Resume(tramp)
}

/// `set_far_distance(eax = -distance)` (82fa4): every far-plane dependent constant of the
/// 3D module (cull distances, clipper limits, the frustum's far z) is derived from it.
/// Called with the track's distance at race start (37cdf) and -44320 for menu scenes.
const SET_FAR: u32 = 0x82fa4;

/// Scale (percent) applied to the game's far distance; 100 = original. Kept below the
/// point where the module's squared-distance arithmetic would overflow 32 bits.
pub const MAX_DRAW_DISTANCE: u32 = 300;

/// The culling frustum (7a158 / 7a33c): far corners (x, z) at 7a08c/7a094 and 7a098/7a0a0
/// (static x = -/+57433, z written by set_far as -(distance + 4096)) are rotated by the
/// camera heading in Q14 fixed point (`imul` by sin/cos, `sar 14`) and their bounding box
/// culls objects. The corners must grow with the draw distance (or the frustum narrows)
/// and with the widescreen extension, which would overflow the products; so the shifts
/// become `sar 12` and the corners are stored divided by 4 (same results, 4x headroom).
const FRUSTUM_SHIFTS: &[u32] = &[0x7a18e, 0x7a191, 0x7a1b4, 0x7a1b7, 0x7a375, 0x7a378, 0x7a39b, 0x7a39e];
const CORNER_LEFT_X: u32 = 0x7a08c;
const CORNER_LEFT_Z: u32 = 0x7a094;
const CORNER_RIGHT_X: u32 = 0x7a098;
const CORNER_RIGHT_Z: u32 = 0x7a0a0;
const ORIGINAL_CORNER_X: i64 = 57433;
const ORIGINAL_RACE_Z: i64 = 54096;

static DRAW_DISTANCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

/// Change the draw distance (percent); takes effect when the game next sets its far plane
/// (the start of a race).
pub fn set_draw_distance(percent: u32) {
    DRAW_DISTANCE.store(percent.clamp(100, MAX_DRAW_DISTANCE), std::sync::atomic::Ordering::Relaxed);
}

pub fn install_draw_distance(m: &mut Machine, percent: u32) -> Result<(), String> {
    for &a in FRUSTUM_SHIFTS {
        let at = m.img.link(a);
        if unsafe { mem::rd8(at) } != 0x0e {
            crate::warn!("draw distance: unexpected frustum code at {a:#x}; left as is");
            return Ok(());
        }
    }
    for &a in FRUSTUM_SHIFTS {
        unsafe { mem::wr8(m.img.link(a), 0x0c) };
    }
    m.widescreen.frustum_q12 = true;
    m.widescreen.far_z = unsafe { mem::rd32(m.img.link(CORNER_LEFT_Z)) } as i32;
    set_draw_distance(percent);
    // the distance may be raised later from the settings overlay
    enlarge_object_lists(m)?;
    m.install_observer("draw distance", SET_FAR, set_far)?;
    write_frustum(m);
    Ok(())
}

/// Frustum corners for the current far distance and widescreen extension (stored / 4).
fn write_frustum(m: &mut Machine) {
    if !m.widescreen.frustum_q12 {
        return;
    }
    let z = m.widescreen.far_z as i64;
    let x = (z.abs() * ORIGINAL_CORNER_X / ORIGINAL_RACE_Z).max(ORIGINAL_CORNER_X) * (320 + m.widescreen.applied as i64) / 320;
    unsafe {
        mem::wr32(m.img.link(CORNER_LEFT_X), (-x / 4) as i32 as u32);
        mem::wr32(m.img.link(CORNER_RIGHT_X), (x / 4) as i32 as u32);
        mem::wr32(m.img.link(CORNER_LEFT_Z), (z / 4) as i32 as u32);
        mem::wr32(m.img.link(CORNER_RIGHT_Z), (z / 4) as i32 as u32);
    }
}

fn set_far(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let d = f.eax as i32 as i64;
    let pct = DRAW_DISTANCE.load(std::sync::atomic::Ordering::Relaxed);
    m.widescreen.draw_distance = pct;
    let scaled = (d * pct as i64 / 100).clamp(-150_000, 150_000) as i32;
    if pct != 100 {
        crate::info!("draw distance: far plane {} -> {} ({pct}%)", -d, -(scaled as i64));
    }
    f.eax = scaled as u32;
    // set_far stores z = distance - 4096 in full units; set_view rewrites the corners
    m.widescreen.far_z = scaled - 0x1000;
    // the race set-up's call marks a newly loaded race (native renderer: a new scene)
    crate::native::frame::set_far_called(m, m.img.to_link(f.stack(0)));
    HookResult::Resume(tramp)
}

/// The 3D module's per-frame object lists (built by 81fbc / 82130): fixed 0x3200-byte
/// arrays at 57d90, 5af90 and 5e190 without bounds checks, sized for the original far
/// plane. A longer draw distance lets more objects through, so the lists are moved to
/// larger buffers (all 13 references are the arrays' base addresses).
const OBJECT_LISTS: &[(u32, &[u32])] = &[
    (0x57d90, &[0x78d7d, 0x7a53d, 0x7a57d, 0x7a594, 0x7a631, 0x7a668, 0x82144]),
    (0x5af90, &[0x8248e, 0x8214a]),
    (0x5e190, &[0x78deb, 0x78e07, 0x82150, 0x82329]),
];
const OBJECT_LIST_SIZE: u32 = 0x3200 * 8;

pub fn enlarge_object_lists(m: &mut Machine) -> Result<(), String> {
    for &(base, refs) in OBJECT_LISTS {
        let old = m.img.link(base);
        for &r in refs {
            if unsafe { mem::rd32(m.img.link(r)) } != old {
                return Err(format!("object list reference at {r:#x} does not hold {base:#x}"));
            }
        }
    }
    for &(_, refs) in OBJECT_LISTS {
        let new = mem::alloc_guest(OBJECT_LIST_SIZE).ok_or("cannot allocate object lists")?;
        m.widescreen.object_lists.push((new, OBJECT_LIST_SIZE));
        for &r in refs {
            unsafe { mem::wr32(m.img.link(r), new) };
        }
    }
    Ok(())
}
