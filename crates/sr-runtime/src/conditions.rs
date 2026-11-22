//! The race's condition on demand: day, night or fog on any race, switched live (STRT3FX).
//!
//! The game races at night or in fog only in some championship stages: the race's condition
//! `[1435c4]` (0 day, 1 night, 2 fog) is 0 in every other mode, chosen by the championship's
//! table or at random (`3cad2`), copied into the race at the set-up (`3723d`) and read by
//! the track loader (`docs/re/strt3fx.md`, "Condition"; the loader's calls traced with
//! `SR_TRACE_LOADS`):
//!
//! * the **night assets** (`c & 1`): the backdrop `map3` + '0' / '1' from `gfx\map32.dat`
//!   (`53fda` kind 2: downloaded to texture memory, `[173f80]`, its RAM copy freed), the
//!   cars' reflection map `env` from `gfx\envp<track><0|1>2.dat` (kind 4: `[aa6a8]`), and each
//!   car's model and textures `auto<n>` from `gfx\cars<model><0|1>2.dat` (`20aa0` puts it in
//!   a scene table entry, `20320` loads the model into the model table `[147390]`): the
//!   night cars carry the lit headlights, their beams and the tail-light glows;
//! * the **fog** (`37fb2`, `381cb`): day fog off, sky on; night a black fog, sky on, a black
//!   ground; fog a grey fog (B8B8B8h), no sky, a grey flat fill (BDF7h); `54e99` sets the
//!   fog colour (and with it whether the fog table runs to the far distance or half as far
//!   again, `[174008]`); the fog's far distance is chosen by the condition only when a
//!   camera is set up or changed (`23620` and the camera keys' `23b13` ..: fog -25000, night
//!   -35000 / -6000, or -27000 in the camera mode 1; day keeps the last one), which builds
//!   the fog table again (`5527a`, cached by its parameters at `[174000]`).
//!
//! A race loads with the setting's condition (the race's copy set at the track loader's
//! entry, `37c00`, the same observer as `crate::lod`'s detail level). At its first frame the
//! other night assets are loaded too, through the game's own loaders (`call_guest`), and
//! kept aside: the other backdrop's picture, the other reflection map, and per car the other
//! model (its scene table entry freed again; the model table keeps it). A change of the
//! setting during the race then switches at the next frame: the condition, the fog values
//! as the loader sets them, each player's camera set again in its own mode (the fog's
//! distance and table), and with the night bit the backdrop's picture written into texture
//! memory, the reflection map's pointer and each car's model table entry pointed at the
//! other model; the native renderer builds its scene again (about a third of a second).
//! The frames after a switch are the same, pixel for pixel, as those of a race loaded with
//! that condition. A restart keeps the assets as they are (the loader runs without
//! loading); the game frees every set when the race ends, the other ones with them.
//!
//! **Stadium has no night**: its archive `gfx\map72.dat` holds no night backdrop
//! (`MAP71.BCK`; the six rallies have both), and the game's set loader ends the game when a
//! set is missing ("Unknown error in Engine interface", `53fda` -> `53f38` -> `52e58`). So
//! the archives' directories are read first (`assets`, `missing`, at the track loader's
//! entry): a condition whose assets the track lacks is neither forced at the load nor
//! loaded at the first frame; on Stadium day and fog still switch live, night is refused.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::mem;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

