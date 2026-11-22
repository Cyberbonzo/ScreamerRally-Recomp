# Native renderer: cars and moving objects (wave 3, topic W3-1)

With the native renderer on, everything the game draws in 3D in a race now comes from the
native renderer: the track and scenery (wave 2) and, from this topic, the scene table's
instances: cars with their wheels, shadows, smoke and dust puffs, tyre marks. The game's own
drawing of those instances is skipped; the HUD, the mirror frame and all 2D stay on the Glide
path, on top. Link addresses as in `docs/re/strt3fx.md`. Every number is Rally China (Italian
3dfx build), the Shield race of `docs/04-native-renderer.md` unless said otherwise, and says
how it was obtained.

## Files

| File | What |
|---|---|
| `crates/sr-runtime/src/native/dynamics.rs` | New. The scene-table models (parts, levels, meshes), the instances per simulation step, placement, detail levels, interpolation (orientations as rotations, the wheels' steering, camber and spin, `SpinRates`: the fix of 2026-11-21), the reflection scroll emulation, the skip of the game's drawing, `SR_DYN_CHECK`, `SR_DYN_INTERP_CHECK`, `SR_DYN_INFO`, `SR_DYN_PROBE`, `SR_DYN_HIDE`; unit tests. Since 2026-11-21 also: the objects of a view (instance, kind, position, radius, the parts' world heights) handed to `dynvis`, its fades and test marks per part instance (`DynOut`), the player's car (below), the `native dyn:` log line |
| `crates/sr-runtime/src/native/dynvis.rs` | New (2026-11-21). Which moving objects are drawn: the ground map of the race's statics, the rule (drawn only where the world under them is drawn; never the player's car or a near object hidden), fades, the floating detector; unit tests. `docs/re/native/visibility.md`, "Moving objects" |
| `crates/sr-runtime/src/native/extract.rs` | `SceneBuild::part_meshes` (every level of every scene-table part); shadows, smoke and tyre marks always decals (China's marks are mode 0); folded quads of scene-table models: later fan triangles more than 5 degrees out of the first triangle's plane also drawn reversed; `SR_DYN_CULL` (diagnostics) |
| `crates/sr-runtime/src/native/frame.rs` | Three additive lines: `NativeState::dynamics` built with the scene, `FrameData::dynamics` from `dynamics::view_dynamics`, its statistics line; `SR_NATIVE_DYNAMICS=0`. Since 2026-11-21: the race's view context for the visibility (`RaceCtx`: the view's `static_fade`, the fades' clock, the backing tag), `FrameData::dynamic_fade` / `dynamic_backing`, the detector's results routed to `dynvis` |
| `crates/sr-runtime/src/native/world.rs`, `mod.rs` | `read_part` made visible to the module; the module and its install |
| `crates/sr-render/src/native/renderer.rs`, `native.hlsl` | The depth rules for moving objects (stencil classes, decal and overlay passes, the overlays' depth pass), submission order of moving instances, a per-draw constant buffer (`DrawCb`, b2) |
| `crates/sr-render/src/native/gpu_tests.rs` | `moving_objects_depth_rules` |

## What is drawn, from where

### The instances of a view

The frame's instance list (`[7bb7c]` before the object loop: `78d84` stores the list `12f478`
there, `81fbc` walks it) holds instance A pointers ending with 0; an entry with bit 30 set is
not drawn this frame (unused puffs and marks; the player's car around the bumper camera's
drawing). It is read at every view (the game changes it around its drawing). Each entry's
model (instance A +0) is one of the scene-table models, known from the scene table `12f860`
by name (`auto*` car, `ashadow`, `asmoke` / `a2smoke`, `aztri` / `az1tri`): China 6 cars of
12 part slots, 1 shadow, 2 smoke, 2 mark models; 165 part meshes (every level present, a level
sharing its part data with another sharing its mesh).

### Placement (`transforms.md`, "Recipe")

Part p of an instance: `world = Ro (Rp v + tp) + po`, Ro = R(instance A +14h..+1Ch), po =
instance A +8..+10h, Rp / tp = instance B (A + 40h) + 20h p: angles +8..+10h, offset
+14h..+1Ch (the wheels' steering a1 and spin a0, the body on its suspension), `R = Ry(-a1)
Rx(a0) Rz(a2)` in true sine and cosine; billboards (flag 8000h, the smoke) take the angles
relative to the view's camera (`52dc0`: `a0 - cam.a0`, `-(a1 - cam.a1 + 400h)`, `7FFh -
cam.a2`), so the main view and the mirror get different smoke angles. `Instance::transform`
carries the product (f32 basis, f64 origin); `phase` = the engine's `[7bb94]` from +20h
(`extract::phase_of`: car states, smoke frames); `scroll` below.

Within one game frame the game changes some instances between the main view and the mirror
(the shadows' tilt, written by `2d0bc`): a later view of the same frame reads them again (the
check below found a shadow vertex 17.8 units off in the mirror before this, 5.3 after).

### One version of each car (detail levels)

The game picks each part's level when it sets the part up (`828c4`, read in the disassembly):
the distance from the view's camera position to the part's level-0 centre (part data +4;
an entry of 0 reads the model itself), rounded (`fistp`), negated; level 0 when `-d >
[81e60]`, level 1 when `> [81e64]`, else level 2; the rear-view mirror never uses level 0.
In a race on China the thresholds are -2000 and -8000. A part without data at that level is
not drawn. (Three-letter names, fourth byte 0, not starting with `A` / `a`, ending in `i` /
`x` get 4000 / 1000 units added, but only to the part's sort key: `829b0` swaps the plain
distance back for the comparisons. `lod.rs` and `strt3fx.md` describe that rule as moving
the level; a first version of this module did so and placed 5 305 parts at another level than
the engine, the check below.) China's cars: slot 0 (`MV_mauto`) at levels 0, 1 and 2 (level 2
is the stand-in), slots 1-11 at levels 0 and 1 (`SR_DYN_INFO=1` lists them per race).

* Detail setting "original" (`crate::lod::FORCE` off): exactly the game's choice per part
  (`game_level`).
* Detail setting "maximum" (default): every part at the model's finest level present (0 for
  the cars), in both views: the detailed car at any distance, never also its stand-in.
  (`lod.rs` patches the engine to each slot's own highest level present, which draws a
  model's level-1/2-only slots together with the detailed ones; China's cars have none, so
  the Glide path and this agree there: the check below found every engine part at this
  module's level.)

### Reflections (`scroll`)

`bb380` (read in the disassembly, `bb3a5`..`bb5b0`), called for every drawn part whose part
data has flag 2000h, adds to the instance's float accumulators +30h / +34h / +38h `[bb7e8]`
(0.008) times the displacement since the position saved at +24h..+2Ch, rotated by the view's
float camera matrix `82420` (`x' = m0 x + m3 y + m6 z`, x87 precision, stored as floats),
then saves the position; the offsets are `round(acc) & FFh` of x and z. So the first view that
draws the object in a frame moves its reflection; the next sees no displacement. With the
game's drawing skipped this module does the same in the game's own frames (`scroll_update`,
written to the same fields), per view for the instances the engine would draw there: the
engine's object cull replicated (`object_visible`: `81fbc`'s distance limit and `82130`'s
bounding sphere at its nearest depth, projected at half resolution against `[81e40]`,
`[81e28]`, `[81e44]`, `[81e2c]`, behind / beyond `[cf470]`). A first version used the native
frustum instead: 228 of 5 272 updates differed from the game's (the engine draws cars its
coarser test keeps); with the engine's cull, 2.

Interpolated frames use the accumulators interpolated between the two steps, without
rounding (`scroll_of(.., false)`: `acc mod 256`); the game's own frames use the game's
rounded offsets.

### Interpolated frames

Every simulation step's instances are kept (position, angles, instance B's part angles and
offsets, +20h, the accumulators), keyed by the game frame; an extra frame between steps n-1
and n (consecutive) gets them interpolated in floating point: positions and part offsets
linearly, orientations as below, smoke's +20h while it grows within one puff's life (as
`crate::interp`; since 2026-11-21 the puffs' pictures dissolve between the steps instead,
"Smoke and dust puffs" below); car states are not interpolated; an instance that jumped more
than 4000 units (a reused puff) is shown at step n.

