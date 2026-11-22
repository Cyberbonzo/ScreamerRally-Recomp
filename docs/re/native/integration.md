# Native renderer: integration (wave 2, topic W2-3)

How the runtime puts the native renderer into a race: the switch, the scene of each race,
the hooks that draw a native view where the game draws its world, the `FrameData` built
from the game's own camera structures, the visibility rule, interpolated frames, and what
was verified and measured. Link addresses as in `docs/re/strt3fx.md`.

Status: working in races on Rally China with the real scene (`extract::build_scene`, W2-1)
and the W2-2 renderer: hybrid frames (native track and scenery, the game's cars, shadows,
smoke and tyre marks through the Glide path in the same depth buffer), main view and
rear-view mirror, chase and bumper cameras, interpolated frames. Off by default; with the
switch off every frame is identical to the build without this work (below).

## Files

| File | What |
|---|---|
| `crates/sr-runtime/src/native/frame.rs` | The switch, `NativeState` (`Machine::native`), the scene per race, `GameCamera` / `ViewMap` and the pure functions `camera()` and `sky()`, the per-view entry points `skip_sky` and `draw_world_view`, statistics, `SR_NATIVE_PROBE`; unit tests against `transforms.rs` |
| `crates/sr-runtime/src/native/visibility.rs` | The visibility rule: `GameCells` (the game's cells), `StaticCells::targets`, `on_screen`, the `Fader`; unit tests |
| `crates/sr-runtime/src/glide/mod.rs` | `Backend::native_scene / native_frame / native_view` (default: refused / no-op), `NativeViewStats`, `Glide::flush` |
| `crates/sr-render/src/d3d.rs` | The D3D backend forwards the three methods (three one-line methods in `impl Backend`, the bodies in their own `impl D3dBackend` block at the end of the file) |
| `crates/sr-runtime/src/visibility.rs` | The observer at `7a531` also draws the native view and empties the engine's cell collection (`game_cells`) |
| `crates/sr-runtime/src/interp.rs` | Extra frames hand the native views their cameras in floating point |
| `crates/sr-runtime/src/widescreen.rs`, `profile/strt3fx.rs`, `native/mod.rs`, `lib.rs` | One call each: race loaded (set_far), menus (menu frame timer), install / race frame, the state |
| `apps/screamer/src/settings.rs`, `menu.rs`, `main.rs` | `graphics.renderer`, the F12 item, the start-up value |
| `crates/sr-render/src/overlay.rs` | The F12 list scrolls to the selected row (the Graphics tab had more rows than fit: two were already hidden before this item) |

## The switch

* `graphics.renderer = glide | native` in `settings.ini` (default `glide`; the comment in the
  file says experimental). `SR_NATIVE=1` (or `on`, `native`) forces native, `SR_NATIVE=0`
  (or `off`, `glide`) forces the Glide path, whatever the setting.
* F12 overlay, Graphics tab: "Renderer (experimental)": Original (Glide) / Native
  (experimental), saved at once; it takes effect at the next race (decided once per race, at
  its first race frame).
* Verified: `renderer_switch` (settings round trip, invalid value warned); a run with
  `renderer = native` in the file built the scene twice (attract race, race), the same with
  `SR_NATIVE=0` built none; the overlay scripted (`F12, PGDN, DOWN x 10, RIGHT, LEFT`):
  the item shown selected with "Native (experimental)" and "Saved", the file written
  (`renderer = glide` again after LEFT) (screenshots `OV`, not kept).

## The backend

```rust
// crates/sr-runtime/src/glide/mod.rs, trait Backend (defaults: no native renderer)
fn native_scene(&mut self, scene: &SceneData) -> Result<(), String>   // default Err
fn native_frame(&mut self, frame: &FrameData) -> Result<(), String>   // default Ok
fn native_view(&mut self, frame: &FrameData, view: usize) -> Result<NativeViewStats, String>
```

The D3D backend forwards them to `native_upload` (and logs its statistics),
`native_begin_frame` and `native_draw_view`. Backends without a renderer (`NullBackend`)
refuse the scene, so the runtime stays on the Glide path. The renderer flushes after view 0
of every `FrameData` (`flush_after_main`, W2-2); there is no separate flush call.

## Frame flow

| When | Where | What |
|---|---|---|
| start-up | `native::install` | observer on the sky routine `7b218` (always installed: it does nothing while the native renderer is off) |
| race set-up | set_far observer (`widescreen.rs`), called from `37cdf` | marks a newly loaded race (the world key, row table `[123720]` and model table `[147390]`, is a fallback) |
| first race frame | race frame timer observer `4f793` -> `native::race_frame` | if asked for: `extract::build_scene`, `Backend::native_scene`; `None` or an error: logged, the Glide path draws this race |
| menu frames | menu frame timer observer | `in_race = false`: menus are never drawn natively |
| each view: sky | observer at `7b218` (first call of the world drawing `78d84` / `78dbc`, after set_view's view start) | skipped (`Return`); `Glide::race_frame` set as the sky's quads would have (the HUD anchoring and the car part ids depend on it) |
| each view: cells | the observer at `7a531` (after the cell set-up `7a2e4` in `7a52c`) | the native view is drawn now (`draw_world_view`); then `7a52c` returns with `edi` = the object list's base (the immediate at `7a53d`, moved by `widescreen.rs`) and `[79868]` = 0, so `81fbc` appends only the scene table's instances (cars, shadows, smoke, tyre marks), which the engine draws through the Glide path, depth tested against the native world |
| after the views | (unchanged) | HUD, mirror frame, 2D: Glide path, on top |

`draw_world_view` reads the view the 3D module is set up for (`ViewMap::read`: projection
centre immediate `[7ab1a]` / `[7ac1e]`, `[81e3c]`, `[81e50]`, `[81e24]`, the mirror bit
`[aa5b0]`, the x and y tables and offsets `[aa6c4]`, `[aa6b8]`, `[aa6bc]`, `ac6cc`), the camera
structure `[81e0c]` (or the interpolated one), the far distance `[cf474]`, the game's cells,
builds one `FrameData` with one view, flushes the batched Glide triangles, calls
`native_frame` and `native_view(frame, 0)`. The mirror is drawn later in the frame with its
own `FrameData` (its camera is updated by the game only after the main view: `236dc` for
view 2 runs after the HUD), so a game frame makes two `begin_frame` calls.

`FrameData`: `views` = the one view; `static_fade` (below); `clock` = `[1473a0] & 3FFFh`;
`env_phase` = the player's car instance +3Ch (the first `auto*` entry of the scene table);
`dynamics` and `meshes` empty (wave 3).

## Cameras and projections

The game: `q = Rc^T (p - pc)` (x left, y up, z back), grid `X = C + 256 q.x / (q.z - 128)`,
`Y = cy + f q.y / (q.z - 128)` (f = 256, mirror 128; the mirror's x arrives negated), Glide
`x = x_table(x_off + X)`, `y = 1.2 (y_off + Y)` (`transforms.md`). The contract's view space
is x right, y down, z forward, so:

* `Camera::pos` = the eye `pc + 128 Rc[:, 2]` (`sr_scene::EYE_OFFSET`), rows of `rot` =
  `-Rc[:, k]` (in coordinates a reflection: the game's world is left-handed);
* view z = 128 - q.z = the game's w (the depth both paths write);
* `Projection::center` = the Glide position of the grid centre (`(319, 1.2 cy)` in the main
  view at any widescreen extension), `scale` = `(256 sx, 1.2 f)` with sx the x table's slope;
  `near` = 128 (view z' = 0), `far` = D + 8192 + 128 (D = set_far's distance, `[cf474]`,
  with the draw distance setting: 150 000 on China at 300 %);
* `viewport` = the game's own 640 grid columns (main view `[0, 0, 640, 480]`, the renderer
  widens it by its extension like the Glide path's clip windows; mirror
  `[161, 33.6, 481, 117.6]`). (A first version gave the widened rectangle: the renderer then
  spread the sky's `s` over 854 columns and the sky slid; seen in a frame comparison and
  fixed.)
* mirror: `View::mirror = true`; the renderer flips x about the viewport's centre, so the
  unflipped centre is `x0 + x1 - centre` = 321.5 (the game's 320.5 after the flip).

Verified by unit tests (`cargo test -p sr-runtime native::frame`): for 4 cameras (chase
camera on China, zero angles, a far one, fractional angles and positions) and 6 points each
(5 in front at 10 to 45 000 units, one behind the near plane), the contract's camera and
projection as the renderer applies them (mirror flip included) give the same Glide position
and depth as `transforms.rs`'s float model of the engine (`world_to_view`, `project`, the
view's tables) within 1e-3 px + 2e-6 of the depth (the f32 rotation rows), and the same
visibility, for the race main view (extension 0 and 107), the menus (0 and 213) and the
mirror; the eye, near and far; view space right / down / forward. Negative control: with
the mirror's centre left unflipped the mirror test fails.

## Sky

`View::sky` from the camera with `7b218`'s mapping (`polygons.md`), continuous in the
camera's floating-point angles (fix of 2026-11-21, below): s0 = ((2047 - heading) mod
1024) / 4 (256 texels per 180 degrees of heading) and s0 + 159 (swapped in the mirror),
horizon row h = 16384 sin(pitch) / 128 + cy + 40 - 512 + 2 (the game's `[798ac]` / 128),
t = (|h| / 2, 255 - te), ground colour `[81e1c]` 5:6:5 -> 888 (shifts: 4288h -> 405040h)
when the horizon is above the view's bottom; `[123740]` = 0: `View::clear` with `[123744]`
instead.

**The vibration ("the background vibrates at the start of some races, like Sweden, or when
the car vibrates").** Wave 2 used the game's integer arithmetic for whole-unit cameras
(`[798ac]` = the sine table's entry >> 1, `((-54152 s) >> 7) / -54152` truncated towards
zero, whole rows) and the float formula only for fractional ones. The two differ by up to a
row (the truncation; h is about -248 for China's chase camera: -247 against -247.91). With
interpolated frames on, a frame's camera is whole-unit at every step's own time and in every
frame between two steps with the same camera, fractional otherwise: so whenever the camera
moved in one step and not in the next (the camera settling at a race start, a shaking car,
a camera turning by one unit now and then) the horizon jumped by up to a row (1.2 pixels at
480 lines, 1.8 in a 720-line window) and back, several times a second. Now every frame takes
the float formula (`frame::sky`); the game's integer horizon (`game_horizon_row`) stays for
the tests and the diagnostics. A behaviour change of the native renderer, also without
interpolated frames: its sky is no longer pixel-identical to the Glide path's at the game's
own frames (wave 2's frame comparisons), but within one row of it (the game's truncation).

Verified (`SR_NATIVE_SKY_LOG=1`: per main view the camera, s0, t0, the horizon drawn and, for
a whole-unit camera, the game's integer horizon, which is what the earlier code drew;
deterministic, autopilot, native, `SR_INTERP=1 SR_INTERP_STEPS=3`, the horizon in pixels at
480 lines, per presented frame):

| | presented frames | change per frame: mean / max | second difference: mean / max |
|---|---|---|---|
| Rally Sweden start (frames 1540-1700, bumper camera), before | 483 | 0.358 / 1.627 | 0.677 / 2.783 |
| Rally Sweden start, now | 483 | 0.055 / 0.471 | 0.031 / 0.471 |
| Rally China (frames 1540-1800, the camera shaking with the car), before | 783 | 0.373 / 1.728 | 0.664 / 2.827 |
| Rally China, now | 783 | 0.105 / 1.414 | 0.048 / 1.727 |

(The remaining maxima are the camera's own moves: pitch steps of several units.) Sweden after
GO, the car starting off (presented frames, alpha = position between two steps):

| game frame, alpha | pitch | horizon before | now |
|---|---|---|---|
| 1553, 1/3 | 3.333 | 362.771 | 362.771 |
| 1553, 2/3 | 3.667 | 362.928 | 362.928 |
| 1553, 1 | 4 | 362.400 | 363.085 |
| 1554-1555 (camera still) | 4 | 362.400 | 363.085 |
| 1556, 1/3 and 2/3 (heading 1454.7, 1454.3) | 4 | 363.085 | 363.085 |
| 1556, 1 | 4 | 362.400 | 363.085 |
| 1558, 1/3, 2/3, 1 | 4.333, 4.667, 5 | 363.242, 363.399, 362.400 | 363.242, 363.399, 363.556 |

Same mapping: unit test `sky_as_the_game_computes_it` (the chase camera, the mirror, a
clamped horizon by hand; for every whole pitch 0-2047 in the race view, the menus and the
mirror the continuous horizon is within 1.01 rows of the game's integer one; a hundredth of
an angle unit moves it by less than 0.01 pixels). s0 is unchanged (no quantisation in it).
Frames otherwise as before (China, presented frames 3800-3830 against the previous build):
in the frames where the earlier code had already used the float formula the only differing
pixels are the player's car's wheels (2-2 088 pixels in x 648-936, y 480-581; the wheel fix
in `dynamics.md`); in the others 75 000-77 700 pixels (8.2-8.4 %, mean difference 9.4-10.6)
in y 0-459, the sky. The Glide path is unchanged (`SR_NATIVE=0`, menus, attract race and the
race with the mirror, frames 0-1900: 20 of 20 byte-identical to the previous build).

## Visibility

Superseded in races by the rule "backed by the world" (`visibility.md`, after the user's
test); the menus' scenes and `SR_NATIVE_VIS=lists` keep the rule below.

The rule (decision of 2026-11-21 in `docs/04-native-renderer.md`), per view, per simulation
step (`native/visibility.rs`):

1. **The game's cells**: the cells `7a52c` would collect now: its visibility list as
   `crate::visibility` leaves it (original or extended per `graphics.visibility`; with
   "everything", or a camera cell without a list, every cell of the box) **inside the frustum
   box** (`[7a8ac] >> 16`, `[7a8b0]`, `eax + 1` rows, `[7a5cb] + 1` columns: the loops at
   7a556 / 7a5c6). Every static of those cells is drawn.
2. **Grounded statics** (`StaticMeta::grounded`, W2-1: 946 of 2 260 on China) whose
   bounding sphere touches the view frustum (left, right, top, bottom from the projection
   and the widened viewport; near 128; far D + 8320) are drawn too.
3. **Fades**: a static whose state changes while its sphere is in the view fades over
   `FADE_STEPS` = 8 simulation steps (255 / 8 per step, about 0.24 s at 33 steps per second;
   the renderer dithers, W2-2); one outside the view takes its state at once (nobody sees
   it, and statics entering the view from its sides are already there instead of fading in).
   The first step of a race starts at the targets. Extra frames show the value between the
   last two steps at their alpha (a static that switched at once keeps its value). Fades are
   kept per view (main, second player, mirror) and advance only on the game's own frames.

Why these choices: (1) the box is part of what the game draws (a listed cell outside it is
not collected); (2) the frustum test is what "within the view" means for statics beyond the
game's cells; (3) fading only on screen avoids both popping and statics fading in at the
screen's edges when the camera turns.

Verified: unit tests `listed_cells_and_grounded_statics` (list inside / outside the box,
grounded in view / behind / beyond far, the box without a list, the game's cells only),
`frustum_follows_the_widescreen_extension`, `fades_on_screen_snaps_off_screen` (steady
state stays, on-screen fade, off-screen snap, interpolation, 8 steps). In races (statistics
every 5 s): 388 to 517 of 2 260 statics drawn per view, 0.7 to 40 of them mid-fade on
average. `SR_NATIVE_GROUNDED=0` (diagnostics) draws the game's cells only.

## Interpolated frames

`interp.rs` keeps interpolating the game's camera in whole units for the game's own drawing
(cars), and now also hands the native views the same camera in floating point
(`GameCamera::lerp`: angles the short way round, positions linear; the mirror's from its two
steps, the step's own after a cut) and the alpha (`set_camera_override` before the guest
calls `349b8(0)` / `349b8(2)`, cleared after). The native world thus moves smoothly while the
cars still carry the whole-unit camera (at most half a unit and half an angle unit apart;
cars move to native in wave 3). Verified: deterministic run with `SR_INTERP=1
SR_INTERP_STEPS=2`: the extra frames at alpha 0.5 and 1 of game frame 1659 drawn native
with the mirror redrawn live (`interp_cmp.png`, statistics below); `interpolated_cameras_take_the_short_way`.

## Verification in races (Rally China, deterministic)

Runs: `SR_DETERMINISTIC=1 SR_AUTOPILOT=1`, keys `12:ENTER,16:ENTER,20:ENTER,24:ENTER,
28:ENTER,32:ENTER,38:M,42:F1,47:F4 --seconds 52`, window 1280 x 720 (widescreen extension
107), draw distance 300 %, visibility extended, `SR_SHOT=<dir>,50,200,2250` (38 frames: the
attract-mode race, menus, the race with the default chase camera, the mirror from about frame
1700, the bumper camera F1 from about 1830, the far chase camera F4 from about 1990). Logs,
the hybrid and Glide frames and the comparison images are in `work/agents/integration/`
(`R1` Glide, `H1` native, `real_*.png` = original | hybrid | difference x 4, half size).

* **Switch off: no change.** The build of `main` (01f48db, the base after merging W2-2) and
  this branch with the switch off: 38 of 38 frames byte-identical (SHA-256). After merging
  W2-1 (d20862d) the switch-off run `R1` again matched those 38 frames.
* **Switch on**: the 21 menu frames identical to the Glide run; the 17 frames with 3D races
  differ, as they should. Every view drawn without error (0 errors in the statistics).
* **Hybrid frame next to the original** (pixel statistics on every third pixel; "control" =
  two original extra frames half a simulation step apart, for scale):

  | frame | camera | mean abs. difference | pixels > 16 | pixels > 64 |
  |---|---|---|---|---|
  | 300 | attract race | 8.5 | 28.5 % | 2.2 % |
  | 1700 | chase, mirror | 7.5 | 20.2 % | 2.9 % |
  | 1800 | chase, mirror | 11.1 | 35.9 % | 3.8 % |
  | 1900 | bumper (F1), mirror | 10.3 | 34.1 % | 3.2 % |
  | 2050 | far chase (F4), mirror | 14.3 | 35.7 % | 7.9 % |
  | 1659 extra, alpha 0.5 | chase, interpolated | 8.9 | 23.0 % | 3.5 % |
  | 1659 extra, alpha 1 | chase, interpolated | 7.1 | 17.9 % | 2.9 % |
  | control | | 13.5 | 35.0 % | 5.8 % |

  The difference panels: sky black (identical), the Glide-drawn cars black (identical
  pixels: not hidden or shifted by the native world), HUD and mirror frame black; the world
  differs at texel and polygon edges (the native vertices are the true float positions, the
  engine's carry its Q14 and whole-unit rounding, up to 2.7 grid pixels near the camera,
  `transforms.md`); no object seen missing. Frame 2050 (far chase camera) differs more: see
  "Findings".
* Depth agreement Glide <-> native: in the difference panels of 1700-2050 and of the extra
  frame the cars are black (identical pixels), so the Glide-drawn cars are tested against the
  native world as against the game's own (W2-2 showed the depth values agree bit for bit).
  Shadows, smoke and tyre marks are blended over road texels that differ slightly, so this
  comparison cannot show them exactly; they are present in the hybrid frames where the
  original has them. Not looked for separately: a car partly behind a wall.

## Measurements

Machine: i9-10900K, RTX 3090, Windows 11, 1280 x 720 window, release build; real time,
autopilot, VSync off, interpolated frames on (`SR_INTERP=1`), mirror on from 38 s; frame
rate = the backend's "presents per second" over windows of 500 presents (`--log debug`).
No other game instance was running.

| | Glide path | Native world (hybrid) |
|---|---|---|
| race frames per second, median of the windows | 147 (7 windows, 115-218) | 550 (32 windows, 456-856) |
| before the mirror is switched on (38 s) | 218 (1 window, with the race start) | 715-856 (7 windows) |
| with the mirror | 115-180 (6 windows) | 456-608 (25 windows) |
| extra frame median / 95 % | 3.2 / 5.6 ms | 1.3 / 1.6 ms |
| extra frames in the run | 6 151 | 19 409 |

Per native view (CPU, statistics every 5 s): building the `FrameData` 0.031-0.033 ms,
`native_frame` + `native_view` 0.127-0.140 ms; 52-93 draws, 11 600-20 700 triangles.
Race start: `build_scene` 113-126 ms + upload 52-106 ms in the first race frame (the
longest game frame of the run, 177 ms).

## Findings

* **Objects close to the camera that the game does not show.** With the far chase camera
  (frame 2050) a dark terrain surface covers the road at the bottom right and a dark strip
  the end of the wall at the right in the native frame. They are statics of the game's own
  cells (they stay with `SR_NATIVE_GROUNDED=0`), drawn where their geometry is; the probe
  (`SR_NATIVE_PROBE=2050`) shows the nearest ones at view depth 213-750, i.e. right beside
  the camera. Not explained yet: likely the engine dropping or clipping polygons with
  vertices next to its near plane (its integer projection overflows there, `transforms.md`
  counted 91 such vertices; a polygon with one is clipped as if it were behind). If so the
  native frame shows the true geometry and the original looked cleaner by accident. To be
  decided (organiser / W2-2): keep, or clip like the game.
* **The wave-1 renderer drew every static** (no `static_fade`): with it the first hybrid
  frames showed backdrops and trees the game's lists exclude, covering the sky. With W2-2's
  renderer (fades honoured) they are gone. A useful check that the rule matters.

## Unknowns

* The cause of the near-camera differences above (not traced in the engine).
* Split screen (two players: views 0 and 1, `34af4`): coded (fades per view, the view read
  from the game), not run.
* Menus' 3D scenes: not drawn natively (no scene for them yet); the projection for their
  centre row is unit-tested only.
* Other tracks, replays: not run.
* `env_phase` is the player's car's value for every object (China's reflection map has one
  frame).
* The pause screen: assumed to run on the menu frame timer (then a paused race is drawn by
  the Glide path until it resumes); not looked at (deterministic runs stop at the pause menu).
* `SR_POLY_CHECK` installs its own observer at `7b218`; with it the native renderer is
  unavailable (logged).
* The interpolation's step-gap test uses wall time (`MAX_STEP_GAP`), so the scene build at the
  first race frame (about 0.2 s) leaves that step uninterpolated even in deterministic runs:
  the present count shifts by one against a Glide run (game frames stay aligned).

## Proposed contract changes

* `FrameData` documented as "the views drawn next" rather than "main, mirror": the mirror's
  camera exists only after the main view is drawn, so each view comes with its own
  `FrameData` (one `begin_frame` per view). Alternatively a `Backend` / renderer call that
  replaces only the views (and fades) of the current frame without re-sending transforms.
* With one view per `FrameData`, `flush_after_main` also flushes after the mirror (cheap,
  measured 0.04 ms by W2-2); a flag in `View` ("main view") would make it exact.
* `View::viewport` documentation: say explicitly that a widened main view is given as the
  game's 640-column rectangle and that the sky's `s` refers to it.

## Diagnostics

* `SR_NATIVE_SKY_LOG=1`: the sky of every main view (camera, s0, t0, horizon, and the game's
  integer horizon for a whole-unit camera).
* `SR_NATIVE=1|0`: switch override. `SR_NATIVE_GROUNDED=0`: the game's cells only.
  `SR_NATIVE_PROBE=<game frame>`: every static within 30 000 units of that frame's main
  camera, with cell, grounded, view position, screen position and fade.
* Log: `native renderer: first view ...` (camera, eye, projection, viewport, far, cells, statics
  drawn) once per race; every 5 s: views, build and submit time per view, draws, triangles,
  statics drawn and fading, errors.
* Interim test scene: before W2-1 was merged a temporary reader (`native/interim.rs`,
  `SR_NATIVE_SCENE=interim`) built a scene for these tests; removed after the merge.
