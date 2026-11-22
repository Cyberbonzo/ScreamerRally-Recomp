# Native renderer: visibility, backed by the world

Which statics the native renderer draws in a race, after the user's test of the first rule
(`docs/04-native-renderer.md`, "Decisions": "Visibility, revised ... backed by the world").
The first rule (`integration.md`, "Visibility") drew the camera cell's list *merged with the
lists of the track cells in it* (`graphics.visibility = extended`) plus every "grounded"
static in view, and faded statics in and out with membership of that set. The user saw
"long pieces of road and trees floating in the sky above hills" and objects "seen from far
away" that "fade out when you get closer". Both are measured below: with the new rule the
main view draws no floating object (a few single-pixel bases aside), floating statics left
by the designers' lists go within a frame or two, and nothing on screen nearer than one cell
ever fades out.

Status: implemented for races, main view and rear-view mirror (each with its own camera and
state); the menus' scenes keep the first rule (unchanged). Off by default like the whole
native renderer (`SR_NATIVE=1` or F12 > Graphics > Renderer: Native).

Second pass (2026-11-21, "nothing floats"): **moving objects** (cars, shadows, smoke, tyre
marks) now follow the same principle through the world under them ("Moving objects"); the
**mirror's** remaining floating frames were bases of one pixel that the thresholds, set for
the main view's pixels, never called unbacked ("The mirror"); and a **camera cell without a
list** no longer keeps a stale list that leaves the view empty ("Cells without a list", found
by the moving objects' detector: the player's car floating over the ground colour). Measured
on the same six-track sweeps: frames with a car floating over undrawn world 1.7-8.2 % (main
view) and 0-1.5 % (mirror) -> 0 on every track; the mirror's frames with a floating static
2.5-8.8 % -> 0.0 % (0-2 frames per track); main-view frames with an almost empty base (fewer
than 10 statics on screen) 69-145 on Canada, Arizona, Wales -> 0.

Third pass (2026-11-22, the visibility editor, `docs/05-visibility-editor.md`): while the
editor's edits are active (in the tool, or for a track with a saved edits file) a view draws
exactly the **edited lists**, nothing else ("Edited lists (visibility editor)" below); every
other track keeps the rule described here, unchanged (captures byte-identical).

## The rule

Per view, every drawn frame (`native/visibility.rs`, `Backed`; called from
`native/frame.rs`, `race_visibility`):

1. **Base**: the designers' list of the camera's cell as the game collects it: the cells of
   that list inside the frustum's box of cells (`7a52c`; the box when the cell has no list,
   the engine's own rule `7a640`), never merged with other cells' lists. Drawn at once, in
   the first opaque pass. `crate::visibility` now reads the camera cell's own list before it
   merges anything (`GameCells::own`), so the `graphics.visibility` setting (original /
   extended / everything) no longer changes what native draws (it still governs the Glide
   path).
   * A camera cell **without a list** (the camera outside the track's cells: the far chase
     camera on a bend, a car off the road, and long stretches of track through cells without
     lists): the engine collects the whole frustum box, which with the 300 % draw distance is
     nearly the whole world, floating objects included (Canada, frames 4430-4437 of the sweep:
     1 856-1 868 statics, then 92 when the camera re-entered a listed cell). Native takes
     instead the **list of the nearest cell with one** (its centre nearest to the camera,
     within `NOLIST_REACH` = 8 cells) **merged with the last list the camera had** (per view)
     and the camera's own cell (`nearby_list`; "Cells without a list" below: the first
     version kept the last list alone, which emptied the view on long stretches); at a race's
     start without either, the box.
2. **Candidates**: statics in the cells named by the lists of the **track cells in view**
   (cells with a list whose statics' bounding sphere touches the view frustum, within the
   draw distance: the track ahead as far as the camera sees) or by the camera cell's own
   list (cells outside the box), not in the base. Why: these are what the game itself shows
   from some stretch of the track the player is looking at; anything no list names (never
   shown by the game from anywhere in view) stays hidden. A candidate is drawn only while
   its **base lies in front of world geometry already drawn**, never against the sky, the
   background or the void, tested on the GPU every frame (below). Results arrive one frame
   late; hysteresis: drawn after `ON_RESULTS` = 2 consecutive backed results; an accepted
   candidate stops after `OFF_RESULTS` = 2 consecutive unbacked results spanning at least
   `OFF_STEPS` = 1 simulation step; between the thresholds nothing changes.
3. **Leaving the sets**: a static that leaves the base while on screen is **kept** (no pop,
   no fade) and tested like a candidate; it stops only when off screen (at once) or when its
   base shows the sky (it floats): then it fades out over `FADE_OUT_STEPS` = 4 steps
   (0.12 s). **Near objects are never faded out**: a static on screen whose bounding sphere
   is within `NEAR_KEEP` = 8 192 units of the eye (one cell) stays drawn whatever the
   tests say (depth hides it when something is in front).
   * **Statics at risk** of leaving the base: those of the base whose cell is missing from
     the list of some neighbouring track cell of the camera's cell (the camera moves into one
     of them next). They are drawn in pass 1 instead of pass 0 and tested against the rest
     of the base, so that when they leave the base their state is already known: a floating
     one starts fading out in the frame it leaves (instead of one frame later plus
     hysteresis).
4. **Fades**: only an object that **starts** being drawn while on screen fades in, over
   `FADE_STEPS` = 8 simulation steps (0.24 s); one that starts or stops while off screen
   takes its state at once. The fades run on a clock of simulation steps that the
   interpolated frames continue (`t = step - 1 + alpha`), so they are smooth at any frame
   rate and do not depend on it.

What counts as "backed" (`classify`, per test level): of the base's pixels that are not
behind what is drawn ("visible"), at most `ACCEPT_SKY` = 10 % show nothing drawn, and at
least `VISIBLE_SHARE` = 50 % of the base is visible: a base hidden behind the corridor's
banks proves nothing (it would float once the camera comes over the crest; with hidden
bases counted as backed, the first version accepted 666 candidates at once on China that
then floated). "Unbacked": at least `REJECT_SKY` = 25 % of the visible base pixels, and at
least `MIN_SKY_PIXELS` = 2 pixels in the main view, show nothing drawn; the minimum scales
with the area a view's pixel covers in the world (`min_sky_pixels`: the product of its focal
lengths against the main view's 256 x 307.2, at least 1), so the mirror (128 x 153.6, a
quarter) needs 1 ("The mirror" below). A static drawn in pass 1 that pass 0 does not back
moves to pass 2, where test 2 sees it.

## The base of a static

`sr-render` `prepare::build_bases`: the static's depth-writing triangles (opaque and
colour-keyed world materials: what can stand in front of the world) clipped to world height
`y <= lowest + max(0.2 x height, 100)`: the lowest fifth (a tree's trunk foot, a tower's
ground floor, a road section's lower end on a slope); flat objects (road, terrain on level
ground) are all base. Built once per scene (China: 52 017 base triangles for 81 446 static
triangles), stored chunk-relative like the merged statics.

## The GPU test

`sr-render` `native/backing.rs`, shaders `vs_base` / `ps_base` in `native.hlsl`:

* `FrameData::static_backing` (contract addition, below) gives per static its opaque pass
  (0, 1, 2) and its tests. The renderer draws the opaque statics of pass 0, runs **test 1**,
  draws pass 1, runs **test 2**, draws pass 2, then the moving objects and everything else
  as before (statics stay merged in chunks: each pass draws the chunks that hold one of its
  statics, the vertex shader drops the others like a fade of 0).
* A test draws the bases of the statics marked for it (one indexed draw over this frame's
  tested bases, no colour written, no culling) three times and counts per static with an
  atomic add in the pixel shader into a UAV (`[earlydepthstencil]`: the depth test decides
  first): **area** (no depth test; test 1 only), **visible** (the world's depth test,
  nothing written), **sky** (depth forced to 0 by a viewport with `MinDepth = MaxDepth = 0`:
  passes only where nothing wrote depth, i.e. sky, background quad or void).