/// The race's condition (`[1435c4]`), track and car records (`[1435cc + 2Ch n]`: +0 model,
/// +4 the number in its object name `auto<n>`), car count.
const CONDITION: u32 = 0x1435c4;
const TRACK: u32 = 0x1435c0;
const RACE_CARS: u32 = 0x1435cc;
const CAR_COUNT: u32 = 0x1435bc;
/// The car records (`+BF0h`: the car's scene table entry).
const CARS: u32 = 0x1296c8;
const CAR_SIZE: u32 = 0xcc8;
const SCENE: u32 = 0x12f860;
const MODELS: u32 = 0x147390;
/// The install path (`SCREAMER.CFG`), the track's map name (`map3` on China).
const INSTALL: u32 = 0x1313bc;
const MAP_NAME: u32 = 0x13669c;
/// The game's loaders: archive (`202d8(path, path)`), named set (`53fda(name, kind, -1)`),
/// scene entry (`20aa0(scene, name, &index)`), model (`20320(scene, index, 1)`: 0 = loaded),
/// fog colour (`54e99(colour)`).
const SET_ARCHIVE: u32 = 0x202d8;
const LOAD_SET: u32 = 0x53fda;
const SCENE_ENTRY: u32 = 0x20aa0;
const LOAD_MODEL: u32 = 0x20320;
const FOG_COLOUR: u32 = 0x54e99;
/// The loaders' current set and archive paths (saved and put back around the extra loads).
const CURRENT_SET: u32 = 0x14739c;
/// The loaded sets: 32 entries of 2Ch (+0 the set, 0 = free), all freed when a race ends
/// (39b40 -> 53d52).
const SETS: u32 = 0x147420;
const SET_SIZE: u32 = 0x2c;
const SET_SLOTS: u32 = 32;
const ARCHIVE_PATHS: u32 = 0x128e24;
const ARCHIVE_PATHS_LEN: u32 = 0x68;
/// The reflection map's set (`TexGen::Reflection`).
const ENV_SET: u32 = 0xaa6a8;
/// The backdrop in texture memory: its start and its `GrTexInfo` (`native::textures`).
const SKY_TMU: u32 = 0x173f80;
const SKY_INFO: u32 = 0x173fd4;
/// Fog on, sky on, flat fill, the view block (`[81ea4]`, +10h the ground colour), the
/// ground colour of the track (5:6:5 parts), the fog table's cached parameters.
const FOG_ON: u32 = 0x12373c;
const SKY_ON: u32 = 0x123740;
const FLAT_FILL: u32 = 0x123744;
const VIEW_BLOCK: u32 = 0x81ea4;
const GROUND_RGB: u32 = 0x136524;
const FOG_TABLE_KEY: u32 = 0x174000;
/// The players' cameras (`[1296bc]`, `[1296c0]` in two-player races, flag 40h of
/// `[1436dc]`), a camera's mode (+AE4h), and `23620(camera, mode)`: the mode set with the
/// fog distance of the race's condition (`[123734]`, `[123738]`) and the fog table built
/// from it (`5527a`). The game does this only when a camera is set up or changed.
const CAMERAS: u32 = 0x1296bc;
const TWO_PLAYERS: u32 = 0x1436dc;
const CAMERA_MODE: u32 = 0xae4;
const SET_CAMERA_MODE: u32 = 0x23620;

/// The setting: the game's choice (the championship's stages), or always day, night or fog.
pub const GAME: u8 = 0;
pub const DAY: u8 = 1;
pub const NIGHT: u8 = 2;
pub const FOG: u8 = 3;

static FORCED: AtomicU8 = AtomicU8::new(GAME);

/// The setting (applied at the next race frame).
pub fn set(v: u8) {
    FORCED.store(v.min(FOG), Ordering::Relaxed);
}

pub fn get() -> u8 {
    FORCED.load(Ordering::Relaxed)
}

/// The condition the setting asks for (`None`: the game's own).
fn wanted() -> Option<u32> {
    match get() {
        DAY => Some(0),
        NIGHT => Some(1),
        FOG => Some(2),
        _ => None,
    }
}

/// The race's two sets of night assets and what is shown.
#[derive(Default)]
struct Live {
    /// A race was loaded: prepare at its first frame.
    pending: bool,
    /// Prepared (both sets known).
    ready: bool,
    /// The game's own condition for this race (before the setting forced it).
    game: u32,
    /// The condition applied now.
    shown: u32,
    /// The night bit of the assets in place.
    bit: u32,
    /// Per night bit: the reflection map's set.
    env: [u32; 2],
    /// Per night bit: the backdrop's texels, and where they go.
    sky: [Vec<u8>; 2],
    sky_at: u32,
    /// Per car: the model table entry of its model and the two models (per night bit).
    cars: Vec<(u32, [u32; 2])>,
    /// The track's day ground colour (the view block's +10h, as the loader computes it).
    ground: u32,
    /// Per night bit: what the track's archives lack of its assets (`None`: all there).
    /// Stadium has no night backdrop (`MAP71.BCK`): the game's set loader ends the game
    /// when a set is missing, so nothing of a lacking bit is loaded or forced.
    lacks: [Option<String>; 2],
    /// A condition asked for that the track cannot show (logged once).
    refused: Option<u32>,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Live) -> R) -> R {
    let mut g = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Live::default))
}

pub fn install(m: &mut Machine) -> Result<(), String> {
    install_trace(m);
    Ok(())
}

