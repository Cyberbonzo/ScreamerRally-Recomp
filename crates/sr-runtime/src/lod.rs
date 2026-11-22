//! Highest detail for every object (STRT3FX).
//!
//! Every model part has up to three detail levels: its part record points to a table of
//! three entries, 34h bytes apart (offsets 0, 34h, 68h), each the level's polygon data or 0
//! (none). When setting up a part for drawing, the 3D engine computes its distance from
//! the camera and picks the level (docs/re/strt3fx.md, "Level of detail"):
//!
//! * `828c4` (parts of instanced objects: cars): level 0 nearer than [81e60], level 1
//!   nearer than [81e64], else level 2; part names starting with `i` / `x` count as 4000 /
//!   1000 units nearer;
//! * `82a04` (other objects: track sections, scenery): the same with [81e14] / [81e18] and
//!   half the distance (names starting with `Z`: 2000h farther);
//! * the rear-view mirror never uses level 0.
//!
//! A part without data at the chosen level is not drawn. Many parts exist only at some
//! levels: a car far away is one stand-in part (levels 1-2 only) instead of its twelve
//! detailed parts (level 0 only); distant hills and some road sections have no level 0.
//! With this option each selector's distance tests are replaced (in place, 29 bytes) by:
//! the part's highest level that has data. Detailed parts are drawn at any distance and
//! parts that only exist at lower levels still appear (a first version that always took
//! level 0 lost those: holes in the far road, buildings floating without their hill).
//! Stand-ins and detailed versions of the same object can then both be drawn; they
//! occupy the same place and the depth buffer resolves them.
//!
//! Separately, the game's own "Livello dettaglio" (Impostazioni: ALTO / BASSO) is made
//! irrelevant, always (whatever this module's setting): every race is loaded and run at the
//! high level (`install_game_detail`, `docs/re/strt3fx.md`, "Progression and settings").

use crate::Machine;
use crate::mem;
use std::sync::atomic::{AtomicBool, Ordering};

/// Start of each selector's level choice (`test dword ptr [aa5b0], 1`) and where the
/// chosen entry is tested (`test edi, edi`).
const SITES: [(u32, u32); 2] = [(0x829b8, 0x829e5), (0x82ada, 0x82b07)];
/// The original bytes of the 29 replaced bytes (the [aa5b0] address is relocated: its 4
/// bytes at +2 are compared against the linked address instead).
const ORIGINAL: [u8; 29] = [
    0xf7, 0x05, 0, 0, 0, 0, 0x01, 0x00, 0x00, 0x00, 0x74, 0x05, 0xbf, 0x34, 0x00, 0x00, 0x00, 0x3b, 0x41, 0x08, 0x7f, 0x0f, 0xbf,
    0x34, 0x00, 0x00, 0x00, 0x3b, 0x41,
];
const LEN: usize = 29;

/// Highest detail on (settings: graphics.detail).
pub static FORCE: AtomicBool = AtomicBool::new(true);

pub fn set_forced(on: bool) {
    FORCE.store(on, Ordering::Relaxed);
}

#[derive(Default)]
pub struct Lod {
    /// The selector code was found as expected.
    pub available: bool,
    /// The replacement is in place.
    pub applied: bool,
    /// The original bytes of each site.
    saved: Vec<[u8; LEN]>,
}

/// Code bytes this module rewrites.
pub fn protected(m: &Machine) -> Vec<(u32, u32)> {
    SITES.iter().map(|&(s, _)| (m.img.link(s), LEN as u32)).collect()
}

