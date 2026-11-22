# Near-camera fidelity (wave 3, topic W3-2)

Why the native world and the game's differed beside the camera (the frame-2100 wall of the
Shield run, the far chase camera's "extra statics" at frame 2050), what the engine does with
polygons near, across and behind its near plane, and what the native renderer now does.
Link addresses as in `docs/re/strt3fx.md`. Diagnostic: `crates/sr-runtime/src/native/near.rs`
(`SR_NEAR_CHECK`); fix: `crates/sr-render/src/native/native.hlsl` (one statement).

## Summary

* **Cause: a native renderer bug, not a difference in geometry, visibility or culling.** The
  vertex shader wrote the depth as clip-space z = `DEPTH_SCALE * z / max(zd, 1e-6)` (view
  depth z; zd = z, or the decal's pulled depth). For a vertex **behind the eye** (z <= 0) that
  is a huge negative number (the frame-2100 wall's vertex at z = -93.6: about -366 000),
  and the hardware's depth clip (0 <= z_clip <= w, `DepthClipEnable`) then cuts every
  triangle with such a vertex off at its front vertices. The polygon vanishes almost
  completely, and whatever lies behind it shows: the forest backdrop through the frame-2100
  wall, the surfaces under the road and a giant tree at frame 2050. Such polygons are common
  beside a chase camera: world polygons reaching behind the eye are on screen in 699 of 1011
  frames of the Shield run and 550 of 865 of the China run.
* **The engine clips at the same near plane as native** (view z' = 0, 128 units in front of
  the eye), with an exact Sutherland–Hodgman clipper. Its decisions were predicted with
  0 errors for 403 880 polygons of parts reaching the near plane, and for the polygons it
  draws its screen coverage is the native one (the frame-2100 wall's two polygons: engine
  24 649 and 38 903 grid px^2, native 24 543 and 38 976).
  Its errors are rare: polygons dropped when a clipped vertex overflows 16 bits, polygons
  stretched when an unclipped vertex's projection overflows (visible once: a green wedge
  in frame 2496 of the Shield run), and back faces judged from the view origin rather than
  from the eye (negligible area).
* **Decision**: native draws the true geometry clipped at the game's near plane (the
  contract's `near` = 128, unchanged) and does not copy the engine's errors. **Fix**: the
  depth is now written so that clipping stays exact for vertices behind the eye (world:
  `DEPTH_SCALE`; decals behind the eye: `4 * DEPTH_SCALE`, the limit of z / zd).
* **Verified** (details in "Before and after"): a GPU test (a wall and a decal floor
  reaching behind the eye: every pixel missing before, none after); both reproductions fixed;
  sweeps of 337 and 289 frames: frames with more than 5 % of their edge columns strongly
  different from the original 18 + 7 -> 0, with their bottom rows strongly different
  33 + 35 -> 0, worst frame 25.5 % -> 3.8 % strong pixels; no frame worse. Switch off:
  38 of 38 frames byte-identical to the base build.

## Reproductions

Data folder `work/agents/near-data` (the template: 1280 x 720, draw distance 300 %, extended
visibility), `SR_DETERMINISTIC=1 SR_AUTOPILOT=1 SR_INTERP=0`, frames with
`SR_SHOT=<dir>,<every>,<first>,<last>`, `SR_NATIVE=0` (the original, the Glide path) or `1`:

* **A, the Shield** (camera F3, mirror): keys
  `12:ENTER,16:ENTER,20:ENTER,24:ENTER,26:DOWN,26.4:DOWN,26.8:DOWN,27.2:DOWN,28:ENTER,32:ENTER,36:F3,38:M`,
  `--seconds 66` (sweep: 80). Frame 2100: the stone wall at the right edge missing natively,
  dark trees and a blue backdrop instead; frame 2050 of the same run: a dark surface over the
  road at the bottom left.
* **B, China** (the integration agent's run: mirror, bumper camera F1, far chase F4): keys
  `12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER,38:M,42:F1,47:F4`, `--seconds 52`
  (sweep: 60). Frame 2050: a dark surface over the road at the bottom right and a dark strip
  at the end of the wall at the right.

Images (not in the repository, `work/agents/near/`): `shield_2100.png`, `shield_2050.png`,
`china_2050.png` (original | native before | native after, with difference panels x 4).

## The engine near the camera

Every claim here was read from the code (`srtool dis`, `work/engine_dis.txt`,
`work/glue_dis.txt`) and, where marked **measured**, checked by `SR_NEAR_CHECK` over both
sweeps (1 876 frames, every world part of the main view).

### Objects (`82130`, read from the code)

The object list's culling: the object's position minus the camera's is rotated by the
camera rows `820b4` patched in as Q13 immediates (`sar 13`: exact view coordinates x', y',
z'), r = the model's radius ([model + 0]). The object is skipped when `z' - r > 0` (the
sphere wholly behind the near plane), when `z' + r <= -D` (`[cf470]`, beyond the far
distance), or when the sphere's extreme points projected at the sphere's nearest depth
`z' - r - 128` with half the focal length (`(x' -+ r) * 2 * 800000h / (z' - r - 128) >> 17`)
fall wholly to one side of the view (`[81e40]`, `[81e28]` in x, `[81e44]`, `[81e2c]` in y).
Projecting at the nearest depth makes the test conservative. Not checked in numbers; no
object the game skipped showed up as a difference in the sweeps after the fix.

### Parts: the list builder's three paths (`7ac50`, measured)

`7ad1c`..`7ad46`: s = the view z' of the part's origin (draw record +144h, the translation
of the camera-space matrix +118h) plus the part's radius (level data +0):

| condition | path | what it does per polygon |
|---|---|---|
| `z' + r >= -8` | **near** (`7adf0`, loop `7ae3a`) | back face; side test; near classification |
| `z' - r <= [cf470]` (-D) | far (`7b0b8`, loop `7b0fa`) | back face; far test (`7b15f`, -D patched) and the far clipper (variant C) |
| otherwise | plain (`7ad4c`, loop `7ad8a`) | back face only: every vertex is in front |

Measured: 232 860 + 199 071 part drawings, 12 400 + 11 207 of them on the near path,
none on the far path (draw distance 300 %, D = 150 000).

### Back faces (`7ad8a`.., all paths, measured)

`(v0 - c) . n >= 0` keeps the polygon (v0 the first vertex's model coordinates, n the
record's normal +14h, 32-bit products). c is patched into the three paths' `sub r, imm32`
(`7ada3`/`7ada9`/`7adaf`, `7ae57`/`7ae5d`/`7ae63`, `7b117`/`7b11d`/`7b123`) from the view->model
matrix's translation (draw record +16Ch..+174h) plus the level's int16 centre (level +4).
Measured in view space: c lies at z' -7.5 .. +4.0 and within 20 units in x', y': **the view
origin**, 128 units in front of the eye, not the eye. A polygon whose plane passes between
the two is judged differently by the game and by the screen winding native culls. Area
native draws that the game culled this way (near-path parts; W2-1's `extract.md` measured the
others): 2 010 + 3 461 grid px^2 over the 1 876 frames (about 3 px^2 per frame): negligible.

### The near path's side test (`7ae87`..`7aff2`, measured)

With d = record +20h and T the table read backwards from `aa3a4` (`[aa3a4 + 4 k]`, k < 0),
a polygon is kept when some vertex has `z' - d < 0` and `|x'| <= T[z' - d] + d` (four vertex
slots), or `|y'| <= T[z' - d] + d` (three slots; the fourth slot's "y" test reads its x,
`gs:[ebx]` at `7afdb`: a slip in the game). View positions come through segments: fs = the
view z' array (68fd0), gs = x', y' (68fc8). Measured: 58 + 266 polygons dropped by it, of
which native draws 0 px^2 on screen: the test never removes anything visible.

### Near classification (`7b010`..`7b0a5`, measured)

All vertices z' < 0: kept as they are (variant 0). All z' >= 0: dropped. Mixed: the type's
near clipper `[798e8 + type]` (variant 8), which writes a variant-4 entry.

### The near clipper (`ced48` textured, `cfaf0` flat, `d4de8` reflection; measured)

Sutherland–Hodgman against the plane z' = 0 over the record's closed vertex loop, into the
buffer at `[7050c]` (`70510`..): per output vertex 16 bytes `z, z, packed x << 16 | y,
u | v << 16`:

* a vertex in front (z' <= 0) is copied with its packed position from the transform
  (`6ed94 + off`) and its u, v;
* an edge crossing the plane adds the intersection: `t = -z'_c / (z'_p - z'_c)` (the
  `add ecx, 0` at `ceebb` / `ceff4` is the plane), y and x interpolated in integers, then
  projected at z' = 0: `X = 13Fh (+ e, patched) - 2 x`, `Y = cy - 2 y` (mirror: `cy - y`),
  as `(v << 7) / [79ecc]` (-128) doubled with 16-bit `add ax, ax` / `add ax, imm`; u, v
  interpolated; z = 0 (w = 128);
* a 16-bit overflow of X or Y sets `[cf46c] = -1` and **the whole polygon is dropped**; so
  is a result with fewer than 3 vertices;
* otherwise the list entry becomes `(material | 4, buffer)` and the variant-4 set-ups
  (`ca4a0` and copies) draw it.

The 3D module's own clipper `79c58` never ran for a world part in these runs (a polygon it
clipped would have shown up as a misprediction: 0).

### Projection overflows (`7aa00`, measured)

`r = 800000h / (z' - 128)`; y = `(y' r) >> 15 + cy`, 32-bit, **stored in 16 bits**: wraps for
|Y| >= 32 768; x = `2 x' r + C << 16`, an overflow of either addition gives `BEHIND`
(10001000h) **also for a vertex in front of the plane** (the product `x' r` itself is not
checked: it wraps when |x'| > 256 |z' - 128|). The set-ups read `BEHIND` as grid
(4096, 4096). Measured: 100 + 488 vertices in front with an x overflow, 1 with a wrapped y;
23 + 73 polygons kept or clipped by the list builder with such a vertex.

### Set-ups and the 2D clipper (`ca670` / `ca4a0`, `ded50`; read from the code)

The set-ups reject a polygon whose x or y extent is 7FF0h or more (`ca7e5`..`ca80f`,
`ca5a2`..`ca5cd`) or that lies wholly outside the view; one crossing the view's edges goes
to `ded50`: Sutherland–Hodgman in screen space against x = 0 (`dedd2`..), y = 0 (`df015`..),
then the right edge (`27Fh` + 2e, patched at `df25a` by `widescreen.rs`) and, presumably,
the bottom one (the view height, stored at `df788` by `ced18`; not followed), on 32-bit
integer positions, u, v and depth interpolated in floating point; fewer than 3 vertices:
dropped.
It works on whatever positions it is given: a (4096, 4096) vertex is clipped like any other.

### The engine's errors near the camera (measured, both sweeps)

| error | count | effect | seen in the original frames |
|---|---|---|---|
| clipper overflow (16-bit X or Y of an intersection) | 1 962 + 1 725 polygons | dropped: a hole; native draws 30 630 + 40 171 grid px^2 there (more than 100 px^2 in 23 frames) | no: the largest are backdrop panels (`a)011end`, `a)012end`); those frames differ from the original by texel noise only after the fix (Shield 1752, 2067, 2121: 2.6, 3.3, 2.2 % strong pixels); in China 2097 the panel lies behind the wall |
| projection overflow of a vertex in front | 23 + 73 polygons kept or clipped with one | stretched toward (4096, 4096) unless the extent test rejects them (China 1831: rejected) | yes, Shield frame 2496: a green wedge left of the road (peak `a57vetta`, record 1171be4h, vertex at z' = -21, x' = 21 355); not in China 1981 (a tree, not visible) |
| back-face point = view origin | | 2 010 + 3 461 px^2 | no |

## Why the native picture differed

* **The geometry and its clipping agree** (measured, frame 2100 of A, the region x 1000-1280,
  y 180-560 of the window = grid x 667..854, y 100..311): the polygons covering it are the
  same in the game and natively, with the same coverage: the wall `am994s` (records
  110641a0h clipped, 110641c4h kept), the road `ax17s`, and behind them the forest backdrop
  `a)01for` (kept, z' -1 657 .. -19 005). Outlines of the native and the game's polygons over
  both frames: `work/agents/near/c1/ol_native_2100.png`, `ol_game_2100.png`.
* **The lost polygons all reach behind the eye**: 110641a0h has a vertex at z' = 221.6
  (renderer view z = 128 - 221.6 = -93.6), 1106417ch at z' 221.6 and 1103.1; in B's frame
  2050 the road `ax12s` 10bb0234h (z' 269, 181) and the wall `am989s` 10f01c74h (z' 335);
  in A's frame 2050 the road `ax1471s` 10f2819ch and 10f280c4h (z' 176, 411). What showed
  through instead: the backdrop (A 2100); a giant tree's panel `a3tree` (record 112780c4h,
  drawn by both, its lower corner 1 155 units below the camera, under the road: A 2050);
  surfaces under the road and behind the wall (B 2050: statics of the game's own cells, the
  integration agent found with `SR_NATIVE_GROUNDED=0`; not identified further). So the
  integration agent's "statics beside the camera drawn natively but not by the game" were
  seen through the holes the lost road and wall polygons left; after the fix the road and
  the wall cover them as in the original.
* **The renderer's depth**: `o.pos.z = offset.w * z / max(zd, 1e-6)` gives such a vertex
  `(1 / 256) * -93.6 / 1e-6` = -365 600 while its w = z = -93.6; the depth clip
  `0 <= z_clip` crosses zero within 1e-8 of the front vertex along each edge, so the
  rasteriser keeps only a sliver. Reproduced by the GPU test below (WARP): a wall from 500
  units behind the eye to 3000 in front: 34 822 of 34 822 expected pixels missing; the same
  wall starting in front of the near plane: drawn completely (control).

## Decision

* Native draws **the true geometry, clipped at the game's near plane**: `Projection::near` =
  128 (view z' = 0), as the game's clipper; the camera and projection (`frame.rs`) stay as
  they are. Moving the plane nearer would show geometry the game never shows (between the
  eye and z' = 0); farther would cut what it shows.
* It does **not copy the engine's errors**: no holes where the clipper overflows, no stretched
  polygons where the projection overflows, back faces by the eye's view (the screen
  winding) rather than from the view origin. Each is rare, and the one visible case (the
  wedge of frame 2496) is an artefact.
* So the fix is in the renderer: clipping must stay exact for vertices behind the eye.

## The fix

`native.hlsl`, `vs_main`: the clip-space z is `offset.w` (= `DEPTH_SCALE`) for the world
(exactly the value `offset.w * z / z` gave for every vertex in front of the eye), and for
decals `offset.w * z / zd` in front of the eye, `4 * offset.w` behind it (the limit of z / zd
as z -> 0, zd = max(z - pull, z / 4)). Both are finite and between 0 and w on the part of
every edge in front of the near plane, so the hardware clips at `SV_ClipDistance0`
(z - near) only and the depth after the divide is `DEPTH_SCALE / z` (`DEPTH_SCALE / zd` for
decals) as before. The decal branch matters for wave 3's native shadows and tyre marks under
the camera.

## Before and after

* **GPU test** `polygons_through_the_eye_plane_are_clipped_at_the_near_plane`
  (`crates/sr-render/src/native/gpu_tests.rs`, WARP, 320 x 240): every pixel whose centre's
  neighbourhood lies wholly on the polygon (CPU reference by ray casting) must be drawn,
  none wholly off it. Before: the wall 34 822 of 34 822 missing, the decal floor 35 520 of
  35 520 missing (the control wall 0). After: 0 missing, 0 drawn outside, for all three. The
  other 26 renderer tests pass (among them the depth agreement with the Glide path).
* **Reproductions**: A 2100 (the wall at the right edge), A 2050, B 2050: the difference
  panels after the fix show only the texel-edge noise of every hybrid frame
  (`shield_2100.png`, `shield_2050.png`, `china_2050.png`).
* **Sweeps** (every 3rd frame; original vs native; per frame: strong pixels = max channel
  difference > 64; an edge column = one of the window's outer 160 columns on either side,
  strongly different when more than 25 % of its 720 pixels are; bottom rows likewise for the
  last 160 rows; tool: `edgecmp` in the agent's scratchpad, the per-frame tables in
  `work/agents/near/sweep_*/cmp_*.tsv`):

  | run | frames | frames with > 5 % edge columns different | frames with > 5 % bottom rows different | strong pixels per frame, mean / p95 / max | mean abs. difference, mean / max |
  |---|---|---|---|---|---|
  | A before | 337 (1551-2559) | 18 | 33 | 3.02 / 6.64 / 25.5 % | 7.9 / 31.3 |
  | A after | | **0** | **0** | 2.23 / 3.51 / 3.80 % | 7.2 / 11.9 |
  | B before | 289 (1500-2364) | 7 | 35 | 3.36 / 5.94 / 14.8 % | 8.9 / 20.5 |
  | B after | | **0** | **0** | 2.80 / 3.73 / 4.19 % | 8.3 / 12.2 |

  After the fix every frame has 0.31 % of its edge columns different: one column, the
  window's last (x = 1279), which the native view leaves black in every frame (the Glide
  path draws it; not a near-plane matter, noted for the renderer). No frame got worse:
  the largest increase of strong pixels from before to after is 0.00 %. 178 of 337 and 155
  of 289 frames are unchanged by the fix (mean difference 0.000), the others carry polygons
  reaching behind the eye.
* **Switch off**: the base commit (4bfd176) and this branch, the native renderer not asked
  for, run B with `SR_SHOT=<dir>,50,200,2250`: 38 of 38 frames byte-identical (SHA-256).

## Diagnostic: `SR_NEAR_CHECK`

`SR_NEAR_CHECK=FIRST,LAST,DIR[,FRAME...]`, with `SR_NATIVE=0` (the engine must draw the
world). Observers: the polygon list drawer's loop head `7ba2d` with esi = `67088` (the list
builder has just finished), the six clippers' entries (`ced48`, `cfaf0`, `d4de8`; `cf0d4`,
`cfcbc`, `d50f0`: record and output buffer). Per world part of the main view it reads the
list, the clipper outputs, the engine's view and packed positions and recomputes every
polygon of the part's level (the union of its 16 orders) as native sees it (true sine and
cosine, as the scene's statics); it predicts the engine's decision from the decoding above
and counts mispredictions. Output: `DIR/near_summary.tsv` (one line per frame: parts per
path, near-path decisions, areas on screen native / engine / both and drawn by one only by
decision, overflow counts, the back-face point, polygons reaching behind the eye) and, for
each FRAME listed, `DIR/near_<frame>.tsv` (every polygon of near-path parts and every
polygon whose coverage differs: engine view, packed, float view, native and engine screen
polygons). Results of both sweeps' ranges: `work/agents/near/check_shield`, `check_china`.

## Unknowns

* The far path (`7b0b8`, the far clipper `cf0d4` and copies) was never taken at 300 % draw
  distance; not decoded.
* The object test `82130` is read from the code only; `[81e40]`, `[81e28]`, `[81e44]`,
  `[81e2c]` were not read at run time.
* The 2D clipper `ded50` is decoded to its structure (edges, integer positions, float
  interpolation); the constant `[df764]` its interpolation subtracts was not read (likely
  the 128 of w).
* Why no stretched polygon showed in China frame 1981 (an `a3tree` polygon with an
  overflowed vertex): the Glide path's tables and `subpixel.rs`'s exact positions for such a
  vertex were not followed.
* Only Rally China (two cars, three cameras, the mirror) was run; the mirror's near clipping
  goes through the same code (y not doubled) and was compared only as part of the frames.
* Interpolated frames (`SR_INTERP=1`) were not swept; the fix is in the shader, the same for
  them.

## Proposed contract changes (through the organiser)

* `sr-scene`, `Projection::near` (or the crate doc's "Units and axes"): say that statics and
  instances reach behind the eye (view z <= 0) next to the camera, and that a renderer must
  clip them at `near` in homogeneous clip space with values that stay finite there (no
  division by z, no clamping of z, before clipping); the game clips at the same plane
  (z' = 0) with an exact clipper.
* `sr-scene`, `Layer::Decal`: the pulled depth's clip-space form behind the eye (above), so
  that wave 3's decals under the camera are not lost.

## Corrections proposed for other documents

* `integration.md`, "Findings": the near-camera differences were the native renderer's depth
  clip (this document), not the engine dropping polygons; the native frame did not show the
  true geometry there, it lost it.
* `polygons.md`, "Unknowns": the near clipper (variant 8) is decoded here; the far one
  (variant C) still is not. Record +20h is the near path's side-test distance d.
* `transforms.md`, "Unknowns": the 3D module's clipper `79c58` did not run for world parts
  of the main view in 1 876 frames.
* `extract.md`, "Culling": the back-face point, read there from the code, is measured here:
  within 20 units of the view origin on the near path.

## Files

| File | Change |
|---|---|
| `crates/sr-render/src/native/native.hlsl` | the clip-space z of `vs_main` (the fix) |
| `crates/sr-render/src/native/gpu_tests.rs` | test `polygons_through_the_eye_plane_are_clipped_at_the_near_plane` |
| `crates/sr-runtime/src/native/near.rs` | new: the engine's decisions as functions (paths, side test, near class), screen clipping helpers, `SR_NEAR_CHECK`; unit tests |
| `crates/sr-runtime/src/native/mod.rs` | `pub mod near;`, `near::install` |
| `docs/re/native/near.md` | this document |