* After the view the counts are copied to a staging buffer and read back without stalling
  (`Backend::native_backing`, contract addition); the runtime applies the newest result per
  view. Deterministic runs (`SR_DETERMINISTIC`) wait for them so that a run repeats.
* Verified: `gpu_tests::backing_test_counts_bases_against_the_world` (WARP): a box standing
  on the ground (area 192, visible 192, sky 0), one floating above the horizon (sky = visible
  at test 1; 0 at test 2 with a backdrop drawn in pass 1 behind it), one behind an occluder
  (visible 0), the backdrop itself (tested once, drawn: its colour read back where only it
  can be); a frame without marks returns no results. `prepare::tests::bases_are_the_lowest_part`.

## Moving objects (2026-11-21)

**What the original does.** The game draws every listed scene instance within its far distance,
whatever the lists leave of the world around it. Rally China frame 6 750 of the sweep (bumper
camera, an AI car on the far hillside's stretch of road): the Glide path with
`graphics.visibility = original` (the game's own lists) shows the car hanging in the sky exactly
as native did; with `extended` the hillside and its road are drawn under it
(`work/agents/float/pairs/china_6750.png`, `china_6750_car.png`: original | native before |
native after).

**The rule** (`native/dynvis.rs`, called from `native/dynamics.rs` per view): a moving object
(car, shadow, smoke, tyre mark: one scene instance, all its parts together) is drawn only while
**the world under it is drawn** in that view:

* *Under it*: the statics' surfaces (opaque and colour-keyed triangles in world space, bucketed
  by x / z in 1 024-unit cells once per race: China 70 672 triangles, built in 25 ms) under the
  instance's position, from `SUPPORT_DROP` = 4 000 units below it to `SUPPORT_RISE` = 300 above
  it; the highest and those within `SUPPORT_LAYER` = 100 units of it are its support. Drawn
  when one of their statics has a fade above 0 in this view's `static_fade` (the same frame's:
  no latency). The statics under an object are looked up again when it has moved `REQUERY` =
  64 units. Measured where instances stand (China): cars' positions on the road (height over
  the surface median 0, -55 to +1 650 in jumps), shadows 2 units under it, smoke 20 over it;
  unused shadows wait at x = z = -2^31 and unused tyre marks 2 000-4 000 units under the road
  (listed without the skip bit): no surface under them, so they are not drawn.
* *Why the ground and not the GPU test*: a car at a crest seen from below has the sky behind
  its lowest part although its wheels are on drawn road (`work/agents/float/pairs/china_2884_crest.png`:
  the green car at the end of the road); the test would hide it, the ground keeps it. And a car on
  a stretch of road that is not drawn floats even in front of a drawn hillside; the test would
  keep it, the ground hides it. The ground costs no GPU time and has no latency.
* **Never hidden**: the player's car (the car instance within 1 500 units of player 1's car
  record position, `dynamics.md`), and any object on screen within `NEAR_KEEP` = 8 192 units
  of the eye (to its bounding sphere: instance position, model radius): drawn at full whatever
  the rule says.
* **Fades** as the statics' (`FrameData::dynamic_fade`, the same screen-door dither): an object
  that starts or stops being drawn on screen fades in over 8 / out over 4 simulation steps; off
  screen, newly listed (a puff spawned, a car listed again) or after a jump of more than 4 000
  units (a reused puff or mark) it takes its state at once.

**The floating detector for moving objects** (diagnostics: `SR_NATIVE_VIS_LOG`, or
`SR_NATIVE_DYN_TEST=1` without the log): every object on screen is tested like a static's base,
**test 3** (`backing.rs` `run_dynamics`, shaders `vs_dyn_base` / `ps_dyn_base`): after every
opaque static and before the opaque moving objects, its part instances' surfaces below its
base top (lowest point + 30 % of its height, at least 30 units; world height from the parts'
boxes, clipped per vertex with a clip distance) are drawn three times (area, visible, sky) and
counted per object (`FrameData::dynamic_backing`, `BackingResults::dynamics`). A drawn object
whose base shows the sky in at least half of its visible pixels is floating; the log splits
those with the world under them not drawn (floating in the rule's terms) from those with drawn
world under them (at a silhouette: the crest cars above). Verified by
`gpu_tests::moving_objects_are_tested_and_faded` (WARP: a box standing on the ground counts
area = visible, sky 0; one above the horizon sky = visible = area, and its base only, about a
third of its faces' pixels; one behind an occluder visible 0; a fade of 0 hides an instance,
the others unchanged, the test unchanged; no marks, no results) and the runtime's
`native::dynvis` tests (the ground under a point, a bridge over the road, blended surfaces not
ground; fades, near objects and the player's car never hidden; new and off-screen objects
take their state at once; the detector's bookkeeping of tags).

**Results** (the sweeps below: six tracks, two laps, four cameras, mirror on; before = this
branch with `SR_NATIVE_DYN_VIS=0 SR_NATIVE_VIS_SCALE=0 SR_NATIVE_VIS_NOLIST=last`, whose
statics' log lines are byte-identical to the first pass's runs on all six tracks, 12 097 lines
each; after = defaults; logs `work/agents/float/logs/<track>_before.log` / `_after.log`,
summaries by `work/agents/float/tools/dyn.py`, the runs by `tools/sweep.ps1`):

| track | view | frames with a car floating over undrawn world: before -> after | object-frames floating over undrawn world (any kind): before -> after | frames with a car at a silhouette (drawn world under it): before / after | near objects or the player's car not fully drawn | on screen not fully drawn per frame, after (cars / shadows / marks / smoke) |
|---|---|---|---|---|---|---|
| China | main | 133 (2.19 %) -> 0 | 396 -> 0 | 125 / 125 | 0 | 0.38 / 0.22 / 2.19 / 2.13 |
| China | mirror | 29 (0.50 %) -> 0 | 161 -> 0 | 89 / 89 | 0 | 0.09 / 0.04 / 1.91 / 0.70 |
| Canada | main | 168 (2.76 %) -> 0 | 891 -> 0 | 151 / 144 | 0 | 0.03 / 0.03 / 2.58 / 0.02 |
| Canada | mirror | 8 (0.14 %) -> 0 | 166 -> 0 | 7 / 8 | 0 | 0.00 / 0.00 / 0.53 / 0.02 |
| Italy | main | 174 (2.86 %) -> 0 | 943 -> 6 | 68 / 68 | 0 | 0.70 / 0.31 / 6.39 / 3.34 |
| Italy | mirror | 15 (0.26 %) -> 0 | 21 -> 0 | 1 / 1 | 0 | 0.06 / 0.03 / 1.06 / 0.30 |
| Arizona | main | 369 (6.07 %) -> 0 | 1 982 -> 0 | 213 / 187 | 0 | 0.14 / 0.08 / 4.61 / 0.90 |
| Arizona | mirror | 42 (0.72 %) -> 0 | 378 -> 0 | 99 / 99 | 0 | 0.09 / 0.07 / 2.56 / 0.74 |
| Sweden | main | 106 (1.74 %) -> 0 | 185 -> 0 | 111 / 36 | 0 | 0.20 / 0.15 / 2.59 / 0.20 |
| Sweden | mirror | 0 -> 0 | 0 -> 0 | 20 / 20 | 0 | 0.00 / 0.00 / 0.18 / 0.01 |
| Wales | main | 500 (8.22 %) -> 0 | 2 666 -> 0 | 138 / 156 | 0 | 0.98 / 0.50 / 6.19 / 5.05 |
| Wales | mirror | 90 (1.54 %) -> 0 | 563 -> 0 | 19 / 10 | 0 | 0.14 / 0.05 / 0.50 / 0.52 |

(6 083 main-view and 5 834 mirror frames per run, race frames from 1 450.) The 6 object-frames left
on Italy are tyre marks of 2 pixels within 7 830-8 185 units (near: protected). Before the fix of the cells without a list
(below) the same rule still left frames with a floating car on Canada (118, main view),
Arizona (31) and Wales (159 main, 19 mirror): all of the player's car or near cars
(protected), floating because the world around the car was not drawn at all; with that fix,
none. The silhouette frames are the detector's limit, not
floating cars (example above); "hidden" objects on screen are mostly the parked tyre marks and
smoke over undrawn ground. Frames: `work/agents/float/pairs/china_6750.png` (original, before,
after: the car in the sky is gone).

## The mirror (2026-11-21)

**Cause.** Of the mirror's detections in the first pass's runs, 99.6 % were bases of **one
visible pixel showing the sky** (China 460 of 465 object-frames, Canada 255 of 257, Italy 191
of 191, Arizona 1 078 of 1 078, Sweden 682 of 683, Wales 419 of 422; at most 2 per track with
2 or more sky pixels): statics kept after leaving the mirror's base (they recede into the
mirror instead of leaving the screen: 9-20 drawn beyond the base per mirror frame against
0.7-4 in the main view), 40 000 units away and more. The mirror's focal lengths are half the
main view's (128 x 153.6 against 256 x 307.2): the same object covers a quarter of the pixels,
and a base that is 2-4 pixels in the main view is 1 in the mirror, where `MIN_SKY_PIXELS` = 2
never calls it unbacked. Neither the one-frame latency of the results (frames with 2 or more
sky pixels: 0-2 per track) nor the mirror's own lists are the cause. (At 4K the same bases
would be 9 pixels and go: the threshold is in target pixels.)

