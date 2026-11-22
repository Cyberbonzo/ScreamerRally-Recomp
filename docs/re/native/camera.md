# Camera collision (native renderer)

The user: "when the camera goes close to a wall, it clips through it. This was an issue in the
original game too, but I want to fix it." With the native renderer the whole world's geometry
is in memory, so the drawn camera can be kept out of walls. Only the drawn camera changes: the
game's camera structure, its smoothing state and the simulation are not touched (no feedback
into the game). Code: `crates/sr-runtime/src/native/camera.rs`; setting
`graphics.camera_collision` (F12, Graphics, Experimental: "Camera collision", under
"Renderer"; on by default, effective only with the native renderer). Link addresses as in
`docs/re/strt3fx.md`.

## What the game's cameras do (measured)

`SR_CAMERA_PROBE=N` logs every N game frames the view's camera object `[1296bc + 4 v]`, its
mode (+AE4h), the followed car's record (+ADCh) and every car in the camera's view space
(`transforms.md`: x left, y up, z back, the eye 128 units behind the view origin).

* **The followed car**: camera object +ADCh points to the followed car's record
  (`1296c8h + n * CC8h`); record n's car is the n-th `auto*` entry of the scene table
  (names repeat: `auto1`..`auto4`, `auto1`, `auto2`). Verified in the attract race, where
  the camera switches from record 0 to record 1 and the car on the view axis switches from
  the first to the second `auto` instance (view x, y within 20 units of 0 in every sampled
  frame), and in races (record 0, the player's car). The car records hold no pointer to the
  instance (searched).
* **Camera modes** (F1 2, F2 4, F3 6, F4 3, `strt3fx.md`), the followed car's origin in the
  game's view space, constant while driving (China, frames 1500..2430):

  | key | mode | car origin in view space (x, y, z) | eye above the road | focus - eye distance |
  |---|---|---|---|---|
  | F1 | 2 (own car hidden) | (0, -422, 0) | ~550 | |
  | F2 | 4 | (0, -568, 4): over the car, looking ahead | ~580 | ~430 |
  | F3 | 6 | (0, -230, -1106) | ~580 | ~1190-1210 |
  | F4 | 3 | (0, -188, -1354) | ~610 | ~1440 |

  Pitch 1946..1958 (16-18 degrees down) in the chase modes. Mode 1 is the attract mode's
  roadside cameras (the camera looks at a car from up to 9 000 units away).
* **The car's size**: the union of the part meshes of the default car (model 1590): x
  -322..322, y -109..386, z -755..751 (the scene's part meshes, logged at the scene build).
* **How close the eye gets to walls.** The log's `clear_game` is the game eye's distance to
  the nearest solid triangle. The chase eyes stay high (median 550-600 units: the camera
  height over the road) and in ordinary driving keep 275-400 units or more from opaque
  geometry: in autopilot laps of China (F2, F3, F4) and of the second track (F3, F4) the
  game's eye was never behind a wall and the game's near plane never cut opaque geometry
  (table below), nor in scripted weaving and crash / turn-around runs on China in F3 and F4
  (minimum clearance 390-410 units in the turn-around runs). The clipping happens when the
  car is pressed against **tall** walls. Its clearest case: on the second track a car
  scraping the wooden wall after the start (game frames 1932-1937 of the scripted runs)
  brings the F2 eye within 7 units of the wall and then behind it, the F2 and F3 near planes
  cut the wall open (the flat ground colour of the empty space outside the stage set shows
  through), the bumper view's (F1) too, and the rear-view mirror's eye goes behind it. On China
  the mirror's near plane cut opaque geometry in one frame of each of two scraping runs, and
  an early manual run (F4, throttle held, steering bursts) had 3 frames with the F4 near
  plane cutting a wall (game frames 2744-2746).
  Trees, bushes and posts (colour-keyed or tiny) are passed through more often (with every
  surface counted, `SR_CAMERA_SOLID=all`: 3 frames with the eye behind such a surface in an
  autopilot lap (F3, then F4 from 130 s), 5 and 3 frames (events of one or two frames) in two
  weaving runs on China in F3; frames 2460-2464 and 2590-2594 of one of them, looked at: a
  tree and a bush brushed past, nothing visibly wrong).
  Whether the game's camera code (`236dc` and the mode handlers) avoids walls itself was not
  examined.

## Method

Per native view of a race (`frame::draw_view` calls `camera::adjust` after building the view's
camera from the game's camera structure, before visibility, dynamics and the sky are
computed; the sky depends on the angles only, which do not change):

1. **Which views**: the main view (and a second player's) in the chase modes 4, 6, 3 (F2-F4):
   pull-in and near plane. The rear-view mirror: the near plane only (its eye sits by the
   car; there is no segment to pull along), and only while the player's car is not between
   its eye and the game's near plane (otherwise moving the plane in would show more of the
   own car in the mirror than the game does). Not the bumper view (mode 2, the own car
   hidden), mode 5, the attract mode's cameras (mode 1) or the menus.
2. **The world's obstacles** (once per race, at the scene build, 11-17 ms): every
   triangle of the scene's statics whose material is in the `World` layer and `Opaque`
   (flat colours included). Left out: decals, blended surfaces, colour-keyed surfaces
   (fences, tree panels, backdrops: see-through) and statics whose bounding sphere is
   smaller than 120 units (posts, signs). China: 72 442 of 81 446 triangles; the second
   track 72 176. A uniform grid over (x, z), cells of 1 024 units (China 263 x 251 cells,
   274 572 entries: a triangle is entered in every cell its box covers), with a per-query
   mark per triangle so that each is tested once. Front sides: the numerical normal
   `(b - a) x (c - a)` of the scene's triangles points to the side they are seen from (in the
   game's left-handed world coordinates); checked on the ground: of the nearly horizontal
   solid triangles 51 647 face up and 542 down (China).
3. **The focus**: the followed car's origin (instance A +8..+10h, on the road) raised 150
   units (world up), about the middle of the car's height. In extra frames the car's
   position and angles of the last two steps, interpolated in floating point at the frame's
   alpha (the guest's copies hold whole units during extra frames).
4. **Pull-in** (chase modes): the segment from the focus to the game's eye (extended by the
   margin) is tested against the obstacles with Moller-Trumbore; only surfaces facing the
   focus count (a wall between the car and the eye, seen from the eye from behind: back
   faces are culled, which is the see-through effect). Target: the eye `MARGIN` = 64 units in
   front of the first hit, along the segment, but never nearer the focus than where the
   segment leaves the car's box plus 60 units (the car is never entered; the eye never passes
   the focus). Smoothing per simulation step: towards a larger pull-in with time constant
   0.05 s (45 % of the way per step at 33 steps per second), back out with 0.4 s, but never
   leaving the eye less than 24 units (`NEAR_MIN + NEAR_MARGIN`) in front of the wall, where
   the car allows: the eye is never left behind the wall, it eases into the rest of the
   margin. Within half a unit of its target the value is the target, so the release ends
   exactly at the game's camera. A cut (mode change, the game's
   camera jumping more than 3 000 units, a gap in the steps) starts at the target. The drawn
   eye moves along the focus-eye segment; the rotation is the game's (the horizon, the sky,
   billboards and the car's screen position along the segment stay as the game has them; the
   car grows).
