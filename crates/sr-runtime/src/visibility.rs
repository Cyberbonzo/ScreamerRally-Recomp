//! What the 3D engine draws of the world: the game's hand-made visibility lists, extended
//! (STRT3FX).
//!
//! The world is a grid of cells 2000h units wide (docs/re/strt3fx.md, "Visibility"). Every
//! cell the track passes through has a list of the cells visible from it (cell +10h: list
//! of `row << 16 | col`, +24h: count; rows through the table at [123720]). Each frame
//! `7a2e4` takes the camera's cell list and the bounding box of the view frustum in cells,
//! and `7a52c` draws the objects of the listed cells inside that box (a cell without a list
//! makes it draw every cell in the box instead, `7a640`). The lists reach about seven cells:
//! roughly the original draw distance. Farther objects never appear (pop-in where the
//! lists end, holes in the distance with a longer draw distance), and neither do some
//! beside a widened view.
//!
//! Modes (settings: graphics.visibility):
//! * original: the camera cell's list;
//! * extended (default): the camera cell's list plus the lists of the track cells in it
//!   (inside the frustum's box): what the game shows from the stretches of track visible
//!   from here. A track cell hidden from here (behind a hill) adds nothing, so buildings
//!   only it would show stay hidden (a first version merged every track cell in the box and
//!   showed buildings on the far side of hills); the depth buffer decides what is in front;
//! * everything: the engine's own no-list path, every cell inside the box.
//!
//! The grid and its lists do not change during a race: they are copied once per track
//! (keyed by the row table) and merged per frame from the copy.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;
use std::sync::atomic::{AtomicU8, Ordering};

/// After `call 7a2e4` in 7a52c (`mov [7a8a0], eax`; eax = rows of the frustum box - 1).
const AFTER_SETUP: u32 = 0x7a531;
/// Row table pointer.
const ROWS: u32 = 0x123720;
/// The list to walk: end and start ([7a150] = 0: no list, all cells of the box).
const LIST_END: u32 = 0x7a150;
const LIST_START: u32 = 0x7a154;
/// The end is also patched into `cmp edx, imm32` at 7a5e9.
const LIST_END_IMM: u32 = 0x7a5eb;
/// The box: first row (<< 16), first column, column count (`cmp ebx, imm32` at 7a5c9).
const BOX_ROW: u32 = 0x7a8ac;
const BOX_COL: u32 = 0x7a8b0;
const BOX_COLS_IMM: u32 = 0x7a5cb;
/// `lea edi, [57d90h]` at 7a53b: the immediate is the collected objects' list (moved to a
/// larger buffer by `crate::widescreen`).
const LIST_BASE_IMM: u32 = 0x7a53d;
/// The number of objects collected (7a52c's result, read by the draw loop at 78dd2).
const OBJECT_COUNT: u32 = 0x79868;
const GRID: usize = 256;

pub const ORIGINAL: u8 = 0;
pub const EXTENDED: u8 = 1;
pub const EVERYTHING: u8 = 2;

pub static MODE: AtomicU8 = AtomicU8::new(EXTENDED);

pub fn set_mode(mode: u8) {
    MODE.store(mode.min(EVERYTHING), Ordering::Relaxed);
}

#[derive(Default)]
pub struct Visibility {
    /// Guest buffer for the merged list (GRID * GRID entries).
    buffer: u32,
    /// Cells marked for the merged list (row-major bitset).
    marks: Vec<u64>,
    /// Copy of the grid's lists: key (row table pointer and a hash of it), and per cell
    /// (row-major) its list of cells, empty for cells without one.
    key: (u32, u64),
    lists: Vec<Vec<u32>>,
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    m.install_observer("visibility lists", AFTER_SETUP, after_setup)?;
    Ok(())
}

