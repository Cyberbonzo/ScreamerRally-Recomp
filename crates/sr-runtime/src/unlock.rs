//! "Unlock all content": every track, championship and the bonus car selectable, as the
//! game's own cheat entries do, without changing the player's saved progress.
//!
//! Progress is the 0x8500-byte block at 13b03c, saved obfuscated to `CONFIG\CHOICE.DAT`
//! (load 3b56c, save 3b934). Relevant fields (docs/re/strt3fx.md, "Progression"):
//!
//! * [13b060] extra tracks (0-4; the track menu allows indices 0 .. value + 2),
//! * [13b064] highest championship that can be chosen (0-3),
//! * [13b104 + p * 188h] bonus car of player profile p (the car list holds 4 + value),
//!   p = 0..3 ([13b03c] = current profile).
//!
//! Winning a championship (4a3e9) sets the current profile's bonus car and raises the two
//! counters to the championship's row of the table at 123064 (1Ch bytes: +4 tracks,
//! +0Ch championship); with the bonus car on all four profiles, tracks become 4. The
//! game's cheat table (12301c, 24-byte entries `{name[12], field*, value}`) sets
//! [13b060] = 4, [13b064] = 3 and profile 0's bonus car: this module does the same for all
//! profiles, every frame while the option is on.
//!
//! The save routine reads the block through one address (3b976); it is pointed at a shadow
//! copy holding the player's real values (as loaded, plus what the game's own rule awards
//! for championships won meanwhile), so switching the option off restores the original
//! progression.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem;

const PROGRESS: u32 = 0x13b03c;
const PROGRESS_SIZE: u32 = 0x8500;
const CHAMPIONSHIP: u32 = 0x13b05c;
const BONUS_CAR: u32 = 0x13b104;
const PROFILE_STRIDE: u32 = 0x188;
const UNLOCK_TABLE: u32 = 0x123064;
const UNLOCK_ROW: u32 = 0x1c;
const SAVE: u32 = 0x3b934;
/// `mov eax, [eax + 13b03c]` in the save loop: its displacement.
const SAVE_SOURCE: u32 = 0x3b978;
/// `mov dword ptr [eax + 13b104], 1` after a championship win (eax = profile * 188h).
const CHAMPIONSHIP_WON: u32 = 0x4a3e9;
/// Return from the progress load (`call 3b56c` at 41074) in the main menu loop, which runs
/// again after every race: the menu copies the unlock fields into its per-player table
/// (3f85c) in the same frame, so they are forced here, before that.
const AFTER_LOAD: u32 = 0x41079;

/// Fields forced while the option is on: (link address, value).
fn forced() -> [(u32, u32); 6] {
    [
        (0x13b060, 4),
        (0x13b064, 3),
        (BONUS_CAR, 1),
        (BONUS_CAR + PROFILE_STRIDE, 1),
        (BONUS_CAR + 2 * PROFILE_STRIDE, 1),
        (BONUS_CAR + 3 * PROFILE_STRIDE, 1),
    ]
}

#[derive(Default)]
pub struct Unlock {
    /// The save redirection is installed.
    pub enabled: bool,
    /// Values are currently forced.
    active: bool,
    /// The player's own value of each forced field (None = not seen yet).
    real: [Option<u32>; 6],
    shadow: u32,
}

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switch the option (also while running: the settings overlay).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn install(m: &mut Machine, enabled: bool) -> Result<(), String> {
    set_enabled(enabled);
    let src = m.img.link(SAVE_SOURCE);
    if unsafe { mem::rd32(src) } != m.img.link(PROGRESS) {
        crate::warn!("unlock: save routine not as expected; option unavailable");
        set_enabled(false);
        return Ok(());
    }
    m.unlock.enabled = true;
    let shadow = mem::alloc_guest(PROGRESS_SIZE).ok_or("cannot allocate the progress shadow")?;
    unsafe { mem::wr32(src, shadow) };
    m.unlock.shadow = shadow;
    m.install_observer("progress save", SAVE, save)?;
    m.install_observer("championship won", CHAMPIONSHIP_WON, championship_won)?;
    m.install_observer("progress loaded", AFTER_LOAD, |m, _f, tramp| {
        tick(m);
        HookResult::Resume(tramp)
    })?;
    if enabled {
        crate::info!("unlock all content: on (saved progress keeps the real values)");
    }
    Ok(())
}

fn rd(m: &Machine, link: u32) -> u32 {
    unsafe { mem::rd32(m.img.link(link)) }
}

fn wr(m: &Machine, link: u32, v: u32) {
    unsafe { mem::wr32(m.img.link(link), v) }
}

/// Called once per frame: keep everything unlocked. A field that differs from the forced
/// value was (re)loaded by the game: that is the player's own value.
pub fn tick(m: &mut Machine) {
    if !m.unlock.enabled {
        return;
    }
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        // switched off: give the player's own values back
        if m.unlock.active {
            for (i, (addr, _)) in forced().into_iter().enumerate() {
                if let Some(v) = m.unlock.real[i] {
                    wr(m, addr, v);
                }
            }
            m.unlock.active = false;
        }
        return;
    }
    m.unlock.active = true;
    for (i, (addr, value)) in forced().into_iter().enumerate() {
        let v = rd(m, addr);
        if v != value {
            crate::debug!("unlock: [{addr:#x}] = {v} (the game's value) -> {value}");
            m.unlock.real[i] = Some(v);
            wr(m, addr, value);
        }
    }
}

/// The game's own reward for a championship, applied to the real values (4a3e9..4a465).
fn championship_won(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    let profile = (f.eax / PROFILE_STRIDE).min(3) as usize;
    let champ = rd(m, CHAMPIONSHIP);
    let row = UNLOCK_TABLE + champ.min(3) * UNLOCK_ROW;
    let real = &mut m.unlock.real;
    real[2 + profile] = Some(1);
    let tracks = unsafe { mem::rd32(m.img.link(row + 4)) };
    let champs = unsafe { mem::rd32(m.img.link(row + 0xc)) };
    real[0] = Some(real[0].unwrap_or(0).max(tracks));
    real[1] = Some(real[1].unwrap_or(0).max(champs));
    if real[2..].iter().all(|r| *r == Some(1)) {
        real[0] = Some(4);
    }
    HookResult::Resume(tramp)
}

/// Observer on the save routine: refresh the shadow copy it writes from.
fn save(m: &mut Machine, _f: &mut HookFrame, tramp: u32) -> HookResult {
    let shadow = m.unlock.shadow;
    unsafe {
        let live = mem::slice(m.img.link(PROGRESS), PROGRESS_SIZE).to_vec();
        mem::slice_mut(shadow, PROGRESS_SIZE).copy_from_slice(&live);
    }
    // while unlocked, the live block holds the forced values: save the player's own
    for (i, (addr, _)) in forced().into_iter().enumerate() {
        if let (true, Some(v)) = (m.unlock.active, m.unlock.real[i]) {
            unsafe { mem::wr32(shadow + (addr - PROGRESS), v) };
        }
    }
    crate::debug!("progress saved with the player's own values {:?}", m.unlock.real);
    HookResult::Resume(tramp)
}