5. **Near plane** (all corrected views): the drawn camera's view pyramid (the projection and
   the viewport, widened by the widescreen extension as the renderer does) is clipped against
   every obstacle facing the eye in the grid cells under the slab from the eye to 136 units
   (Sutherland-Hodgman against the four side planes); the nearest depth found, and that of
   the followed car's box (12 triangles, placed by the car's position and angles), give the
   plane: 8 units in front of the nearest, between 16 and the game's 128. Moving the plane
   in only shows what the game's plane would cut away, so it comes in at once; it goes back
   out with the 0.4 s time constant (ending exactly at 128, as the pull-in). Extra frames:
   the plane between the steps, but never farther than the target at the frame's own eye.
6. The corrected eye and near plane go into `View::camera` (`pos`, `proj.near`); the
   renderer draws as before (the depth is `DEPTH_SCALE / z`, valid for any near plane, and
   clipping is at `SV_ClipDistance0 = z - near`: `near.md`).

Parameters (constants in `camera.rs`): `FOCUS_HEIGHT` 150, `MARGIN` 64, `CAR_CLEARANCE` 60,
`TAU_IN` 0.05 s, `TAU_OUT` 0.4 s, `NEAR_GAME` 128, `NEAR_MIN` 16, `NEAR_MARGIN` 8,
`TINY_RADIUS` 120, grid cell 1 024.

