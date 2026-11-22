//! Controller vibration from the player's car (STRT3FX; the 1997 game had none).
//!
//! Once per race simulation step (the race frame timer, `4f793`) the player's car record
//! is read: what the game itself computed in its last step (velocity before and after,
//! wheels on the ground, the road kind under each wheel and the track's unevenness for it,
//! the wall and car contact flags, sliding tyres, engine speed). `sr_input::rumble` turns
//! that into motor levels and the input thread sends them to the controller. Nothing is
//! written to guest memory. Addresses and how they were verified: `docs/re/rumble.md`.
//!
//! Only in races the player drives (not the attract mode's demo race); outside races no
//! step runs, so the levels time out (`sr_input::rumble::TIMEOUT`): menus, the pause menu,
//! the settings overlay.
//!
//! Diagnostics: `SR_RUMBLE_LOG=FILE` (one CSV line per step: the values read, the event,
//! the motor levels), `SR_RUMBLE_DUMP=FILE` (the raw car record per step, for reverse
//! engineering).

use sr_input::rumble::{Event, Motors, Step, Synth};

use crate::Machine;
use crate::mem;

/// Game mode bits; bit 7: the attract mode's demo race (the game then plays no impact
/// sounds and applies no damage: `29378`, `32011`, `33ae5`).
const MODE: u32 = 0x1436dc;
const MODE_ATTRACT: u32 = 0x80;
/// Index of player 1's car.
const PLAYER1: u32 = 0x1436d4;
/// Car records.
const CARS: u32 = 0x1296c8;
const CAR_SIZE: u32 = 0xcc8;
/// The simulation step, seconds (smoothed; `4f780`).
const STEP: u32 = 0x1469c4;
/// The road kinds of the track (`MAPnDAT.DTR`, "ROAD KINDS"): 24h bytes per id.
const ROAD_KINDS: u32 = 0x12eb74;
const ROAD_KIND_SIZE: u32 = 0x24;
const ROAD_KIND_MAX: u32 = 48;
/// Road kind: unevenness ("sconn."; 0 on asphalt).
const RK_UNEVEN: u32 = 0x0c;

// Car record fields.
/// Flags; bit 3: in contact with another car (set at `3163f`).
const CAR_FLAGS: u32 = 0x4c;
const FLAG_CAR_CONTACT: u32 = 8;
/// Top speed (the game's speed unit; `2ae91` divides by it).
const TOP_SPEED: u32 = 0x5c;
/// Maximum engine speed (rpm).
const RPM_MAX: u32 = 0x60;
/// Touching a wall (set by `3314c`).
const WALL: u32 = 0x150;
/// Speed (float).
const SPEED: u32 = 0x500;
/// Fraction of the wheels on the ground (0, 0.25 .. 1).
const GROUND: u32 = 0x510;
/// Position x, y, z (float, world units).
const POS: u32 = 0x52c;
/// Velocity x, y, z (float, world units per second; y up).
const VEL: u32 = 0x55c;
/// The game's copy of the previous step's state (position, velocity, wheels, ...) lies
/// this far after the current one.
const PREV: u32 = 0x25c;
/// Wheels: four of 70h bytes.
const WHEELS: u32 = 0x574;
const WHEEL_SIZE: u32 = 0x70;
/// Wheel: on the ground (0 / 1).
const W_CONTACT: u32 = 0x04;
/// Wheel: suspension travel (-30..30).
const W_TRAVEL: u32 = 0x48;
/// Wheel: road kind id under it.
const W_ROAD: u32 = 0x5c;
/// Wheel: sliding beyond its grip (non-zero).
const W_SLIDE: u32 = 0x60;
/// Engine speed (rpm, float).
const RPM: u32 = 0xc7c;
/// Gear.
const GEAR: u32 = 0xc80;
/// Gravity in world units per second squared (measured: a car in the air loses 212.1
/// units/s of vertical speed per 0.033 s step).
const GRAVITY: f32 = 6427.0;

#[derive(Default)]
pub struct Rumble {
    synth: Synth,
    /// When the last step was taken (a gap restarts the effects).
    last: Option<std::time::Instant>,
    /// The road kind ids under the wheels last step (for the log).
    road: [u32; 4],
}

/// What was read in one step (logged).
struct Read {
    step: Step,
    speed: f32,
    wall: bool,
    car: bool,
    road: [u32; 4],
    gear: u32,
}

fn f32_at(a: u32) -> f32 {
    f32::from_bits(unsafe { mem::rd32(a) })
}

fn vec3(a: u32) -> [f32; 3] {
    [f32_at(a), f32_at(a + 4), f32_at(a + 8)]
}

