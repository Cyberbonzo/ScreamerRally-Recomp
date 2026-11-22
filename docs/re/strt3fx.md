# `START0\STRT3FX.EXE` — reverse-engineering notes

Italian CD, 3dfx Glide build. SHA-256
`9eac6e12ab309389bdda019cfa695bfbbc791fbda1a4634464a444fce35165d1`. The English CD carries
the same file; the language is in the data (`docs/re/english.md`).

All addresses are **link-time** addresses (as shown by `srtool dis`). The
runtime loads the image at `0x30000000`; `runtime = link - 0x10000 + 0x30000000` for this
layout (`Machine::img.link()` converts). Verified = seen in disassembly *and* observed at
runtime; inferred = from disassembly only.

## Memory map (objects)

| Range | Content |
|---|---|
| 010000 | obj1, 16-byte code stub |
| 020000–078000 | game code (Watcom C, `-s` no stack checks in most functions) |
| 050000–051200 | game asm helpers: keyboard ISR, PIT, joystick, tick wait |
| 078000–084000 | hand-written asm 3D module: transform, near-plane clipper, sort, ISR-style routines |
| 0b9000–0ca000 | Watcom C runtime (+ HMI timer module at c0f81–c16c1) |
| 0ca000–0d8000 | hand-written asm polygon engine ("Engine interface"), self-modifying |
| 0d8000–0e6000 | HMI SOS digital / MIDI (inferred) |
| 0e6000–0e8000 | DOS/4GW DLL loader (`LINEXE_*`), misc |
| 0ee92c | 130 Glide import thunks (`call 0xdfd8c`), names at 0eebb8 |
| 120000–197b10 | obj6: data, BSS, original stack |

## Startup (verified)

* Entry `bf194` = Watcom `cstart_`. `int 21h AH=30h` with `EBX='PHAR'`, then the DOS/4GW
  branch at `bf2c8` (replaced by a hook, see `profile/strt3fx.rs`):
  `__Extender` 123a06, `_psp` 1239e4, `_Envptr` 123a09 (far pointer), `__GETDS` slot
  bf469, `_end` 178710.
* `bf412` → `__InitRtns(255)` (db200), `bf41e` → main path (db1b0).
* Calibration loop `e671e`: counts `int 21h AH=2Ch` calls per second (~1 s at startup, as on
  the original hardware).

## Configuration files (verified)

* `SCREAMER.CFG` (text, read at 3b430): `fscanf("%s%s%s")` = install path (1313bc), CD path
  (1313f0), data path (131424); then `%c%c`: second char `'n'` sets 131458.
  Data path == install path means a full installation (compared at 41182).
* `CONFIG\SETUP.CFG`: 160 bytes read at 3b3c4 into 131628. Fields used: +00, +08, +10
  (sound hardware, inferred), +38/+3c/+40, +94, +98 (21 uses; video/detail, inferred).
  An all-zero file works (no sound card, defaults).
* `CONFIG\CONTROLS.CFG`: 1496 bytes, read at 3ac74, written by `DRV\ST.EXE` (the DOS
  setup program); `CONFIG\DEFCTRL.CFG` holds the defaults. Layout in the next section.
* `CONFIG\CHOICE.DAT`: optional (absent on first run).

## Timing (verified)

* HMI SOS timer API (Watcom register calls):
  `c0f81 sosTIMERInitSystem(rate, flags)`, `c1058 sosTIMERUnInitSystem`,
  `c1092 sosTIMERRegisterEvent(rate=EAX, callback=EDX, &handle=EBX)` (16 slots, 125c10 /
  125c50), `c127d AlterEventRate(handle, rate)`, `c13e4 RemoveEvent(handle)`,
  `c1579 GetEventRate(handle)`; `c15a3` programs the PIT (1193180/rate).
* Game events: **100 Hz** `5042a` (`inc [50512]; inc [50516]`, registered at 36f64) and
  **1 Hz** `3a1e4` (seconds counter 136934; registered at 36f8c).
* HMI's own events: from bb10b (rate 0x78 = 120 Hz, digital driver service), d9005 and
  d9a12 (MIDI). Held (not run) until the HMI digital/MIDI HLE exists.
* Tick wait `504dc`: `while ([50512] < 1);` (the comparison's immediate at 504e4; the
  runtime sets it on entry to each frame timer: 2 ticks in the menus, 3 in the race by
  default, see below).
* Frame timers: `4f6de` (menus, 19 callers) and `4f780` (race, called from 3486b): wait
  for a tick, `raw = ticks * 0.01` -> [1469c8], reset the counter, smoothed step
  [1469c4] = `(9 * old + raw) / 10` (race: `(9 * old + 1.1 * raw) / 10`, i.e. the race
  simulation runs 10 % faster than the clock by design), clamped to 0.02 .. 0.2 s (race
  also <= 2x the previous step). `4f863` accumulates real time [1469d0] (lap clock) and
  simulation time [1469d4]. Above 50 frames per second the 0.02 floor makes the game run
  faster than real time (2x at 100 fps); on 1997 hardware it never got there. The runtime
  therefore paces frames (`504e2: cmp eax, n`); verified: lap clock 3.00 s per 3 s,
  simulation 3.30 s (the original 1.1 factor).
* The car physics depends on the step size: on Rally Wales (deterministic runs, AI cars'
  slip angle in the 4 s after GO from their scene-object positions and headings) 50 steps
  per second give about twice the slip of 33 or 25 (mean 11-20 degrees against 5-12):
  cars slide sideways at the start and in corners. 1997 hardware ran the game at roughly
  20-35 frames per second; the runtime waits 3 ticks (33 steps per second) by default
  (`pacing.rs`, `gameplay.simulation_rate`). The menus' timer shares the tick wait; they
  have no car physics and are not interpolated, so they keep 2 ticks (50 frames per
  second; with 3 they ran at 33).
* `50413`: set PIT channel 0 divisor (EAX); `4f530` restores 18.2 Hz.

## Input (verified)

* Keyboard ISR `502fc` on IRQ1 (installed at 3961c via `int 21h AX=2509h`): DS = CS+8,
  `in al,60h`, `key[sc & 7Fh] = !(sc >> 7)` in the live table at 50330 (128 bytes), EOI.
  `503b0` copies it to 110002 once per frame; game code tests `[110002 + scancode]`.
  E0 prefixes land harmlessly in `key[60h]`.
* LED update at 39590: sends EDh to port 60h and waits for the ACK (FAh). One ACK stays
  unread in the controller; the runtime drops such bytes when it raises the next IRQ1
  (otherwise every later scancode arrives one interrupt late, and a held key's make code
  only with the next key event).
