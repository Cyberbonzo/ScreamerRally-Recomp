//! The visibility editor's game window (`--vis-editor`, `docs/05-visibility-editor.md`): the
//! game's rules for the tool's races, and the race's track name.
//!
//! In the tool every race started from the menus is the game's own **Time Attack with free
//! laps** (`GIRI LIBERI`, the menu's mode 4), whatever mode the player picked: one car, no
//! opponents, the laps count on and the race never finishes. The game has no checkpoint
//! countdown and no time limit in any mode (its `TIME OVER` text is never referenced), so
//! nothing ends the race but the pause menu's Esci. Addresses and evidence:
//! `docs/re/strt3fx.md`, "Race set-up, modes, laps and the race's end".
//!
//! Installed only in tool mode: in normal play nothing here is hooked or patched (the frame
//! log below is a diagnostic behind `SR_TOOLMODE_LOG`).
//!
//! * **Race set-up** (observer at `412b0`, right after the menus' set-up `3bfd8` returns, before
//!   the race is loaded by `345d0`): car count [1435bc] = 1, race flags [1436dc] = 8 (time
//!   attack, free laps: with that flag the lap-complete routine `2dad4` never flags a car as
//!   finished, `2dbf0`), laps [143714] = 1, menu mode [145608] = 4 (the pause menu's Esci
//!   returns to the time attack menus, whose post-race path is the one made for a single
//!   car), and after another mode's menus the record's split times [1436e8..1436f8] = "no
//!   time" (the time attack's HUD compares the lap's splits with them). The car records, the
//!   player's record 0 and the menu's other choices (track, car, transmission, condition)
//!   stay as the chosen mode set them.
//! * **Leader's sector times** (`2d939`: `mov [12ea64 + 4 * sectors], time`): the race leader's
//!   time at every fifth of a lap, indexed by the car's sector count since the start (+4B0h).
//!   The table has 50 entries (10 laps); [12eb2c] follows (cleared every frame), then the
//!   track's road kinds (12eb50 = kind -1, 12eb74 = kind 0, 24h bytes each). The game's own
//!   free laps write past it from the 11th lap on, into the road kinds from sector 59 (kind
//!   -1, lap 12) and 68 (kind 0's grip, lap 14). In the tool the store is skipped beyond the
//!   table (the lone car is always the leader; the entries are only read for another car's
//!   gap to the leader).
//! * **Saved game** (`4152c: call 3b934`, writes `CONFIG\CHOICE.DAT` after every race, its only
//!   caller): skipped, so a tool session leaves the profiles, the championship and the
//!   records as they were (the menu reloads the file after every race, `41074`).

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

const CARS: u32 = 0x1296c8;
const CAR_SIZE: u32 = 0xcc8;
/// Index of player 1's car record.
const PLAYER1: u32 = 0x1436d4;
/// Cars in the race.
const CAR_COUNT: u32 = 0x1435bc;
/// The race's track (0-6, `map1`..`map7`).
const TRACK: u32 = 0x1435c0;
/// The race's condition (0 usual, 1 the second sky, 2 fog; championship races only).
const CONDITION: u32 = 0x1435c4;
/// Race kind: 1 arcade, 2 championship, 4 time attack (3 laps, with the ghost car), 8 time
/// attack (free laps), 10h / 20h network / serial link, 40h split screen, 80h replay / demo.
const RACE_FLAGS: u32 = 0x1436dc;
/// Laps of the race.
const LAPS: u32 = 0x143714;
/// The menu's mode: 0 championship, 1 arcade, 2 / 4 time attack (3 laps / free laps), 3
/// split screen.
const MENU_MODE: u32 = 0x145608;
/// Race state: 1 the race loop ends (with 2: back to the menus, else the race restarts),
/// 8 the player retired, 20h racing (after the start), 80h set at the race's load.
const RACE_STATE: u32 = 0x13146c;
/// The record's split times shown against the lap's in time attack (5 floats).
const BEST_SPLITS: u32 = 0x1436e8;
/// The game's "no time" (3599.99 s as a float; `2d9ba` shows no difference against it).
const NO_TIME: u32 = 0x4560_fffc;
/// The scene object table (28-byte entries, name at +0, index at +18h, -1 ends it).
const SCENE: u32 = 0x12f860;

