//! `STRT3FX.EXE` (3dfx build, Italian CD). Addresses are link-time addresses as shown by
//! `srtool dis`; see `docs/re/strt3fx.md`.

use crate::Machine;
use crate::cpu::HookFrame;
use crate::hooks::HookResult;
use crate::mem::{self, FLAT_DS};

pub fn install(m: &mut Machine) -> Result<(), String> {
    // Watcom cstart, DOS/4GW branch: extender detection, PSP / environment / command line.
    //   bf2c8: mov dx,78h / mov ax,0FF00h / int 21h ...   up to bf3b3
    m.install_hook(
        "cstart: DOS/4GW environment setup",
        0xbf2c8,
        &[0x66, 0xba, 0x78, 0x00, 0x66, 0xb8, 0x00, 0xff, 0xcd, 0x21],
        cstart_dos4g,
    )?;
    // Glide 2.x imports: 130 lazy-binding thunks (`call 0xdfd8c`) at ee92c, names at eebb8.
    // Binding them directly replaces the DOS/4GW DLL loader and glide2x.ovl.
    crate::glide::bind_imports(m, 0xee92c, 0xeebb8, 130, 0xdfd8c)?;

    // HMI SOS timer service (c0f81..c15a3).
    use crate::hmi;
    const PRO6: &[u8] = &[0x53, 0x51, 0x52, 0x56, 0x57, 0x55, 0x89, 0xe5];
    m.install_hook("sosTIMERInitSystem", 0xc0f81, &[0x53, 0x51, 0x56, 0x57, 0x55, 0x89, 0xe5], hmi::timer_init)?;
    m.install_hook("sosTIMERUnInitSystem", 0xc1058, PRO6, hmi::timer_uninit)?;
    m.install_hook("sosTIMERRegisterEvent", 0xc1092, &[0x51, 0x56, 0x57, 0x55, 0x89, 0xe5], hmi::timer_register)?;
    m.install_hook("sosTIMERAlterEventRate", 0xc127d, &[0x53, 0x51, 0x56, 0x57, 0x55, 0x89, 0xe5], hmi::timer_alter)?;
    m.install_hook("sosTIMERRemoveEvent", 0xc13e4, PRO6, hmi::timer_remove)?;
    m.install_hook("sosTIMERGetEventRate", 0xc1579, PRO6, hmi::timer_get_rate)?;

    // HMI SOS digital audio API (d6c89..d8a4b) and card detection (d9cf9..), on the
    // native mixer. The MIDI side stays unused (music card 0 in SETUP.CFG; music is CD).
    crate::sos_digi::install(m)?;
    crate::widescreen::install(m)?;
    crate::depth::install(m)?;
    crate::subpixel::install(m)?;
    // SR_TRANSFORMS_CHECK (diagnostics): float transforms against the engine's
    crate::native::transforms::install(m)?;
    crate::lod::install(m)?;
    crate::conditions::install(m)?;
    crate::camera::install(m);
    crate::visibility::install(m)?;
    crate::ui2d::install(m)?;
    // SR_DUMP_WORLD only (diagnostics)
    crate::native::world::install(m)?;
    // diagnostics (SR_POLY_CHECK), nothing installed otherwise
    crate::native::polygons::install(m)?;
    let dd = std::env::var("SR_DRAW_DISTANCE").ok().and_then(|v| v.parse().ok()).unwrap_or(m.options.draw_distance);
    crate::widescreen::install_draw_distance(m, dd)?;
    let unlock = m.options.unlock_all;
    crate::unlock::install(m, unlock)?;
    // the visibility editor's game window (`--vis-editor` only; nothing installed otherwise)
    crate::toolmode::install(m)?;
    // interpolated frames: the main camera of each frame is taken when the HUD starts
    crate::interp::set_enabled(m.options.interpolate);
    crate::interp::set_fps_cap(m.options.fps_cap);
    m.install_observer("hud (interpolation)", crate::interp::HUD_ENTRY, hud_entry)?;
    // the game's end-of-frame drawing follows the rear-view mirror: where it ends in the
    // recording (extra frames draw the mirror again)
    m.install_observer("frame end (interpolation)", crate::interp::FRAME_END, |m, _f, tramp| {
        if let Some(g) = m.glide.as_mut() {
            g.mirror_end();
        }
        crate::hooks::HookResult::Resume(tramp)
    })?;
    // end of the race timer's tick wait (4f78e: call 504dc): the game starts its frame
    m.install_observer("frame start (interpolation)", 0x4f793, |m, _f, tramp| {
        m.interp.work_start = Some(std::time::Instant::now());
        crate::autopilot::tick(m);
        crate::toolmode::race_frame(m);
        crate::conditions::race_frame(m);
        crate::interp::log_objects(m);
        crate::native::race_frame(m);
        crate::rumble::race_step(m);
        crate::hooks::HookResult::Resume(tramp)
    })?;
    crate::native::install(m);

    // Frame pacing. Both frame timers (4f6de menus, 4f780 race) wait for one 100 Hz tick,
    // then use the elapsed time as the simulation step but never less than 0.02 s: above
    // 50 frames per second the game runs faster than real time (on 1997 hardware it never
    // got there). The tick wait (504dc, `while (ticks < n)`) waits for `n` ticks
    // (`crate::pacing`), set on entry to each timer: the menus 2 (50 frames per second),
    // the race 3 by default (33 steps per second, as 1997 hardware ran it; the car physics
    // depends on the step size), 2 or 4 by choice. See docs/re.
    let _ = crate::pacing::ticks();
    m.patch("frame pacing", 0x504e2, &[0x83, 0xf8, 0x01], &[0x83, 0xf8, crate::pacing::MENU_TICKS])?;
    crate::pacing::install(m);
    m.install_observer("menu frame timer (pacing)", crate::pacing::MENU_TIMER, |m, _f, tramp| {
        crate::pacing::enter_timer(false);
        // menus: the native renderer draws race views only
        crate::native::frame::menu_frame(m);
        crate::hooks::HookResult::Resume(tramp)
    })?;
    m.install_observer("race frame timer (pacing)", crate::pacing::RACE_TIMER, |_m, _f, tramp| {
        crate::pacing::enter_timer(true);
        crate::hooks::HookResult::Resume(tramp)
    })?;
    if crate::time::deterministic() {
        m.install_observer("tick wait (deterministic)", 0x504dc, tick_wait)?;
    }

    // Gameport joystick (50437..504e0): replaced by the virtual joystick, see joystick.rs.
    use crate::joystick;
    if std::env::var_os("SR_NO_JOYSTICK").is_none() {
        m.install_hook("joystick axis present", 0x50437, &[0x57, 0x51, 0x53, 0x8b, 0xda], joystick::axis_present)?;
        m.install_hook("joystick buttons", 0x50474, &[0x52, 0x8b, 0xd0, 0x33, 0xc0], joystick::read_buttons)?;
        m.install_hook("joystick axis", 0x50482, &[0x51, 0x57, 0x53, 0x52, 0x8b, 0xda], joystick::read_axis)?;
    }

    if std::env::var_os("SR_OBSERVE_INPUT").is_some() {
        m.install_observer("input mapper", 0x3a2e0, joystick::observe_mapper)?;
    }
    // diagnostics: SR_OBSERVE="link,link,..." logs registers and stack at those addresses
    if let Ok(list) = std::env::var("SR_OBSERVE") {
        for a in list.split(',').filter_map(|a| u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()) {
            if let Err(e) = m.install_observer("observer", a, crate::observe_generic) {
                crate::warn!("{e}");
            }
        }
    }
    if std::env::var_os("SR_OBSERVE_HMI").is_some() {
        for &a in hmi::ENTRY_POINTS {
            if let Err(e) = m.install_observer("hmi observer", a, hmi::observe) {
                crate::warn!("{e}");
            }
        }
    }

    // Instructions addressing game tables through emulated selectors whose base is not zero
    // (set up by cb4c0 from the table at cb5cc). FS cannot carry a base on WOW64, so each
    // of these traps and is completed at base + offset. See docs/re/strt3fx.md.
    for (link, bytes) in SEGMENT_SITES {
        m.patch_segment_site(*link, bytes)?;
    }
    // The engine and sreg lists come from heuristic disassembly of hand-written asm with
    // data tables in between: a site whose relocated bytes do not match is not code and
    // is skipped.
    let mut skipped = 0;
    for (link, bytes) in ENGINE_SEGMENT_SITES.iter().chain(SREG_SITES) {
        if let Err(e) = m.patch_segment_site(*link, bytes) {
            crate::debug!("{e} (skipped)");
            skipped += 1;
        }
    }
    crate::info!("segment emulation: {} sites patched, {skipped} skipped", m.seg_sites.len());
    let mut protect = crate::widescreen::protected(m);
    protect.extend(crate::lod::protected(m));
    crate::segjit::SegJit::init(m, &protect);
    Ok(())
}

