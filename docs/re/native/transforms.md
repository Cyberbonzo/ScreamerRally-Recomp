# Transforms, camera and projection (topic D)

How the 3D engine of `STRT3FX.EXE` takes a model vertex to a screen position, step by step,
and the same math in floating point for the native renderer. Addresses are link addresses
(`docs/re/strt3fx.md`). Code: `crates/sr-runtime/src/native/transforms.rs` (the float model,
an exact replica of the engine's integer pipeline, unit tests, and the check
`SR_TRANSFORMS_CHECK`).

**Status: verified.** The replica of the engine's integer pipeline, fed only with the
instance (position, angles, flags), the camera structure's angles and position, the part
placement and the model vertices, reproduces the engine's camera-space matrices (`rec+118h`),
view positions (`68fc8 + 12 n`) and packed screen positions (`6ed94 + 6 n`) **bit for bit**:
0 differences in 17.8 million vertices of 670,000 part transforms (Rally China race with
the chase camera and the rear-view mirror, the attract-mode race, the menus' 3D scenes; see
"Verification"). The float model agrees with the engine's screen positions within the
engine's own rounding (max 2.7 grid pixels near the camera, rms 0.37; details below).

## The model in short

```text
model vertex v (int16 x, y, z)
part -> object:   p1 = Rp v + tp            Rp, tp: the part's angles and offset (instance B,
                                            or the model's placement block, or an animation frame)
object -> world:  p2 = Ro p1 + po           Ro, po: the instance's angles and position
world -> view:    q  = Rc^T (p2 - pc)       Rc, pc: the camera structure's angles and position
R(a0, a1, a2) = Ry(-a1) Rx(a0) Rz(a2)       angles in units of 2 pi / 2048 (11 bits)
view -> grid:     X = C  + 256 q.x / (q.z - 128)     C  = 319 (+ widescreen extension e)
                  Y = cy + 256 q.y / (q.z - 128)     cy = [81e3c] (race 260); mirror: 128 q.y
                  visible when q.z <= 0 (near plane), 1/w = 1 / (128 - q.z)
grid -> Glide:    x = X - e (main view; mirror: 0.5 (322 + X))
                  y = 1.2 (Y + y offset)    (main view: offset 0; mirror: 1.2 (28 + Y))
```

Axes: **x left, y up, z back** (towards the viewer) in every space: world, object, part and
view (left-handed). The camera looks along its -z; the eye is 128 units behind the view
origin.

## Data the math reads

### Camera structure (`[81e0c]`, 30 dwords)

`[81e0c]` points to the camera structure of the view being drawn: view v's camera object
`[1296bc + 4 v]` + A64h (v = 0 main, 2 rear-view mirror; set by `349b8` -> `82338`), or the
menus' own structure (link `144fb0` in the main menu; verified in the menu run). Camera mode at
camera object + AE4h.

| dwords | offset | content |
|---|---|---|
| 0-2 | +0 | angles a0 (pitch), a1 (heading), a2 (roll), 0..2047 |
| 3-5 | +0Ch | position (world units) |
| 6-14 | +18h | Q14 rotation Rc (view -> world), stored as `e[r + 3c]` |
| 15-17 | +3Ch | Rc's translation = position (copied by `82d04`; zeroed by `82338`) |
| 18-26 | +48h | Rc^T (world -> view) |
| 27-29 | +6Ch | Rc^T's translation (zeroed by `82338`) |