/// At the track loader's entry (`edx` = load): the race's condition as the setting asks; a
/// loading call starts a new race's live state.
pub fn at_track_loader(m: &mut Machine, f: &HookFrame) {
    let at = m.img.link(CONDITION);
    let race = unsafe { mem::rd32(at) };
    // a loading call: what the track's archives hold; a restart keeps the race's
    let lacks = if f.edx != 0 { [missing(m, 0), missing(m, 1)] } else { with(|l| l.lacks.clone()) };
    let shown = match wanted() {
        Some(want) if want != race && lacks[(want & 1) as usize].is_some() => {
            if f.edx != 0 {
                crate::info!(
                    "race condition: no {} on this track ({}); the race keeps its own ({})",
                    NAMES[want as usize],
                    lacks[(want & 1) as usize].as_deref().unwrap_or(""),
                    NAMES[race.min(2) as usize]
                );
            }
            race
        }
        Some(want) => {
            if race != want {
                crate::info!(
                    "race condition: {race} forced to {want} ({})",
                    NAMES[want as usize]
                );
                unsafe { mem::wr32(at, want) };
            }
            want
        }
        None => race,
    };
    if f.edx != 0 {
        with(|l| {
            *l = Live {
                pending: true,
                game: race.min(2),
                shown: shown.min(2),
                bit: shown & 1,
                lacks,
                ..Default::default()
            }
        });
    }
}

const NAMES: [&str; 3] = ["day", "night", "fog"];
/// The conditions of each night bit.
const BIT_NAMES: [&str; 2] = ["day and fog", "night"];

/// The archives and entries the game's loaders read for the assets of night bit `bit`, for
/// the race set up now: each car's model (`auto<n>` from `gfx\cars<model><bit>2.dat`), the
/// cars' reflection map (`env` from `gfx\envp<track><bit>2.dat`) and the backdrop
/// (`map<track><bit>` from `gfx\map<track>2.dat`).
fn assets(m: &Machine, bit: u32) -> Vec<(String, Vec<String>)> {
    let l = |a: u32| m.img.link(a);
    let install = String::from_utf8_lossy(&unsafe { mem::cstr(l(INSTALL), 200) }).into_owned();
    let track = rd(l(TRACK)) + 1;
    let mut v = Vec::new();
    for n in 0..rd(l(CAR_COUNT)).min(6) {
        let (model, number) = (rd(l(RACE_CARS) + 0x2c * n), rd(l(RACE_CARS) + 0x2c * n + 4));
        let car = format!("AUTO{}", number + 1);
        v.push((
            format!("{install}\\gfx\\cars{}{bit}2.dat", model + 1),
            ["HIC", "MLB", "3DX"].iter().map(|e| format!("{car}.{e}")).collect(),
        ));
    }
    v.push((format!("{install}\\gfx\\envp{track}{bit}2.dat"), vec!["ENV.HIC".into(), "ENV.MLB".into()]));
    v.push((format!("{install}\\gfx\\map{track}2.dat"), vec![format!("MAP{track}{bit}.BCK")]));
    v
}

/// What the track's archives lack of the assets of night bit `bit` (`None`: all there).
fn missing(m: &Machine, bit: u32) -> Option<String> {
    let mut read: Vec<(String, Option<Vec<String>>)> = Vec::new();
    for (path, entries) in assets(m, bit) {
        if !read.iter().any(|(p, _)| *p == path) {
            read.push((path.clone(), m.vfs.archive_names(&path)));
        }
        let Some((_, names)) = read.iter().find(|(p, _)| *p == path) else { continue };
        let Some(names) = names else { return Some(format!("no {path}")) };
        if let Some(e) = entries.iter().find(|e| !names.iter().any(|n| n.eq_ignore_ascii_case(e))) {
            return Some(format!("no {e} in {path}"));
        }
    }
    None
}