## Verification

All runs: release build, data folder `work/agents/camera-data` (the template: 1280 x 720,
draw distance 300 %, extended visibility, interpolation off), `SR_DETERMINISTIC=1`,
`SR_NATIVE=1`, `SR_CAMERA_LOG`; keys after the menus' `12:ENTER,...,32:ENTER` (Rally China;
`22:DOWN` for the second track), `36:F2|F3|F4` for the camera, `37:M` for the mirror; the race
starts at game frame ~1560 (t ~ 56 s). Logs, tables and pictures in `work/agents/camera/`
(`summary.md`, `ident.txt`, `*.tsv`, `*.png`).

* **Unit tests** (`cargo test -p sr-runtime native::camera`, 12): segments hit walls facing
  their start only, the nearest of several across grid cells; the near slab finds a wall
  beside the eye at the exact depth (100 x 256 / 321) and ignores one facing away or outside
  the view; the car box's exit distance; the smoothing (fast in, slow out, exact end at the
  game's camera, extra frames between steps, cuts, a step drawn twice); the whole correction:
  the drawn eye in front of a wall facing the car, also when the wall appears on a step that
  is not a cut and the target is limited by the car; the near plane in front of a wall beside
  the eye, the mirror keeping the game's plane with its car in the slab, nothing changed
  without obstacles. `cargo test -p screamer`: the setting's round trip and default, the F12
  item present once and every tab fitting without scrolling at 1280 x 720 (and 5 other
  sizes).
* **Sweep** (15 runs, 96 920 corrected views; per view the game's camera and the drawn one
  checked independently of the correction: "eye behind a wall" = the first surface facing
  the car along the focus-eye segment is nearer than the eye; "near plane cuts" = solid
  geometry facing the eye crosses the near plane inside the view). Laps: the autopilot for
  230 s (6 467 race frames, more than a lap); scrape: throttle held, a 0.25 s steering pulse
  to one side every 0.9 s (150 s); weave: throttle held, alternating pulses (180 s).
  Per view over all runs (the per-run table is `work/agents/camera/summary.md`):

  | view | frames | eye behind a wall: game / drawn | near plane cuts: game / drawn | pulled in | near plane moved in |
  |---|---|---|---|---|---|
  | F2 (mode 4) | 18 870 | 2 / 0 | 16 / 2 | 146 | 498 |
  | F3 (mode 6) | 25 337 | 0 / 0 | 2 / 0 | 0 | 98 |
  | F4 (mode 3) | 25 337 | 0 / 0 | 0 / 0 | 0 | 0 |
  | rear-view mirror | 27 376 | 0 / 0 | 7 / 0 | 0 | 368 |
  | F1 (mode 2), not corrected | 6 556 | 0 / 0 | 8 / 8 | 0 | 0 |

  Every game-side event of the main views is on the second track, where the car scrapes the
  wooden wall: F2 frames 1932-1934, 1936-1937 and 1996-1999 (a second contact), F3 1935, F1
  also 2239; the autopilot laps of both tracks have none in any mode. The "near plane moved
  in" frames beyond them are geometry inside the slab without crossing the game's plane and
  the 0.4 s release. The 2 remaining F2 frames are frame 1934 of the two F2 runs on track 2
  (the near plane at its 16-unit minimum, "Limits").