`82338` (start of each view's drawing) recomputes dwords 6-29 from the angles and position
(`82d04`), copies the position to `[82454..8245c]`, zeroes both translations, and caches a
float copy of Rc^T (x `[82450]` = 0x387fffd5, just under 1/16384) at `82420` (not used by the
transform). **Only the angles and the position matter**: writing other rows into the
structure has no effect (verified: the transpose stored in the structure always matched the
one recomputed from the angles, every frame of the check).

### Object being drawn (`7bab4`, filled by `8246c`)

| address | content |
|---|---|
| 7bab4 | model pointer (`[[147390] + 4 * [instance + 0]]`) |
| 7bab8 | part count (`[model + 14h]`) |
| 7babc | part counter of the draw loop (78e2c) |
| 7bac0 | the current part's draw record |
| 7bacc | the current part's vertex list (int16 x, y, z, ends with x = 7FFFh) |
| 7bae4 | object angles (3 dwords; billboard angles for flag 8000h, see below) |
| 7baf0 | object position minus camera position (`[instance + 8..10h] - [82454..5c]`) |
| 7bafc | object matrix Ro (Q14, translation = 7baf0), transpose at 7bb2c |
| 7bb7c | (+C8h) first the scene's instance list (written by the draw entry 78d84, read by `81fbc`), then per object -1, or instance B (= A + 40h) for scene-table instances (+20h per part during the set-up) |
| 7bb88 | animation offset: `([model+10h] * [1473a0] >> 16) * 30h`, or < 0 = none |
| 7bb8c | the object list entry: instance A pointer, bit 30 set = scene-table instance |

The instance pointer comes from the object lists (`[798dc]` -> `5af90 + off + 4`): bit 30 marks
the scene-table instances added by `81fbc` from `[7bb7c]` (cars, shadows, smoke, marks); world
objects collected from the visible cells (`7a52c`) have it clear.

### Instances

* **Instance A** (scene table `12f860`, entry + 0Ch; world objects: the cell's object
  record): +0 model index (into the table at `[147390]`), +4 flags (bit 15 = billboard),
  +8 / +0Ch / +10h position, +14h / +18h / +1Ch angles a0, a1, a2, +20h smoke animation.
* **Instance B** = A + 40h (scene-table instances only): 20h bytes per part:
  +8 / +0Ch / +10h the part's angles, +14h / +18h / +1Ch the part's offset in the object.
  The game's car code writes them every step (wheels: steering, spin, suspension; see below).

### Model and part placement

* model +10h: animation frame count (negative = not animated), +14h part count,
  +18h + 4 p: offset of part p's data (relative to the model).
* part +4: int16 bounding centre (used for the detail level), +0Ch: offset of the part's
  placement block (relative to the model), +68h: the part's name (`MV_mauto`, `fli`,
  `ax233s`, ...).
* placement block +4 / +8 / +0Ch angles, +10h / +14h / +18h offset (world objects, i.e.
  non-scene objects), +1Ch animation frames: 30h bytes each = 12 int16 (Q14 rotation +
  translation, `e[r + 3c]` order) and 12 int16 (its transpose).

### Draw record of a part (`7bb9c + 500 p`)

+0 the level's polygon data (after the detail selection), +10h..+24h the part's angles and
offset (as copied from instance B or the placement block; +10h is later overwritten with
-distance by the detail selection `828c4` / `82a04`), +28h Rp (12 dwords), +58h Rp^T,
+A0h Ro Rp, +D0h Ro^T Rc, **+118h Rc^T Ro Rp (the camera-space matrix, loaded by `7a920`)**,
+148h Rp^T Ro^T Rc (view -> model: the camera position in model space for the polygon order
of `81eac` and back faces).

### Sine table (`c48e0`)