**Fix.** `min_sky_pixels`: the minimum scales with the area of a view's pixels in the world
(the product of its focal lengths against the main view's), at least 1: main view 2 (as
before), mirror 1 (`the_mirrors_pixels_count_for_more`). `SR_NATIVE_VIS_SCALE=0` restores 2
everywhere.

| track | mirror: frames with a floating static, before -> after | object-frames | fading out while floating | main view (unchanged rule; after) |
|---|---|---|---|---|
| China | 350 (6.0 %) -> 0 (0.0 %) | 465 -> 0 | 1 679 -> 1 665 | 3 (0.0 %) |
| Canada | 192 (3.3 %) -> 0 (0.0 %) | 257 -> 0 | 1 411 -> 1 348 | 2 (0.0 %) |
| Italy | 147 (2.5 %) -> 0 (0.0 %) | 191 -> 0 | 485 -> 506 | 2 (0.0 %) |
| Arizona | 426 (7.3 %) -> 1 (0.0 %) | 1 108 -> 1 | 1 238 -> 1 204 | 20 (0.3 %) |
| Sweden | 511 (8.8 %) -> 2 (0.0 %) | 683 -> 2 | 1 286 -> 1 351 | 12 (0.2 %) |
| Wales | 272 (4.7 %) -> 1 (0.0 %) | 422 -> 1 | 1 264 -> 1 147 | 4 (0.1 %) |

(The after column includes the fix of the cells without a list; on China, Italy and the mirror
in general it changes nothing measurable: China and Italy have 3 and 8 frames without a list.)

## Cells without a list (2026-11-21)

**Found** by the moving objects' detector: after the rule above, the player's car still floated
on Canada (frames 4 885-4 970), Wales (4 893-4 935, 7 344-7 375) and Arizona (5 778-5 808):
the main view drew no road, no buildings, only the background and the ground colour, where the
original (Glide path, the game's own lists) draws the road and the village
(`work/agents/float/pairs/canada_4900.png`, `wales_7345.png`: original, before, after;
`nolist_canada_wales_4900_reach.png`: intermediate versions, top Canada with the nearest
ring's lists within 2 cells then 8, bottom Wales with the last list then the ring within 8).

**Cause.** The camera's cell has no list on 8-16 % of the frames on four tracks (Canada 533,
Arizona 936, Sweden 508, Wales 949 of 6 083 main-view frames; China 3, Italy 8), in stretches of
up to 403 frames (Canada 4 618-5 020: the track crosses cells without lists up to 6 cells from
the nearest list; `SR_NATIVE_VIS_CELLCHECK=1` logs it, and confirms that the cell computed from
the camera's position gives the engine's own list in every listed cell). The first version kept
the last list the camera had: on a long stretch it no longer covers the view ahead, and the base
on screen fell to 0 (main-view frames with fewer than 10 statics of the base on screen: Canada
145, Arizona 69, Wales 108).

**Fix.** `nearby_list`: the list of the nearest cell with one (within 8 cells) merged with the
last list the view had, plus the camera's cell. Main-view frames with fewer than 10 base statics
on screen: 0 on every track; the least in those stretches 15-71. Variants measured on Canada,
Wales, Arizona and Sweden: every list of the nearest ring of cells merged (main-view floating
statics on Sweden 25 frames instead of 11), the nearest cell's list alone (the mirror, looking
back where the last list was, lost its world when the nearest cell changed: Canada frames
4 863-4 880, near statics floating against the sky, kept because near). Statics floating in the
main view, before -> after: China 3 -> 3, Canada 2 -> 2, Italy 2 -> 2, Arizona 20 -> 20,
Sweden 11 -> 12, Wales 15 -> 4 frames. `SR_NATIVE_VIS_NOLIST=last` restores the last list.

## Edited lists (visibility editor)

(2026-11-22, agent "visrule"; the tool: `docs/05-visibility-editor.md`; the shared state:
`crate::visedit`.) The rule above depends on the view direction and on the frame (candidates
accepted by a GPU test), so a tool could not show what will be drawn. While the visibility
editor's edits are **active** (`visedit::edits_active`: in the tool, `--vis-editor`, or in normal
play for a track with a saved edits file `<data>\visibility\<track id>.vis`) the races draw the
edited lists instead: what the editor shows is what is drawn.

**The rule** (`native/frame.rs` `edited_visibility`, `native/visibility.rs` `Edited`), per view
(main view, mirror, second player: each with its own camera and state), every drawn frame:

1. **The camera's cell** as the automatic rule finds it (`7a2e4`: the game camera's x and z).
2. **Drawn**: exactly the statics of the cells in that camera cell's **effective list**
   (`visedit::effective_list`: the designers' list, or for a camera cell without one the list of
   the nearest cell with one within 8 cells; minus the sectors the user hid, plus those added)
   that lie inside the view frustum's **box of cells** (the game's draw distance:
   `StaticCells::in_list`). Nothing else: no candidates, no GPU backing test (`static_backing`
   empty: every static in the first opaque pass, nothing tested), no protection of near objects.
3. **Edits show at once**: the effective list is read again whenever `visedit::version()` (every
   edit, undo, load) or the camera cell changes, so an edit made in the editor's thread is drawn
   in the next frame (interpolated frames included). Verified with a test edit at frame 1 890
   (`SR_VISEDIT_TEST_EDIT`, below).
4. **Fades**: a static that starts being drawn while on screen fades in over `FADE_STEPS` (8
   steps, 0.24 s, as the other rules); one that stops while on screen fades out over
   `FADE_OUT_STEPS` (4 steps, 0.12 s: a camera cell change would otherwise pop; an edit is still
   seen at once); off screen both take their state at once. The fades run on the races' clock of
   simulation steps (`vis_clock`; interpolated frames between the steps).
5. **No effective list at all** (no list within 8 cells and nothing added): the base as the
   automatic rule takes it for a cell without a list (`nearby_list`: the nearest list merged with
   the last one, or the box). Not met on China (0 of 3 251 main-view frames).
6. A view whose rule changes (edits becoming active or not during a race: the first save of a
   track in the tool) starts the new rule afresh (its state reset).