Orientations are not interpolated angle by angle (fix of 2026-11-21, "the wheels are
spinning on the wrong axis"; the first version did that, see "Wheels in interpolated frames"):

* parts (`lerp_part`): each step's rotation `Ry(-a1) Rx(a0) Rz(a2)` taken apart as a wheel's
  steering s (about y), camber c (about z) and spin p about the part's own x axis, innermost
  (`R = Ry(-s) Rz(c) Rx(p)`, `wheel_angles`: s and c from the axle, the part's x axis; this
  order is defined at every spin), each interpolated the short way, the spin by more than a
  quarter turn possibly the other way round (below); parts whose angles did not change are
  taken as they are;
* the object (`lerp_rotation`): quaternion slerp between the two steps' rotations;
* billboards (smoke): angle by angle as before (taken relative to the camera).

Spin direction: the steps cannot tell a spin of more than half a turn per step from one
backwards. Measured at 33 steps per second (autopilot, China and the Shield race, every car):
at most about 155 degrees per step, about 0.33 degrees per unit of the car's travel (travels
of 438 and 451 units per step gave 143 and 155 degrees; the most seen was 457): under half a
turn, so the short way is right; at 25 steps per second the same speeds give about 205
degrees. Per car
part the module learns the spin per unit of travel along the car's forward axis from the
steps turning 15-120 degrees (`SpinRates`, an average of the last steps); where a step's spin
is more than a quarter turn it takes the way (forward or back) closer to that rate times the
step's travel. A spin of less than a quarter turn is always the short way (a wheel locked
under braking stays locked).