* **Before / after, the same deterministic moments** (collision off / on, track 2, the F2 and
  F3 weaving runs; `t2_F2_off_on.png`, `t2_F3_mirror_off_on.png`): F2 1933 and 1937: the
  wall cut open by the near plane, the green ground colour of the void beside it / the wall
  drawn whole. F2 1934: the eye behind the wall, the wall gone and the void and the scenery
  outside visible / the eye pulled 117 units towards the car, in front of the wall: no void,
  the wall's near edge a strip at the screen's middle (the near plane at 16 still cuts it:
  the remaining frame). F3 1935: a strip of void at the left edge / the wall. Mirror 1933:
  the wall cut open in the mirror's left part / whole. Mirror 1934: its eye 10 units behind
  the wall, the outside visible in both (not corrected, "Limits").
* **Interpolated frames** (`SR_INTERP=1 SR_INTERP_STEPS=2`, the F2 weaving run on track 2:
  2 848 extra frames): extra frames with the eye behind a wall game 1 / drawn 0, near plane
  cutting solid geometry game 18 / drawn 1 (the alpha-1 copy of step 1934); pulled in 146,
  near plane moved 308. `t2_F2_interp_off_on.png`: the extra frames halfway between steps
  1933 / 1934 and 1934 / 1935, off and on (the void / the wall, the eye pulled in front of
  it). Extra frames are labelled with the next game frame in `SR_SHOT`'s names and the log
  (the log's pull 58.5 halfway between the steps' 0 and 117 shows which).
* **No change with the setting off, no feedback into the game** (`ident.txt`; main's build
  c70252e, the merge base, built separately, against this branch; the F3 + mirror weaving
  run on track 2, every 3rd presented frame of 1500-2100, 201 frames, SHA-256):
  native renderer with `SR_CAMERA_COLLISION=0`: 201 of 201 identical; the Glide path (native
  off): 201 of 201 identical; collision on: 199 identical, the 2 that differ (1935, 1938) are
  frames the log shows corrected, and no frame outside the corrected ones differs (the game's
  state is the same with the correction: it writes only the drawn camera; `camera.rs` has no
  guest memory writes).