**Moving objects** (`native/dynvis.rs`, unchanged): they read the frame's `static_fade`, the
edited one, so a car or shadow over a sector the user hid is hidden like over any undrawn world
(the player's car and near objects never). They do not see the blinking (below): a blinking sector
would otherwise make the objects over it fade in and out.

**Blinking** (`visedit::highlight`: the sector under the editor's mouse, blinking on or off): in
the main view, half of each period the sector's statics are **inverted**: drawn ones hidden,
hidden ones drawn at full (what a hidden sector holds), at `BLINK_HZ` = 3.5 periods per second of
real time (`time::uptime`: virtual in deterministic runs, so a scripted run repeats); every drawn
frame, interpolated ones included (`blink_inverted`, `invert`; applied to the frame's fades after
the rule).

**Live data** (tool mode only, no cost in normal play): every drawn main-view frame
`visedit::publish_live` with the game camera's position, its viewing direction on the ground
(the view's forward axis, unit length), its cell, the player's car and the statics drawn (fade
above 0, before the blinking). The car: the position of player 1's car record (`[1436d4]`,
records at `1296c8`, +52Ch; every camera); its heading from its instance's angles (model -z
forward, `transforms.md`; between the last two steps on an interpolated frame) as the moving
objects find it (`Dynamics::player_car`); the bumper camera lists no instance for the player's
car (`dynamics.md`) and looks where the car heads: the camera's direction then. The attract
race has no player: the camera's position.

**The map at a race's start** (`activate`, `visedit::TrackMap::from_scene`): it was built
before the grid's lists of the new world had been copied (`crate::visibility` copies them at the
first view's cell set-up): every map had the previous world's lists or none ("0 cells with a
list" in every race of the first runs), so its track id hashed only the number of statics and
every effective list was `None`. `crate::visibility::refresh_lists` now copies them first (the
same copy, once per track; the frames are unchanged). Measured on China (`SR_VISEDIT_MAPCHECK=1`):
300 cells with a list; built in 14.8-18.7 ms (0.5-0.6 ms in normal play); **22.1 MB** in the tool (81 446 triangles, 325
textures expanded to RGBA, the lists), 0.9 MB in normal play (statics and lists only); every one
of the 244 338 triangle vertices lies inside its static's bounding sphere (the farthest at 1.000
radii: the spheres are tight), i.e. the triangles are in world coordinates. The chroma key now
holds the colour bits only, as the renderer compares them. **Memory** (the 32-bit process is
large-address-aware: 4 GB on 64-bit Windows; the game's process sampled every 20 ms, deterministic
China race, `work/agents/visrule/runs/mem_*/mem.txt`): in the race 197-212 MB private and 1 202-1 232
MB virtual in the tool against 146-182 MB and 1 192-1 204 MB in normal play (the map and the live
data: +25-30 MB). Both modes show a transient of about 30 ms at the race's start (while the
scene is built and uploaded; not attributed further): 1 591 / 2 634 MB private / virtual in normal
play, 1 626 / 2 656 MB in the tool, i.e. 1.3 GB of the 4 GB left at the worst moment (present
without the editor; its map adds about 22 MB to it).

### Verification (China, deterministic, autopilot, mirror on)

Runs and captures in `work/agents/visrule/runs/<name>` (logs `runtime.log`), edit files in
`work/agents/visrule/vis/`, montages in `work/agents/visrule/pairs/`; keys
`12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER,40:M,75:F4,110:F2` unless noted. China's
track id: `bdc53f01da7fe2a8`.

* **Not active, byte-identical** (`ident_*`): no edits file, not the tool, keys
  `...,40:M,50:F1,60:F4`, `SR_SHOT` every 50 presents from 200 to 2 250 (menus, attract race,
  race with mirror and camera changes): main (8917b93, `work/agents/visrule/base`) against this
  branch, 42 of 42 frames byte-identical with the native renderer and 42 of 42 with the Glide
  path (`ident_new_*`); again with the final build (`ident_final_*`): 40 of 42 each, frames 1 050
  and 1 100 (the car selection) differing only in the real-time "F12 / BACK: SETTINGS" hint in the
  corner (3 189 pixels, x 1 014-1 259, y 682-707: shown by wall-clock time, as the second pass
  found).
* **The designers' lists** (`tool_noedits2`, the tool without edits, 130 s, two laps' worth of
  cells): the statics drawn equal the designers' list of the camera cell inside the box (the
  automatic rule's base) in 3 250 of 3 251 main-view frames and 3 000 of 3 001 mirror frames;
  the one frame each (2 420) is in camera cell (120, 125) without a list of its own, where the
  editor's default is the nearest cell's list (79 entries). Statics drawn per frame: mean 191.3
  (92-338) against the designers' 191.5 (the difference: fades). The tool without edits draws
  exactly as normal play with an empty edits file (`tool_ref` against `a_ref`, frames 1 880-1 900:
  21 of 21 byte-identical).
* **(a) A sector hidden** (`a_ref`: an edits file without edits; `a_edit`: `cam 130 120 -128,119`,
  the big pagoda left of the road): the captures of every frame 1 860-1 925 are byte-identical up
  to frame 1 879 (camera in cell (131, 120)); from frame 1 880, the frame the camera enters
  (130, 120), the pagoda's 10 statics fade out (targets 229 -> 219) and are gone at 1 883; at
  1 906 the camera enters (129, 120), whose list names the sector, and it fades back in; from
  1 912 the frames are identical again (pixels differing 0 ... 0, 4 183 at 1 880, up to 93 428 at
  1 897, 75 at 1 911, 0). `pairs/a_hide_pagoda.png` (top: lists, bottom: edited; frames 1 879,
  1 880, 1 881, 1 883, 1 890, 1 906): two roof pieces of the neighbouring sector (128, 120) stay,
  floating: what the user hides next.
* **(b) A far sector added** (`b_124_116`: `cam 130 120 +124,116`, about six cells ahead, not in
  the designers' list): 9 more statics drawn (204 -> 213), a far pagoda appears between the big
  pagoda and the blue-roofed hall (1 534 pixels at frame 1 890, 2 671 at 1 900;
  `pairs/b_add_far_sector_crop.png`, left lists, right edited). Three other far sectors tried
  (`b_125_116`, `b_124_114`, `b_125_118`) are drawn (targets +5, +1, +1) but hidden behind the
  near world (0-1 pixels): the designers' lists hold what can be seen from there.
* **Edit during the race** (`h_live_edit`: the tool, `SR_VISEDIT_TEST_EDIT=1890,130,120,128,119,0`):
  the camera in (130, 120) since frame 1 880; against the tool without the edit (`tool_ref`) frames
  1 887-1 889 are identical, at 1 890 the effective list loses the entry (79 -> 78 entries, targets
  204 -> 194) and the pagoda starts fading out in that very frame (10 366 pixels differ), gone at
  1 893 (62 936 pixels: the same as (a) at that frame).
* **(e) Blinking** (`e_blink_drawn`: highlight on the pagoda's sector 128,119, drawn;
  `e_blink_hidden`: on 124,116, not drawn; `SR_VISEDIT_TEST_HIGHLIGHT`): against the tool without
  a highlight, frames 1 880-1 883 and 1 889-1 893 differ (the pagoda hidden, resp. the far pagoda
  shown: the same pixel counts as in (a) and (b)), 1 884-1 888 and 1 894-1 895 are identical:
  inverted for 4-5 game frames, then as drawn (`pairs/e_blink_1886_1891.png`, top the drawn
  sector, bottom the hidden one). In real time with interpolated frames (`e_blink_interp`,
  `SR_INTERP=1`, 23 700 drawn main-view frames, 22 866 of them interpolated): 3.49 periods per
  second, inverted in 49.9 % of the frames, 150 of the 176 changes between two frames of the same
  game frame.
* **(f) Live data** (`f_live`, `SR_VISEDIT_LOG=1`, bumper camera, then far and close chase,
  then bumper): 3 251 main-view frames; the published camera cell equals the cell of the
  published camera position in all; the car's heading from its instance (2 215 frames, chase
  cameras) against its direction of travel over 5 steps: median cosine 0.984 (5th percentile
  0.864); the camera's direction standing in for it (bumper camera, 913 frames): 0.992 (0.888);
  every direction of unit length.