fn after_setup(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let mode = MODE.load(Ordering::Relaxed);
    let link = |a: u32| m.img.link(a);
    let has_list = unsafe { mem::rd32(link(LIST_END)) } != 0;
    // the native renderer: the camera cell's own list (the designers', before any merge) and
    // every cell's list (`docs/re/native/visibility.md`)
    let native = crate::native::frame::drawing(m);
    let own = if native { unsafe { read_list(m) } } else { None };
    if native {
        let rows = unsafe { mem::rd32(link(ROWS)) };
        if rows != 0 && rows != u32::MAX && (m.visibility.key.0 == rows || mem::readable(rows, 4 * GRID as u32)) {
            unsafe { refresh(m, rows) };
        }
    }
    let link = |a: u32| m.img.link(a);
    if mode == EVERYTHING && has_list {
        unsafe { mem::wr32(link(LIST_END), 0) };
    } else if mode == EXTENDED && has_list {
        unsafe { merge(m, f.eax as i32) };
    }
    // the native renderer: the view's world is drawn now from the GPU, and the engine skips
    // the cells' objects (7a52c returns an empty list: the scene table's instances follow)
    if native {
        let mut cells = unsafe { game_cells(m, f.eax as i32) };
        cells.own = own;
        if crate::native::frame::draw_world_view(m, &cells) {
            let link = |a: u32| m.img.link(a);
            unsafe {
                // the list's base as 7a53b loads it (moved by crate::widescreen), no objects
                f.edi = mem::rd32(link(LIST_BASE_IMM));
                mem::wr32(link(OBJECT_COUNT), 0);
            }
            return HookResult::Return(0);
        }
    }
    HookResult::Resume(tramp)
}

/// The cells the engine would draw now (7a52c): the list it walks (as left by the mode
/// above) inside the frustum's box of cells, or every cell of the box when there is no list
/// (7a640). The box: `box_rows` + 1 rows from [7a8ac] >> 16, [7a5cb] + 1 columns from
/// [7a8b0] (the loops at 7a556 / 7a5c6).
unsafe fn game_cells(m: &Machine, box_rows: i32) -> crate::native::visibility::GameCells {
    use crate::native::visibility::{CellBox, GameCells};
    let link = |a: u32| m.img.link(a);
    let frustum_box = unsafe {
        CellBox { row: mem::rd32(link(BOX_ROW)) >> 16, col: mem::rd32(link(BOX_COL)), rows: box_rows.max(0) as u32 + 1, cols: mem::rd32(link(BOX_COLS_IMM)) + 1 }
    };
    GameCells { list: unsafe { read_list(m) }, own: None, frustum_box }
}

/// The list the engine will walk now ([7a154] .. [7a150]); `None`: no list.
unsafe fn read_list(m: &Machine) -> Option<Vec<u32>> {
    let link = |a: u32| m.img.link(a);
    let (start, end) = unsafe { (mem::rd32(link(LIST_START)), mem::rd32(link(LIST_END))) };
    if end == 0 {
        return None;
    }
    let n = if end > start && end - start < 4 * (GRID * GRID) as u32 { (end - start) / 4 } else { 0 };
    mem::readable(start, 4 * n.max(1)).then(|| (0..n).map(|i| unsafe { mem::rd32(start + 4 * i) }).collect())
}

/// Every cell's visibility list (row-major, `row * 256 + col`; empty: none), as copied for
/// the current track; empty before the first copy.
pub fn lists(m: &Machine) -> &[Vec<u32>] {
    &m.visibility.lists
}

/// Copy the grid's lists of the world loaded now (once per track, as the cell set-up's
/// observer does), for a reader that needs them before the first view is drawn (the
/// visibility editor's map at a race's start). False: no grid or no list.
pub fn refresh_lists(m: &mut Machine) -> bool {
    let rows = unsafe { mem::rd32(m.img.link(ROWS)) };
    rows != 0 && rows != u32::MAX && (m.visibility.key.0 == rows || mem::readable(rows, 4 * GRID as u32)) && unsafe { refresh(m, rows) }
}

/// The grid's lists of the world loaded now, read without keeping them (for the scene build,
/// before [`refresh_lists`]); `None`: no grid.
pub fn read_lists(m: &Machine) -> Option<Vec<Vec<u32>>> {
    let rows = unsafe { mem::rd32(m.img.link(ROWS)) };
    (rows != 0 && rows != u32::MAX && mem::readable(rows, 4 * GRID as u32)).then(|| unsafe { read_grid(rows) }.0)
}

/// Every cell's list of the grid at `rows`, and how many cells have one.
unsafe fn read_grid(rows: u32) -> (Vec<Vec<u32>>, usize) {
    let mut lists = vec![Vec::new(); GRID * GRID];
    let mut cells = 0;
    for r in 0..GRID {
        let rp = unsafe { mem::rd32(rows + 4 * r as u32) };
        if rp == u32::MAX || rp == 0 || !mem::readable(rp, 4 * GRID as u32) {
            continue;
        }
        for c in 0..GRID {
            let cell = unsafe { mem::rd32(rp + 4 * c as u32) };
            if cell == u32::MAX || cell == 0 || !mem::readable(cell, 0x28) {
                continue;
            }
            let (list, n) = unsafe { (mem::rd32(cell + 0x10), mem::rd32(cell + 0x24) as i32) };
            if n > 0 && n < 4096 && mem::readable(list, 4 * n as u32) {
                lists[r * GRID + c] = (0..n as u32).map(|i| unsafe { mem::rd32(list + 4 * i) }).collect();
                cells += 1;
            }
        }
    }
    (lists, cells)
}