* Gameport (port 201h), all replaced by the virtual joystick (`joystick.rs`):
  `50437 axis_present(mask, port)`, `50474 buttons(port)` = `(in >> 4) ^ 0Fh`,
  `50482 axis(mask, port)` = `(raw buttons << 12) | count` with `504b0` timing the one-shot
  (cli, `out`, count `in` loops up to 1800h). Only caller of 50482 / 50474: the mapper.

### Controls (`CONTROLS.CFG`, mapper `3a2e0`, physics) (verified)

* File = player-1 bindings (6 × 1Ch at +0), player-2 bindings (6 × 1Ch at +A8h, split
  screen), axis calibration (4 axes × 10 devices × 1Ch at +150h), port per device
  (10 dwords at +5B0h). Loaded into 139a50 / 139b10 / 13a050 / 13a4b0; 139bd0 / 139c90 are
  working copies. Player struct (1296c8 + n·CC8h) +B4h and car struct +B4h point to the
  binding table in use.
* Actions in order: accelerate, brake, steer left, steer right, gear up, gear down.
* Binding (ST.EXE 1a442): bound, is-axis, kind (0 analog joystick, 1 keyboard,
  2 joypad = digital axis), device, axis (0–3, mask 1 << axis), direction (±1),
  scancode / button mask / axis sensitivity (×n/10, 0 = none).
* Calibration (ST.EXE 19dcf, 13f34): present, center, rest (mid of the noise range),
  noise range, port, scale above center = `1023 / (max − 3 − center)`, scale below =
  `1023 / (center − min − 2)`.
* Mapper `3a2e0(bindings)` writes six values to bindings+A8h, per frame: keyboard
  `key ? 3FFh : 0`; joystick button `buttons & mask ? 3FFh : 0`; axis
  `round((count − center) × scale)`, clamped to ±3FFh, joypad kind snaps past ±1FFh,
  **dead zone |v| < 66h → 0**, optional sensitivity. Entries sharing an axis reuse its value.
* Physics: throttle `|out[0]| / 1023` when `sign(out[0]) == dir` (26658, 27239), brake
  likewise from out[1] (27290): both analog. Steering `28ea8`: `t = −out[3] / 1024`
  (plus `out[2]` for keyboard); by steer-left kind: **analog** (0): `|t| < 0.6 → 0.5·t`,
  else `sign · ((|t| − 0.6) · 1.7 + 0.3)` times the lock, and the lock itself × 1.3 (25560);
  keyboard (1): full lock; joypad (2): full lock past 0.5.
* Network / replay code copies the six values and the binding kinds (3a869, 3f4e2, 4f026).

## Graphics (verified)

* Glide resolver `dfd8c`/`dfd90` patches the thunks to `jmp` after `LINEXE_GETPROCADDR`;
  the runtime binds them directly.
* Call pattern: `grGlideInit`, `grSstQueryHardware`, `grSstSelect(0)`,
  `grSstWinOpen(0, GR_RESOLUTION_640x480, 60 Hz, ARGB, upper-left, 2, 0)`,
  `grCullMode(0)`, `grGammaCorrectionValue(0.9)`.
* No depth buffer: painter's algorithm (sorted polygons; sort routine ~7a8c0).
* Textures: P_8 with a palette download before almost every `grTexSource` (~200 per
  frame), RGB_565, ARGB_4444; single LOD; TMU addresses up to ~0x180000.
* Combine modes seen: colour (3,8,1,1) texture decal, (3,8,1,2) constant, (3,8,1,0)
  iterated; alpha (3,8,1,1) texture alpha, (3,8,1,2) constant. Blend (1,5,4,0) and (4,0,4,0).
  Chroma key with values like 63654Ah, 4A7139h.
* 2D (menus, HUD, logos, text) via `grLfbLock(write)` … `grLfbUnlock`.
* 3D background: sky = 2D textured quad (y 0–342), lower base quad in constant colour
  405040h, then the world.

## 3D view, projection and screen clipping (verified)

* The 3D view is 640 x 400 integer "game pixels"; y is scaled by 1.2 when Glide vertices
  are built (y table at ac6cc: `1.2 * i`), x goes through the table at ae6cc
  (`float(i)`, 2048 entries, entry 639 = 639.8 so the last column is covered;
  pointer in [aa6c4], written at 551b0).
* `53300 set_view(half_width, height, center_y)`, called every frame with (320, 400, 260)
  (200 in some menus): fills the block at 81e0c through the pointer at 81ea4:
  81e20 = W-1, 81e24 = H, 81e28 = (W-1)/2, 81e2c = H/2, 81e30/34 = those << 16, 81e38 = W/2,
  81e3c = center y, 81e40/44 = half-resolution centres, 81e48/4c packed, 81e50 = W,
  81e54 = H+1; then `82f10` patches [81e24] into the engine's y-limit immediates.
* Projection (transform 7aa00 and the near-plane clippers): `x' = 319 + x * 256 / z`,
  `y' = [81e3c] + ...`, packed as `x << 16 | y` (`0x10001000` = behind the camera). The
  centre 319 is an immediate (`add r, 0x13f`, 33 sites; `add edx, 0x13f0000` in 7aa00).
* Polygon set-up per rendering mode (ca670 and copies up to d60fe): bounding box of the
  packed vertices; reject if entirely off screen, accept if inside `x <= 638`
  (`cmp r, 0x27e0000`, 42 sites) and `y <= [81e24]` (patched), else the 2D clipper
  `ded50` (edges x 0, x `0x27f` (df25a), y 0, y [df788]). Then `call [aa6ac]` builds
  the Glide vertices (cc5e8: x through the x table, y through the y table,
  `oow = K / (z - 128)`, fan of `grDrawTriangle`).
* Sky and ground background: quads from x table[0] to table[639] (`fld [eax + 0x9fc]`
  at cb971 and cbabe), drawn by the C code at cb800 (callers return to cb99f, cb9b3,
  cba39, cba4d, cbb77, cbb8b).
* Bounding-sphere cull 82240: half-resolution projection against [81e40]..[81e28] and
  [81e44]..[81e2c].