* **Cost**: per corrected view (segment, slab, car box; the log's own checks not included),
  over the sweep: mean 9.9 us, median 8.8, 99th percentile 25.0; the maximum per run 24 us
  to 1.0 ms (four runs above 300 us: outliers, the runs' 99th percentiles are 10-37 us);
  22-48 grid candidates per segment query on China (probe). Per race: the obstacle grid built in 10.9-17.2 ms at the
  first race frame (China: 72 442 triangles, 263 x 251 cells, 274 572 entries; track 2:
  72 176, 352 x 225, 246 161), about 6 MB (estimated from the element sizes: 64 bytes per
  triangle, 4 per entry and per cell). (Measured with the machine otherwise idle apart
  from the run; an earlier sweep while another build ran gave mean 14 us, p99 35.)

## Limits (what remains)

* **The near plane cannot come nearer than 16 units.** A wall within about 16 units of the
  eye inside the view is still cut open. In the sweep this is the one remaining frame of each
  F2 run on track 2 (game frame 1934): the eye, pulled 24 units in front of the wall along the
  focus-eye segment, is 2 units from the oblique wall. A smaller minimum would need the
  renderer's depth precision checked (`DEPTH_SCALE / z`).
* **The car limits the pull-in.** The eye never comes nearer the focus than where the segment
  leaves the car's box plus 60 units (medians over a China lap: F2 298 of its 433 units, F3
  758 of 1 195, F4 831 of 1 442). A wall nearer the car than that (the car pressed into it) can stay between the car and the eye;
  the near plane then still keeps the wall from being cut where it is in front of the eye.
* **The segment sees what lies between the car and the eye.** A wall beside the eye that the
  segment does not cross is handled by the near plane only; an eye that enters a closed object
  from a side the segment does not cross is not detected.
* **Over a wall, the void.** When the eye is above a wall and looks down past it (track 2,
  F2, frame 1935), the flat ground colour of the empty space outside the stage set shows
  (`docs/04-native-renderer.md`, "Decisions"): not a collision, the game shows the same; no
  camera correction fills it.
* **The rear-view mirror** gets the near plane only: its eye going behind a wall (track 2,
  frame 1934, 10 units behind) is not corrected (there is no segment to pull along that would
  keep the mirror's framing), and while the player's car is between its eye and the game's
  near plane the plane stays at 128.
* **See-through and small objects** (colour-keyed trees, fences, backdrop panels; posts and
  signs under 120 units) are not obstacles: the camera passes through them as in the game
  (single frames; pulling in for them would make the camera jump). `SR_CAMERA_SOLID=all`
  counts them for diagnosis.
* **Sudden walls.** The pull-in eases in, but never leaves the eye less than 24 units in front
  of the wall (`NEAR_MIN + NEAR_MARGIN`): when a wall appears between the car and the eye
  from one step to the next, the eye moves to it in that step and eases the rest of the margin.
* **Not corrected**: the bumper view (F1, mode 2: its near plane cut the track-2 wall in 8
  frames of the weaving run, the own car being hidden), the attract mode's cameras, the menus.
  Split screen (view slot 1) is coded like the main view but was not run. Rally China and the
  second track (selected with one DOWN at the track list) were run; the other tracks were not.
* **The game's camera code** (`236dc` and its mode handlers) was not examined: whether it
  avoids walls itself is not known; it keeps its eye high (above).
* The obstacle grid is built at every race start (11-17 ms on the first race frame, in
  addition to the scene's ~120 ms), also with the setting off, so that switching it on in a
  race takes effect at once.

## Diagnostics

* `SR_CAMERA_COLLISION=0|1`: overrides the setting. `SR_CAMERA_NEAR=0`: pull-in only, the
  game's near plane. `SR_CAMERA_MIRROR=0`: the mirror is left alone.
* `SR_CAMERA_LOG=<file>`: one line per race view drawn (mode -1 = the mirror): frame, extra
  frame and alpha, mode, focus-eye distance, first hit (distance from the focus) and its
  static, target and smoothed pull-in, drawn distance, minimum distance, near plane, nearest
  solid depth in the slab, the eye behind a wall (game / drawn: the first hit nearer than the
  eye), the near plane cutting solid geometry facing the eye (game: at 128 from the game's
  eye; drawn: at the drawn plane from the drawn eye; computed independently of the
  correction), microseconds, grid candidates, the car's position, the game's and the drawn
  eye's clearance (`b` = behind the nearest surface).
* `SR_CAMERA_SOLID=all|keyed`: colour-keyed (and with `all` tiny) objects count as obstacles.
* `SR_CAMERA_PROBE=N`: the camera objects, modes and cars every N game frames, and a ray
  straight down from the focus (it must find the road 150 units below: it does, e.g.
  151.9, 149.0, 150.2 in a China race).
* Log line at each race: `native camera: N solid triangles ...` and every 5 s
  `native camera: ... views ...; pulled in ..., near plane moved in ...`.

## Files

| File | Change |
|---|---|
| `crates/sr-runtime/src/native/camera.rs` | new: obstacles grid, segment and slab queries, smoothing, the correction, log, probe, unit tests |
| `crates/sr-runtime/src/native/frame.rs` | `NativeState::camera`; built in `activate` (races only); `camera::adjust` in `draw_view` |
| `crates/sr-runtime/src/native/mod.rs` | `pub mod camera` |
| `apps/screamer/src/settings.rs`, `menu.rs`, `main.rs` | `graphics.camera_collision`, the F12 item (dimmed with the Glide renderer), the start-up value |
| `docs/re/native/camera.md` | this document |