/// Copy the grid's lists (once per track).
unsafe fn refresh(m: &mut Machine, rows: u32) -> bool {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for r in 0..GRID as u32 {
        h = (h ^ unsafe { mem::rd32(rows + 4 * r) } as u64).wrapping_mul(0x0100_0000_01b3);
    }
    if m.visibility.key == (rows, h) && !m.visibility.lists.is_empty() {
        return true;
    }
    let (lists, cells) = unsafe { read_grid(rows) };
    crate::info!("visibility: {cells} cells with lists");
    m.visibility.lists = lists;
    m.visibility.key = (rows, h);
    cells > 0
}

unsafe fn merge(m: &mut Machine, box_rows: i32) {
    let addr: std::collections::HashMap<u32, u32> =
        [ROWS, LIST_START, LIST_END, LIST_END_IMM, BOX_ROW, BOX_COL, BOX_COLS_IMM].iter().map(|&a| (a, m.img.link(a))).collect();
    let link = |a: u32| addr[&a];
    let rows = unsafe { mem::rd32(link(ROWS)) };
    if rows == 0 || !(m.visibility.key.0 == rows || mem::readable(rows, 4 * GRID as u32)) {
        return;
    }
    if !unsafe { refresh(m, rows) } {
        return;
    }
    if m.visibility.buffer == 0 {
        // outside the guest's own allocations: their addresses stay as without this buffer
        match mem::alloc(0, 4 * (GRID * GRID) as u32, false) {
            Some(b) => m.visibility.buffer = b,
            None => return,
        }
        m.visibility.marks = vec![0; GRID * GRID / 64];
    }
    let row0 = (unsafe { mem::rd32(link(BOX_ROW)) } >> 16) as usize;
    let col0 = unsafe { mem::rd32(link(BOX_COL)) } as usize;
    let cols = unsafe { mem::rd32(link(BOX_COLS_IMM)) } as usize;
    let mut marks = std::mem::take(&mut m.visibility.marks);
    marks.iter_mut().for_each(|w| *w = 0);
    let mut mark = |e: u32| {
        let (r, c) = ((e >> 16) as usize, (e & 0xffff) as usize);
        if r < GRID && c < GRID {
            let k = r * GRID + c;
            marks[k / 64] |= 1 << (k % 64);
        }
    };
    // the camera cell's own list, and the lists of the track cells in it that lie inside
    // the frustum's box: what the game shows from the stretches of track visible from here
    // (a track cell hidden from here, e.g. on the far side of a hill, adds nothing, so
    // what only it would show stays hidden as the designers intended)
    let start = unsafe { mem::rd32(link(LIST_START)) };
    let end = unsafe { mem::rd32(link(LIST_END)) };
    let own: Vec<u32> = if end > start && end - start < 4 * 4096 { (0..(end - start) / 4).map(|i| unsafe { mem::rd32(start + 4 * i) }).collect() } else { Vec::new() };
    let (r_end, c_end) = ((row0 + box_rows.max(0) as usize + 1).min(GRID), (col0 + cols + 1).min(GRID));
    for &e in &own {
        mark(e);
        let (r, c) = ((e >> 16) as usize, (e & 0xffff) as usize);
        if r < row0 || r >= r_end || c < col0 || c >= c_end {
            continue;
        }
        for &f in &m.visibility.lists[r * GRID + c] {
            mark(f);
        }
    }
    // the merged list, sorted by row and column as the engine walks it
    let buf = m.visibility.buffer;
    let mut out = buf;
    for (w, &bits) in marks.iter().enumerate() {
        let mut b = bits;
        while b != 0 {
            let k = w * 64 + b.trailing_zeros() as usize;
            b &= b - 1;
            unsafe { mem::wr32(out, ((k / GRID) as u32) << 16 | (k % GRID) as u32) };
            out += 4;
        }
    }
    m.visibility.marks = marks;
    if out > buf {
        unsafe {
            mem::wr32(link(LIST_START), buf);
            mem::wr32(link(LIST_END), out);
            mem::wr32(link(LIST_END_IMM), out);
        }
    }
}