* HUD: LFB writes (one lock per element), the rev needle is three `grDrawLine` calls from
  the line routine c0dca (called from 4e011) whose x already follows the view width. On
  the same rows the race HUD's elements are 168 or more columns apart; the intro's
  credits (also LFB writes, during the attract-mode race) have word gaps of about 16
  columns. The runtime splits LFB images into elements at gaps of 64 columns, so a line
  of credits stays whole (split into words, a word in the outer quarter was moved to
  the screen's edge in widescreen).

* Far plane: `82fa4 set_far_distance(eax = -distance)` writes everything derived from it
  (frustum far z at 7a094/7a0a0 = distance + 4096, cull thresholds `(d + 8192)^2 / 8` as
  immediates at 7a6df, 7a743, 7a7c9, 7a82d, 82022, horizon / far-clip immediates at
  7b15f.., [cf470], [cf474], [79ed0]). Race: track value [136520] (50000 on Rally China),
  x 0.8 unless [1435c8] = 2; menus: 43808 (`0FFFF54E0h`; an earlier note said 44320,
  corrected by `native/transforms.md`). Fog is off in races (`grFogMode(0)`).
* Culling frustum 7a158 / 7a33c: far corners (x, z) = (-/+57433, -(d + 4096)) rotated by
  the camera heading in Q14 (`imul` by sin/cos, `sar 14`); their bounding box plus a
  0x2000 margin culls objects (81fbc), then the squared-distance test. Object lists:
  57d90, 5af90, 5e190 (0x3200 bytes each, no bounds checks).
* Level of detail: see "Level of detail" below (an earlier version of these notes said
  there was none; that was wrong: it is chosen per object part, not per model).
* Rear-view mirror (key M, flag [131488]): drawn after the main view (34c7e), same set-up
  (236dc -> set_view), half resolution through the x table at b06cc (`78eb0`). The
  single-player frame (34c48): `236dc([1296bc])`, world `349b8(0)`, HUD `4e224`, then if
  the mirror is on and the camera mode ([cam+AE4h]) is not 1: `236dc([1296c4])`, world
  `349b8(2)`; then the end-of-frame drawing `4b6c4`. `349b8(v)` takes view v's camera
  object from [1296bc + 4v] and passes its camera structure (+A64h) to `82338`, which
  stores it as the camera pointer [81e0c]: after the mirror the pointer is left on the
  mirror's camera. `236dc` is the whole per-frame camera update (follow logic per mode,
  smoothing state) ending in set_view; in camera modes 2 and 5 `349b8` hides the
  player's car around the drawing (20cfc / 20cd0).

Draw distance (`widescreen.rs`): set_far's argument is scaled (up to 300 %); the frustum
rotation is switched to Q12 with corners stored / 4 (4x headroom) and the corners are
rewritten every frame from the far distance and the widescreen extension; the object lists
are moved to buffers 8x larger.

## Level of detail (verified)

* Every model holds a table of three detail levels for up to 13 part slots: slot i at
  level L is the entry at model +18h + 34h L + 4 i, reached through the object block's
  +B4h (`native/world.md`; an earlier note put the table in the part data); each entry is
  the level's part data (relative to +B8h) or 0.
  When a part is set up for drawing the engine computes its distance from the camera and
  picks the level:
  * `828c4` (parts of instanced objects, i.e. cars): level 0 nearer than [81e60], level 1
    nearer than [81e64], else level 2; names starting with `i` / `x` only change the sort key (4000 / 1000), not the level
    (`native/dynamics.md`; an earlier note said they count as nearer)
  * `82a04` (other objects: track sections, scenery): the same with [81e14] / [81e18] and
    half the distance; names starting with `Z` count as 2000h farther;
  * the rear-view mirror ([aa5b0] bit 0) never uses level 0.
  A part without data at the chosen level is not drawn.
* Measured (attract mode, China): a car nearer than about 8000-9000 units is twelve parts,
  100-220 triangles; farther it is one stand-in part of about 15 triangles (slot 0's
  level 2; slots 1-11 have no level 2: `native/world.md`). Track sections change mesh at the thresholds; where differently detailed
  neighbours meet, gaps open (at 4K: cracks several pixels wide across the road).
* Level 0 is not always present: some parts (distant hills, some road sections, pagoda
  walls) have data only at lower levels. (Not confirmed by the full world export of
  wave 1: every world part on Rally China has all three levels, `native/world.md`; the
  floating buildings may have come from the visibility, not the levels.) The runtime (`lod.rs`) replaces the distance
  tests at 829b8 / 82ada (29 bytes each, original bytes verified) with "the highest level
  that has data" (+0, else +34h, else +68h): full-detail parts at any distance, nothing
  lost. (Always taking level 0 left buildings floating and holes in the far road.)
* The game's own "detail" setting ([13b078] -> [1435c8], see "Progression and settings")
  is separate: the track file variant (`H` / `L`), the full draw distance, the smoke and
  dust puffs, the per-frame handling of the track's objects at 13910c; its default (2,
  ALTO) is the highest. The runtime forces 2 in every race (below).

## Hidden surfaces and depth (verified)

* No depth buffer: the engine sorts objects (`7a8c0`, a quicksort on squared distance),
  the parts of each object, and draws each part's polygons in a precomputed order chosen
  by the viewing octant (`81eac`, 16 lists). Overlaps it orders wrongly stay wrong
  (scenery behind a wall drawn over it, a car's spoiler and body).
  The object key (`82130`, sorted ascending, drawn in that order): high word
  `-(round(sqrt(d^2 / 8)) >> 2)` (d from the camera structure's position, `81fbc` / `7a52c`),
  `+12Ch` for scene instances (list entries with bit 30), minus model +C4h; low word the
  entry's offset (ties in list order). Far to near, with every scene instance (car, shadow,
  puff, mark) drawn as if 3 394 units nearer than the scenery, and among the instances by plain
  distance: a smoke puff nearer than a car's position is drawn over the car
  (`native/dynamics.md`, "Smoke and dust puffs").
* Polygons reach Glide through one vertex builder, entry `cb6b0` (`[aa6ac]`; `cc5e8` is a
  label inside it; `grDrawTriangle` at cc6f3; `native/polygons.md`),
  called by eight set-up routines (one per rendering mode, written to [aa5c8] before the
  call: 0 ca670, 6 ce47b, 3 ceceb, 2 d0430, 4 d28ab, 7 d310b, 5 d5a32, 1 d622b). It reads
  each vertex's view z from the list at [aa5dc] (`oow = K / (z - 128)`, K = [122314] =
  -1). Mode 2 (flat colour, entries d01c4 with screen coordinates and d02cc with vertex
  indices) never fills that list: its polygons get whatever 1/w it held.
* Transformed vertices: view position (3 x i32) at `68fc8 + 12 n`, packed screen position
  (`x << 16 | y`, 10001000h = behind the camera) at `6ed94 + 6 n`; a polygon record holds
  its vertex count at +6 and vertex offsets (6 n) from +8. Scenery records are in the
  image; the cars' models (and so their records) are loaded into the heap.