/// At the start of each race frame (`crate::profile::strt3fx`'s frame-start observer): the
/// other night assets prepared at a race's first frame; a change of the setting applied.
pub fn race_frame(m: &mut Machine) {
    test_switch();
    if with(|l| std::mem::take(&mut l.pending)) {
        let t0 = std::time::Instant::now();
        let (bit, lacks) = with(|l| (l.bit, l.lacks.clone()));
        match &lacks[(bit ^ 1) as usize] {
            // the other bit's assets are not on this track: only this bit's conditions
            // switch (fog and day share the backdrop, the cars and the reflection map)
            Some(why) => {
                let ground = ground(m);
                with(|l| {
                    l.ground = ground;
                    l.ready = true;
                });
                crate::info!(
                    "race condition: no {} on this track ({why}); {} switch live",
                    BIT_NAMES[(bit ^ 1) as usize],
                    BIT_NAMES[bit as usize]
                );
            }
            None => match prepare(m) {
                Ok(slots) => crate::info!(
                    "race condition: the other night assets loaded in {:.0} ms (live switching ready; {slots} of the {SET_SLOTS} set slots in use)",
                    t0.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => crate::warn!("race condition: live switching unavailable this race: {e}"),
            },
        }
    }
    let (ready, shown, game, bit, lacks, refused) = with(|l| (l.ready, l.shown, l.game, l.bit, l.lacks.clone(), l.refused));
    let target = wanted().unwrap_or(game);
    if !ready || target == shown {
        if refused.is_some() {
            with(|l| l.refused = None);
        }
        return;
    }
    if target & 1 != bit && lacks[(target & 1) as usize].is_some() {
        if refused != Some(target) {
            crate::info!("race condition: no {} on this track; the race stays {}", NAMES[target as usize], NAMES[shown as usize]);
            with(|l| l.refused = Some(target));
        }
        return;
    }
    switch(m, target);
}

/// The track's day ground colour, as the loader computes it (`38163`) from the track's
/// colour parts.
fn ground(m: &Machine) -> u32 {
    let at = m.img.link(GROUND_RGB);
    let (r, g, b) = (rd(at), rd(at + 4), rd(at + 8));
    let c = ((r << 8) & 0xf800) | ((g << 3) & 0x7e0) | (b >> 3);
    c | c << 16
}

/// `SR_CONDITION_SWITCH=<game frame>,<setting>[/...]` (tests): the setting changed at those
/// frames, as the menu would.
fn test_switch() {
    static SWITCH: Mutex<Option<Vec<(u64, u8)>>> = Mutex::new(None);
    let mut g = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
    let list = g.get_or_insert_with(|| {
        std::env::var("SR_CONDITION_SWITCH")
            .ok()
            .map(|s| s.split(['/', ';']).filter_map(|p| p.split_once(',')).filter_map(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))).collect())
            .unwrap_or_default()
    });
    let frame = crate::glide::FRAME.load(Ordering::Relaxed);
    while list.first().is_some_and(|&(at, _)| frame >= at) {
        let (_, v) = list.remove(0);
        crate::info!("race condition: setting switched to {v} at frame {frame} (SR_CONDITION_SWITCH)");
        set(v);
    }
}

/// A guest scratch block for the loaders' strings (allocated once).
fn scratch(m: &mut Machine) -> u32 {
    static AT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let a = AT.load(Ordering::Relaxed);
    if a != 0 {
        return a;
    }
    let a = m.arena.reserve(1024);
    AT.store(a, Ordering::Relaxed);
    a
}

fn cstring(at: u32, s: &str) -> u32 {
    let b = s.as_bytes();
    unsafe {
        mem::slice_mut(at, b.len() as u32).copy_from_slice(b);
        mem::wr8(at + b.len() as u32, 0);
    }
    at
}

fn rd(a: u32) -> u32 {
    unsafe { mem::rd32(a) }
}

/// The backdrop's texels in texture memory now (start, bytes).
fn sky_texels(m: &Machine) -> Option<(u32, Vec<u8>)> {
    let info = unsafe { crate::glide::tex::TexInfo::read(m.img.link(SKY_INFO)) };
    let (w, h) = crate::glide::tex::level_size(info.large_lod, info.aspect);
    let start = rd(m.img.link(SKY_TMU));
    let n = (w * h * 2) as usize;
    let px = m
        .glide
        .as_ref()?
        .tmu
        .ram
        .get(start as usize..start as usize + n)?
        .to_vec();
    Some((start, px))
}