/// fs:/gs: table accesses in the 3D asm module (79xxx-7bxxx): fs = projected Z table,
/// gs = X/Y table. 79c8f-79e62 is the near-plane polygon clipper, which also writes
/// clipped vertices back through fs:/gs: (left unpatched it would write into the TEB).
const SEGMENT_SITES: &[(u32, &[u8])] = &[
    (0x79c8f, &[0x64, 0x83, 0x7d, 0x00, 0x00]), // cmp dword ptr fs:[ebp], 0
    (0x79cac, &[0x64, 0x83, 0x7d, 0x00, 0x00]), // cmp dword ptr fs:[ebp], 0
    (0x79cb3, &[0x64, 0x83, 0x3b, 0x00]), // cmp dword ptr fs:[ebx], 0
    (0x79d14, &[0x64, 0x83, 0x3b, 0x00]), // cmp dword ptr fs:[ebx], 0
    (0x79d2f, &[0x64, 0xc7, 0x06, 0x00, 0x00, 0x00, 0x00]), // mov dword ptr fs:[esi], 0
    (0x79d36, &[0x64, 0x8b, 0x55, 0x00]), // mov edx, dword ptr fs:[ebp]
    (0x79d3a, &[0x64, 0x8b, 0x0b]), // mov ecx, dword ptr fs:[ebx]
    (0x79d46, &[0x65, 0x8b, 0x45, 0x04]), // mov eax, dword ptr gs:[ebp + 4]
    (0x79d4a, &[0x65, 0x2b, 0x43, 0x04]), // sub eax, dword ptr gs:[ebx + 4]
    (0x79d55, &[0x65, 0x03, 0x43, 0x04]), // add eax, dword ptr gs:[ebx + 4]
    (0x79d5a, &[0x65, 0x89, 0x46, 0x04]), // mov dword ptr gs:[esi + 4], eax
    (0x79d71, &[0x65, 0x8b, 0x45, 0x00]), // mov eax, dword ptr gs:[ebp]
    (0x79d75, &[0x65, 0x2b, 0x03]), // sub eax, dword ptr gs:[ebx]
    (0x79d7f, &[0x65, 0x03, 0x03]), // add eax, dword ptr gs:[ebx]
    (0x79d83, &[0x65, 0x89, 0x06]), // mov dword ptr gs:[esi], eax
    (0x79e0d, &[0x64, 0xc7, 0x06, 0x00, 0x00, 0x00, 0x00]), // mov dword ptr fs:[esi], 0
    (0x79e14, &[0x64, 0x8b, 0x13]), // mov edx, dword ptr fs:[ebx]
    (0x79e17, &[0x64, 0x8b, 0x4d, 0x00]), // mov ecx, dword ptr fs:[ebp]
    (0x79e24, &[0x65, 0x8b, 0x43, 0x04]), // mov eax, dword ptr gs:[ebx + 4]
    (0x79e28, &[0x65, 0x2b, 0x45, 0x04]), // sub eax, dword ptr gs:[ebp + 4]
    (0x79e33, &[0x65, 0x03, 0x45, 0x04]), // add eax, dword ptr gs:[ebp + 4]
    (0x79e38, &[0x65, 0x89, 0x46, 0x04]), // mov dword ptr gs:[esi + 4], eax
    (0x79e4f, &[0x65, 0x8b, 0x03]), // mov eax, dword ptr gs:[ebx]
    (0x79e52, &[0x65, 0x2b, 0x45, 0x00]), // sub eax, dword ptr gs:[ebp]
    (0x79e5d, &[0x65, 0x03, 0x45, 0x00]), // add eax, dword ptr gs:[ebp]
    (0x79e62, &[0x65, 0x89, 0x06]), // mov dword ptr gs:[esi], eax
    (0x79222, &[0x65, 0x88, 0x0f]), // mov byte ptr gs:[edi], cl
    (0x7923b, &[0x65, 0x88, 0x0f]), // mov byte ptr gs:[edi], cl
    (0x7924b, &[0x65, 0x88, 0x0f]), // mov byte ptr gs:[edi], cl
    (0x7ae8f, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7ae9a, &[0x65, 0x8b, 0x0b]), // mov ecx, dword ptr gs:[ebx]
    (0x7aebd, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7aec8, &[0x65, 0x8b, 0x0b]), // mov ecx, dword ptr gs:[ebx]
    (0x7aeeb, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7aef6, &[0x65, 0x8b, 0x0b]), // mov ecx, dword ptr gs:[ebx]
    (0x7af19, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7af24, &[0x65, 0x8b, 0x0b]), // mov ecx, dword ptr gs:[ebx]
    (0x7af47, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7af52, &[0x65, 0x8b, 0x4b, 0x04]), // mov ecx, dword ptr gs:[ebx + 4]
    (0x7af76, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7af81, &[0x65, 0x8b, 0x4b, 0x04]), // mov ecx, dword ptr gs:[ebx + 4]
    (0x7afa5, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7afb0, &[0x65, 0x8b, 0x4b, 0x04]), // mov ecx, dword ptr gs:[ebx + 4]
    (0x7afd0, &[0x64, 0x8b, 0x03]), // mov eax, dword ptr fs:[ebx]
    (0x7afdb, &[0x65, 0x8b, 0x0b]), // mov ecx, dword ptr gs:[ebx]
    (0x7b028, &[0x64, 0x83, 0x3c, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[eax*2], 0
    (0x7b033, &[0x64, 0x83, 0x3c, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ebx*2], 0
    (0x7b03e, &[0x64, 0x83, 0x3c, 0x4d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ecx*2], 0
    (0x7b049, &[0x64, 0x83, 0x3c, 0x55, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[edx*2], 0
    (0x7b084, &[0x64, 0x83, 0x3c, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ebx*2], 0
    (0x7b08f, &[0x64, 0x83, 0x3c, 0x4d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ecx*2], 0
    (0x7b09a, &[0x64, 0x83, 0x3c, 0x55, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[edx*2], 0
    (0x7b15f, &[0x64, 0x81, 0x3c, 0x45, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[eax*2], 0xffff54e0
    (0x7b16d, &[0x64, 0x81, 0x3c, 0x5d, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[ebx*2], 0xffff54e0
    (0x7b17b, &[0x64, 0x81, 0x3c, 0x4d, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[ecx*2], 0xffff54e0
    (0x7b189, &[0x64, 0x81, 0x3c, 0x55, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[edx*2], 0xffff54e0
    (0x7b1d0, &[0x64, 0x81, 0x3c, 0x5d, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[ebx*2], 0xffff54e0
    (0x7b1de, &[0x64, 0x81, 0x3c, 0x4d, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[ecx*2], 0xffff54e0
    (0x7b1ec, &[0x64, 0x81, 0x3c, 0x55, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x54, 0xff, 0xff]), // cmp dword ptr fs:[edx*2], 0xffff54e0
];

/// Replaces `bf2c8..bf3b3` of Watcom's `cstart_` for DOS/4GW. The original reads the PSP
/// and the environment through DOS/4GW selectors; here the same results are produced in
/// flat memory:
/// * `__Extender` (123a06) = 1 (DOS/4G), `__ExtenderSubtype` (123a07) = 0
/// * the DS slot used by `__GETDS` (bf469) = flat DS
/// * `_Envptr` (123a09, far pointer) = flat environment block
/// * `_psp` (1239e4) = PSP selector
/// * command tail copied to `_end` (rounded to 16), then the program name
/// * on resume: ESI = program name, EDI = end of copied data, EBP = "NO87" present
fn cstart_dos4g(m: &mut Machine, f: &mut HookFrame, _: u32) -> HookResult {
    let l = |a: u32| m.img.link(a);
    unsafe {
        mem::wr8(l(0x123a06), 1);
        mem::wr8(l(0x123a07), 0);
        mem::wr16(l(0xbf469), FLAT_DS);
        mem::wr32(l(0x123a09), m.env_linear);
        mem::wr16(l(0x123a0d), FLAT_DS);
        mem::wr16(l(0x1239e4), m.dos.psp_sel);
    }
    let dest = (l(0x178710) + 15) & !15;
    // empty command tail: "\0", then the program name over the second terminator
    let name = m.program.as_bytes();
    unsafe {
        mem::wr8(dest, 0);
        mem::slice_mut(dest + 1, name.len() as u32).copy_from_slice(name);
        mem::wr8(dest + 1 + name.len() as u32, 0);
    }
    f.esi = dest + 1;
    f.edi = dest + 1 + name.len() as u32 + 1;
    f.ebp = 0;
    crate::debug!("cstart: command line at {dest:#x}, program '{}'", m.program);
    HookResult::Resume(l(0xbf3b3))
}

/// fs:/gs: accesses of the asm polygon engine's near-plane clipper (ced83-cf4xx; reviewed
/// as one contiguous block of code).
const ENGINE_SEGMENT_SITES: &[(u32, &[u8])] = &[
    (0xceda3, &[0x64, 0x83, 0x3c, 0x6d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ebp*2], 0
    (0xcedae, &[0x64, 0x8b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebp*2]
    (0xcede5, &[0x64, 0x83, 0x3c, 0x6d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ebp*2], 0
    (0xcedf4, &[0x64, 0x83, 0x3c, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ebx*2], 0
    (0xcee03, &[0x64, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebx*2]
    (0xcee8c, &[0x64, 0x83, 0x3c, 0x5d, 0x00, 0x00, 0x00, 0x00, 0x00]), // cmp dword ptr fs:[ebx*2], 0
    (0xceea9, &[0x64, 0x8b, 0x14, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov edx, dword ptr fs:[ebp*2]
    (0xceeb1, &[0x64, 0x8b, 0x0c, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov ecx, dword ptr fs:[ebx*2]
    (0xceec1, &[0x65, 0x8b, 0x04, 0x6d, 0x04, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebp*2 + 4]
    (0xceec9, &[0x65, 0x2b, 0x04, 0x5d, 0x04, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebx*2 + 4]
    (0xceed6, &[0x65, 0x03, 0x04, 0x5d, 0x04, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebx*2 + 4]
    (0xcef12, &[0x65, 0x8b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebp*2]
    (0xcef1a, &[0x65, 0x2b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebx*2]
    (0xcef27, &[0x65, 0x03, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebx*2]
    (0xcef83, &[0x64, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebx*2]
    (0xcefe2, &[0x64, 0x8b, 0x14, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov edx, dword ptr fs:[ebx*2]
    (0xcefea, &[0x64, 0x8b, 0x0c, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov ecx, dword ptr fs:[ebp*2]
    (0xceffa, &[0x65, 0x8b, 0x04, 0x5d, 0x04, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebx*2 + 4]
    (0xcf002, &[0x65, 0x2b, 0x04, 0x6d, 0x04, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebp*2 + 4]
    (0xcf00f, &[0x65, 0x03, 0x04, 0x6d, 0x04, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebp*2 + 4]
    (0xcf04b, &[0x65, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebx*2]
    (0xcf053, &[0x65, 0x2b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebp*2]
    (0xcf060, &[0x65, 0x03, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebp*2]
    (0xcf128, &[0x64, 0x8b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebp*2]
    (0xcf138, &[0x64, 0x8b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebp*2]
    (0xcf16f, &[0x64, 0x8b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebp*2]
    (0xcf183, &[0x64, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebx*2]
    (0xcf197, &[0x64, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebx*2]
    (0xcf218, &[0x64, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebx*2]
    (0xcf23a, &[0x64, 0x8b, 0x14, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov edx, dword ptr fs:[ebp*2]
    (0xcf242, &[0x64, 0x8b, 0x0c, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov ecx, dword ptr fs:[ebx*2]
    (0xcf255, &[0x65, 0x8b, 0x04, 0x6d, 0x04, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebp*2 + 4]
    (0xcf25d, &[0x65, 0x2b, 0x04, 0x5d, 0x04, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebx*2 + 4]
    (0xcf26a, &[0x65, 0x03, 0x04, 0x5d, 0x04, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebx*2 + 4]
    (0xcf2a6, &[0x65, 0x8b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebp*2]
    (0xcf2ae, &[0x65, 0x2b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebx*2]
    (0xcf2bb, &[0x65, 0x03, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebx*2]
    (0xcf317, &[0x64, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr fs:[ebx*2]
    (0xcf376, &[0x64, 0x8b, 0x14, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov edx, dword ptr fs:[ebx*2]
    (0xcf37e, &[0x64, 0x8b, 0x0c, 0x6d, 0x00, 0x00, 0x00, 0x00]), // mov ecx, dword ptr fs:[ebp*2]
    (0xcf391, &[0x65, 0x8b, 0x04, 0x5d, 0x04, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebx*2 + 4]
    (0xcf399, &[0x65, 0x2b, 0x04, 0x6d, 0x04, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebp*2 + 4]
    (0xcf3a6, &[0x65, 0x03, 0x04, 0x6d, 0x04, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebp*2 + 4]
    (0xcf3e2, &[0x65, 0x8b, 0x04, 0x5d, 0x00, 0x00, 0x00, 0x00]), // mov eax, dword ptr gs:[ebx*2]
    (0xcf3ea, &[0x65, 0x2b, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // sub eax, dword ptr gs:[ebp*2]
    (0xcf3f7, &[0x65, 0x03, 0x04, 0x6d, 0x00, 0x00, 0x00, 0x00]), // add eax, dword ptr gs:[ebp*2]
];

/// Segment-register saves and restores of the asm routines that switch to emulated
/// selectors (each sequence reviewed in the disassembly), emulated with virtual selector
/// values so `push fs` ... `pop fs` restores them exactly, as the CPU would.
const SREG_SITES: &[(u32, &[u8])] = &[
    (0x50300, &[0x1e]), // push ds
    (0x50308, &[0x66, 0x8e, 0xd8]), // mov ds, ax
    (0x5032a, &[0x1f]), // pop ds
    (0x503b1, &[0x1e]), // push ds
    (0x503b2, &[0x06]), // push es
    (0x503b3, &[0x0f, 0xa0]), // push fs
    (0x503b5, &[0x0f, 0xa8]), // push gs
    (0x503c9, &[0x0f, 0xa9]), // pop gs
    (0x503cb, &[0x0f, 0xa1]), // pop fs
    (0x503cd, &[0x07]), // pop es
    (0x503ce, &[0x1f]), // pop ds
    (0x503de, &[0x1e]), // push ds
    (0x503df, &[0x06]), // push es
    (0x503e0, &[0x0f, 0xa0]), // push fs
    (0x503e2, &[0x0f, 0xa8]), // push gs
    (0x503fa, &[0x1e]), // push ds
    (0x503fb, &[0x07]), // pop es
    (0x503fe, &[0x0f, 0xa9]), // pop gs
    (0x50400, &[0x0f, 0xa1]), // pop fs
    (0x50402, &[0x07]), // pop es
    (0x50403, &[0x1f]), // pop ds
    (0x78d4b, &[0x1e]), // push ds
    (0x78d4c, &[0x06]), // push es
    (0x78d4d, &[0x0f, 0xa0]), // push fs
    (0x78d4f, &[0x0f, 0xa8]), // push gs
    (0x78e15, &[0x66, 0x2e, 0x8e, 0x1d, 0xd8, 0x72, 0x14, 0x00]), // mov ds, word ptr cs:[0x1472d8]
    (0x78e9e, &[0x0f, 0xa9]), // pop gs
    (0x78ea0, &[0x0f, 0xa1]), // pop fs
    (0x78ea2, &[0x07]), // pop es
    (0x78ea3, &[0x1f]), // pop ds
    (0x79ef5, &[0x1e]), // push ds
    (0x79ef6, &[0x06]), // push es
    (0x79ef7, &[0x0f, 0xa0]), // push fs
    (0x79ef9, &[0x0f, 0xa8]), // push gs
    (0x79efb, &[0x66, 0x2e, 0x8e, 0x1d, 0xd8, 0x72, 0x14, 0x00]), // mov ds, word ptr cs:[0x1472d8]
    (0x7a000, &[0x0f, 0xa9]), // pop gs
    (0x7a002, &[0x0f, 0xa1]), // pop fs
    (0x7a004, &[0x07]), // pop es
    (0x7a005, &[0x1f]), // pop ds
    (0x82f11, &[0x1e]), // push ds
    (0x82f12, &[0x06]), // push es
    (0x82f13, &[0x0f, 0xa0]), // push fs
    (0x82f15, &[0x0f, 0xa8]), // push gs
    (0x82f17, &[0x66, 0x2e, 0x8e, 0x1d, 0xd8, 0x72, 0x14, 0x00]), // mov ds, word ptr cs:[0x1472d8]
    (0x82f7b, &[0x0f, 0xa9]), // pop gs
    (0x82f7d, &[0x0f, 0xa1]), // pop fs
    (0x82f7f, &[0x07]), // pop es
    (0x82f80, &[0x1f]), // pop ds
    (0x82f95, &[0x1e]), // push ds
    (0x82f96, &[0x06]), // push es
    (0x82f97, &[0x0f, 0xa0]), // push fs
    (0x82f99, &[0x0f, 0xa8]), // push gs
    (0x82f9b, &[0x0f, 0xa9]), // pop gs
    (0x82f9d, &[0x0f, 0xa1]), // pop fs
    (0x82f9f, &[0x07]), // pop es
    (0x82fa0, &[0x1f]), // pop ds
];

/// Observer on the HUD drawing (4e224): from here to the swap the frame draws over the main
/// view's world; its camera is the main camera.
fn hud_entry(m: &mut Machine, _f: &mut crate::cpu::HookFrame, tramp: u32) -> crate::hooks::HookResult {
    crate::interp::capture_camera(m);
    if let Some(g) = m.glide.as_mut() {
        g.hud_recording = crate::interp::enabled();
    }
    crate::hooks::HookResult::Resume(tramp)
}

/// Deterministic mode: the game's tick wait (504dc, `while ([50512] < n)`, n set per timer)
/// gets exactly the ticks it waits for, at once.
fn tick_wait(m: &mut Machine, _f: &mut crate::cpu::HookFrame, tramp: u32) -> crate::hooks::HookResult {
    let counter = m.img.link(0x50512);
    let target = unsafe { crate::mem::rd8(m.img.link(0x504e4)) } as u32;
    if let Some(s) = m.sched.as_ref() {
        s.virtual_ticks_until(|| unsafe { crate::mem::rd32(counter) } as i32 >= target as i32, 1000);
    }
    crate::hooks::HookResult::Resume(tramp)
}