* Transform (`7aa00`, model vertex list at [7bacc], int16 x, y, z, ends with x = 7FFFh):
  in floating point `x' = m0 x + m3 y + m6 z + m9`, `y' = m1 x + m4 y + m7 z + m10`,
  `z' = m2 x + m5 y + m8 z + m11` with the part's matrix at `7a9c4..7a9f0` (12 floats),
  then `fistp` to integers (the mirror variant `7ab44` negates x' first), then the integer
  projection. The matrix is loaded at `7a920` (called from 81f73) from twelve integers of
  the part's draw record (+118h): rotation x [7a9c0] = 1/16384 (for an object without its
  own rotation, approximately the camera's Q14 rows: zero angles give a 0.99988-scaled
  identity and every object goes through the same products, `native/transforms.md`),
  translation in whole view units. Near the camera
  one unit is most of a game pixel: the rounding made every vertex jitter on its own. The
  runtime recomputes the view position without it (`subpixel.rs`; in a race all vertices
  checked round to the game's values).
* Back faces: the set-up routines cull them (2D cross product of the first three vertices
  < -20) when [aa6b4] != 0 and [aa5b0] bit 0 is clear, but only on the path for polygons
  entirely on screen; those that go through the 2D clipper (ded50) are drawn whatever their
  facing.
* Frame-end fade (54b4c): a quad of constant colour 88000000h (alpha-blended black) over
  0..639.8 x 0..479, two `grDrawTriangle` calls returning to 54cc7 / 54cde. The menus'
  screen transitions use it too (traced: 88 of the first 807 frames of the default run,
  among them the name entry's white flash 08FFFFFFh..F8FFFFFFh and its fade
  08000000h..E8000000h after a clear to F8FCF8h). It never covers row 479; the runtime
  extends it to the screen's edges (`glide::cover_screen`).
* Object set-up in the draw loop (`8246c`, at `8253f` esi = the object's instance): the
  scene table's names give the kind (`auto*` car, `ashadow`, `asmoke` / `a2smoke`,
  `aztri` / `az1tri`; everything else scenery). The wheels are part of each car's model
  (the race scene table holds no separate wheel objects); the tyres are colour-keyed
  opaque polygons (mode 1). A car's glass and overlays are the blended mode-6 polygons: the
  Shield's rear face is one quad drawn twice, the window layer (opaque, mode 5: a 128 x
  128 sky reflection texture, wrapped, whose coordinates move with the view) and an
  overlay with the frame, the sticker and the number plate (blended, the next part),
  split along different diagonals (with rounded vertices the overlay lay up to 0.5 %
  behind; with the exact ones the quad is flat to 0.002 %); its side glass lies up to
  1.3 % behind the window layer of its own part. The overlay is the car's last polygon:
  the objects drawn next (often a smoke puff or a shadow) must not lend it their kind
  (the renderer draws batched triangles when the state changes, and picks the depth
  rules by the kind).
* The renderer's depth buffer (`depth.rs`, `d3d.rs`): builder triangles are depth tested
  with their 1/w (flat-colour polygons with a 1/w plane fitted from their vertices' z);
  everything else is 2D. Opaque polygons write depth and their part id (stencil). Blended
  polygons write no depth: a car's (ids 80h | object << 4 | part) with a narrow window
  over the same car, in the game's order over the rest of their own part (the reverse
  test with the same bias, so the passes are disjoint) and with none over anything else;
  the scenery's with a wide window over their own part and a narrow one elsewhere.
  Shadows, smoke and tyre marks are tested as if 250 / 500 / 120 units nearer over the
  scenery and as they are over cars. The menus' 3D scenes (no scene table) use the car
  rules for everything. Back faces of objects the game does not cull test without
  writing; clipped back faces of objects it culls are culled; opaque scenery polygons are
  grown by 2 target pixels (crack fill).

## Visibility (verified)

* The world is a grid of cells 2000h units wide (index = (coordinate + 100000h) >> 13; up
  to 256 x 256). Rows through the table at [123720] (-1 = empty row), each a table of cell
  pointers (-1 = no cell). A cell holds at +10h a list of `row << 16 | col` entries, the
  cells visible from it, and at +24h their count. China: 300 cells along the track carry
  lists of 44-118 cells, reaching 1-8 cells from their own (Chebyshev distance), 884
  distinct cells in all: the hand-made visibility of the 1997 game.
* Each frame `7a2e4` takes the camera's cell list ([7a154] start, [7a150] end, also patched
  into `cmp edx, imm32` at 7a5e9) and the view frustum's bounding box in cells (first row
  << 16 at [7a8ac], first column [7a8b0], columns patched into 7a5c9; the box always
  reaches 2000h around the camera, 7a3c4 / 7a3d5), and `7a52c` collects the objects of the
  listed cells that are inside the box. A camera cell without a list makes it collect
  every cell of the box (`7a640`).
* What lies beyond the lists never appears: objects pop up as the camera moves on, and
  with a longer draw distance there are holes in the distance. The runtime
  (`visibility.rs`, `graphics.visibility`) extends the camera cell's list with the lists
  of the cells in it that lie inside the frustum box (extended, default: what the game
  shows from the stretches of track visible from here), or uses the engine's no-list
  path (everything). Merging the lists of every cell in the box (an earlier version) also
  took cells hidden behind hills, whose lists show buildings that then floated on the far
  side.

## Autopilot (verified)

* Car records at 1296C8h + n * CC8h; +B4h points to the control bindings the car reads, set
  at race start (38741): player 1's car (index [1436D4h]) gets its working bindings at
  139BD0h, a second human player's 139BD0h + C0h * k, the computer's cars 0. The physics
  and steering code branches on the pointer (25345, 26647, 27223, 29190, 2d728, 2db0d,
  336b9): with 0 the AI drives. Clearing it for player 1's car hands the car to the AI;
  positions, laps and times continue as for any car.

## Menus (verified)

* All menus share one `set_view` call (4afb4, centre y 200). Their 3D parts (the main
  menu's background, cars, the arcade cabinet, the name entry's letters, transitions) are
  drawn by the engine and widen with it. The other backgrounds are full-screen pictures
  blitted into the frame buffer every frame (`c0a92`: image at [1454f4] to (0, 0); the
  animated ones come from `anims\scene%02d%c.dat`, `sccar`, `team`): car, track and
  gearbox selection, name entry.

## Cameras (verified)

* `23aa8` scans F1..F10 in the frame's key copy (11003d..) and sets the view's camera mode
  [view + AE4h] (view pointers at 1296bch): player 1 F1 -> 2 (bumper), F2 -> 4, F3 -> 6,
  F4 -> 3 (chase views, near to far), F9 -> 1 only when [1436dd] bit 0 is set; player 2
  (split screen) F5..F8, F10. Skipped when [1436dc] bit 7 or the player's flag 2 at
  +129714 is set. Modes 2 and 5 hide the own car (349cc).

## Camera and scene objects (verified, used by the interpolated frames)