/// Return address of the menus' race set-up (`412ab: call 3bfd8`).
const MENU_SETUP_DONE: u32 = 0x412b0;
/// `mov [ecx * 4 + 12ea64], eax` (7 bytes) in the sector routine `2d8ec`.
const LEADER_TIME_STORE: u32 = 0x2d939;
const LEADER_TIMES: u32 = 0x12ea64;
/// Entries of the leader's sector times (10 laps x 5).
const LEADER_TIMES_LEN: u32 = 50;
/// `call 3b934` (saves `CONFIG\CHOICE.DAT`) in the menus after every race.
const SAVE_PROGRESS_CALL: u32 = 0x4152c;

/// The game's tracks by [1435c0], as the track selection names them.
const TRACK_NAMES: [&str; 7] = ["Rally Wales", "Rally Sweden", "Rally China", "Rally Canada", "Rally Italy", "Rally Arizona", "Stadium"];

fn rd(m: &Machine, link: u32) -> u32 {
    unsafe { mem::rd32(m.img.link(link)) }
}

fn wr(m: &Machine, link: u32, v: u32) {
    unsafe { mem::wr32(m.img.link(link), v) }
}

/// The race's track as the game names it, with its condition when it is not the usual one
/// (`None`: no track set up, e.g. before the first race).
pub fn track_name(m: &Machine) -> Option<String> {
    let name = TRACK_NAMES.get(rd(m, TRACK) as usize)?;
    Some(match rd(m, CONDITION) {
        1 => format!("{name} (second sky)"),
        2 => format!("{name} (fog)"),
        _ => name.to_string(),
    })
}

/// Installs the tool's game rules (only with `--vis-editor`).
pub fn install(m: &mut Machine) -> Result<(), String> {
    if !crate::visedit::tool_mode() {
        return Ok(());
    }
    m.install_observer("race set-up (tool mode)", MENU_SETUP_DONE, race_setup)?;
    let table = m.img.link(LEADER_TIMES).to_le_bytes();
    m.install_hook(
        "leader's sector times (tool mode)",
        LEADER_TIME_STORE,
        &[0x89, 0x04, 0x8d, table[0], table[1], table[2], table[3]],
        leader_time,
    )?;
    // `call 3b934` (relative, e8 03 a4 ff ff) -> five nops; the routine itself carries the
    // unlock option's observer (`crate::unlock`)
    m.patch("saved game (tool mode)", SAVE_PROGRESS_CALL, &[0xe8, 0x03, 0xa4, 0xff, 0xff], &[0x90; 5])?;
    crate::info!("tool mode: one car, free laps, no race end");
    Ok(())
}

/// After the menus' race set-up: the race becomes a free-laps time attack with the player's
/// car alone.
fn race_setup(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    crate::info!(
        "tool mode: race set-up (menu mode {}, {} cars, flags {:#x}, {} laps, track {}): 1 car, free laps",
        rd(m, MENU_MODE) as i32,
        rd(m, CAR_COUNT),
        rd(m, RACE_FLAGS),
        rd(m, LAPS),
        track_name(m).unwrap_or_else(|| "?".into())
    );
    // the time attack's split times are compared with the record's (the time attack menus
    // load them, `4594e`); after another mode's menus they are stale or zero: "no record"
    if !matches!(rd(m, MENU_MODE), 2 | 4) {
        for k in 0..5 {
            wr(m, BEST_SPLITS + 4 * k, NO_TIME);
        }
    }
    wr(m, CAR_COUNT, 1);
    wr(m, RACE_FLAGS, 8);
    wr(m, LAPS, 1);
    wr(m, MENU_MODE, 4);
    HookResult::Resume(tramp)
}

/// `mov [ecx * 4 + 12ea64], eax` within the table only.
fn leader_time(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    if f.ecx < LEADER_TIMES_LEN {
        unsafe { mem::wr32(m.img.link(LEADER_TIMES) + 4 * f.ecx, f.eax) };
    }
    HookResult::Resume(m.img.link(LEADER_TIME_STORE + 7))
}

/// Once per race frame (frame start).
pub fn race_frame(m: &mut Machine) {
    if crate::visedit::tool_mode() {
        log_race_start(m);
    }
    log_frame(m);
}

