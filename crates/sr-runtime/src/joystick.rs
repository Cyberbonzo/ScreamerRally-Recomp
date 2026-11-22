//! Virtual PC gameport joystick.
//!
//! The game supports an analog joystick on the standard gameport (port 201h): steering,
//! throttle and brake are read as analog axes, calibrated by the original setup program
//! (`DRV\ST.EXE`) and stored in `CONFIG\CONTROLS.CFG`. The physics has a dedicated analog
//! steering path (its own response curve and lock) next to the keyboard path.
//!
//! A real gameport is read by timing a one-shot: the game writes the port and counts loop
//! iterations until each axis bit drops (`0x504b0`), with interrupts disabled. That cannot
//! work on Windows (and would cost thousands of traps per frame), so the two small routines
//! that read the port are replaced. They return what the hardware would have returned:
//! the loop count for an axis and the button bits, for a joystick whose position comes from
//! the host's controller.
//!
//! The counts are chosen to match the calibration written to `CONTROLS.CFG` by the host
//! (see [`calibration`]): center [`CENTER`] and scale 1.0, so the game's own conversion
//! `(count - center) * scale` yields exactly the value the host computed, in the game's
//! native range -1023..1023. Everything after that (the game's dead zone, steering curve,
//! clamping) is the original code.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;

/// The gameport.
pub const PORT: u32 = 0x201;
/// Loop count of an axis at rest.
pub const CENTER: i32 = 1024;
/// Full deflection in the game's units.
pub const FULL: i32 = 1023;
/// The game ignores axis values below this (`0x3a442: cmp eax, 66h`).
pub const GAME_DEAD_ZONE: i32 = 0x66;

/// Joystick state shared with the host's input thread.
pub struct GamePort {
    axes: [AtomicI32; 4],
    buttons: AtomicU32,
    connected: AtomicBool,
    pub reads: AtomicU32,
}

pub static GAMEPORT: GamePort = GamePort {
    axes: [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)],
    buttons: AtomicU32::new(0),
    connected: AtomicBool::new(false),
    reads: AtomicU32::new(0),
};

impl GamePort {
    /// `axes` in game units (-1023..1023, 0 = centered); `buttons` bit n = button n+1 pressed.
    pub fn set(&self, axes: [i32; 4], buttons: u32, connected: bool) {
        for (a, v) in self.axes.iter().zip(axes) {
            a.store(v.clamp(-FULL, FULL), Ordering::Relaxed);
        }
        self.buttons.store(buttons & 0xf, Ordering::Relaxed);
        self.connected.store(connected, Ordering::Relaxed);
    }

    fn count(&self, axis: usize) -> u32 {
        // a disconnected controller reads as a centered joystick, never as a timeout
        // (which the game would take as full deflection)
        let v = if self.connected.load(Ordering::Relaxed) { self.axes[axis].load(Ordering::Relaxed) } else { 0 };
        (CENTER + v) as u32
    }

    fn buttons(&self) -> u32 {
        if self.connected.load(Ordering::Relaxed) { self.buttons.load(Ordering::Relaxed) } else { 0 }
    }
}

fn axis_of_mask(mask: u32) -> Option<usize> {
    match mask & 0xf {
        1 => Some(0),
        2 => Some(1),
        4 => Some(2),
        8 => Some(3),
        _ => None,
    }
}

/// `0x50474`: `buttons(port)` -> bits 0-3 = buttons 1-4 pressed (`(in(port) >> 4) ^ 0Fh`).
pub fn read_buttons(_m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    f.eax = if f.eax == PORT { GAMEPORT.buttons() } else { 0 };
    HookResult::Return(0)
}

/// `0x50482`: `axis(mask = 1 << axis, port)` -> `(raw button bits << 12) | count`.
/// The raw port bits are active low; the caller masks the count with 0FFFh.
pub fn read_axis(_m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    GAMEPORT.reads.fetch_add(1, Ordering::Relaxed);
    let raw_buttons = (!GAMEPORT.buttons() & 0xf) << 4;
    let count = match (f.edx == PORT, axis_of_mask(f.eax)) {
        (true, Some(axis)) => GAMEPORT.count(axis),
        // no device: the measuring loop runs into its limit (1800h)
        _ => 0x1800,
    };
    f.eax = (raw_buttons << 12) | (count & 0xfff);
    HookResult::Return(0)
}