* Camera: `[81e0c]` points to the 3D module's camera structure: three angles at +0, +4, +8
  (11 bits: the engine indexes the sine table at `c48e0` with the angles directly,
  `82d04`; `79f7d` caches the frustum's values with `7FFh - angle`, 8-byte entries,
  values / 2 into `798ac..798d8`; `native/transforms.md`), position at +0Ch..+14h, a Q14 rotation matrix
  at +18h (rows) and its transpose at +48h. The frustum code (`7a158`) and the transform
  read only this structure, so writing an interpolated camera into it before calling
  `349b8` draws the world from that camera.
* Scene object table at `12f860`: 28-byte entries {name[8], 0, instance A at +0Ch, B = A +
  40h at +10h, model at +14h, index at +18h}; the used part ends at an entry with index
  -1 (about 170 entries in a race: `asmoke` x48, `a2smoke` x48, `aztri` / `az1tri` x30
  (marks), `ashadow` x6, `auto1`..`auto4` and their wheel objects). Instance A holds the
  position at +8, +0Ch, +10h and three 11-bit angles at +14h, +18h, +1Ch (the shadow code
  at `2d0bc` writes +14h and +1Ch); the drawing reads them every frame, so interpolated
  values written there before `349b8` move the object. Smoke and dust puffs keep their
  animation at +20h (grows through a puff's life, below 80000h), interpolated too. (Measured
  in a race, 2026-11-21: a puff never moves; +20h grows CABh per step from CABh to FD5Ch, then
  the slot leaves the list for at least one step and is reused elsewhere.)

Widescreen (`widescreen.rs`): the view is given `W = 640 + 2e`, every centre and right-edge
constant above is patched for `e`, and the x table maps `i -> i - e`, so Glide receives x in
`-e .. 639.8 + e` with the 4:3 picture unchanged in the middle. Patched only between frames
(from the `set_view` observer), verified against the original values first.

## Emulated selectors (verified)

`cb4c0` builds 4 GB data selectors from the table at `cb5cc` (selector variable ← base):

| Variable | Base (link) | Used as |
|---|---|---|
| 147304 | 068fc8 | GS: projected X/Y vertex table |
| 147308 | 068fd0 | FS: projected Z table |
| 147300 | 06ed94 | ES: vertex output table |
| 1472d8 / 147324 | 078d84 | DS for ISR-style routines (1472d8 later overwritten with real DS) |
| 147314 | 0aa5a4 | |
| 1472ac | 0b26cc | |
| 1472b0 | 078510 | |
| 1472f8, 1472fc, 1472f4, 1472bc, 1472a8, 1472d0, 1472c4–1472cc | 0 | flat aliases |

* Transform routine (7aa00/7ad00): loads ES/GS/FS from 147300/147304/147308, reads
  `fs:[idx*2]` / `gs:[idx*2(+4)]`, self-modifies immediates (7ad93, 7ada3).
* Near-plane clippers: 79c8f–79e62 (3D module) and ced83–cf4xx (engine); both read and
  write the FS/GS tables.
* `82f10` (called once per frame from 53402): `push ds/es/fs/gs`, `sgdt`, loads FS from
  1472f8 (base 0), calls the engine set-up functions d5a58, cb490, d6950, d28d8, d3138,
  ced18, ce4a8, d2078, d4db8 (they patch screen-stride immediates into the engine's
  code), `pop gs/fs/es/ds`. The descriptor address it computes (82f84) is never read.
* `79250`: data (constants 1000/100/10 and a digit table for the debug counter at 791fc),
  not code.

On WOW64 these selectors cannot exist; see `docs/02-architecture.md` §4 and the segment
sites in `crates/sr-runtime/src/profile/strt3fx.rs`.

## Sound effects (verified)

* Game sound module b9c00-bb400 (mixed with C library helpers), object at `[1366a8]`:
  +5fdc digital driver handle, +6264 digital enabled, +6278 driver info (+004 output
  rate, +044 buffer size, +05c port/IRQ/DMA copied from SETUP+10, +108 the driver's
  timer service, registered at 120 Hz), +1c sample records (0F0h each, count at +0c),
  +6164 current handle per channel.
* Init at 36fb6: if `SETUP.CFG +08` (digital device ID, e.g. E016h SB16) is non-zero:
  `baf34(obj, 3, id, 0)` (detection: d9cf9, d9ece, da70d) then
  `baf34(obj, 6, 11025, &SETUP+10)` -> `d7005 InitDriver(info, &handle)` and the timer
  event. Output rate requested: **11025 Hz**.
* Sound lists: `%s.dat` text (packed on disc, read through the game's file layer) with
  `SOUND_DATA` ... `END_LIST` entries of `s` / `x` / `d` fields; `ba414` loads each sample
  file whole and fills a record.
* Sample record (SOS 4 sample structure): +00 data, +04 current, +0c length (bytes),
  +1c ID, +20 flags (8000h active, 4000h stop request), +28 handle, +2c volume
  (L << 16 | R, 0-7FFFh), +30 loop count (-1 = until stopped), +34 rate (Hz), +38 bits (8),
  +3c channels (1), +40 format (8000h = unsigned), +44 pan (8000h = center).
  Observed: engine loops (IDs 40h/41h) and one-shots, 8-bit unsigned mono, 9200-11025 Hz.
* HMI digital API used (all replaced, `sos_digi.rs`): d6c89 InitSystem, d6ce5 UnInitSystem,
  d7005 InitDriver, d74b3 UnInitDriver, d8267 SetMasterVolume, d88cd StartSample (copies
  the record into a free voice slot of `driver+58h`, returns the slot), d8a4b StopSample,
  d8826 SampleDone, d86f8 GetSampleID, d82a0/d8328 Set/GetSampleVolume, d848a/d8510
  Set/GetPanLocation, d8586/d860c Set/GetSampleRate. The mixer itself is in the card
  driver loaded from `DRV\HMIDRV.386` (not used).
* `b9f04 play(obj, id, channel, rate, volume, stop_previous)`: 58 call sites.
* MIDI (d6d4d, d79e9, d8b55-d9c54, `HMIMDRV.386`) is only initialised for a music card
  (`SETUP.CFG +0c`); music is CD audio. The driver set-up (ba99c) loads the OPL banks
  `melodic.bnk` / `drum.bnk` for devices A002h / A009h, but the song loader `baba8`
  (`%s.dat` archive, `%s.hmi` song) has **no callers** and no song files exist in the
  game's data (the only HMI songs on the disc are in `CONFIG\SETUP.DAT`, the installer's
  own data): the game never plays MIDI, so there is nothing to synthesise.

## SETUP.CFG (verified)