/// At a race's first frame (state 80h, set at its load): the scene's cars, in the log.
fn log_race_start(m: &mut Machine) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let start = rd(m, RACE_STATE) == 0x80;
    if start == STARTED.swap(start, std::sync::atomic::Ordering::Relaxed) || !start {
        return;
    }
    let mut counts: Vec<(String, u32)> = Vec::new();
    let base = m.img.link(SCENE);
    for i in 0..512 {
        let e = base + i * 28;
        if unsafe { mem::rd32(e + 0x18) } == u32::MAX {
            break;
        }
        let raw = unsafe { mem::slice(e, 8) };
        let name: String = raw.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, c)) => *c += 1,
            None => counts.push((name, 1)),
        }
    }
    let cars: u32 = counts.iter().filter(|(n, _)| n.starts_with("auto")).map(|(_, c)| c).sum();
    let list: Vec<String> = counts.iter().map(|(n, c)| format!("{n} x{c}")).collect();
    crate::info!(
        "tool mode: race on {}: {} car(s) [1435bc], flags {:#x}; scene table: {cars} car instance(s) ({})",
        track_name(m).unwrap_or_else(|| "?".into()),
        rd(m, CAR_COUNT),
        rd(m, RACE_FLAGS),
        list.join(", ")
    );
}

/// `SR_TOOLMODE_LOG=FILE` (diagnostics): the race set-up and player 1's progress per frame.
fn log_frame(m: &Machine) {
    use std::io::Write;
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(p) = std::env::var_os("SR_TOOLMODE_LOG") {
            *FILE.lock().unwrap() = std::fs::File::create(p).ok();
        }
    });
    let mut g = FILE.lock().unwrap();
    let Some(f) = g.as_mut() else { return };
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let idx = rd(m, PLAYER1);
    let car = CARS + idx.min(7) * CAR_SIZE;
    let i = |off: u32| rd(m, car + off) as i32;
    let fl = |off: u32| f32::from_bits(rd(m, car + off));
    let splits: Vec<String> = (0..10).map(|k| format!("{:.2}", fl(0xc48 + 4 * k))).collect();
    // fields: frame; set-up; player 1's car: flags (+4Ch), position in the race (+C30h),
    // laps (+4A8h, best +4ACh), sectors (+4B0h, best +4B4h), progress (+4B8h..+4C0h), race
    // time, lap time, best lap (+C3Ch..+C44h), split times (+C48h..), position (+52Ch),
    // bindings (+B4h, 0 = driven by the AI), speed (+500h); the word after the leader's
    // sector times and the first road kind's grip (must not change)
    let _ = writeln!(
        f,
        "{frame} cars {} track {} cond {} flags {:#x} laps {} state {:#x} p1 {idx} | flags4c {:#x} c30 {} lap {} {} sec {} {} prog {:.1} {:.1} {:.1} t {:.2} {:.2} {:.2} split {} | pos {:.0} {:.0} {:.0} b4 {:#x} speed {:.1} after {:#x} road {:#x} ms {}",
        rd(m, CAR_COUNT),
        rd(m, TRACK),
        rd(m, CONDITION),
        rd(m, RACE_FLAGS),
        rd(m, LAPS),
        rd(m, RACE_STATE),
        i(0x4c),
        i(0xc30),
        i(0x4a8),
        i(0x4ac),
        i(0x4b0),
        i(0x4b4),
        fl(0x4b8),
        fl(0x4bc),
        fl(0x4c0),
        fl(0xc3c),
        fl(0xc40),
        fl(0xc44),
        splits.join(","),
        fl(0x52c),
        fl(0x530),
        fl(0x534),
        rd(m, car + 0xb4),
        fl(0x500),
        rd(m, LEADER_TIMES + 4 * LEADER_TIMES_LEN + 4),
        rd(m, 0x12eb74),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_names_follow_the_track_menu() {
        // the track menu's table at 1231fc maps menu rows to tracks: 2 3 4 5 1 0 6
        let rows = [2usize, 3, 4, 5, 1, 0, 6];
        let menu: Vec<&str> = rows.iter().map(|&t| TRACK_NAMES[t]).collect();
        assert_eq!(&menu[..4], &["Rally China", "Rally Canada", "Rally Italy", "Rally Arizona"]);
    }
}