8 bytes per angle a = 0..2048: int16 sin, int16 cos in the low words of two dwords
(`[c48e0 + 8a]`, `[c48e4 + 8a]`), Q15: trunc(32768 sin(2 pi a / 2048)) clamped to +-32767
(table[512] = (32767, 0), table[1024] = (0, -32767)). 63 of the 2048 entries differ from that
formula by 1, each where the exact value lies within 0.013 of a whole number (verified against
the game's table at run time: the table was made with slightly different arithmetic). The
engine shifts both right by 1: **Q14 sin and cos**. Angles index it directly (`82d04`); the
frustum code also caches the entries of 7FFh - angle (`79f7d`).

## Step by step

### 1. Three angles -> rotation (`82d04` -> `793b0`)

With s_i = table_sin(a_i) >> 1, c_i = table_cos(a_i) >> 1 (Q14), `793b0` stores (each `>> 14`
an arithmetic shift, i.e. rounding down):

```text
e0 = (c2 c1 - ((s1 s0) >> 14) s2) >> 14      e3 = (-(c2 ((s1 s0) >> 14) + c1 s2)) >> 14     e6 = (-(c0 s1)) >> 14
e1 = (s2 c0) >> 14                           e4 = (c2 c0) >> 14                             e7 = -s0
e2 = (((c1 s0) >> 14) s2 + s1 c2) >> 14      e5 = (((c1 s0) >> 14) c2 - s1 s2) >> 14        e8 = (c1 c0) >> 14
```

`e[r + 3c]` is row r, column c of the matrix applied to column vectors (`x' = e0 x + e3 y +
e6 z`). In float this is exactly

```text
R(a0, a1, a2) = Ry(-a1) * Rx(a0) * Rz(a2)
Rx(t) = [[1,0,0],[0,c,-s],[0,s,c]]   Ry(t) = [[c,0,s],[0,1,0],[-s,0,c]]   Rz(t) = [[c,-s,0],[s,c,0],[0,0,1]]
R = [[c2 c1 - s2 s1 s0,  -c2 s1 s0 - s2 c1,  -c0 s1],
     [s2 c0,              c2 c0,             -s0   ],
     [s2 c1 s0 + c2 s1,   c2 c1 s0 - s2 s1,   c1 c0]]
```

i.e. a vertex is rotated by the roll a2 about z first, then by a0 about x, then by -a1 about
y. `82d04` (argument: a block of angles at +0 and position at +0Ch) stores R at +18h with the
position as translation, the transpose at +48h with the inverse translation `R^T (-pos)`
(`79360`). Zero angles give 16382 on the diagonal (cos 0 =
32767 >> 1 = 16383, and 16383 x 16383 >> 14 = 16382): the engine's "identity" scales by
0.99988 (unit test `q14_rotation_is_the_float_rotation_with_truncation`).

Meaning of the angles (from the formula, confirmed with the data): with a0 = a2 = 0 the
camera looks along world -z for a1 = 0 and along +x for a1 = 512 (forward = (c0 s1, s0,
-c1 c0)); a0 > 0 raises the view (the chase camera has a0 = 1948..1959, i.e. 89..100 units =
16-18 degrees down); a2 rolls. The player's car in the chase view had heading 1549 against the
camera's 1543.

### 2. World -> view (camera)

`view = Rc^T (p - pc)`, Rc = R(camera angles), pc = camera position. In the engine Rc^T is
applied last (see 4) with the translation folded into the object's position: the engine never
forms world coordinates, it subtracts the camera position from each object's position
(integers) first.

### 3. Object placement (`8246c`)

`Ro = R(instance angles +14h, +18h, +1Ch)`, translation `instance position - camera
position`, built by `82d04` at 7bae4 (rows at 7bafc).

**Billboards** (flag 8000h at instance +4; path `825a8` -> `82c3c` -> `52dc0`): the angles are
replaced by angles relative to the camera of the view being drawn:

```text
a0 = (inst.a0 - cam.a0) & 7FFh
a1 = (-(inst.a1 - cam.a1 + 400h)) & 7FFh
a2 = 7FFh - cam.a2
```

Every smoke and dust puff (`asmoke`, `a2smoke`) took this path in the check (27,842 parts, all
exact); nothing else did. The main view and the mirror get different angles for the same puff.

### 4. Part placement and the camera-space matrix (`825f8`: `826ec` / `827d8`)

Per part p, `Rp` with translation `tp`:

* scene-table instances (`826ec`): angles and offset from instance B + 20h p (+8..+1Ch);
* world objects (`827d8`): from the model's placement block (`[part + 0Ch] + model`, +4..+18h);
* if the model is animated (`[7bb88]` >= 0, both paths): `826b4` copies the frame's 12 + 12
  int16 from placement block + 1Ch + `[7bb88]` as Rp and Rp^T directly (Q14, int16
  translation), and `82690` patches 7Dh (`jge`) into 7adc3, 7ae77, 7b137 (7Ch otherwise).

Then (`79638`, C = A after B: `C[r+3c] = (A[r] B[3c] + A[r+3] B[3c+1] + A[r+6] B[3c+2]) >> 14`,
translation `(A_rot tB) >> 14 + tA`, 32-bit products and sums):

```text
rec+A0h  = Ro after Rp                    (translation: (Ro tp) >> 14 + (po - pc))
rec+118h = Rc^T (no translation) after rec+A0h
```

`7a920` loads rec+118h as 12 floats into `7a9c4..7a9f0`: rotation elements / 16384 (exact),
translation as is. (An object with zero angles gets the 0.99988 identity: its matrix is
Rc^T scaled by about 0.9998, not exactly the camera's rows.)

**Cars and wheels.** A car (`auto1`..`auto4`) is one scene-table instance whose model has
twelve parts, each with its own matrix relative to the body from instance B (observed on
China): the body pieces `MV_mauto`, `fauto`, `rauto`, `fai` share one small tilt and offset
(e.g. a0 = 4 units = 0.7 degrees, offset (-2, 19, 0): the body's motion on the suspension); the wheels
`fli`, `fri`, `rli`, `rri` (32 vertices each) sit at offsets like (+-210, 112..133, -408 front /
+524 rear), the front ones yawed by the steering (a1 = +-9 units seen); the 8-vertex parts
`flx`, `frx`, `blx`, `brx` at the same offsets add the spin a0 (e.g. -49 degrees). The offsets' y
changes with the suspension. "fl"/"fr" lie at +x / -x: x points to the car's left. The scene
table holds no separate wheel objects in a race (`fauto`, `rauto`, `MV_mauto` are part names).

**Overflow.** `79638`'s 32-bit sums wrap when a translation exceeds about 2^31 / 16384 =
131072 units. Not seen in the checked frames (0 parts, also at 300 % draw distance, far plane
150000), but possible on larger tracks at long draw distances; the float path has no such limit.

### 5. Vertex transform (`7aa00`; mirror `7ab44`)

For each model vertex (int16, list at `[7bacc]`, ends at x = 7FFFh), in the x87 (control word
037Fh: 64-bit precision, round to nearest):

```text
x' = m0 x + m3 y + m6 z + m9      y' = m1 x + m4 y + m7 z + m10      z' = m2 x + m5 y + m8 z + m11
```

mirror: `x' = -x'` (`fchs`). Stored with `fistp` (round to nearest even) at `68fc8 + 12 n`
(x', y', z'). All steps are exact in double precision, so `engine_view` reproduces them.

### 6. Projection (`7aa00`)

```text
z  = z' - 128;  behind (packed 10001000h) unless z <= -128, i.e. z' <= 0
r  = 800000h / z                         (idiv: truncated towards zero; r < 0)
y  = (y' r) >> 15 + [81e3c]              (mirror: >> 16)          32-bit, wraps
x  = (2 x' r + C << 16)                  C = [7ab1a] >> 16 = 319 + e (mirror [7ac1e], 319)
     overflow of either addition -> behind (10001000h)
packed = (x & FFFF0000h) | (y & FFFFh)   at 6ed94 + 6 n
```

So `X = floor(C + 256 x' / (z' - 128))`, `Y = floor(cy + 256 y' / (z' - 128))` up to r's
truncation (relative error 1/|r|: at 50,000 units |r| = 167, at 150,000 only 56, so the
engine's far geometry is off by up to 1-3 pixels at the screen edges). Focal length 256 grid
pixels in x and y; the mirror's y uses 128.

**Near plane**: view z = 0, 128 units in front of the eye (vertices with z' > 0 are behind).
Polygons crossing it are clipped by the polygon engine's clipper (`ced83..cf4xx`, e.g.
`ceea9`): the new vertex is interpolated at z = 0 (the `add ecx, 0` / `mov eax, 0` immediates)
and projected with `x * 128 / [79ecc]` doubled (`[79ecc]` = -128 from `82fa4`) + 13Fh, y
likewise (not doubled in the mirror): the same projection at z' = 0 (X = 319 - 2 x', Y = cy -
2 y'). (The 3D module's own clipper at `79c58` projects without the doubling; whether it runs
was not checked.)

### 7. Grid -> Glide (vertex builder `cc5e8`)

```text
Glide x = [[aa6c4] + 4 ([aa6b8] + X)]      x table: main ae6cc, i -> i - e (entry 639 + 2e = 639.8 + e)
Glide y = [ac6cc + 4 ([aa6bc] + Y)]        y table: 1.2 i
oow     = -1 / (z' - 128) = 1 / (128 - z')  (K = [122314] = -1.0)
```

| view | set_view (W x H, cy) | C | x table | x offset | y offset | Glide x | Glide y |
|---|---|---|---|---|---|---|---|
| race main | (640 + 2e) x 400, 260 | 319 + e | ae6cc: i - e | 0 | 0 | X - e | 1.2 Y |
| mirror | 640 x 70, 45 | 319 | b06cc: 0.5 i | 322 | 28 | 0.5 (322 + X) | 1.2 (28 + Y) |
| menus | (640 + 2e) x 400, 200 | 319 + e | ae6cc: i - e | 0 | 0 | X - e | 1.2 Y |

(Main and mirror values logged by the check each frame: "view 854 x 400, centre 426 cy 260,
x table ae6cc (glide x = -107 + 1.0 i, i = 0 + grid x), y table glide y = 1.2 i" and "view 640
x 70, centre 319 cy 45, x table b06cc (glide x = 0.5 i, i = 322 + grid x), y table glide y =
1.2 i (i = 28 + grid y)", window 1280 x 720, e = 107. The menus' cy 200 is from
`docs/re/strt3fx.md`, `4afb4`; the mirror's offsets come from the arguments of `78eb0`.)

So in Glide coordinates the main view's focal lengths are 256 (x) and 307.2 (y), principal
point (319, 1.2 cy) = (319, 312) in a race; the mirror's 128 and 153.6 at (320.5, 87.6). The
1.2 stretch is the original look: the 640 x 400 grid was made for 4:3 screens, so everything
is drawn 20 % taller than a square-pixel projection would. Field of view of the 4:3 race view:
102.7 degrees horizontally (51.25 left + 51.42 right), 74.1 vertically (45.45 up + 28.67 down).

### 8. Far plane (`82fa4 set_far(eax = -D)`)

D = the track's value [136520] (50000 on China) x 0.8 unless detail 2, scaled by the runtime's
draw distance (`widescreen.rs`); menus 43808 (`mov eax, 0FFFF54E0h` before the calls at 220a6,
4242e, 4afa5). It writes: per-polygon far tests at 7b15f.. (`cmp fs:[...], -D`, polygons with
vertices beyond z' = -D take another path), `[cf470]` = -D (bounding sphere test at 8229b),
`[cf474]` = D, `[79ed0]` = -D - 128, the culling frustum's far z -(D + 4096) (7a094, 7a0a0) and
the squared-distance cull `((D + 8192)^2) >> 3` (82022 and four more). The native renderer can
put its far plane at view z = -(D + 8192) to keep everything the game keeps.

## Conventions for the renderer

* Units: world units (the cell grid is 2000h units; China's camera ran from x = -104289 to
  -6221, z = -58025 to 40994 in the sampled frames). World coordinates can reach +-1048576 (cell index
  `(c + 100000h) >> 13`): compute camera-relative positions in double precision.
* Axes: x left, y up, z back in world, object, part and view space (x × y = -z: left-handed).
  World +y is up (camera y about 270-3000, cars at -13..-41 on China's start).
* Angles: 0..2047 per turn, R = Ry(-a1) Rx(a0) Rz(a2) (local -> parent); a1 heading
  (a1 = 0 looks along -z, 512 along +x), a0 pitch (positive raises -z), a2 roll.
  Interpolate them the short way round (`interp.rs` does) or as quaternions of R.
* The game's screen space (grid and Glide): x right, y down. Front faces: the polygon set-up
  routines cull a polygon when `(x1-x0)(y2-y0) - (y1-y0)(x2-x0) < -20` over its first three
  vertices (`ca89c..ca8f2`, mode 0; the 8-byte entries at cf4b0 hold the packed position, y in
  the low word and x in the high word, filled at ca6e1): polygons that are
  **clockwise on screen are front faces** (only for polygons entirely on screen, see
  `depth.rs`; topic B owns the rules).

## Recipe for a modern renderer

1. **Model -> world** (per part, `Instance::transform`, 3 x 4 row-major):
   `M = [Ro | po] * [Rp | tp]` with Ro = R(instance angles; billboard angles from the view's
   camera for flag 8000h), po = instance position, Rp = R(part angles), tp = part offset
   (instance B for scene-table instances, the placement block for world objects; animated
   models: the frame's int16 matrix / 16384). `part_to_world()`.
2. **World -> game view**: `V = [Rc^T | -Rc^T pc]` with Rc = R(camera angles), pc = camera
   position. `world_to_view()`. Do the subtraction of pc in double precision.
3. **Game view -> eye** (standard right-handed eye space: x right, y up, looking down -z, eye
   at the origin): `E = Translate(0, 0, -128) * Scale(-1, 1, 1)` for the main view and the
   menus; `Translate(0, 0, -128)` (no flip) for the rear-view mirror, whose picture is mirrored
   by the game's transform. `game_view_to_eye()`. The flip changes the winding in eye space
   but not on screen: screen positions are identical to the game's.
4. **Projection**: an off-centre perspective (`D3DXMatrixPerspectiveOffCenterRH` in column
   form, depth 0..1) with near n = 128 and far f = D + 8192 + 128 (or D + 128 for the
   game's exact far plane), and the near-plane extents (`Frustum::glide()`):

   | view | left | right | bottom | top |
   |---|---|---|---|---|
   | race main (cy 260) | -(319 + e) / 2 | (321 + e) / 2 | -70 | 130 |
   | menus (cy 200) | -(319 + e) / 2 | (321 + e) / 2 | -100 | 100 |
   | mirror (cy 45, H 70) | -159.5 | 160.5 | -25 | 45 |

   In general: left = (x0 - 319) n / 256, right = (x1 - 319) n / 256 for Glide x0..x1,
   top = cy n / fy, bottom = (cy - H) n / fy with fy = 256 (mirror 128) in grid rows.
5. **Viewport**: the main view covers Glide x -e .. 640 + e, y 0 .. 480; the mirror Glide
   x 161 .. 481, y 33.6 .. 117.6. Map Glide coordinates to the window as the Glide path does,
   so both renderers agree.

`Frustum::matrix()` with `eye_to_glide()` reproduces `project()` to 1e-9 (unit test
`frustum_reproduces_the_glide_projection`), including the depth mapping (view z = 0 -> 0,
z = -far -> 1). No 640 x 400 grid, no whole-unit matrices, no integer r: the native renderer
removes every rounding listed in "Verification".

## Verification

**Method.** `SR_TRANSFORMS_CHECK=FIRST,LAST[,CSV]` installs an observer at 78e58 (just after
`call 7aa00` at 78e41). For each part it reads only: the camera structure's angles and
position, the object list entry (instance A and its flags), the model (part table, placement
block, animation count, `[1473a0]`), instance B, the game's sine table and the part's model
vertices, plus the view constants (centre immediates, [81e3c], tables, offsets). From those it
computes (a) the engine replica (`q14_rotation`, `mul_q14`, `engine_view`, `engine_project`)
and compares it with rec+118h, 68fc8 and 6ed94; (b) the float model (true sin/cos, doubles)
and compares it with the engine's view and packed positions and, through the builder's
tables, with the Glide positions. Runs (deterministic, autopilot, data folder
`work/agents/transforms-data`, window 1280 x 720, widescreen extension 107):

```text
SR_DETERMINISTIC=1 SR_AUTOPILOT=1 SR_DRAW_DISTANCE=100 SR_TRANSFORMS_CHECK=1600,2350,<csv>
ScreamerRally.exe --data <dir> --keys "12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER,36:F2,46:M" --seconds 62
```

(Rally China, default car; F2 = chase camera mode 4 from frame ~1600, M = mirror from frame
~1900.) Logs and CSV in `work/agents/transforms/` (run4.log, run5_dd300.log, run6_menus.log,
run7_menus.log, china_chase.csv).

**Replica (bit exact).**

| run | frames | parts | vertices | rec+118h / view / packed differences |
|---|---|---|---|---|
| China race, chase + mirror, draw distance 100 % | 1600-2350 | 244,662 | 6,295,856 | 0 / 0 / 0 |
| the same at 300 % (far plane 150000) | 1600-2350 | 355,546 | 9,252,982 | 0 / 0 / 0 |
| intro (attract race, camera mode 1) and main menu | 244-604 | 51,895 | 1,548,890 | 0 / 0 / 0 |
| menus, car / track selection, race start | 605-1480 | 17,917 | 751,312 | 0 / 0 / 0 |

Covered paths: scene-table instances with instance B parts (cars, wheels, shadows, marks),
billboards (all smoke), world objects with model placement (track, scenery), the main view,
the mirror (x negated, y >> 16), menu cameras. Not covered: the animation path (no part of an
animated model was drawn in these scenes).

**Float model against the engine** (China, 100 %, frames 1600-2350, on-screen vertices; grid
pixels; "transform" = float model against the exact projection of the engine's own view
integers, "engine rounding" = that projection against the engine's packed position + 0.5):

| kind | on screen | all, max / rms | within the engine's pixel | transform max | engine rounding max | view max / rms (units) |
|---|---|---|---|---|---|---|
| car part 0 (body) | 343,121 | 2.72 / 0.34 | 79.5 % | 2.97 | 1.38 | 7.73 / 1.26 |
| car parts 1-11 | 802,965 | 2.67 / 0.34 | 78.7 % | 2.27 | 1.39 | 7.88 / 1.22 |
| shadow | 12,091 | 1.23 / 0.31 | 84.9 % | 0.78 | 1.22 | 7.28 / 0.89 |
| smoke | 61,080 | 1.92 / 0.31 | 86.1 % | 2.13 | 1.28 | 7.19 / 0.87 |
| marks | 46,830 | 2.09 / 0.31 | 87.3 % | 1.70 | 0.84 | 3.42 / 0.72 |
| world (track, scenery) | 2,216,414 | 1.97 / 0.38 | 69.8 % | 1.22 | 1.97 | 13.72 / 1.91 |
| mirror, all kinds | 1,248,543 | 1.79 / 0.37 | 73-86 % | 0.98 | 1.79 | 14.95 / 1.99 |
| **all** | 4,731,044 | **2.72 / 0.37** | 73.9 % | 2.97 | 1.97 | 14.95 / 1.79 |

Glide coordinates: max 2.72 px in x, 2.36 in y (= 1.2 x the grid error). At 300 % the far
geometry adds the engine's r truncation: all max 3.15 px (rms 0.48), engine rounding max 3.17.

**Error budget** (all kinds, maximum view error in units, by eye distance 128 - z):

| eye distance | < 1000 | 1000-4000 | 4000-16000 | > 16000 |
|---|---|---|---|---|
| vertices | 206,505 | 522,619 | 1,931,916 | 3,237,626 |
| float model vs engine | 8.45 | 10.15 | 13.72 | 14.95 |
| engine's quantised rotations (their exact product vs true sin/cos) | 8.39 | 9.05 | 13.01 | 13.34 |
| `79638`'s truncated products (vs the exact product) | 2.53 | 2.49 | 2.51 | 2.49 |
| grid error on screen (px) | 2.72 | 1.68 | 1.24 | 1.97 |

The differences are the engine's: the Q14 sine table and the truncations of `793b0` (a
relative error of a few 1e-4, larger on world objects whose vertices lie far from their
origin), the whole-unit truncation of the translations in `79638` (up to about 2.5 units
anywhere: near the camera this is the 2-3 pixel error on the player's car), the `fistp` (0.5),
r's truncation and the final rounding down. Vertices on the near plane may fall on either side
(140 at |z| <= 2.1); vertices next to the near plane far to the side can overflow the engine's
projection (91, all off screen).

**Samples** (frame 1800, main view, chase camera; view = (x', y', z'), grid = (X, Y)):

| object | model vertex | engine view | float view | engine grid | float grid | engine Glide | float Glide |
|---|---|---|---|---|---|---|---|
| player's car body `MV_mauto` | (-280, 279, -433) | (-252, -152, -339) | (-251.88, -150.72, -338.78) | (564, 343) | (564.14, 342.66) | (457.0, 411.6) | (457.14, 411.19) |
| its front wheel `fli` | (0, 102, -42) | (285, -223, -326) | (284.57, -221.51, -323.94) | (265, 385) | (264.80, 385.48) | (158.0, 462.0) | (157.80, 462.57) |
| track section `ax233s` | (-667, 264, -1609) | (22404, 5895, -18374) | (22403.68, 5895.18, -18376.81) | (116, 178) | (116.06, 178.44) | (9.0, 213.6) | (9.06, 214.13) |
| pagoda `a4pagi` | (5198, -708, 1790) | (40500, 15986, -41921) | (40498.13, 15987.40, -41925.95) | (180, 162) | (179.47, 162.68) | (73.0, 194.4) | (72.47, 195.21) |

(The engine rounds down: its pixel (564, 343) covers 564..565 x 343..344.)

**Unit tests** (`cargo test -p sr-runtime native::`): the sine-table generator against
entries read from the game, the Q14 rotation against the float rotation, the axis meanings,
`mul_q14` against float composition, the integer projection (centre, scales, behind, against
the float projection), the `fistp` rounding, the frustum recipe against the projection, the
billboard angles.

## Unknowns

* The animation path (`826b4`, frames in the placement block, `[1473a0]`) is read from the code
  only: no animated model was drawn in the checked scenes. The meaning of the code bytes
  `82690` / `826a2` patch (7Dh / 7Ch at 7adc3, 7ae77, 7b137) is not known.
* The exact arithmetic that made the sine table (63 entries differ by 1 from
  trunc(32768 sin); irrelevant for the float model, the replica reads the game's table).
* The far-plane polygon path at 7b15f (what happens to polygons partly beyond z' = -D) was not
  followed; the renderer only needs D.
* The 3D module's clipper at `79c58` (projection without the doubling) may be unused.
* Other tracks, the split screen, replays: not checked (same code; the views' constants are
  read at run time).
* The mirror camera's roll is not the main camera's negated (frame 2000: main a2 = 44, mirror
  1958): computed by the game's camera code (`236dc`), not examined.

## Interfaces needed

* **A (world and objects)**: per object: instance A (model index, flags bit 15 billboard,
  position, angles) and whether it is a scene-table instance (bit 30 of the list entry); per
  part: instance B (scene-table instances) or the placement block (others), the animation
  count; the part name (+68h) for labelling. Static world objects can be placed once with
  `part_to_world()`; scene-table instances (cars, wheels, shadows, smoke, marks) change every
  step. Smoke is drawn as billboards: their angles depend on each view's camera.
* **B (polygons and materials)**: 1/w = 1 / (128 - z') per vertex; near clipping at z' = 0;
  front faces are clockwise on screen (D3D11 default `FrontCounterClockwise = FALSE` matches)
  with the game's exceptions; the reflection coordinates of mode 5 depend on the view (B).
* **E (renderer core)**: the camera as (position f64, Rc, projection = `Frustum` + viewport in
  Glide coordinates, mirror flag = no x flip), per view; keep the 1.2 vertical stretch and the
  off-centre principal point to match the original framing; widescreen = wider frustum (left /
  right) and viewport, nothing else; far plane D + 8192 (or D). Suggested contract additions:
  `Projection { left, right, bottom, top, near, far }` and `View { viewport_glide: [x0, y0, x1,
  y1] }`; `Instance::transform` = model -> world in the game's axes (3 x 4 row-major).

## Corrections proposed for other documents

* `docs/re/strt3fx.md`, far plane: the menus' distance is 43808 (`0FFFF54E0h`), not 44320.
* `docs/re/strt3fx.md`, transform: "for an object without its own rotation, the camera's Q14
  rows" is approximately right only: zero angles give the 0.99988 identity, and every object
  goes through the same products.
* `docs/re/strt3fx.md`, camera: "angles at +0, +4, +8 (11 bits)" — the engine indexes the table
  with the angles directly (`82d04`); `7FFh - angle` is only the frustum's cache (`79f7d`).
* `docs/04-native-renderer.md`: the axes of `SceneData` are left-handed (x left, y up, z back);
  the `Camera` would carry the frustum above rather than a generic projection.