* `3b3c4` reads `<dir>\config\setup.cfg` (160 bytes, written by `DRV\ST.EXE`) to
  `131628`; "Run SETUP before." if it is missing. References to the block (all of them):
  +08 digital device (HMI ID, read at 36fb6), +0C music device (through the same block),
  +10..+1C digital port / IRQ / DMA (passed to the driver), +38 / +3C serial link (port,
  IRQ: `c1808(port, irq, 8)` at 42b97), +40 (non-zero: `c1ace(link, 8)`), +94 network
  (`c3568(8, [+94])` at 42afd, IPX), +98 multiplayer link flag (21 tests, menus and race).
  No other byte is read by the game; the runtime writes the sound fields (SB16 at 220h /
  5 / 1, which only selects the code path) and leaves the rest zero: no music card, no
  serial or network link.

## Progression and settings (verified)

* `CONFIG\CHOICE.DAT`: the 8500h-byte block at 13b03c, loaded at 3b56c (defaults when
  missing) and saved at 3b934 (called after a race, 4152c): dword i is stored as
  `(value + i * 1050Ch) XOR i`. Verified by decoding a saved file. The main menu loop
  loads it again after every race (`call 3b56c` at 41074) and copies the unlock fields
  into its menu table in the same frame; the runtime's unlock is applied right after that
  load (observer at 41079).
* Fields: [13b03c] current player profile (0-3), profiles of 188h bytes with the name at
  +4; [13b05c] current championship; [13b060] extra tracks (0-4, the track menu allows
  indices 0 .. value + 2; Rally Arizona is the fourth); [13b064] highest championship
  that can be chosen (0-3); [13b104 + p * 188h] bonus car of profile p (the car list holds
  4 + value; the fifth car is "Shield"); [13b078] detail level (2 = high, copied to
  [1435c8]: full draw distance, otherwise x 0.8); [13b0a8] / [13b0ac] / [13b0b0] volumes.
* Championship won (4a3e9): bonus car for the current profile, counters raised to the
  championship's row of the table at 123064 (1Ch bytes: +4 tracks, +0Ch championship);
  bonus car on all four profiles -> tracks = 4.
* Cheat table at 12301c (24-byte entries `{name[12], field*, value}`): TRAMO -> [13b060] = 4,
  CARBO -> [13b104] = 1, LEALL -> [13b064] = 3.
* The game's own settings screen ("Impostazioni"): transmission, detail level, laps,
  opponent skill, multiplayer opponents, time-attack ghost, damage, music, high scores.
* **Detail level ("Livello dettaglio": ALTO / BASSO; verified 2026-11-21).** ENTER on the
  item toggles it (LEFT / RIGHT do nothing); the menu writes [13b078]: ALTO = 2 (the
  default, 3b5c9), BASSO = 0 (seen at the copy below with an observer). The race uses its own
  copy [1435c8] (+28h of the first of the 2Ch-byte records from 1435a0; the loops at 3c36a and
  43162 write 2 at that field of the others): 1 at start-up (34595), then [13b078] copied at
  every race set-up, from the menus (3c021) and for the attract-mode race (4314c), before
  the track loader `37c00` (order seen with observers). Read (disassembly,
  `work/game_dis.txt`):
  * 37cb0: the far distance passed to set_far (82fa4): the track's value [136520] at 2,
    x 0.8 otherwise;
  * 37f7c: the track files: `%s%c` with `L` when the value is 0, `H` otherwise: China's
    `MAP3L.MMP` / `MAP3L.UNR` instead of `MAP3H.*` (entries of `GFX\MAP32.DAT`);
  * 38070: at 2 only, the smoke models (`ASMOKE.3DX`, `A2SMOKE.3DX` read) and 8 `asmoke` + 8
    `a2smoke` instances per car (car +228h, +368h);
  * 2dd02 (2dcf4, called by the cars' per-frame code at 2a8af / 2a91d): at 2 only, the cars
    emit those dust and smoke puffs (from the code);
  * 39d34 (after the loader): [136630] = (value == 2); at 2 it returns at once and 35e8c
    (every race frame, from 34d00) handles the track's objects of the table at 13910c
    ([139108] entries of 4Ch bytes, within CB20h = 52 000 units); otherwise 39d34 places them
    once at load and 35e8c skips them (from the code only; not looked at in frames);
  * 3b1d6 (in 3b074): written into the block at 146848 just before that block is copied to
    1435bc, so the value stays the same.
  Verified with `SR_IOLOG` (China, deterministic): BASSO loads `MAP3L.UNR` and `MAP3L.MMP` and
  no `ASMOKE.3DX` / `A2SMOKE.3DX` (attract race and race), ALTO `MAP3H.*` and both smoke
  models.
* **Forced to ALTO by the runtime (2026-11-21, `lod.rs`, `install_game_detail`):** an observer
  on the track loader's entry `37c00` (called from 37602 at every race start; with edx = 0,
  no loading, it returns at once) writes 2 to [1435c8] after the race set-up's copy, so the
  reads above all see 2 in every race, the attract race included; [13b078], the menu and
  `CHOICE.DAT` are left alone (the log says
  `game detail level: menu 0, race 0: forced to 2 (high)` when it changes something).
  Verified: with `CHOICE.DAT` saying BASSO (the menu shows BASSO) the attract race and the
  race read `MAP3H.*` and the smoke models like ALTO, and the race's frames (1500-1900, every
  100th, native renderer) are byte-identical to the ALTO run's (5 of 5; before the fix the
  BASSO frames differed in 4-21 % of their pixels); with ALTO nothing changes (Glide path,
  frames 0-1900: 20 of 20 identical to the previous build). Not checked: the pause menu's
  restart (deterministic runs stop at the pause menu) and the caller of `382b8` (another
  path to 39d34, reached through a pointer).
* Pause menu (Esc in a race): Riprendi, Ricomincia, Lettore CD, Esci.

## Race set-up, modes, laps and the race's end (verified 2026-11-22, tool mode)