### Wheels in interpolated frames (fix, 2026-11-21)

**Cause.** The game writes a part's orientation (instance B + 20h p, +8..+10h) as an angle
triple taken from a matrix, with |a0| <= 512: whenever a wheel's spin passes a quarter turn
the triple switches to the other one of the same orientation, `(1024 - a0, a1 + 1024, a2 +
1024)` (attract race, frames 245-247: a wheel part `(1695, 1, 1)` -> `(2037, 1024, 1023)` ->
`(370, 2047, 1)`), and near a spin of +-512 (the game's order has the spin in the middle,
where it is degenerate) steering and roll split arbitrarily (`(511, 1713, 429)` is steering
94, spin 511). The game's frames are right (the matrix is the same; `SR_DYN_CHECK` passed);
interpolated angle by angle between the two triples, a1 and a2 swept through 1024: the wheel
(the parts that flip are `flx`, `frx`, `blx`, `brx`, quads of 4 vertices with the wheel's
picture) turned about its vertical and lengthwise axes in the frames between. Counted from
the steps' part angles (a temporary log, since removed; autopilot, every car): in 16 060 of
87 816 part steps on China and 21 214 of 107 796 in the Shield race a1 jumped by more than
512.

**Verification** (the Shield race, `SR_DETERMINISTIC=1 SR_AUTOPILOT=1 SR_NATIVE=1 SR_INTERP=1
SR_INTERP_STEPS=3`, data `work/agents/fix1-data`; evidence in `work/agents/fix1/`):

* `SR_DYN_INTERP_CHECK=1500,2300`: every car part of the main view's interpolated frames
  (144 180) against steering, camber and spin interpolated (`wheel_angles` of each step; the
  same definition the fix uses, so this checks the angle round trips and that no step took
  the long way at 33 steps per second), and the axle against the two steps' axles linearly
  interpolated (no angle decomposition):

  | | parts | vertices: max / rms (units) | axle: max (degrees) |
  |---|---|---|---|
  | now, steps on one branch | 119 178 | 0.000 / 0.000 | 0.002 |
  | now, steps across the branches | 25 002 | 0.000 / 0.000 | 0.002 |
  | before (angle by angle), one branch | 119 178 | 93.69 / 0.24 | 35.3 (triples near spin 512) |
  | before, across the branches | 25 002 | 333.69 / 70.52 | 119.2 |

  At t = 1 (the step itself) 48 060 parts, max 1.3e-12 units from the step's own rotation.