/// The replacement for a selector at `site` continuing at `resume` (link addresses).
fn replacement(site: u32, resume: u32) -> [u8; LEN] {
    let rel = resume.wrapping_sub(site + LEN as u32).to_le_bytes();
    [
        0x8b, 0xba, 0xb4, 0x00, 0x00, 0x00, // mov edi, [edx + 0B4h]   level table
        0x8b, 0x0f, // mov ecx, [edi]              level 0
        0x85, 0xc9, // test ecx, ecx
        0x75, 0x0a, // jnz found
        0x8b, 0x4f, 0x34, // mov ecx, [edi + 34h]   level 1
        0x85, 0xc9, // test ecx, ecx
        0x75, 0x03, // jnz found
        0x8b, 0x4f, 0x68, // mov ecx, [edi + 68h]   level 2
        0x8b, 0xf9, // found: mov edi, ecx
        0xe9, rel[0], rel[1], rel[2], rel[3], // jmp (test edi, edi; add the base)
    ]
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    install_game_detail(m);
    let flags = m.img.link(0xaa5b0).to_le_bytes();
    let mut saved = Vec::new();
    for (s, _) in SITES {
        let at = m.img.link(s);
        let mut found = [0u8; LEN];
        found.copy_from_slice(unsafe { mem::slice(at, LEN as u32) });
        let mut expect = ORIGINAL;
        expect[2..6].copy_from_slice(&flags);
        if found != expect {
            crate::warn!("detail levels: unexpected code at {s:#x}; the game's levels stay");
            return Ok(());
        }
        saved.push(found);
    }
    m.lod.saved = saved;
    m.lod.available = true;
    apply(m);
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The game's own "Livello dettaglio" (Impostazioni: ALTO / BASSO)

/// The menu's value (`CHOICE.DAT`, saved): ALTO = 2 (the default), BASSO = 0.
const MENU_DETAIL: u32 = 0x13b078;
/// The race's detail level: copied from the menu's at each race set-up (`3c021` from the
/// menus, `4314c` for the attract-mode race; 1 at start-up, `34595`) and read by the track
/// loader (`37cb0`, `37f7c`, `38070`), `39d34` and every frame (`2dd02`, `35e95` through
/// `[136630]`). See `docs/re/strt3fx.md`, "Progression and settings".
const RACE_DETAIL: u32 = 0x1435c8;
/// The track loader (`37c00(eax, edx = load)`, called at every race start from `37602`,
/// after the race set-up's copy and before any of the reads above; with edx = 0, no
/// loading, it returns at once).
const TRACK_LOADER: u32 = 0x37c00;
/// The high level.
const HIGH: u32 = 2;

/// The game's detail setting made irrelevant: every race runs at the high level (the `H`
/// track files, the full draw distance, the smoke models and puffs, the track's objects at
/// 13910c handled per frame), whatever the menu says. The menu and the saved value are
/// left alone: only the race's copy (`[1435c8]`) is set, at the track loader's entry.
fn install_game_detail(m: &mut Machine) {
    if let Err(e) = m.install_observer("game detail level (forced high), race condition", TRACK_LOADER, |m, f, tramp| {
        // the race's condition (day, night, fog) as the setting asks (`crate::conditions`)
        crate::conditions::at_track_loader(m, f);
        let at = m.img.link(RACE_DETAIL);
        let (race, menu) = unsafe { (mem::rd32(at), mem::rd32(m.img.link(MENU_DETAIL))) };
        if race != HIGH {
            crate::info!("game detail level: menu {menu}, race {race}: forced to {HIGH} (high)");
        }
        unsafe { mem::wr32(at, HIGH) };
        crate::hooks::HookResult::Resume(tramp)
    }) {
        crate::warn!("game detail level: cannot force it ({e}); the menu's value applies");
    }
}

/// Bring the code in line with the setting (between frames: from the set_view observer).
pub fn apply(m: &mut Machine) {
    if !m.lod.available {
        return;
    }
    let want = FORCE.load(Ordering::Relaxed);
    if want == m.lod.applied {
        return;
    }
    for (i, &(s, resume)) in SITES.iter().enumerate() {
        // the jump is relative: the code is position-independent within the image
        let bytes = if want { replacement(m.img.link(s), m.img.link(resume)) } else { m.lod.saved[i] };
        unsafe { mem::slice_mut(m.img.link(s), LEN as u32).copy_from_slice(&bytes) };
    }
    m.lod.applied = want;
    crate::info!("detail levels: {}", if want { "highest available for every object" } else { "the game's distance levels" });
}