Found for the visibility editor's game window (`crates/sr-runtime/src/toolmode.rs`,
`docs/05-visibility-editor.md`). Disassembly in `work/game_dis.txt`; runs with
`SR_TOOLMODE_LOG` (one line per race frame: the set-up fields and player 1's car record)
in `work/agents/toolmode/`.

* **Menu to race.** The main menu's carousel (Arcade, Campionato, Time Attack,
  Multigiocatore, Impostazioni; LEFT / RIGHT, input routine `41570` turns the frame's key
  table into events [14502c]: 1 up, 2 down, 4 left, 8 right, 10h Enter, 20h Esc) sets the
  **menu mode [145608]** (0 championship, 1 arcade, 2 time attack 3 laps, 3 split screen, 4
  time attack free laps (`GIRI LIBERI`; 2 or 4 chosen at `43a46` / `43ae6` by the time
  attack's laps item), -1 replay viewer). `412ab: call 3bfd8` is the race set-up (one
  caller), then `41377: call 345d0` plays the race; the attract-mode race (`431c9`) and the
  replay viewer (`41265`) have their own set-ups. `3bfd8` switches on [145608] at `3d849`
  (table `3bfc4`: 0 `3ca03`, 1 `3c2ab`, 2 `3d365`, 3 `3d6c0`, 4 `3d5b5`) and writes:
  | Mode | [1436dc] race flags | [1435bc] cars | [143714] laps |
  |---|---|---|---|
  | 0 championship | 2 | [13b06c] | not written by this set-up (not traced) |
  | 1 arcade | 1 | 6 | [1231d0 + 4 * [13b07c]] (1 / 3 / 5 / 10, the settings' laps) |
  | 2 time attack | 4 (the ghost car, `attack%ld`) | 2 | 3 |
  | 3 split screen | 40h | 6 with opponents ([13b084]), else 2 | as arcade |
  | 4 time attack, free laps | 8 | 1 | 1 |
  Network / serial link races (10h / 20h, [145610] = 1 / 2) take the players' records from
  the link. Seen in runs: arcade `flags 0x1 laps 3`, 6 cars; time attack `flags 0x4 laps
  3`, 2 cars.
* **Set-up fields** (block 1435bc, 17Ch bytes, copied to 143738 at `37424`): [1435bc] cars
  in the race (read by every per-car loop: physics `34e3c..`, drawing `34a38`, the loader's
  smoke instances `38093`, positions `367c9`, ...), [1435c0] track 0-6 (files `map%c` with
  track + '1': `map3` = China; the track menu's rows map to tracks through the table at
  1231fc: 2 3 4 5 1 0 6, i.e. China, Canada, Italy, Arizona, Sweden, Wales, Stadium; the
  high-score table names them WALES, SWEDEN, CHINA, CANADA, ITALY, ARIZONA, STADIUM by
  track, `STRINGS.TXT`), [1435c4] condition (below), [1435c8] detail, then six 2Ch-byte car
  records from 1435cc (+4 car model, +8 name; record 0 is player 1, [1436d4] = 0 outside
  link races). [1436d8] human players, [1436dc] race flags, [1436e0] / [1436e4] best total /
  best lap shown, [1436e8..1436f8] best split times.
* **Condition [1435c4]** (0 in every mode but the championship, where the table at 1231e0
  and [13b070] choose 1, 2 or at random, `3cad2`): `(c & 1) + '0'` names the backdrop
  (`MAP30.BCK` / `MAP31.BCK` in `GFX\MAP32.DAT`, `37f48`) and the environment maps
  (`envp%c%c%c.dat`, `37b0f`); 2 loads `depth2.dpt` with fog colour B8B8B8h instead of
  `depth1.dpt` (`38034`); the cameras' far distance shrinks for 1 and 2 (`235bf`,
  `23b4f`), chosen when a camera is set up or changed (`23620(camera, mode)` at the race's
  start and restart, the camera keys), not every frame; it builds the fog table (`5527a`).
  **Stadium has no night**: `GFX\MAP72.DAT` holds `MAP70.BCK` but no `MAP71.BCK` (the six
  rallies' `MAPn2.DAT` hold both); its `DEPTH2.DPT` and the night reflection maps
  (`ENVP711/712.DAT`) exist. A set the loader cannot find ends the game: `53fda` ->
  `53f38` (`52ec9` finds no entry `%s.BCK`, -2) -> `54435: call 52e58` prints "Unknown
  error in Engine interface <>." and exits with -2 (status 254). The game never races
  Stadium at night; `crate::conditions` checks the archives' directories before loading
  or forcing a condition (found 2026-11-22: a race on Stadium ended at its first frame).
  No track is raced in reverse: the track data (`MAPn.DAT`: `MAPnOPP.SPL`,
  `MAPnDIR.SPL`, `MAPn.PTH`, `CAMERASn.MCM`, `MAPnDAT.DTR`) has one direction.
* **Splines** (`3ab00(map, name, &table)`: `%s.spl` from `%s\data\<map>.dat`, the
  "GAMESPLINE" file; it returns how many splines it read): the track loader loads
  `<map>opp` into [139700] (count [139704], `37dd9`), `<map>ost` into [136a08] (count
  [136a0c]) and `<map>dir` into [1396fc]. A table entry is 24h bytes (+0 the nodes, +1Ch
  their count); a node 38h bytes: +4 / +8 point x, z (integers, the ground plane), +10h
  the next node (the last: 0, a closed chain), +14h / +18h the handle the cubic Bezier
  curve arrives on, +20h / +24h the one it leaves on, +34h the distance along the line
  (the loader measures it in 100-unit steps of `363d4`, `37e21`). `opp` is the line the
  computer's cars follow (one per track, 44 to 76 nodes, 1.15 to 1.31 million units a
  lap; a car's +C4h the spline, +C8h the node, +BCh the distance, the progress routine
  `25ead..26571`); on Rally Italy [139704] reads 0 during the race although its line is
  loaded. `dir` has 21 to 35 straight points per track (handles on the points; +0Ch 0..4:
  the pace notes' arrows), up to 47 000 units off the road; `ost` is empty on these
  tracks. `sr_runtime::road` reads the `opp` line for the visibility editor.
* **Car record progress fields** (1296c8 + n * CC8h): +4A8h laps done (+4ACh most), +4B0h
  fifths of a lap since the start ("sectors", +4B4h most), +4B8h..+4C0h progress along the
  track (+4BCh = lap length * laps + position: the order of the cars), +C30h position in
  the race, +C3Ch race time, +C40h lap time, +C44h best lap, +C48h..+C58h the lap's five
  split times (index `sectors % 5`: `idiv` remainder at `265c2`, `2d9a4`), +C5Ch..+C6Ch the
  best lap's splits, +4Ch flags (bit 0 + 1 finished, set at `2dbfd`; bit 3 car contact).
  Per car and frame `26598` adds the step to the times; the progress routine (`264e7..26571`)
  computes the sectors from the progress and calls `2d8ec(car, delta)` (a new fifth: split
  strings 1465dc + 11 * (sectors % 5), the leader's times below) and `2dad4(car, laps)` (a
  lap: +4A8h, best lap, record [1436e4] (only for a car with bindings, i.e. not while the AI
  or the autopilot drives it, `2db0d`), then **the finish: laps == [143714] and not flag 8**
  at `2dbe8` sets +4Ch |= 3 and +4h = (cars - position) x [120aac], counted down every frame
  at `26600`). With flag 8 (free laps) no car ever finishes. Verified (tool mode, China,
  autopilot, 1 300 s): 13 laps, sectors 0..67, the lap's splits cycling through
  +C48h..+C58h (no index past them), the car never flagged.
* **The leader's sector times** `12ea64` (`2d939: mov [12ea64 + 4 * sectors], time` when
  +C30h = 1; read at `2d95b` by the other cars for the gap shown at 1465ae): 50 entries (10
  laps); then [12eb2c] (cleared every frame at `34e2f`, a physics step counter tested at
  `34e62`), 12eb30.., the road kinds (12eb50 = kind -1, 12eb74 = kind 0, 24h each). The
  game's own free laps write past the table from the 11th lap, into the road kinds' grip
  from sector 59 (kind -1) and 68 (kind 0, lap 13.6). The tool bounds the store (below);
  verified: [12eb74] stays 3F666666h (0.9, China's `Ghiaia` slick grip) through 13 laps.
* **Race state [13146c]** (80h at the load `37308`, 20h once the lap clock passes [120c90]
  `34e16`): the race loop (`34815..3492f`) ends when bit 0 is set; `345d0` then restarts
  the race without loading (`371f8(0)`) unless bit 1 is set too. Set by the pause menu
  (`3679b: call 4eae8`; Ricomincia `3678a: or 1`, Esci `36779: or 3`), by the post-race
  menu (`4c7f9` / `4c802`), by link races (`348db`, `3a602`, the 4 s wait [1469e8] after a
  player finished, `34907`), and bit 3 when the player retires (`3673d`). **No time limit
  exists in any mode**: no checkpoint countdown (the HUD shows lap and split times only;
  `GAME.TXT`'s `TIME OVER`, table 136938 index 16 = [136978], is never referenced), and an
  arcade race left standing at the start runs on (verified 150 s, `base1`).
* **Car placement.** Cars are placed only by the race's load (`37af7: call 382f8`, the car
  records' init with the scene table's start positions) and the restart; no routine puts a
  car back on the track during a race (writes to the body state +4D8h..: the wall
  push-back `33478` only). Ricomincia (restart from the grid) is the game's only recovery.
* **Saved game.** `4152c: call 3b934` (its only caller) writes `CONFIG\CHOICE.DAT` after every
  race; the main menu loop reloads it (`41074`).
* **After the race** `413d1` switches on [145608] (table 40fd0: 0 `41432` next championship
  race [13b068] + 1, 1 `413e7`, 2 and 4 `414ab` (back to the time attack menus, [145608] =
  2), 3 `41499`).

**Tool mode** (`--vis-editor`; nothing installed otherwise): an observer at `412b0` writes
[1435bc] = 1, [1436dc] = 8, [143714] = 1, [145608] = 4 after the set-up (any mode becomes a
free-laps time attack with the player's car; the chosen track, car, transmission and
condition stay) and, after a mode other than time attack, 4560FFFCh ("no time", 3599.99) to
the record's split times [1436e8..1436f8] (the time attack menus load them at `4594e`; the
time attack HUD shows the lap's splits against them, `2d9ba`; after the arcade menus they
were zero and the HUD showed "+" differences equal to the splits); a hook replaces `2d939`
(7 bytes) with the store for sectors < 50 only; the call at `4152c` is replaced by nops (the
saved game stays as it was). Verified 2026-11-22 (deterministic runs, `SR_AUTOPILOT=1`
unless said, logs and frames in `work/agents/toolmode/`): every track picked in the arcade
menus has one car in the scene table (China 13 laps in 1 300 s; Canada and Arizona 4 laps
in 480 s, past the arcade's 3; Italy, Sweden, Wales, Stadium 60 s), the time attack (2 cars
with the ghost) and the championship (6 cars) set-ups become one car too; the car never
finishes, the race state stays 20h; the car left at the start with no input for 414 s: the
race runs on. Log line at each race:
`tool mode: race on <track>: 1 car(s) [1435bc], flags 0x8; scene table: 1 car instance(s)
(asmoke x8, a2smoke x8, aztri x30, az1tri x30, ashadow x1, auto1 x1)`.

## The DOS release without its CD (verified 2026-11-23)

An installed copy of the DOS game that runs without its CD (an `SR` folder: `STRT3FX.EXE`,
`DATA`, `GFX`, `CONFIG`, `TABS`, `HOTCGL`, `REPLAYS`; the `*1.DAT` files of the other build
and the CD's extras absent, no audio tracks) holds the CD's `START0\STRT3FX.EXE` with 7 bytes
changed (SHA-256 `f7ae60e0…56eccb` against `9eac6e12…5165d1`; same size, 1 044 502 bytes):

* `39c98` (file offset 3EE98h): `push ebx; push ecx; push edx` → `xor eax, eax; ret`. The
  routine is the CD check (`call 50818`, then the CD's files compared): it now returns 0,
  "the CD is there".
* `3a290` (file offset 3F490h): `mov ch, [esi]; inc eax; cmp cl, ch` → `mov [esi], cl; inc
  eax; cmp cl, cl`. The loop compares 32 bytes of code at `520b0` with a table at 122f1ch
  and, on a difference, writes 10.0 into a field of every car record (`3a297`: a penalty for
  modified code); the change copies the table over the code instead, so it always matches.

Neither address is hooked or patched by the runtime, so the same profile runs both files
(`profile::PROFILES`). Without a CD the runtime overlays `C:\SR` on that folder instead of
the CD's root (`Vfs::from_folder`), and MSCDEX reports no CD drive (`int 2Fh AX=1500h`: BX =
0); the game then plays without music (the run: intro, menus in English, a race on Rally
China).

## CD / MSCDEX (verified)

* `505a0`: `int 2Fh AX=1500h` installation check.
* Requests through DPMI 0300h (`int 2Fh AX=1510h`) with a 177-paragraph DOS block:
  device open (13), IOCTL input 3 (media changed, audio disc info, track info, Q channel),
  read long (128) of sector 16 (CD check), play audio (132), stop (133).
* Attract mode plays track 2 (LBA 116140, 20096 sectors).

## Still unknown

* HMI's exact mixing (volume curve, pan law, interpolation, headroom): the native mixer
  uses linear volume and pan at 44.1 kHz; an "authentic 11 kHz" mode would need the
  driver's mix routine from `HMIDRV.386`.
* `SETUP.CFG` fields (sound card +08 = digital device ID; others partly inferred).
* Where the car physics state lives (needed for interpolated rendering and tests): partly
  mapped in `docs/re/rumble.md` (speed, position, velocity, the previous step's copy at
  +25Ch, wheels, road kinds, contact flags, engine speed, gear of the car records).