/// Load the other night assets through the game's loaders and keep both sets.
fn prepare(m: &mut Machine) -> Result<u32, String> {
    let buf = scratch(m);
    let l = |a: u32| m.img.link(a);
    let bit = with(|s| s.bit);
    let other = bit ^ 1;
    let install = String::from_utf8_lossy(&unsafe { mem::cstr(l(INSTALL), 200) }).into_owned();
    let map = String::from_utf8_lossy(&unsafe { mem::cstr(l(MAP_NAME), 16) }).into_owned();
    let track = rd(l(TRACK));
    let (s_path, s_name, s_index) = (buf, buf + 512, buf + 1000);
    // the loaders' state, put back afterwards
    let saved_set = rd(l(CURRENT_SET));
    let saved_paths = unsafe { mem::slice(l(ARCHIVE_PATHS), ARCHIVE_PATHS_LEN) }.to_vec();
    let saved_env = rd(l(ENV_SET));
    let saved_sky_info = unsafe { mem::slice(l(SKY_INFO), 20) }.to_vec();
    let saved_sky_at = rd(l(SKY_TMU));
    let (sky_at, sky_now) = sky_texels(m).ok_or("no backdrop in texture memory")?;
    let models = rd(l(MODELS));
    // the loader's sets: a full table ends the game (53fda, "Aumentare MAXMTRALLOC"); the
    // other sets (one per car, the reflection map, the backdrop) only when they fit
    let used = |l: &dyn Fn(u32) -> u32| (0..SET_SLOTS).filter(|&i| rd(l(SETS) + SET_SIZE * i) != 0).count() as u32;
    let needed = rd(l(CAR_COUNT)).min(6) + 2;
    let before = used(&l);
    if before + needed > SET_SLOTS {
        return Err(format!("{before} of the {SET_SLOTS} set slots in use, {needed} more needed"));
    }
    let call = |f: u32, eax: u32, edx: u32, ebx: u32| unsafe {
        crate::guest::call_guest(l(f), eax, edx, ebx, 0)
    };
    let (cars, env, sky_other) = {
        // each car's other model
        let count = rd(l(CAR_COUNT)).min(6);
        let mut cars = Vec::new();
        for n in 0..count {
            let rec = l(CARS) + n * CAR_SIZE;
            let entry = l(SCENE) + rd(rec + 0xbf0) * 28;
            let inst = rd(entry + 0xc);
            if !mem::readable(inst, 4) {
                return Err(format!("car {n}: no instance"));
            }
            let slot = models + 4 * rd(inst);
            let (model, number) = (rd(l(RACE_CARS) + 0x2c * n), rd(l(RACE_CARS) + 0x2c * n + 4));
            cstring(
                s_path,
                &format!("{install}\\gfx\\cars{}{}2.dat", model + 1, other),
            );
            call(SET_ARCHIVE, s_path, s_path, u32::MAX);
            cstring(s_name, &format!("auto{}", number + 1));
            call(LOAD_SET, s_name, 1, u32::MAX);
            unsafe { mem::wr32(s_index, 0) };
            call(SCENE_ENTRY, l(SCENE), s_name, s_index);
            let index = rd(s_index);
            if index >= 250 {
                return Err(format!("car {n}: scene table full"));
            }
            if call(LOAD_MODEL, l(SCENE), index, 1) != 0 {
                return Err(format!("car {n}: model auto{} not loaded", number + 1));
            }
            let e2 = l(SCENE) + index * 28;
            let other_model = rd(models + 4 * rd(rd(e2 + 0xc)));
            // the extra entry freed again (the model table keeps the model)
            unsafe { mem::slice_mut(e2, 28).fill(0) };
            let mut pair = [0u32; 2];
            pair[bit as usize] = rd(slot);
            pair[other as usize] = other_model;
            cars.push((slot, pair));
        }
        // the other reflection map
        cstring(
            s_path,
            &format!("{install}\\gfx\\envp{}{}2.dat", track + 1, other),
        );
        call(SET_ARCHIVE, s_path, s_path, u32::MAX);
        cstring(s_name, "env");
        call(LOAD_SET, s_name, 4, u32::MAX);
        let mut env = [0u32; 2];
        env[bit as usize] = saved_env;
        env[other as usize] = rd(l(ENV_SET));
        // the other backdrop (downloaded over this one: its texels kept, these put back)
        cstring(s_path, &format!("{install}\\gfx\\{map}2.dat"));
        call(SET_ARCHIVE, s_path, s_path, u32::MAX);
        cstring(s_name, &format!("{map}{other}"));
        call(LOAD_SET, s_name, 2, u32::MAX);
        let (_, sky_other) = sky_texels(m).ok_or("the other backdrop is not in texture memory")?;
        unsafe {
            mem::slice_mut(l(SKY_INFO), 20).copy_from_slice(&saved_sky_info);
            mem::wr32(l(SKY_TMU), saved_sky_at);
            mem::wr32(l(ENV_SET), saved_env);
            mem::wr32(l(CURRENT_SET), saved_set);
            mem::slice_mut(l(ARCHIVE_PATHS), ARCHIVE_PATHS_LEN).copy_from_slice(&saved_paths);
        }
        (cars, env, sky_other)
    };
    write_sky(m, sky_at, &sky_now);
    let ground = ground(m);
    with(|s| {
        let mut sky: [Vec<u8>; 2] = Default::default();
        sky[bit as usize] = sky_now;
        sky[other as usize] = sky_other;
        s.sky = sky;
        s.sky_at = sky_at;
        s.env = env;
        s.cars = cars;
        s.ground = ground;
        s.ready = true;
    });
    Ok(used(&|a| m.img.link(a)))
}