/// The player's car this step, `None` when there is nothing to drive (no player car, the
/// attract mode, a step of 0).
fn read(m: &Machine) -> Option<Read> {
    let mode = unsafe { mem::rd32(m.img.link(MODE)) };
    let idx = unsafe { mem::rd32(m.img.link(PLAYER1)) };
    if mode & MODE_ATTRACT != 0 || idx >= 8 {
        return None;
    }
    let car = m.img.link(CARS) + idx * CAR_SIZE;
    let dt = f32_at(m.img.link(STEP));
    if !(dt > 0.0 && dt <= 0.25) {
        return None;
    }
    let (v, vp) = (vec3(car + VEL), vec3(car + VEL + PREV));
    let (p, pp) = (vec3(car + POS), vec3(car + POS + PREV));
    if v.iter().chain(&vp).chain(&p).chain(&pp).any(|x| !x.is_finite()) {
        return None;
    }
    let dv = [v[0] - vp[0], v[1] - vp[1], v[2] - vp[2]];
    let accel_h = (dv[0] * dv[0] + dv[2] * dv[2]).sqrt() / dt / GRAVITY;
    let accel_v = dv[1] / dt / GRAVITY + 1.0;
    // moved further than its speed allows: put back on the track or restarted
    let moved = ((p[0] - pp[0]).powi(2) + (p[1] - pp[1]).powi(2) + (p[2] - pp[2]).powi(2)).sqrt();
    let fastest = (vp.iter().map(|x| x * x).sum::<f32>().sqrt()).max(v.iter().map(|x| x * x).sum::<f32>().sqrt());
    let teleport = moved > fastest * dt * 1.5 + 500.0;
    let mut wheels_down = 0u8;
    let mut sliding = 0u8;
    let mut road = [0u32; 4];
    let (mut uneven, mut travel) = (0.0f32, 0.0f32);
    let kinds = m.img.link(ROAD_KINDS);
    for k in 0..4 {
        let w = car + WHEELS + k * WHEEL_SIZE;
        let (contact, id) = unsafe { (mem::rd32(w + W_CONTACT), mem::rd32(w + W_ROAD)) };
        road[k as usize] = id;
        travel += (unsafe { mem::rd32(w + W_TRAVEL) } as i32 - unsafe { mem::rd32(w + W_TRAVEL + PREV) } as i32).unsigned_abs() as f32 / 4.0;
        if contact == 0 {
            continue;
        }
        wheels_down += 1;
        if unsafe { mem::rd32(w + W_SLIDE) } != 0 {
            sliding += 1;
        }
        if id < ROAD_KIND_MAX {
            uneven += (unsafe { mem::rd32(kinds + id * ROAD_KIND_SIZE + RK_UNEVEN) } as i32).clamp(0, 10) as f32;
        }
    }
    let top = f32_at(car + TOP_SPEED);
    let speed = f32_at(car + SPEED);
    let rpm_max = f32_at(car + RPM_MAX);
    let flags = unsafe { mem::rd32(car + CAR_FLAGS) };
    let wall = unsafe { mem::rd32(car + WALL) } != 0;
    let car_contact = flags & FLAG_CAR_CONTACT != 0;
    let step = Step {
        dt,
        speed: if top > 1.0 { (speed / top).clamp(0.0, 2.0) } else { 0.0 },
        accel_h,
        accel_v,
        wheels_down,
        // the game's landing sound: the previous step had no wheel down (`2b280`)
        was_airborne: f32_at(car + GROUND + PREV) == 0.0,
        fall: (-vp[1]).max(0.0) / GRAVITY,
        contact: wall || car_contact,
        teleport,
        roughness: if wheels_down > 0 { uneven / wheels_down as f32 } else { 0.0 },
        suspension: travel,
        sliding,
        rpm: if rpm_max > 1.0 { (f32_at(car + RPM) / rpm_max).clamp(0.0, 1.5) } else { 0.0 },
    };
    Some(Read { step, speed, wall, car: car_contact, road, gear: unsafe { mem::rd32(car + GEAR) } })
}

/// Called at the start of every race step (the race frame timer's tick wait has ended:
/// the previous step's physics is complete).
pub fn race_step(m: &mut Machine) {
    dump(m);
    let Some(r) = read(m) else {
        sr_input::rumble::release();
        return;
    };
    let now = std::time::Instant::now();
    // the first step of a race, or after a pause: no pulses carried over, no landing or
    // impact from the jump in time
    let fresh = m.rumble.last.is_none_or(|t| now.duration_since(t).as_secs_f32() > 0.5);
    m.rumble.last = Some(now);
    let mut step = r.step;
    if fresh {
        m.rumble.synth.reset();
        step.teleport = true;
    }
    let (motors, event) = m.rumble.synth.step(&step);
    sr_input::rumble::post(motors);
    log(m, &r, &step, motors, event);
    m.rumble.road = r.road;
}