/// `0x50437`: `axis_present(mask, port)` -> 1 if the axis' one-shot fires and settles.
pub fn axis_present(_m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    f.eax = (f.edx == PORT && axis_of_mask(f.eax).is_some()) as u32;
    HookResult::Return(0)
}

/// `CONTROLS.CFG` layout (1496 bytes, read by `0x3ac74`):
/// player 1 bindings (6 x 1Ch), player 2 bindings (6 x 1Ch), axis calibration
/// (4 axes x 10 devices x 1Ch), port of each device (10 dwords).
pub mod controls_cfg {
    pub const SIZE: usize = 1496;
    pub const PLAYER1: usize = 0;
    pub const PLAYER2: usize = 6 * 0x1c;
    pub const CALIBRATION: usize = 12 * 0x1c;
    pub const PORTS: usize = CALIBRATION + 40 * 0x1c;

    /// Actions in binding order.
    pub const ACCELERATE: usize = 0;
    pub const BRAKE: usize = 1;
    pub const STEER_LEFT: usize = 2;
    pub const STEER_RIGHT: usize = 3;
    pub const GEAR_UP: usize = 4;
    pub const GEAR_DOWN: usize = 5;

    /// A binding as written by `ST.EXE` (`0x1a442`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Binding {
        Key(u8),
        /// Analog joystick axis (0-3) and the direction that activates the action.
        Axis { axis: u32, dir: i32 },
        /// Joystick button mask (bit n = button n+1).
        Button(u32),
    }

    fn put(cfg: &mut [u8], off: usize, v: [i32; 7]) {
        for (i, x) in v.iter().enumerate() {
            cfg[off + i * 4..off + i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        }
    }

    /// Fields: bound, is axis, device kind (0 analog joystick, 1 keyboard, 2 joypad),
    /// device, axis, direction, key scancode / button mask / axis sensitivity (0 = none).
    pub fn set_binding(cfg: &mut [u8], player_off: usize, action: usize, b: Binding) {
        let v = match b {
            Binding::Key(sc) => [1, 0, 1, 0, 0, 1, sc as i32],
            Binding::Axis { axis, dir } => [1, 1, 0, 0, axis as i32, dir, 0],
            Binding::Button(mask) => [1, 0, 0, 0, 0, 1, mask as i32],
        };
        put(cfg, player_off + action * 0x1c, v);
    }

    /// Calibration of device 0 (gameport 201h) matching [`super::read_axis`]: every axis
    /// present, centered at [`super::CENTER`], scale 1.0 on both sides. Fields: present,
    /// center, rest (mid of the noise range), noise range, port, scale above center,
    /// scale below center (`ST.EXE 0x19dcf, 0x13f34`).
    pub fn calibrate_device0(cfg: &mut [u8]) {
        for axis in 0..4 {
            let off = CALIBRATION + (axis * 10) * 0x1c;
            let one = f32::to_bits(1.0) as i32;
            put(cfg, off, [1, super::CENTER, super::CENTER, 1, super::PORT as i32, one, one]);
        }
        cfg[PORTS..PORTS + 4].copy_from_slice(&super::PORT.to_le_bytes());
    }
}

/// Diagnostic observer on the game's input mapper `0x3a2e0(bindings)` (`SR_OBSERVE_INPUT=1`):
/// every 50th call, log the binding kinds, the previous frame's six action values
/// (accelerate, brake, left, right, gear up, gear down) and the Up key's state.
pub fn observe_mapper(m: &mut Machine, f: &mut HookFrame, tramp: u32) -> HookResult {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n % 50 == 0 {
        let t = f.eax;
        let rd = |a: u32| unsafe { crate::mem::rd32(a) as i32 };
        let outs: Vec<i32> = (0..6).map(|i| rd(t + 0xa8 + i * 4)).collect();
        let kb = |l: u32| unsafe { crate::mem::rd8(m.img.link(l)) };
        let kind = |i: u32| rd(t + i * 0x1c + 8);
        crate::info!(
            "mapper#{n} table {:#x} (link {:#x}) kinds {:?} outs {outs:?} live[48]={} copy[48]={}",
            t,
            m.img.to_link(t).unwrap_or(0),
            (0..6).map(kind).collect::<Vec<_>>(),
            kb(0x50330 + 0x48),
            kb(0x110002 + 0x48)
        );
    }
    HookResult::Resume(tramp)
}
