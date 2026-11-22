# Native renderer: the menus' 3D scenes (wave 4, topic W4-2)

With the native renderer on (`SR_NATIVE=1` / `graphics.renderer = native`), the menus' 3D
scenes are now drawn by the native renderer from the game's data (the name entry's letters,
the main menu's carousel with its background and world, the car and transmission
selection's turntable, the championship's league selection and garage), with the menus'
2D (pictures, texts, boxes, fades) on top through the Glide path, widescreen as before.
With interpolation on, the menus get frames in between the game's 50 steps per second,
drawn without running guest code: the menus run at the display's rate with VSync, or as
fast as allowed (or at the frame rate limit) without. The menu logic keeps its 50 steps.
With the switch off nothing changes (45 of 45 frames byte-identical, below). Link addresses
as in `docs/re/strt3fx.md`; numbers say how they were obtained.

## Files

| File | What |
|---|---|
| `crates/sr-runtime/src/native/menus.rs` | New. The hooks (`78d10`, `78d56`, `78d84`, the menu frame's start `4f6f1`), the menu scene per key, the extra frames (recording marks, steps, pacing loop, `clear_frame`), `SR_MENU_PROBE`; unit tests |
| `crates/sr-runtime/src/native/frame.rs` | `NativeState`: `menu`, `menu_drawing`, `menu_extra`, `menus`; `drawing()` for a menu scene; `start_race` split into `activate` (shared with the menus); `draw_world_view` / new `draw_objects_view` over one `draw_view` (objects only: no sky, no statics); extra frames of a menu count as extra; a menu view not widened by the game keeps to the 4:3 columns; `race_frame` drops a menu scene |
| `crates/sr-runtime/src/native/extract.rs` | `build_menu_scene(m, world)` (via `build_with`: the grid only with a world, the background picture only with a world, part meshes of every model the cells do not use, nothing grounded) |
| `crates/sr-runtime/src/native/world.rs` | `read_world_with(m, r, cells)` (`read_world` = with cells) |
| `crates/sr-runtime/src/native/dynamics.rs` | One loop: models with part meshes but no scene-table entry are `Kind::Other` (the menus' objects) |
| `crates/sr-runtime/src/glide/mod.rs` | `Glide::recorded_ops()`; at the buffer swap the menus' extra frames are asked for after the races' (`native::menus::extra_frames` / `fill`) |
| `crates/sr-runtime/src/interp.rs` | `fps_cap()` getter |

## How the menus draw 3D

Found with `SR_MENU_PROBE=1` (observers on `78d10`, `78d84`, `82338`, `349b8`: callers,
camera, instance list, models, libraries) and Glide traces (`SR_TRACE_EVERY`) with shots of
the same frames:

* The menus set up their camera in the structure at `144fb0` (`82338(eax = 144fb0)`, angles
  and position written by the menu code just before) and draw through the engine's own
  entries with the instance list `12f478` (instance A pointers ending with 0, bit 30 =
  not drawn, as in a race). **The menus have no scene table** (`12f860` is empty): the
  list's instances are the menu's objects, each with its instance B (per-part placement), so
  they are placed exactly like a race's scene-table instances (`dynamics.md`).
* Two entries:
  * `78d10(eax = list)`: no sky, no cells: `[79868]` = 0 and the instance list only (its
    set-up: `[7bb7c]`, `[aa5b0]` = 0, x table `ae6cc`, `7a158`, then `81fbc`);
  * `78d84(eax = list)`: sky (`7b218`), cells (`7a52c`) and the list, like a race view
    (the race calls it from `349b8`, return address `34a92`).
* `set_view` is called once when a menu starts (`4afb4`: 320, 400, centre row 200), not per
  frame; `set_far` with 43808 (131424 at 300 % draw distance).

| Menu | Call (return address) | Camera (game frame of the probe) | What | Libraries, model table |
|---|---|---|---|---|
| name entry ("Inserimento Nome") | `78d10` (46b01) | (0, 1000, -3200), angles (10, 1024, 0) | the letters (one model per letter: `aa` .. `az`, `adel`, `aenter`) over a 640 x 480 picture | `logo`, 28 models |
| main menu ("Selezione Principale") | `78d84` (42061) | (-53958, 819, 1261), angles (1990, 1821, 0), turning between the items | the carousel (`a1obj` .. `a5obj`, `a1coppa` ..: cabinet, trophy, stopwatch, computers, wrench) over the world `map1` (5 cells, 57 objects: a garage) and the background picture as the engine's sky (blue "RALLY" pattern; sky on, `[123740]` = 1) | `logo`, `map1`, 67 models |
| track selection | (2D only) | | picture and its animated preview | |
| car selection, transmission | `78d10` (47685) | (0, 800, -1100), angles (1834, 1137, 0) | the turntable: the selected car (`MV_mauto`, 12 parts, instance B with the wheels' offsets) at the origin turning 2 angle units per step, the others parked at x = 10000 | `env`, `auto1` .. `auto5`, `logo`, `map1`; 72 models |
| championship league selection | `78d10` (454c7) | | a car behind the league panels | 204 textures, 90 meshes |
| championship car selection (garage) | `78d84` (44044) | | the garage world `map1` with the cars and mechanics | 263 textures, 384 meshes |

Not reached in the runs (other menus with 3D calls, handled by the same code, not
checked): `78d10` from 42330, 45e65, 4a72a and `78d84` from 425ab, 44d9a.

The intro's title over the attract race is a race (race frame timer, `349b8`, the title and
credits as 2D): drawn natively by the race path since wave 2; the attract mode without
input continues that race with credits (camera changes), also the race path (checked:
`idle2`, one scene build, every frame native). No other attract demo appeared after 90 s
idle in the main menu.

## Native drawing

* **Scene per menu** (`begin_draw`, at each menu call of `78d10` / `78d84`, never in race
  frames nor for the race's `349b8`): a key over the model table and its model pointers,
  the loaded libraries (slot, data block, name) and, with a world, the grid's row table and
  the background picture (its `GrTexInfo`, TMU address and every 509th byte of its texels).
  A new key: `extract::build_menu_scene` and `frame::activate` (as a race's scene); the
  previous scene (a race's or a menu's) is dropped and the next race builds its own again
  (`race_frame`). Measured (deterministic runs, release build): name entry 2.4 ms + upload
  7.4 ms (69 textures, 2 materials, 28 meshes, 1 892 triangles), main menu 8.6 + 15.4 ms
  (140 textures, 105 materials, 57 statics, 10 object models), car selection 12.8 +
  18.2 ms (262 textures, 196 materials, 192 meshes), league selection 21.7 + 16.0 ms,
  garage 18.8 + 18.6 ms. One build per menu in every run (switching cars, items and
  letters does not change the key).
* **Scene contents** (`build_menu_scene`): every texture record in use (the menus' textures
  are records like a race's, `textures.md`); with a world (`78d84`), its cells' statics
  exactly as a race's and the background picture as the sky texture, **none grounded**: the
  menus show the game's own cells only (a grounded garage wall beyond the game's lists
  would appear behind the carousel); and part meshes (every slot and level) of every model
  the cells do not use, `Kind::Other`: back faces culled like scene instances, mode-7
  decals pulled 120, blended materials under the overlay rules of moving objects
  (`dynamics.md`). The Glide path treats everything in the menus with the car rules
  (`strt3fx.md`); natively every menu object is a moving instance, which is the same.
* **Per draw**: `78d84` goes through the race's hooks (`drawing()` is true while a menu
  scene draws): the sky routine skipped (`skip_sky`, which sets the Glide layer's race
  flag as the sky's quads do on the Glide path, so the menus' 2D is anchored as before),
  the native view after the cell set-up (`7a531`, `draw_world_view`: camera from
  `[81e0c]` = `144fb0`, view constants as `ViewMap::read`, centre row 200, the sky from
  the camera, the game's cells), the game's objects skipped (`81fbc`). `78d10`: the native
  view at `78d56` (after its set-up; `draw_objects_view`: no sky or clear, no statics),
  the game's objects skipped at `81fbc` (`native::dynamics`'s observer).
* **Widescreen**: the menus' 3D widens as today (`display.wide_menus` on: the game's view
  is 640 + 2e wide, the renderer widens the 640-column rectangle); with `wide_menus` off the
  game's view stays 640 wide and the native view gets `[0, 0, 639.8, 480]` (the Glide path's
  x table ends at 639.8), so it stays in the 4:3 area like the Glide path
  (`unwidened_menu_views_keep_to_the_game_columns`; frames below). 640 x 480 pictures keep
  their black sides (the backend clears them at each swap).

## Extra frames (interpolation)

The race's extra frames call the game's world drawing again with guest memory put back
afterwards (`crate::interp`). The menus draw their 3D in the middle of their own frame code,
so their extra frames run no guest code at all:

1. **Recording** (`frame_start`, observer at `4f6f1`, the end of the menu frame timer's tick
   wait): with a menu scene native and interpolation on, the Glide layer records everything
   the frame draws (its HUD recording: triangles with their state, LFB images) from the
   frame's start; each native view of the frame marks its place in the recording
   (`view_drawn`: the number of ops before it, `78d10` or `78d84`, the game camera).
2. **At the swap** (`extra_frames`, called in the swap after the races' `interp::active`):
   a recorded frame with native views is not presented; the step is kept (start time, game
   frame, views).
3. **Fill** (`fill`): when the previous step is the frame before (`b.frame == a.frame + 1`,
   at most 0.1 s apart, the same views, no camera cut: 3000 units or 140 angle units), frames
   are drawn until the next menu frame is due (2 ticks: 20 ms after the step's start, minus
   1.5 ms), each at `alpha` = its expected display time after the step / the step period,
   shown one step late like the races': the frame cleared (black, depth), the recorded ops
   replayed up to each view's mark, the native view drawn with the camera interpolated in
   floating point (`GameCamera::lerp`) and the objects interpolated by `native::dynamics`
   (positions, angles the short way, instance B's part placements; instances jumping more than
   4000 units shown at the step), then the rest of the ops. VSync: each frame waits for the
   display (`wait_ready`); frame rate limit: `interp::fps_cap()`; otherwise as fast as
   drawn. Not consecutive: the game's frame is presented. `SR_INTERP_STEPS=n`: exactly n
   frames per step at 1/n .. 1 (diagnostics; then the wall time between steps is not
   tested, the shots themselves take longer than 0.1 s).

Pictures and 2D animations (the car selection's turning floor is a pre-rendered animation)
stay at the game's steps: the 3D moves between them. The floor animation does not advance
every step even on the Glide path (measured: its region changed in 7 of 12 consecutive game
frames, by 2.9 to 27.6 mean levels), so it is not tied to the car's 2 units per step.

## Verification

Runs: `SR_DETERMINISTIC=1 SR_AUTOPILOT=1`, data `work/agents/menus-data` (template: 1280 x
720, widescreen extension 107, draw distance 300 %), keys
`12:ENTER,16:ENTER,20:ENTER,24:ENTER,26:DOWN,26.4:DOWN,26.8:DOWN,27.2:DOWN,28:ENTER,32:ENTER`
(name entry, main menu, track selection, car selection with the Shield chosen, transmission,
race), shots `SR_SHOT`; tools and outputs in `work/agents/menus/` (Python, standard library:
`stats` = mean |diff| on every second pixel of each axis, share of pixels differing by more
than 16 / 64; `except` = every pixel compared).

**Switch off: no change.** The base (`main` at ab1108d, built from `git archive` with its own
target) and this branch, the native renderer off, the same run with the race's cameras and
the mirror (`...,36:F3,42:M,47:F4,52:F1`, 56 s, shots every 50 presents: menus 0-1450,
race 1500-2200): **45 of 45 frames byte-identical** (SHA-256). With the switch off this
branch adds four observers (`78d10`, `78d56`, `78d84`, `4f6f1`) that return at once.

**Native menus next to the original** (the same deterministic run on the Glide path, `g0`,
against native without interpolation, `n1`; whole frames):

| Frames | Screen | mean abs. difference | pixels > 16 | > 64 |
|---|---|---|---|---|
| 0-425, 850-1025 | logos, track selection (2D only) | 0.00 | 0 % | 0 % |
| 250-400 | the intro's attract race (race path, wave 3) | 3.3-11.0 | 7-39 % | 0.6-3.6 % |
| 450-600 | name entry, letters | 0.28-0.97 | 0.4-1.5 % | 0.3-1.1 % |
| 650-800 | main menu (world, sky, carousel) | 2.06-3.01 | 2.9-4.3 % | 0.6-1.4 % |
| 1050-1200 | car selection | 0.61-0.96 | 1.0-1.6 % | 0.4-0.7 % |
| 1225-1400 | transmission (the Shield) | 0.74-1.27 | 1.3-2.3 % | 0.6-1.0 % |
| 1000-1100 (`champ_*`) | league selection | 1.91-2.14 | 6.1-6.7 % | 0.8-0.9 % |
| 1150-1400 (`champ_*`) | championship garage | 2.94-4.22 | 4.1-6.0 % | 1.6-2.5 % |

The difference panels (`menus_p_*`, `menus_champ_1250`) show the same pictures: texel and
polygon edges of the 3D objects (exact float vertices against the engine's rounded ones, as
in the races), the 2D black. The main menu's background (the sky) differs at some texel
edges (mean 1.42 in a background-only box, no pixel by more than 64): the Glide path maps
`s` over x 0..639.8, the native sky over 0..640 (`integration.md`, "Sky"); not changed here
(shared with the races).

**The Shield on the turntable** (car selection, the Shield seen from behind and turning,
frames 1204-1236 every 8, a 360 x 380 crop around the car): mean 4.1-5.2, > 64 3.4-4.4 %
(edges and the reflection's interpolation); the rear window's reflection, the overlay with
its frame, the sticker and the number plate as the original (`menus_shield_1220`).

**Extra frames.** `SR_INTERP=1 SR_INTERP_STEPS=4` (4 frames per step at alpha 1/4 .. 1):

* At alpha 1 an extra frame is the game's own native frame: shots 3383, 3399, 3415 against
  the non-interpolated frames 1208, 1212, 1216: every pixel equal except the settings hint
  (an overlay timed in wall time) and 704 pixels of one row (y = 7 of 720) at the scaled
  edge of an LFB image: the backend draws recorded images from their own texture and
  unrecorded ones from one shared texture, which also holds the neighbouring image's row at
  the rounded scissor edge (`d3d.rs`, `lfb_write`; the races' HUD has the same).
* Smooth: the Shield's crop changes evenly from present to present: median 8.54, max 9.85
  mean levels (max / median 1.15, 39 consecutive presents over 10 steps); the Glide path's
  game frames: median 20.05, max 22.32; no spikes at the step boundaries; the background
  picture's region changes only at step boundaries (as expected).
* Every step of the main menu's carousel turning (`RIGHT` x 3, `LEFT`) was consecutive (no
  camera cut logged by `SR_MENU_PROBE`).
* A whole run with interpolation (`i1`): the race after the menus builds its scene again and
  runs interpolated (shots of the race start, the attract race and the track selection
  checked).

**Unit tests** (`cargo test -p sr-runtime native::menus`): `camera_cuts` (moves, turns, the
angle wrap), `steps_follow_on` (a skipped frame, a pause, another view kind, a cut, the
untimed mode), `unwidened_menu_views_keep_to_the_game_columns`.

## Frame rate

Real time, release build, window 1280 x 720 (monitor 144 Hz), interpolation on
(`SR_INTERP=1`), autopilot, nothing else running; "presents per second" from this module's
statistics (`native menus: ...`, windows of 2 s) and the backend's (`presents: ...`, windows
of 500 presents) and, for the Glide path, its `Glide: ... fps over the last 100 frames`:

| | VSync off | VSync on (144 Hz) | limit 240 |
|---|---|---|---|
| before: menus on the Glide path (native off), or native without the menus | 50 (47.9-50.0 per 100 frames) | 50 | 50 |
| after: name entry | 1 220-1 390 | 144.4 | 238 |
| after: main menu | 1 160-1 880 (median of the windows about 1 700) | 143.9-144.3 | 197-239 |
| after: car selection, transmission | 1 080-1 330 | | |

An extra frame takes 0.16-0.33 ms of CPU (replay and native view). With VSync on, about
one frame per step (48 per second) is drawn and then not shown because the display did not
take it before the menu's next frame ("dropped", as the races' loop does). The longest gap
between this module's presents is 5-12 ms with VSync off (the game's own frame between two
steps: 2.6-3.7 ms in the car selection, plus the tick wait) and 11-13 ms with VSync on (one
refresh missed around the game's frame): the pacing W4-3 works on for the races (`interp.rs`);
the menus' loop is the same and should take the same fix.

## Decisions

* Menu scenes are built at the menu's first 3D draw (during the menu's own loading: the
  longest gap between presents there, 100 ms on the backend's statistics, is the game's
  loading of the menu's files), not prepared ahead.
* Menu worlds show the game's cells only (no grounded statics beyond them).
* The menus' extra frames run no guest code (replay + native views); the races' keep theirs.
* Interpolation of the menus follows the existing `interpolate` setting (and `SR_INTERP`).

## Unknowns

* Replays: not reached (they are saved as `REPLAYS\*.RPL` and need a finished race; no menu
  item in the main menu leads to them). They draw with the race camera set-up `236dc`
  (`widescreen.rs`) and `349b8`: under the race frame timer they take the race path, under
  the menus' timer they stay on the Glide path like the pause screen.
* The pause screen (a race view under the menus' timer) stays on the Glide path, as before.
* The menus' 3D calls not visited (table above), the time attack and multiplayer menus.
* Pacing under VSync (one refresh missed per step in about one of three steps at 144 Hz).
* The main menu's sky texel edges (above); `wide_menus` off: the native view's right edge
  at 639.8 leaves a thin line in the difference panels (W4-3's last-column topic).
* `env_phase` is 0 in the menus (no scene table, no player's car): the menus' environment
  maps were not checked for more than one frame.

## Proposed contract changes

* `View`: a way to say "the game's own 640 columns, not widened" (`View::widen: bool`, or
  `viewport` documented as widened only when `x1 >= 640`): the menus without
  `wide_menus` pass `[0, 0, 639.8, 480]` to keep the renderer from widening a view the game
  did not widen.
* Nothing else: menu scenes are ordinary `SceneData` (statics only with a world, the objects
  as `FrameData::dynamics`), drawn one `FrameData` per view as in the races.

## Corrections proposed for other documents

* `docs/re/strt3fx.md` ("Menus"): the main menu's background is the engine's sky (the
  background picture through `7b218`), drawn by `78d84` with the world `map1` and the
  carousel; the name entry's background is a picture (its letters through `78d10`); the
  menus have no scene table (their objects are only in the instance list `12f478`); they
  draw through `78d10` (no sky, no cells) or `78d84`; `set_view` is called once per menu,
  not per frame.
* `docs/re/native/integration.md` ("Unknowns"): the menus' 3D scenes are drawn natively
  (this document).
* `docs/04-native-renderer.md` (architecture): "menus' 2D" stays on the Glide path, and the
  menus' 3D now goes through the native renderer with its own extra frames.

## Diagnostics

* `SR_MENU_PROBE=1`: the callers of `78d10`, `78d84`, `82338`, `349b8` when they change; per
  menu draw (when the list changes, else every 50 frames) its camera, view constants,
  objects with their models, parts and instance B, the libraries, the model table; per swap
  the extra-frame decision; steps not interpolated with the reason.
* Log: `native renderer: menu scene built ...` per menu scene; every 2 s `native menus: ...
  presents per second (game frames, extra), longest gap, extra frame time, dropped`; debug:
  the scene's ready time and gaps over 100 ms.