/// `SR_RUMBLE_LOG=FILE`: one CSV line per step.
fn log(m: &Machine, r: &Read, s: &Step, motors: Motors, event: Option<Event>) {
    use std::io::Write;
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(p) = std::env::var_os("SR_RUMBLE_LOG") {
            let mut f = std::fs::File::create(p).ok();
            if let Some(f) = f.as_mut() {
                let _ = writeln!(
                    f,
                    "frame,time_s,speed,speed_frac,accel_h_g,accel_v_g,wheels_down,was_airborne,fall_gs,wall,car,teleport,road_kinds,roughness,suspension,sliding,rpm_frac,gear,event,strength,low,high"
                );
            }
            *FILE.lock().unwrap() = f;
        }
    });
    let mut g = FILE.lock().unwrap();
    let Some(f) = g.as_mut() else { return };
    let frame = m.glide.as_ref().map_or(0, |g| g.frames);
    let (name, strength) = match event {
        Some(Event::Impact { strength, .. }) => ("impact", strength),
        Some(Event::Landing { strength, .. }) => ("landing", strength),
        Some(Event::Bump { strength, .. }) => ("bump", strength),
        None if r.road != m.rumble.road => ("road", 0.0),
        None => ("", 0.0),
    };
    let _ = writeln!(
        f,
        "{frame},{:.3},{:.2},{:.3},{:.2},{:.2},{},{},{:.3},{},{},{},{}/{}/{}/{},{:.2},{:.1},{},{:.2},{},{name},{strength:.2},{:.3},{:.3}",
        crate::time::uptime().as_secs_f64(),
        r.speed,
        s.speed,
        s.accel_h,
        s.accel_v,
        s.wheels_down,
        s.was_airborne as u8,
        s.fall,
        r.wall as u8,
        r.car as u8,
        s.teleport as u8,
        r.road[0],
        r.road[1],
        r.road[2],
        r.road[3],
        s.roughness,
        s.suspension,
        s.sliding,
        s.rpm,
        r.gear,
        motors.low,
        motors.high
    );
}

/// `SR_RUMBLE_DUMP=FILE` (reverse engineering): per race step "RMBL", game frame, uptime
/// (ms), player index, five globals (13148Ch, 1436DCh, 12EB2Ch, 1469C4h, 1469D4h), the car
/// record's address and its CC8h bytes, then the first six `auto*` scene instances
/// (address + 1C0h bytes: instance A and the parts of B).
fn dump(m: &Machine) {
    use std::io::Write;
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Some(p) = std::env::var_os("SR_RUMBLE_DUMP") {
            *FILE.lock().unwrap() = std::fs::File::create(p).ok();
        }
    });
    let mut g = FILE.lock().unwrap();
    let Some(f) = g.as_mut() else { return };
    let idx = unsafe { mem::rd32(m.img.link(PLAYER1)) };
    if idx >= 8 {
        return;
    }
    const SCENE: u32 = 0x12f860;
    let frame = m.glide.as_ref().map_or(0, |g| g.frames) as u32;
    let mut rec = Vec::with_capacity(0x2000);
    rec.extend_from_slice(b"RMBL");
    rec.extend_from_slice(&frame.to_le_bytes());
    rec.extend_from_slice(&(crate::time::uptime().as_millis() as u32).to_le_bytes());
    rec.extend_from_slice(&idx.to_le_bytes());
    for a in [0x13148cu32, MODE, 0x12eb2c, STEP, 0x1469d4] {
        rec.extend_from_slice(&unsafe { mem::rd32(m.img.link(a)) }.to_le_bytes());
    }
    let car = m.img.link(CARS) + idx * CAR_SIZE;
    rec.extend_from_slice(&car.to_le_bytes());
    rec.extend_from_slice(unsafe { mem::slice(car, CAR_SIZE) });
    let base = m.img.link(SCENE);
    let mut n = 0;
    for i in 0..1024 {
        let e = base + i * 28;
        let (ptr, index) = unsafe { (mem::rd32(e + 0xc), mem::rd32(e + 0x18)) };
        if index == u32::MAX || n == 6 {
            break;
        }
        if unsafe { mem::slice(e, 4) } != b"auto" || !mem::readable(ptr, 0x1c0) {
            continue;
        }
        rec.extend_from_slice(&ptr.to_le_bytes());
        rec.extend_from_slice(unsafe { mem::slice(ptr, 0x1c0) });
        n += 1;
    }
    for _ in n..6 {
        rec.extend_from_slice(&[0u8; 4 + 0x1c0]);
    }
    let _ = f.write_all(&rec);
}

/// The process is ending: stop the motors (from `crate::terminate` and the host's exit).
pub fn stop() {
    sr_input::rumble::stop_now();
}