* The game's own frames against the engine (`SR_DYN_CHECK=1500,2200`, interpolation on, close
  chase from 36 s): 67 520 engine parts, all at this module's level, none missing; 1 480 098
  vertices, max 5.80 units, rms 1.12 (the engine's rounding, as above); reflection
  accumulators 4 139 compared, 0 divergences.
* Frames (China race start, bumper camera, the car beside the player at GO, game frames
  1556-1578, three presented frames per step, a 360 x 160 crop around that car): this build
  against the previous one: at the steps 0.00 mean difference; in the frames between 1.4-1.7
  every third step (where the triple changed branch), 0.00-0.20 otherwise; panel
  `work/agents/fix1/sheet_wheel.png` (rows: now, before, the original Glide path; columns:
  game frames 1558 at the step, 1559 1/3, 2/3, 1, 1560 1/3): before, the rear wheel stands
  edge-on in the two frames between; now round, as in the original.
* Unit tests: `wheels_spin_about_their_axle` (a flip between steps: the axle stays, angle by
  angle it tumbles), `wheel_angles_take_any_orientation_apart` (round trips at every branch
  and at the degenerate spin), `fast_spins_keep_their_direction` (191 degrees per step:
  backwards without a rate, forwards once a slow step taught it; reversing; a locked wheel),
  `interpolation_is_float_and_short_way`.
* Cost: a car in an interpolated frame (interpolation and placing its 12 parts, 8 of them
  turning; the micro-benchmark `bench_interpolation`, ignored by default): 5.9 us, against
  4.3 us angle by angle, so about 0.01 ms per view with five cars (the steps' `wheel_angles`
  are computed once per step, parts that did not turn are taken as they are). The real-time
  per-view figures of the log were too noisy to compare (other game instances were running
  on the machine).

### Which objects are drawn (2026-11-21)

The game draws every listed scene instance within its far distance, whatever the visibility
lists leave of the world around it: an AI car on a far stretch of road that is not drawn hangs
in the sky (Rally China frame 6 750 of the sweeps: the original, Glide path with
`graphics.visibility = original`, shows it exactly so; `extended` draws the hillside and its
road under it). Natively a moving object (car, shadow, smoke, tyre mark) is now drawn only
while **the world under it is drawn** in that view (the statics' surfaces under its position,
from a ground map built once per race; their `static_fade` in the same frame), with fades,
and never hidden when it is the player's car or on screen within 8 192 units of the eye. Rule,
detector (a GPU test of each object's lowest part like the statics' bases), measurements and
cost: `docs/re/native/visibility.md`, "Moving objects". Contract additions:
`FrameData::dynamic_fade` (per part instance, a screen-door fade as `static_fade`),
`FrameData::dynamic_backing` / `DynamicBacking` and `BackingResults::dynamics` (the detector).

* **The player's car**: the car instance nearest (within 1 500 units) to player 1's car record
  position (`[1436d4]` index into the records at `1296c8`, `CC8h` each, position floats at
  +52Ch, as `rumble.rs`); none in the attract race. Measured: 59-69 % of the views find it
  (the bumper camera lists no instance for the player's car: `SR_DYN_VIS_PROBE` at China 2 400
  lists five cars, the nearest 2 543 units from the record; with the chase cameras the car at
  480 units, China 5 000).
* **Where an instance stands** (`SR_DYN_VIS_PROBE`, the per-5-s statistics): cars' positions
  lie on the road (height over the surface under them: median 0, -55 to +1 650 for jumps,
  China), shadows 2 units under it, smoke 20 over it; unused shadows wait at x = z =
  -2 147 483 648 and unused tyre marks 2 000-4 000 units under the road (listed without the
  skip bit): no surface under them, so they are not drawn (they could only show through
  holes in the world).

### Smoke and dust puffs (fix, 2026-11-21)

The user's report on the native renderer: the puffs were cut by the ground; they stayed under
and around the car, hidden by its body, where the original (1997, 3dfx) draws a cloud over
the car's rear; and they moved at the simulation rate while everything else is interpolated.

**What the original does (disassembly, `work/engine_dis.txt`).** No depth buffer: `82130`
gives every object of the frame a sort key, `7a8c0` sorts the keys ascending (a quicksort on
dwords) and the loop at `78e15` draws the objects in that order, each object's parts and
polygons together. Key = high word `-(round(sqrt(d^2 / 8)) >> 2)` with `d` the distance from
the camera structure's position (`81fbc`: instance +8..+10h minus `[81e0c]` +0Ch..+14h),
`+12Ch` for scene instances (bit 30 of the list entry: cars, shadows, puffs, marks), minus
the model's +C4h (0 for all of China's scene-table models, `SR_DUMP_WORLD`); low word = the
entry's offset (ties in list order). So, far to near: (1) every scene instance is drawn after
the scenery whose key distance is up to 12Ch x 4 x sqrt(8) = 3394 units nearer than its own:
a puff is drawn over the road and ground it stands on, whole; (2) among the scene instances
the order is the plain distance: a puff nearer than a car's position (its centre) is drawn
after the car, over it, a puff farther than it is drawn before, under it. Frames of the same
deterministic race (China, close chase, game frames 2445-2850): the Glide path with
`SR_DEPTH=0` (the game's order) shows the dust over the rear wheel and bumper and the far
puffs round; the Glide path with depth and native before the fix hide the dust behind the
body and cut the far puffs flat at the ground.

**The puffs themselves (measured, `SR_SMOKE_LOG`, China race game frames 1480-3700, every
puff of every step: 90 376 steps of puffs, both models).** A puff never moves (position fixed
for its life), its angles are 0, its +20h grows by CABh every step from CABh to FD5Ch (20
steps; the texture frame `min(n - 1, n * phase >> 14)` changes every 3 to 4 steps: 6 frames
for `asmoke`, 5 for `a2smoke`), then its slot leaves the list for at least one step and is
reused at a new place (4 688 rebirths, all after such a gap, all starting at CABh). The quad is
600 x 580 units centred on the position, which lies 20 units over the ground (median, `dynvis`
statistics; -212 to +249). A new puff lies between the emitting wheel's positions of the two
steps around it (one case: 163 units ahead of the wheel at the previous step, 190 behind it at
its own).

**Native rules now** (contract: `Layer::Decal::stand`, `FrameData::dynamic_order`,
`FrameData::dynamic_dissolve`; renderer `ST_ORDER`, `F_STAND`, `Xform::dissolve`):

1. *Ground*: the puffs' material stands on the ground 20 units under the origin
   (`extract.rs` `SMOKE_STAND`). Seen from above that height, the part of the card below the
   horizontal plane through it is depth-tested at the depth where the view ray meets the
   plane (per vertex; 1/z is linear on screen, so the interpolation is conservative), then
   pulled 500 units as before. The ground in front of the puff no longer cuts it; anything
   rising above the plane in front of it (walls, hills, buildings) still hides it, beyond the
   500-unit pull. (A puff right behind a low wall can show its lower part on the ground in
   front of the wall's foot.)
2. *Order over cars*: the runtime gives each car its place by distance from the camera's
   position (nearest = 1) and each puff the first place behind it (1 + the cars nearer than
   it); the cars' pixels carry their place in the stencil's low six bits; a puff is drawn over
   the pixels of cars at its place or farther with no depth test, never over nearer cars, over
   statics as in 1. The puffs are sent far to near (they blend over each other in the game's
   order). Shadows and tyre marks keep the decal rule.
3. *In-between frames*: a puff's picture dissolves between the two steps' pictures: from the
   earlier step's texture frame to the later one's (alpha weighted), fading in at its birth
   and out at its end (a puff gone at the later step is drawn where it was, from the earlier
   step's state). At the steps the picture is the game's. Before, a new puff appeared whole
   at the first in-between frame (where the wheel would be a step later), an ending one
   vanished there, and the texture frame switched at once.

**Verification** (`SR_DETERMINISTIC=1 SR_AUTOPILOT=1`, China, default car, close chase from
36 s; panels in `work/agents/smoke/`):

* Dust over the car: `sbs_dust_over_car.png` (columns: native before, native after, Glide
  path, Glide path `SR_DEPTH=0` = the game's order; game frames 2480, 2600, 2840, 2850): after
  = the game's order (dust over the rear wheel and bumper, the puffs of the car ahead over its
  rear).
* Ground: `sbs_ground.png` (frames 2445-2455, puffs 3 000-4 000 units ahead): before and the
  Glide path flat-bottomed, after round as in the game's order.
* Order and stand rules on the GPU (WARP): `smoke_follows_the_games_order_over_moving_objects`
  (a puff 300 units behind a farther car's surface drawn over it, one 50 units in front of a
  nearer car's surface not; the opposite without the order; the stencil places),
  `smoke_stands_on_the_ground` (a card's lower half over the ground ahead shown with `stand`,
  hidden without it, hidden behind a wall in front with it), `smoke_dissolves_between_steps`.
* In-between frames (`SR_INTERP=1 SR_INTERP_STEPS=4`, game frames 2400-2600, 804 drawn
  frames; `SR_SMOKE_LOG` lines of what each puff shows, `work/agents/smoke/tools/puffs.py`
  (`energy.py` for the image-level check): the change of
  a puff's picture from one drawn frame to the next, 0 = same, 1 = a complete switch): old
  interpolation (`SR_SMOKE=nodissolve`) 3 196 changes, every one a complete switch (1.0); now
  12 798 changes of 0.25 each (max 0.25 = a step spread over its four frames), none above.
  The same total change, spread evenly. Frames: `strip_birth.png` (8 consecutive presents of a
  far puff's birth; rows: before, the new depth rules without the dissolve, after).
  Image-level check (puffs' own pixels = the run minus the same run with `SR_SMOKE=hide`, 40
  complete steps of four presents): the share of the change at a step's first present 30.0 %
  -> 26.2 % (even: 25 %), mean second difference -29 %; in a window dominated by near puffs
  moving through the view the camera's motion dominates and both are alike.
* Mirror (M from 52 s, frames 2350-2900 every 5): unchanged but for one frame (2640, a faint
  puff at the mirror's bottom, as the game's order shows it: `sbs_mirror.png`).
* Cost (real time, interpolation on, other agents' games running on the machine): 8 to 17
  more draws per view (the puffs split by place and kept in order), submit 0.24-0.26 -> 0.25-
  0.27 ms per view; frame rates too noisy to compare.

Diagnostics: `SR_SMOKE=noorder|nodissolve|hide` (comma-separated), `SR_SMOKE_LOG=FIRST,LAST`
(every puff's state per step, the cars' positions, and what each puff shows per drawn frame of
the main view).

### The game's drawing skipped

An observer on `81fbc` (which appends the scene instances to the frame's objects) returns at
once while a view is drawn natively; with the visibility observer already skipping the cells'
objects the engine then draws no 3D object at all. Nothing else of the game depends on that
drawing except the reflection accumulators (emulated above). `SR_NATIVE_DYNAMICS=0` keeps
wave 2's hybrid frames (the game draws them through the Glide path).

## Depth rules

The Glide path's tuned car rules exist because the game's integer geometry is off by up to a
few game pixels near the camera and the models rely on drawing order for their overlays. With
exact geometry the native renderer needs fewer, geometric rules; what was measured on the
cars' own geometry (`SR_DYN_INFO=2`, every blended polygon of every car against every opaque
one, along its visible-side normal):

* The blended polygons (mode 6: glass tint, stickers, number plates, the Shield's C-pillar
  skin) lie on or under the window layer (mode 5, reflective, opaque) of the car: China's
  cars 31-40 blended polygons each, up to 14.4 units under it (the Shield 18 polygons, up
  to 29.1); 6-21 per car coplanar with an opaque surface (within 0.05 units).
* The only other opaque surfaces in front of a blended polygon are body parts well above it:
  the rear spoiler over a deck sticker 8.4-8.9 units (`auto1`), 17.1 (`auto3`), 39.4 (`auto2`).
  These must hide it ("no glass painting over the spoiler").
* Some blended polygons have no surface of their car behind them (the Shield's C-pillar is a
  skin of mode-6 texels over the road and the far side).

Rules (renderer, `ST_*` in `renderer.rs`):

1. The opaque pass writes a stencil class per pixel (the front-most opaque surface's): statics
   0, moving instances 80h, their reflective surfaces (`TexGen::Reflection`) C0h.
2. Blended surfaces of moving instances ("overlays") are drawn right after the opaque pass, in
   submission order: over class C0h pixels as if 128 units nearer (`OVERLAY_WINDOW_PULL`:
   the Shield's 29.1 units under its window layer along a view ray up to 77 degrees off the
   glass's normal; a first value of 64 covered China's other cars' 14.4 units), over
   everything else as if 1 unit nearer (`OVERLAY_PULL`, coplanar overlays such as the Shield's
   rear overlay on its window layer); then their opaque texels (alpha >= 0.9) write their own
   depth and class 80h, no colour (`DRAW_OVERLAY_DEPTH`), so the decals and smoke drawn later
   are hidden behind them as the game's order hides them.
3. Decals (shadows, tyre marks, smoke; China's marks are mode 0 polygons and become decals
   too) are drawn after the overlays in the order of `FrameData::dynamics` (this module sends
   cars, marks, shadows, smoke): over class 0 pixels pulled by their `pull` (shadow 250, smoke
   500, marks 120, as the Glide path), over moving objects' pixels as they are (the Glide
   path's "over cars as they are"). Since 2026-11-21 the smoke instead follows the game's
   order over the cars and stands on the ground ("Smoke and dust puffs" above); the moving
   objects' pixels carry their place in that order in the stencil's low six bits.
4. Static blended surfaces (none on China) as before, far to near.

Decisions: rule 2's order (overlays before smoke) keeps dust in front of a car's plate in front
of it; smoke behind a car is hidden by the window layer or by the overlay's depth pass (the
overlays' depth pass writes the car's place too, so the smoke's order covers them). The
pulls are geometric (units), not the Glide path's relative windows, because the native
geometry has no screen rounding.

`moving_objects_depth_rules` (GPU, WARP) checks each rule: a decal 100 units behind a static
(shown) and behind a moving body (hidden), an overlay 40 units behind a moving window (shown)
and behind a moving body (hidden), 0.5 units behind a body (shown), a decal behind an overlay
that has nothing else behind it (hidden), and the stencil classes. Negative control: with
decals pulled over moving objects the test fails at the second case.

### Folded quads

The set-ups cull a polygon by its first triangle's winding (`cross(v1 - v0, v2 - v0) < -20`)
and draw all of it; per-triangle culling removes a non-planar quad's second triangle where it
alone faces away. Scene-table polygons whose later fan triangle lies more than 5 degrees out
of the first's plane (China: 19 and 18 of about 1 280 in the two races' car models; 6 more
than 15 degrees) get that triangle a second time, reversed. (A first suspicion for the
Shield's dark C-pillar; the cause was the overlay rule above, found with `SR_DYN_PROBE` and a
traced Glide frame.)

## Verification

Runs: `SR_DETERMINISTIC=1 SR_AUTOPILOT=1`, data `work/agents/cars-data` (1280 x 720,
widescreen, draw distance 300 %, detail maximum), the Shield race, keys
`12:ENTER,16:ENTER,20:ENTER,24:ENTER,26:DOWN,26.4:DOWN,26.8:DOWN,27.2:DOWN,28:ENTER,32:ENTER,36:F3,42:M,47:F4,52:F1,57:F2`:
the race from game frame ~1480 (bumper camera), close chase (F3) from ~1600, the mirror from
~1750, far chase (F4) from ~1900, bumper (F1) from ~2100, F2 from ~2300. Shots, logs and
panels in `work/agents/cars/`.

### Placement against the engine (`SR_DYN_CHECK=1480,2400`)

The game draws its scene instances as well; at every part the engine transforms (observer at
`78e62`, main view and mirror) the part this module placed for that view (instance, slot) is
looked up, its level's part data compared with the engine's chosen one, and the part's model
vertices through this module's placement and topic D's camera compared with the engine's view
positions (`68fc8`; mirror x negated):

| | parts | vertices | max / rms (units) |
|---|---|---|---|
| all | 116 272 | 2 359 561 | 13.56 / 1.19 |
| cars | 63 744 | | 6.77 |
| shadows | 5 327 | | 5.29 |
| tyre marks | 8 926 | | 4.94 |
| smoke | 38 275 | | 13.56 (a puff 75 000 units away: the engine's Q14 rounding, relative 1.8e-4) |

Every engine part was placed natively at the same level (116 272 / 0 other / 0 missing).
The differences are the engine's rounding, as topic D and W2-1 measured (car parts 9.75 max
in W2-1's run). Reflection accumulators, emulated without writing (the game updates them
itself in this mode) and compared at the next frame: 5 272 comparisons, 2 divergences (one
below 1 unit, one 4.2), 2 rounded offsets differing.

The same run with the detail setting "original" (the engine's own level selector, not
patched): 83 716 engine parts, all but one at this module's level (a wheel at 1 998 units
against the threshold 2 000: the engine's integer distance), 0 missing; 1 099 378 vertices,
max 13.56, rms 1.10; reflection accumulators 4 351 comparisons, 0 divergences.

### Frames next to the original (same moments, `SR_SHOT=..,25,1500,2500`)

Native (`N7`, the final build) against the Glide path (`G2`), 38 frames over all cameras: mean |diff| 4.5-11.7
(bumper and F2 4.5-5.4, close chase 6.8-8.2, with the mirror and far chase 8.2-11.7), pixels
differing by more than 64: 1.0-4.2 %. W2-3's hybrid frames: 7.5-14.3 (two original frames half
a step apart: 13.5). Panels: the cars at close chase, far chase and with the mirror show the
same picture; the differences are sub-pixel edges (exact against rounded vertices) and the
world's texel edges. The mirror shows the same cars at the same places; smoke takes the
mirror's own angles.

### The Shield from behind

Frames 1600-2000 (close and far chase): the rear window's reflection, the spoiler, the
overlay with its frame, sticker and number plate as in the original (`work/agents/cars/panels`:
`c1`, `z19h`, `w31_final`);
the C-pillar skin over the road correct after rule 2's depth pass (before: the shadow decal,
drawn after the overlay and pulled over the road behind it, darkened it).

No flicker: an interpolated sequence (`SR_INTERP=1 SR_INTERP_STEPS=4`, 81 presented frames
around game frame 1640-1658, close chase), the rear window's crop compared frame to frame:

| | change between consecutive frames (mean abs, median / max) | second difference of its mean colour (median / max) |
|---|---|---|
| native | 0.70 / 3.46 | 0.18 / 4.90 |
| Glide path | 1.77 / 17.21 | 1.32 / 17.78 |

The Glide path's rear window jumps every few frames (four jumps of 15-17 in 80 frames: its
reflection offsets computed by `bb380` from the whole-unit interpolated positions and the
guest memory put back after each step; seen in the crops as the reflection switching between
two placements: likely the "two alternating reflections" reported). Natively the reflection
moves smoothly.

### Frame rates

Real time, autopilot, VSync off, interpolated frames on, 1280 x 720, close chase then the
mirror on (keys as above without the later cameras), presents per second per window of 500
(`--log debug`); nothing else running:

| | race, no mirror (bumper, then close chase) | race, mirror on (close chase) |
|---|---|---|
| native (this topic) | 1 250-1 780 (two runs; close chase 1 250-1 530) | 740-950 (medians 790 and 865) |
| hybrid (`SR_NATIVE_DYNAMICS=0`, wave 2) | 435-815 | 420-650 (medians 490 and 477) |
| Glide path | 194-221 | 120-175 (median about 150) |

(Ranges over two runs each, with and without the earlier measurement's `VirtualQuery` fix
for native; the spread between runs of the same build is 10-20 %.)

Per native view: building the `FrameData` 0.10-0.12 ms (wave 2: 0.034; a first version read
guest memory through `VirtualQuery` per instance: 0.35 ms), submit 0.14-0.15 ms, 180-215
draws, 23 000-26 000 triangles; 125-150 dynamic parts.

### Switch off

The base (main at 09e7a97, built from a clean `git archive` with its own target) and this
branch with the native renderer off, the same deterministic Shield run with every camera and
the mirror, `SR_SHOT=..,50` (frames 0 to 2 400: menus, the attract race, the race): 49 of 49
frames byte-identical (SHA-256). With the switch off this branch adds one observer (on
`81fbc`, returning to the game unchanged) and nothing else runs.

## Diagnostics

* `SR_NATIVE_DYNAMICS=0`: the moving objects through the Glide path (hybrid).
* `SR_DYN_CHECK=FIRST,LAST`: the check above (the game draws its instances too: both on
  screen); not with `SR_EXTRACT_CHECK` (same observer address).
* `SR_DYN_INTERP_CHECK=FIRST,LAST`: the interpolated frames' car parts against steering,
  camber and spin interpolated, and the angle-by-angle interpolation of the first version
  ("Wheels in interpolated frames"); one report after LAST.
* `SR_DYN_INFO=1`: the scene-table models' slots, levels and meshes; `=2` also the glass
  analysis per car model and its darkest flat colours.
* `SR_DYN_PROBE=<game frame>` (+ `SR_DYN_PROBE_BOX=x0,y0,x1,y1`): the cars' triangles facing
  away on screen in that frame's main view (or those covering the box's centre).
* `SR_DYN_HIDE=s,...`: cars' part slots not drawn. `SR_DYN_CULL=0|flat`: scene-table
  materials without back-face culling (all / flat ones).
* Log every 5 s: instances per view by kind, parts, stand-in parts, share interpolated; since
  2026-11-21 also the visibility's CPU time per view, objects on screen not fully drawn (cars),
  objects without ground under them, and their height over the ground by kind.
* The moving objects' visibility (`docs/re/native/visibility.md`, "Diagnostics"):
  `SR_NATIVE_DYN_VIS=0` (drawn wherever they are, as before), `SR_NATIVE_VIS_LOG=1` (the
  detector and a `native dyn:` line per view), `SR_NATIVE_DYN_TEST=1` (the detector's test
  without the log), `SR_DYN_VIS_PROBE=<frame>` (every object of that frame: position,
  support, fade, test counts; the player's car record).

## Unknowns

* Detail setting "original" was compared with the engine (above), not with frames.
* Other tracks, the split screen, replays and the attract race's cameras: not compared (the
  code reads everything per race; the attract race's cars are drawn natively in the runs).
* The engine's part-level cull (part radius against the near plane in `7ac50`) is not part of
  the reflection emulation; the 2 remaining divergences may be such cases.
* The menus' 3D scenes stay on the Glide path (no scene for them; phase 4).
* Blended surfaces on statics (none on China) keep wave 2's far-to-near order.

## Corrections proposed for other documents

* `docs/re/strt3fx.md` ("Level of detail") and `lod.rs`'s comment: the `i` / `x` name rule
  of `828c4` changes only the part's sort key, not its level (`829b0`: `xchg [esp], eax`
  brings the plain distance back before the comparisons); measured: with the rule applied to
  the level 5 305 of 78 412 engine parts differed, without it 1 of 83 716.
* `docs/re/native/world.md` ("Scene instances"): within a game frame the game changes some
  instances between the main view and the mirror (the shadows' tilt); a reader must take
  them per view.
* `docs/04-native-renderer.md`, mode table, mode 6: "no depth write" holds for the blended
  texels; natively the opaque texels of moving objects' overlays write depth (rule 2).

## Proposed contract changes

* `Layer::Decal`: "pulled over static surfaces only; over moving objects tested as they are;
  drawn in `FrameData::dynamics` order". The renderer now does this.
* Blended materials of `FrameData::dynamics`: document the overlay rules (over moving
  instances' reflective surfaces within `OVERLAY_WINDOW_PULL`, elsewhere `OVERLAY_PULL`;
  opaque texels write depth) or give them their own `Layer::Overlay { window_pull, pull }`;
  today the renderer applies them to every blended dynamic material (only car glass on China).
* `FrameData::dynamics` order is meaningful (decals and overlays are drawn in it); say so.
* `SceneBuild::part_meshes` (runtime side) replaces W2-1's proposed (model, slot) map; the
  level is part of the key.
* `FrameData::env_phase` stays one value (the player's car's +3Ch): China's env map has one
  frame.