fn write_sky(m: &mut Machine, at: u32, px: &[u8]) {
    if let Some(g) = m.glide.as_mut() {
        if let Some(dst) = g.tmu.ram.get_mut(at as usize..at as usize + px.len()) {
            dst.copy_from_slice(px);
        }
    }
}

/// Show condition `c` now.
fn switch(m: &mut Machine, c: u32) {
    let (bit, cars, env, sky, sky_at, ground) = with(|s| {
        (
            s.bit,
            s.cars.clone(),
            s.env,
            s.sky.clone(),
            s.sky_at,
            s.ground,
        )
    });
    let new_bit = c & 1;
    {
        let l = |a: u32| m.img.link(a);
        unsafe {
            mem::wr32(l(CONDITION), c);
            // the fog as the loader sets it (37fb2, 381cb)
            let (fog, sky_on, fill, colour) = match c {
                0 => (0, 1, 1, 0),
                1 => (1, 1, 0, 0),
                _ => (1, 0, 0xbdf7_bdf7, 0xb8_b8b8),
            };
            mem::wr32(l(FOG_ON), fog);
            mem::wr32(l(SKY_ON), sky_on);
            mem::wr32(l(FLAT_FILL), fill);
            let view = rd(l(VIEW_BLOCK));
            if mem::readable(view, 0x14) {
                match c {
                    0 => mem::wr32(view + 0x10, ground),
                    1 => mem::wr32(view + 0x10, 0),
                    _ => {}
                }
            }
            crate::guest::call_guest(l(FOG_COLOUR), colour, 0, 0, 0);
            // the fog distance for the condition, as the race start sets it: each player's
            // camera set again in its own mode (the fog table built again from it)
            mem::wr32(l(FOG_TABLE_KEY), 0x7fff_ffff);
            let players = if rd(l(TWO_PLAYERS)) & 0x40 != 0 { 2 } else { 1 };
            for p in 0..players {
                let camera = rd(l(CAMERAS) + 4 * p);
                if mem::readable(camera + CAMERA_MODE, 4) {
                    let mode = rd(camera + CAMERA_MODE);
                    crate::guest::call_guest(l(SET_CAMERA_MODE), camera, mode, 0, 0);
                }
            }
            if new_bit != bit {
                for (slot, pair) in &cars {
                    mem::wr32(*slot, pair[new_bit as usize]);
                }
                mem::wr32(l(ENV_SET), env[new_bit as usize]);
            }
        }
    }
    if new_bit != bit {
        write_sky(m, sky_at, &sky[new_bit as usize]);
    }
    with(|s| {
        s.shown = c;
        s.bit = new_bit;
    });
    // the native renderer's scene built again (the cars' meshes, the backdrop, the maps)
    crate::native::frame::rebuild_scene(m);
    crate::info!("race condition: switched to {} live", NAMES[c as usize]);
}

/// `SR_TRACE_LOADS=1` (diagnostics): the loader's calls with their arguments.
fn install_trace(m: &mut Machine) {
    if std::env::var("SR_TRACE_LOADS").ok().as_deref() != Some("1") {
        return;
    }
    fn s(a: u32) -> String {
        if a == 0 || a == u32::MAX || !mem::readable(a, 1) {
            return format!("{a:#x}");
        }
        String::from_utf8_lossy(&unsafe { mem::cstr(a, 80) }).into_owned()
    }
    macro_rules! t {
        ($name:expr, $addr:expr) => {
            let _ = m.install_observer($name, $addr, |m, f, tramp| {
                let ret = m.img.to_link(f.stack(0)).unwrap_or(0);
                crate::info!("load trace {} from {ret:#x}: eax {:#x} '{}' edx {:#x} '{}' ebx {:#x} [frame {}]", $name, f.eax, s(f.eax), f.edx, s(f.edx), f.ebx, crate::glide::FRAME.load(Ordering::Relaxed));
                crate::hooks::HookResult::Resume(tramp)
            });
        };
    }
    t!("202d8 set archive", SET_ARCHIVE);
    t!("53fda load set", LOAD_SET);
    t!("20aa0 scene entry", SCENE_ENTRY);
    t!("20320 load model", LOAD_MODEL);
    t!("54e99 fog colour", FOG_COLOUR);
}