* **Unit tests**: `native::visibility` `edited_lists_draw_exactly_the_list_inside_the_box`,
  `edited_fades_in_on_screen_out_quickly_and_snap_off_screen`,
  `blinking_inverts_the_highlighted_cell_at_three_and_a_half_hertz`; `visedit`
  `the_map_has_world_triangles_inside_their_spheres` (a rotated, translated static: world
  triangle, bounds, chroma key, RGBA texels, the same track id with and without the picture,
  a vertex outside its sphere found).

### Cost

Real time, autopilot, interpolated frames on, 1 280 x 720, China, close chase camera from 36 s and
the mirror from 40 s (two views per game frame), 80 s, one run after the other (the machine was
shared with another agent's game runs: noisy); the every-5-seconds statistics of the race's
windows, median (range) (`runs/g_perf_*`):

| | main, automatic rule (two runs) | this branch, automatic rule | this branch, tool (edited lists) |
|---|---|---|---|
| CPU per view: the statics' visibility (`race_visibility`) | 0.143 (0.136-0.154), 0.157 (0.137-0.263) ms | 0.138 (0.131-0.150) ms | **0.039** (0.036-0.041) ms |
| CPU per view: building `FrameData` (visibility, moving objects, live data) | 0.313, 0.344 ms | 0.313 ms | 0.237 ms |
| CPU per view: submitting it | 0.250, 0.250 ms | 0.244 ms | 0.180 ms |
| views per second | 990, 957 | 1 044 | 1 193 |
| statics drawn per view | 205, 212 | 215 | 205 |

The edited lists cost a quarter of the automatic rule's CPU time (no candidates, no tests to
classify) and no GPU time (no backing tests: the automatic rule's 0.07-0.17 ms per view, measured
in the second pass, are saved); the live data and the effective list's lookups are within the
build time. Map at the race's start: 14.8-18.7 ms in the tool (China), 0.5-0.6 ms without the
picture. (A first tool run, `g_perf_new_tool`, stopped at the car selection: a scripted key fell
on a loading screen in real time; not counted.)

### Limits

* The box of cells is the game's draw distance: a sector added beyond it is not drawn from that
  camera cell (the editor's map shows it as drawn).
* An added sector hidden behind the near world costs its triangles and shows nothing (three of
  the four far sectors tried in (b)).
* The blinking draws a hidden sector's statics at full, floating or not, and only in the main
  view.
* The camera's cell is the game camera's, not the car's: with a chase camera the edits of the cell
  the camera is in apply (behind the car); a cell without a list there takes the nearest cell's
  list, as the editor shows (one frame in the China run above).
* The lists are copied once per track keyed by the grid's row table (its address and a hash of
  its row pointers, `crate::visibility::refresh`), not by the lists' contents: a later track whose
  grid landed at the same addresses would keep the previous lists (not seen; the track id and the
  edits would then be the previous track's).

## Verification

### Sweeps: the floating detector, fade-outs, distance (every track, both rules)

Runs: `SR_DETERMINISTIC=1 SR_AUTOPILOT=1 SR_NATIVE=1 SR_INTERP=0 SR_NATIVE_VIS_LOG=1`, window
1 280 x 720, draw distance 300 %, default car, keys
`12:ENTER,16:ENTER,20:ENTER,[21:DOWN ...],24:ENTER,28:ENTER,32:ENTER,40:M,75:F1,110:F2,145:F3,180:F4`
`--seconds 215` (the track list scrolls: 0 DOWN China, 1 Canada, 2 Italy, 3 Arizona, 4 Sweden,
5 Wales, one every 0.4 s from 21 s); race frames 1 450-7 532 (184 s: the whole first lap and
most of the second; the lap counter, read from the frames, turns at about frame 4 950 China
and Wales, 5 150 Canada, 5 350 Italy, 5 450 Arizona and Sweden); the mirror on from frame
~1 700; the four cameras in turn. Each track twice: the first rule (`SR_NATIVE_VIS=lists`,
as the user tested it, with its statics beyond the designers' list tested by the same GPU
test) and the new one. The **floating detector**: a static drawn beyond the designers' list,
on screen, whose last test shows the sky in at least half of its base's visible pixels. Logs,
per-lap tables and frames: `work/agents/vis/` (`logs/`, `vis_laps.md`, `pairs/`; summaries by
`table.py`, `vislog.py` there).

| track | view | floating frames: first rule -> new | floating object-frames: first -> new (fading out while floating, new) | fade-outs on screen: first -> new (any within 8 192 units: first / new; nearest new) | drawn beyond the designers' list per frame: first -> new | farthest drawn static, median / p90 (thousand units): designers' list -> first rule / new |
|---|---|---|---|---|---|---|
| China | main | 74.7 % -> 0.0 % | 113 554 -> 3 (51) | 673 -> 162 (0 / 0; 17.5 k) | 288 -> 0.8 | 58 / 65 -> 170 / 219 ; 58 / 66 |
| China | mirror | 64.2 % -> 6.0 % | 84 126 -> 465 (1 679) | 1 361 -> 1 113 (0 / 0; 17.0 k) | 217 -> 19.5 | 54 / 62 -> 158 / 217 ; 67 / 102 |
| Canada | main | 67.3 % -> 0.0 % | 110 794 -> 2 (113) | 1 834 -> 205 (0 / 0; 17.3 k) | 310 -> 0.7 | 60 / 68 -> 185 / 263 ; 59 / 66 |
| Canada | mirror | 52.1 % -> 3.3 % | 52 467 -> 257 (1 411) | 1 661 -> 1 163 (0 / 0; 20.7 k) | 230 -> 17.9 | 56 / 66 -> 148 / 225 ; 62 / 102 |
| Italy | main | 55.2 % -> 0.0 % | 74 431 -> 2 (90) | 1 017 -> 167 (0 / 0; 23.0 k) | 348 -> 0.8 | 52 / 62 -> 150 / 219 ; 52 / 62 |
| Italy | mirror | 53.4 % -> 2.5 % | 72 574 -> 191 (485) | 785 -> 374 (0 / 0; 10.3 k) | 281 -> 8.9 | 46 / 61 -> 146 / 198 ; 48 / 76 |
| Arizona | main | 48.6 % -> 0.3 % | 199 224 -> 22 (518) | 2 306 -> 328 (0 / 0; 17.4 k) | 304 -> 2.6 | 60 / 67 -> 170 / 247 ; 61 / 68 |
| Arizona | mirror | 45.2 % -> 7.3 % | 128 160 -> 1 108 (1 238) | 1 643 -> 864 (0 / 0; 8.2 k) | 211 -> 20.3 | 60 / 72 -> 149 / 235 ; 67 / 123 |
| Sweden | main | 61.3 % -> 0.2 % | 186 337 -> 11 (232) | 2 886 -> 233 (0 / 0; 16.4 k) | 290 -> 3.6 | 58 / 66 -> 162 / 237 ; 59 / 67 |
| Sweden | mirror | 47.3 % -> 8.8 % | 100 872 -> 683 (1 286) | 1 715 -> 841 (0 / 0; 19.2 k) | 233 -> 16.7 | 53 / 64 -> 154 / 232 ; 56 / 110 |
| Wales | main | 55.3 % -> 0.2 % | 127 446 -> 15 (184) | 2 056 -> 142 (0 / 0; 8.3 k) | 297 -> 4.0 | 58 / 65 -> 172 / 236 ; 58 / 67 |
| Wales | mirror | 54.1 % -> 4.7 % | 84 683 -> 422 (1 264) | 832 -> 871 (0 / 0; 16.8 k) | 235 -> 13.7 | 56 / 67 -> 139 / 217 ; 61 / 94 |

(6 084 main and 5 834 mirror frames per run. The "designers' list" distances are the new
run's; the first rule's runs fall back to the frustum box in list-less cells, so their own
base column is larger on Arizona and Wales: `vis_laps.md`.) Per lap (`vis_laps.md`) the
picture is the same in lap 1 and lap 2 on every track (first rule 38-76 % of frames with a
floating object, new 0.0-0.4 % main, 1.9-11.3 % mirror).

* **What is left floating (new rule)**: of the drawn (not fading-out) detections, the
  bases with at least 2 sky pixels are 0 to 3 object-frames per run and view (China main 1,
  mirror 0; Canada 0 / 1; Italy 0 / 0; Arizona 3 / 0; Sweden 1 / 1; Wales 2 / 2, one of them
  near, kept by `NEAR_KEEP`); the rest show a single sky pixel, at 21 000 units and more
  (a silhouette pixel: the rule needs 2 to call a base unbacked, the detector does not).
* **Fading out while floating**: statics of the designers' list that leave it while on
  screen and are floating: they start fading out in the frame they leave (at risk, tested
  beforehand) or one frame later, over 0.12 s. The first rule kept such statics for 0.24 s
  of fade, or for as long as an extended list or the grounded set held them (its
  detector-counted object-frames above).
* **Near objects never fade out**: 0 fade-outs within 8 192 units on screen in all 12 new
  views (by construction, logged to check it); the nearest at 8 193 units (Arizona, mirror)
  and 8 262 (Wales, main view), the others at 10 000 and more. Every
  fade-out of the new rule is of a floating base (logged with its distance).
* **Extra distance gained** over the designers' list: none worth the name in the main view
  (median of the farthest drawn static 58 -> 58 thousand units on China, the p90 at most
  +0 to +2 000), some in the mirror (the statics kept after leaving the list behind the
  car: median +2 000 to +13 000). The first rule's 150-260 thousand units were the floating
  objects. Why: the candidates on screen by their last verdict (main view, average per frame
  of the whole run, attract race included): China 52 no evidence (no base pixel in the view)
  / 0.00 backed / 49 unbacked / 453 mostly hidden behind the base world / 5 undecided; Canada
  81 / 0.01 / 70 / 515 / 5; Italy 59 / 0.00 / 21 / 459 / 3; Arizona 65 / 0.00 / 75 / 430 / 4;
  Sweden 59 / 0.00 / 69 / 440 / 3; Wales 59 / 0.00 / 68 / 388 / 5. The designers' lists
  already hold what can be seen; beyond the corridor's banks and backdrops the bases are
  hidden, and above them they are against the sky. (The statics drawn beyond the list in the
  table are mostly statics kept after leaving it, not accepted candidates.)

### The user's views, before and after

`work/agents/vis/pairs/<track>_<frame>.png`: the same deterministic moment drawn by the
first rule (left) and the new one (right), half size, at the first rule's worst moments:

* **Wales 3 850** (the first rule's worst stretch, up to 548 floating statics at frames
  3 843-3 852): a long piece of road and trees float in the sky above the hills, upper right
  of the picture; the new frame shows the sky there. This is the user's report reproduced.
* **China 6 750** (134 floating, among them 528 with 28 093 sky pixels at 37 000 units): a
  pagoda and a far hillside with its stretch of road at the upper left; gone in the new
  frame (the background picture's mountains instead). An AI car driving on that stretch was
  still drawn and floated there (as in the original with the game's own lists); since the
  second pass it is not drawn ("Moving objects").
* **Arizona 5 100** (465 at 5 109): rock formations and a dark column above the canyon's rim
  right of the road; gone.
* Sweden 5 700, Italy 7 100 and 3 550, Canada 4 450 and 3 950, China 4 850, Wales 4 650: the
  differences are smaller (trees and backdrops at the edges of the hills).

### Unit and GPU tests

* `native::visibility` (runtime): `verdicts_of_the_counts`, `candidates_are_drawn_when_backed_and_fade_in`
  (two results before a candidate is drawn, fade-in over 8 steps, older results ignored),
  `near_objects_on_screen_are_never_faded_out`, `accepted_candidates_leave_after_several_unbacked_results`
  (hysteresis; a hidden base keeps a drawn object), `statics_at_risk_are_tested_in_the_base_and_leave_at_once_when_floating`,
  `base_candidates_and_cells_at_risk`; the first rule's tests unchanged.
* `sr-render`: `backing_test_counts_bases_against_the_world` (above), `bases_are_the_lowest_part`.
* Second pass: `native::dynvis` (`the_ground_under_a_point`,
  `objects_on_undrawn_ground_fade_out_near_ones_and_the_player_never`,
  `new_and_off_screen_objects_take_their_state_at_once`,
  `the_detector_counts_drawn_objects_whose_base_shows_the_sky`), `native::visibility`
  (`the_mirrors_pixels_count_for_more`, `a_cell_without_a_list_takes_the_nearest_list_and_the_last`),
  `sr-render` `moving_objects_are_tested_and_faded`.
* `cargo test --release --offline -p sr-scene -p sr-runtime -p sr-render`: 59 + 41 passed (the
  `feature_costs` benchmark and one runtime benchmark ignored as before).

### Default behaviour and the menus

* **Switch off** (`SR_NATIVE=0`): the base commit (6c575ca, built from `git archive`) and
  this branch, integration.md's deterministic sequence (China, mirror, F1, F4, `SR_SHOT`
  every 50 presents from 200 to 2 250): 38 of 38 frames byte-identical.
* **Switch on**: the same sequence: frames 200, 350 and 450-1 400 (22, the menus) byte-identical
  to the base commit's native frames (the menus keep the first rule; their frames carry no
  backing marks, so the renderer draws exactly as before); 250, 300, 400 (the attract race)
  and 1 450-2 050 (the race) differ, as they should.
* Second pass (base commit 82b4c5e built from `git archive`, against this branch; keys
  `12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER,40:M,50:F1,60:F4`, `SR_SHOT` every
  50 from 200 to 2 250, `SR_DETERMINISTIC=1`): switch off, 42 of 42 frames byte-identical;
  switch on, 40 of 42 (frame 1 100, the car selection: only the "F12 / BACK: SETTINGS" hint
  in the corner differs, 3 194 pixels in x 1 014-1 259, y 682-707, a real-time overlay; frame
  2 000, the race: 62 pixels in the mirror, x 565-575, y 121-141, a far static the mirror's
  threshold now removes or an object over undrawn world).

### Cost

Real time, autopilot, interpolated frames on, VSync off, 1 280 x 720, China with the mirror
on (two views per game frame), `--log debug`; the machine was shared with other agents' game
runs (frame rates are noisy):

| | first rule (base commit) | new rule |
|---|---|---|
| CPU per view: building `FrameData` (of which the visibility) | 0.09-0.14 ms | 0.23-0.27 ms (0.15-0.16) |
| CPU per view: submitting it | 0.16-0.22 ms | 0.20-0.21 ms |
| GPU per view: the backing tests (`SR_NATIVE_VIS_GPU=1`, timestamp queries) | - | 0.07-0.13 ms on average, at most 0.39-0.96 ms (2 000-view windows) |
| statics tested per view | - | 390-565 |
| triangles drawn per view | 13 900-25 600 | 5 800-7 200 |
| presents per second, race with mirror (last 30 windows of 500) | median 461 (356-520) | median 565 (478-620) |

So the rule costs about 0.15 ms of CPU and 0.1 ms of GPU per view and still gains frame rate,
because it draws a third of the triangles (the first rule's floating far objects).

Second pass (the same setup, close chase camera from 36 s, mirror from 40 s, 75 s; base commit
against this branch, one run each after the other; `tools/perf.ps1`, `perf.py` and the logs in
`work/agents/float`):

| | base commit (82b4c5e) | this branch | this branch with the moving objects' detector (`SR_NATIVE_DYN_TEST=1`) |
|---|---|---|---|
| CPU per view: the moving objects' visibility | - | 0.004-0.016 ms | 0.004-0.017 ms |
| CPU per view: building `FrameData` (of which the statics' visibility) | 0.19-0.29 ms (0.12-0.16) | 0.19-0.32 ms (0.12-0.16) | 0.20-0.34 ms (0.13-0.17) |
| GPU per view: the backing tests (average, 2 000-view windows) | 0.08-0.14 ms | 0.07-0.17 ms | 0.46-1.07 ms |
| presents per second, race with mirror (last 30 windows of 500) | median 560 (514-618) | median 531 (321-612) | median 519 (377-597) |

The ground map is built once per race (China: 70 672 triangles in 25 ms). The rule itself costs
about 0.01 ms of CPU per view and no GPU time (a fade per instance, read in the vertex
shader); the presents per second differ within the spread between runs of the same build on
the shared machine (10-20 %). The detector (diagnostics only, off by default) costs 0.4-0.9 ms
of GPU per view: every moving object's surfaces drawn three times, the near cars' bases large
on screen and counted with one atomic counter each.

## Parameters

| Name (`native/visibility.rs`, `prepare.rs`) | Value | Why |
|---|---|---|
| `BASE_FRACTION`, `BASE_MIN` | 0.2, 100 units | the part that stands on something; flat objects whole |
| `ACCEPT_SKY` / `REJECT_SKY` | 10 % / 25 % (and at least `MIN_SKY_PIXELS` = 2) | a margin for silhouette pixels; between them nothing changes (hysteresis) |
| `VISIBLE_SHARE` | 50 % | a base mostly behind what is drawn proves nothing |
| `ON_RESULTS` | 2 | one result is not enough to start drawing (one-frame flukes) |
| `OFF_RESULTS`, `OFF_STEPS` | 2, 1 step | an accepted candidate is not dropped for one bad frame; a static kept after leaving the base goes at its first unbacked result |
| `NEAR_KEEP` | 8 192 units | one cell: nearer objects on screen are never faded out |
| `FADE_STEPS` / `FADE_OUT_STEPS` | 8 / 4 steps (0.24 / 0.12 s) | "a few tenths of a second" in; out quickly when floating |
| `FLOATING_SKY` (detector only) | 50 % | what the verification counts as floating |
| `REFERENCE_SCALE` (`min_sky_pixels`) | 256 x 307.2 | the main view's focal lengths: `MIN_SKY_PIXELS` scales with a view's pixel area against them (mirror: 1) |
| `NOLIST_REACH` | 8 cells | the farthest a camera cell without a list was measured from one: 6 cells (Canada) |
| `SUPPORT_DROP` / `SUPPORT_RISE` / `SUPPORT_LAYER` (`dynvis.rs`) | 4 000 / 300 / 100 units | what lies under a moving object: jumps (up to 1 650 measured), positions under the surface drawn (shadows -2, at most -300 measured), seams and overlapping statics |
| `REQUERY` | 64 units | an object's statics under it looked up again after this move (a car moves 450 units per step) |
| `BASE_FRACTION`, `BASE_MIN` (`dynvis.rs`, detector only) | 30 %, 30 units | a moving object's base: wheels and lower body; flat decals whole |

## Limits and unknowns

* **Little extra distance.** On every track the candidates' bases are, in the main view,
  almost all either hidden behind the corridor's banks, panels and backdrops (the designers'
  lists already hold what can be seen) or against the sky; very few lie in front of the
  base world (numbers above). So the new rule draws, in practice, the designers' lists:
  the 300 % draw distance adds nothing for statics. The first rule's "far" objects (150-250
  thousand units) were exactly the floating ones.
* **Objects on a skyline are not added**: a tower standing on a drawn hilltop seen from
  below has its base at the silhouette, against the sky, and is rejected (conservative: the
  original game does not show it from there either). Telling "stands on the silhouette"
  from "floats above it" would need the depth of what is under the base (not done).
* **Floating objects of the base are drawn**, as the designers' lists show them (rule 1);
  the detector does not count them (it would need a test of the base against itself). When
  they leave the base they go within one frame (at risk) or two (not predicted), fading out
  over 0.12 s; the detector counts those frames separately ("fading out while floating").
* A static drawn as accepted candidate and later found floating is drawn for at least two
  results plus the fade (hysteresis). Observed: candidates accepted in a whole run (both
  views) China 6, Canada 43, Italy 2, Arizona 8, Sweden 2, Wales 16; object-frames in which an
  accepted one was floating: 1, 1, 0, 0, 1, 2.
* Chains are two deep: a candidate can be backed by the base and by candidates accepted
  against the base (pass 1), not by candidates of pass 2.
* The base footprint includes back faces and transparent texels of colour-keyed polygons
  (a tree's crossed quads): it is the silhouette of the lowest fifth, not its visible
  texels.
* The CPU cost (0.15 ms per view) is the per-view recomputation of the base, the candidates
  of the track in view and the state of every static; it could be cut by updating only on
  cell changes and for on-screen statics (not done). The tests cover 400-600 statics per
  view, almost all hidden or against the sky; testing fewer (e.g. not re-testing a
  candidate unbacked for many frames) would save some GPU time (not done).
* **Moving objects** (second pass): drawn only where the world under them is drawn. A car
  over drawn world can still show the sky behind its lowest part at a crest seen from below
  (the detector's "silhouette" frames, 0.0-3.1 % of the frames per track and view, which the
  rule leaves drawn; inspected on China 2 884: on the road at the crest); the detector cannot tell that
  from floating, the rule does not need to. Near objects and the player's car are never
  hidden, so where the world under them is missing they float (none left in the sweeps since
  the fix of the cells without a list). The player's car is found by position (`dynamics.md`);
  a car record that moved away from its instance (not seen) would leave it to the near rule.
  The detector's base is the parts' boxes, not their triangles.
* **Cells without a list** (second pass): the base there is the nearest cell's list merged with
  the last one; their floating statics are drawn like any base's and not counted by the
  detector. When the camera re-enters a listed cell, statics of that larger base leave it and
  are kept while on screen: their far ones with a one-pixel base are the main view's remaining
  detections on Sweden (12 frames, 0.2 %).
* **Thresholds in target pixels**: `MIN_SKY_PIXELS` scales with the view's focal lengths, not
  with the window's resolution: at 4K the main view's bases have 9 times the pixels of the
  1 280 x 720 sweeps and more one-pixel bases count as evidence (not measured).
* Split screen (view 1): coded (its own state), not run.

## Files

| File | What |
|---|---|
| `crates/sr-scene/src/lib.rs` | the contract additions (below) |
| `crates/sr-runtime/src/native/visibility.rs` | `Backed` (the rule, per view), `classify`, `floating` (detector), `StaticCells::base / candidates / at_risk`, unit tests; the first rule kept for the menus and `SR_NATIVE_VIS=lists` |
| `crates/sr-runtime/src/native/frame.rs` | `race_visibility`: sets, results routing, clock, the list-less fallback, statistics and `SR_NATIVE_VIS_LOG` |
| `crates/sr-runtime/src/visibility.rs` | the camera cell's own list read before any merge; every cell's list kept for native |
| `crates/sr-runtime/src/glide/mod.rs`, `crates/sr-render/src/d3d.rs` | `Backend::native_backing` (default none; one forwarding line) |
| `crates/sr-render/src/native/backing.rs` | the GPU test (bases, passes' tests, counts, readback) |
| `crates/sr-render/src/native/renderer.rs`, `native.hlsl`, `prepare.rs` | opaque statics in three passes, the tests between them, `vs_base` / `ps_base`, `build_bases` |
| `crates/sr-runtime/src/native/menus.rs` | one field (`GameCells::own`) |
| `crates/sr-runtime/src/native/dynvis.rs` | second pass: the moving objects' rule (`GroundMap`, `DynVis`), their detector's bookkeeping, unit tests |
| `crates/sr-runtime/src/native/dynamics.rs` | second pass: the objects of a view handed to `dynvis`, fades and test marks per part instance, the player's car, the `native dyn:` line |
| `crates/sr-runtime/src/native/visibility.rs`, `frame.rs` | second pass: `min_sky_pixels`, `nearby_list`, `Frustum` public; `RaceCtx` for the moving objects, results routed to them, `SR_NATIVE_VIS_CELLCHECK`, `cell_list` in the log |
| `crates/sr-render/src/native/backing.rs`, `renderer.rs`, `native.hlsl`, `gpu_tests.rs` | second pass: test 3 (`run_dynamics`, `vs_dyn_base` / `ps_dyn_base`, `dyn_tops`), the moving objects' fades (`V_DYN_FADES`, the instance transform's `extra.w`), the coverage target bound again after a test (render scale), `moving_objects_are_tested_and_faded` |
| `crates/sr-runtime/src/native/frame.rs` | third pass (edited lists): `edited_visibility` (the rule, the effective list per slot by version and cell), `vis_clock` (the clock shared with `race_visibility`), the blinking and `publish_live` in `draw_view`, `refresh_lists` before the map, the test hooks |
| `crates/sr-runtime/src/native/visibility.rs` | third pass: `Edited` (fades), `StaticCells::in_list` / `in_cell`, `blink_inverted`, `invert`, tests |
| `crates/sr-runtime/src/native/dynamics.rs` | third pass: `Dynamics::player_car` (the live data's car), `player_position` visible to `frame.rs` |
| `crates/sr-runtime/src/visibility.rs` | third pass: `refresh_lists` (the grid's lists before the first view) |
| `crates/sr-runtime/src/visedit.rs` | third pass: the chroma key's colour bits, `TrackMap::bytes` (the map's size, logged), `TrackMap::check_spheres` (`SR_VISEDIT_MAPCHECK`), a test of the export |

## Diagnostics

* `SR_NATIVE_VIS_LOG=1`: per view of the game's own frames, `native vis: frame F slot S
  base B cand C tested T extra E accepted A floating N [static@distance:sky/visible ...]
  fadeouts N [static@distance ...] far base all own 0|1 floating_accepted N verdicts [none,
  backed, unbacked, hidden, undecided] leaving N nearest_fadeout D` (slot 0 main, 2 mirror;
  "extra" = drawn beyond the base on screen; "leaving" = floating while fading out).
* `SR_NATIVE_VIS=lists`: the first rule in races (as the user tested it), with every static it
  draws beyond the designers' list tested (the floating detector only).
* `SR_NATIVE_VIS_GPU=1`: GPU time of the tests, logged every 2 000 views.
* The every-5-seconds statistics line now gives the visibility's CPU time per view and the
  statics drawn beyond the base, tested and floating.
* Second pass: the `native vis:` line ends with `cell_list 0|1` (the camera's cell has its own
  list). With `SR_NATIVE_VIS_LOG=1` the moving objects' detector runs and logs per view
  `native dyn: frame F slot S objects N onscreen O hidden H hidden_cars C floating N
  [kind:instance@distance:sky/visible(g|u) ...] floating_cars N ungrounded N leaving N
  near_hidden N player_hidden N unsupported N hidden_backed N player 0|1 by_kind hidden [car,
  shadow, marks, smoke] unsupported [..] floating [..]` (`g`: drawn world under it, a
  silhouette; `u`: not; "ungrounded": floating cars with no drawn world under them;
  "hidden_backed": objects not drawn whose base the test finds backed). `SR_NATIVE_DYN_TEST=1`:
  the test without the log. `SR_NATIVE_DYN_VIS=0`: moving objects drawn wherever they are.
  `SR_NATIVE_VIS_SCALE=0`: `MIN_SKY_PIXELS` in every view. `SR_NATIVE_VIS_NOLIST=last`: the
  last list in a cell without one. `SR_NATIVE_VIS_CELLCHECK=1`: the camera's cell against the
  engine's list, and the nearest cell with a list. `SR_DYN_VIS_PROBE=<frame>`: every moving
  object of that frame (position, support, fade, counts) and the player's car record. These
  three `=0` / `=last` switches together reproduce the first pass's statics exactly (above).
* Third pass (edited lists): with `SR_NATIVE_VIS_LOG=1` a view under the edited lists logs
  `native vis edit: frame F slot S cell (r, c) list L targets T onscreen O drawn D
  drawn_onscreen DO fading N designers B designers_onscreen BO same_as_designers 0|1 own_list
  0|1` (L: entries of the effective list or `none`; "designers": the designers' list of the cell
  inside the box, the automatic rule's base), and the main view at each new camera cell
  `native vis edit cell: frame F cell (r, c) list [r,c ...] extend3 [r,c ...]` (the effective
  list's cells and what the editor's Extend with radius 3 would add, cells with statics only).
  `SR_VISEDIT_LOG=1`: the live data per main view of the game's frames in the tool (`visedit:
  frame F uptime U camera .. forward .. cell .. car .. forward .. (position record|instance|camera,
  heading instance|camera) drawn N blink 0|1 highlight ..`); `=2`: every drawn frame,
  interpolated ones marked `extra`. `SR_VISEDIT_MAPCHECK=1`: the map's triangles against their
  statics' spheres at each race's start. Test hooks: `SR_VISEDIT_TEST_HIGHLIGHT=row,col` (the
  editor's highlight, blinking), `SR_VISEDIT_TEST_EDIT=frame,cam_row,cam_col,row,col,0|1` (an
  edit through `visedit::apply` at that game frame).

## Contract additions (`sr-scene`, additive; for the organiser)

* `FrameData::static_backing: Vec<u8>` (per static: pass in bits 0-1, `BACKING_TEST1` = 4,
  `BACKING_TEST2` = 8; empty = every static in the first pass, nothing tested: the menus and
  every existing caller) and `FrameData::backing_tag: u64`.
* `BackingCounts { area, visible: [u32; 2], sky: [u32; 2], tested }` and
  `BackingResults { tag, view, statics: Vec<(u32, BackingCounts)> }`.
* `Backend::native_backing(wait) -> Vec<BackingResults>` (default: none), forwarded by the
  D3D backend in one line.
* The `FrameData::static_fade` documentation ("the game's own visibility lists plus objects
  with ground under them") and `docs/04-native-renderer.md` ("Scene description",
  "Visibility" bullet) describe the first rule; they should point here.
* Second pass: `FrameData::dynamic_fade: Vec<u8>` (per `dynamics` entry, as `static_fade`;
  empty = all drawn), `FrameData::dynamic_backing: Vec<DynamicBacking>` with `DynamicBacking
  { object: u32, base_top: f64 }` (`NONE`: not tested; empty = nothing tested) and
  `BackingResults::dynamics: Vec<(u32, BackingCounts)>` (per object: `area`, `visible[0]`,
  `sky[0]`, `tested` = 1). Every existing caller unchanged (empty vectors).
* Proposed for `docs/04-native-renderer.md`, "Visibility" bullet: replace "Open: AI cars on
  undrawn road float; the mirror's remaining floating frames." by "Moving objects are drawn
  only where the world under them is drawn (never the player's car or a near object hidden):
  frames with a car floating 1.7-8.2 % -> 0; mirror 2.5-8.8 % -> 0.0 % (thresholds scaled for
  its pixels); a camera cell without a list takes the nearest cell's list (the view no longer
  empties on Canada, Wales, Arizona)."
