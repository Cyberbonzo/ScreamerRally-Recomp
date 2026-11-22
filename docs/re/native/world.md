# World and objects (native renderer, topic A)

From the loaded world down to "a part's polygon list at a detail level", in `STRT3FX.EXE`
(Italian 3dfx build). Addresses are link addresses (`docs/re/strt3fx.md`). Every statement
says how it was verified; **verified** = read in the disassembly (`srtool dis`) *and* seen
in memory or in a runtime comparison; **code only** = read in the disassembly, not exercised
on Rally China; anything else is under "Unknowns".

The dump code is `crates/sr-runtime/src/native/world.rs` (`SR_DUMP_WORLD`, below). All
counts are Rally China, race started with the documented key script (default car), dump at
game frame 1700.

## Summary

* The world is a grid of cells; each cell record holds its objects inline, in two lists
  (drivable track sections, everything else). Walking all cells of the grid enumerates the
  whole world (2260 objects on China), not only what the visibility lists show.
* An object is a 40h-byte record: model index, flags, position, three 11-bit angles. A
  model has up to 13 part slots per level and three levels; a slot's level entry points to
  a part's data (radius, centre, vertex list, 16 polygon orders, name). The part's own
  placement comes from its level-0 data.
* Placement: `world = Ro (Rp v + tp) + to`, rotations from the engine's sine table. Checked
  against the engine's own view positions for **every** world object and every kind of
  moving object over a whole lap: 46 million vertices, largest difference 4.8 units (the
  engine's Q14 integer rounding). On China every world model is one part with a zero part
  placement and only the heading angle in use.
* World objects never move (a lap: no position, angle or model change). Cars, their
  shadows, smoke puffs and tyre marks are the scene table's instances and move; wheels
  are parts of the car models. No animated scenery on China.
* Completeness: the world is a corridor. Every object lies within 3 cells (24 576 units)
  of a track cell; the ground is a ribbon about one to two cells wide along the track,
  lined with vertical panels; between the ribbons there is no geometry at all (27 % of the
  grid's bounding rectangle is covered by any polygon seen from above). Drawing everything
  cannot give ground to the objects that stand outside it (see "Completeness").
* Found on the way: the engine's integer matrix product overflows for parts farther than
  about 122 000 units, i.e. only with the runtime's extended draw distance (300 %).

## The hierarchy

```
[123720] row table (256)            grid: cell index = (coordinate + 100000h) >> 13
  row -> 256 cell pointers          row = z cell, column = x cell; -1 = none
    cell record (28h header + data)
      +00 / +14h  list 0: objects (40h each)   drivable track sections ("ax<n>s")
      +04 / +18h  list 1: objects (40h each)   everything else
      +10 / +24h  visible-cell list (row << 16 | col)
      object +0 = model index
        [147390] model pointer table (-1 ends it)
          model +18h + 34h L + 4 i  = offset of part slot i at level L (0 = none)
            part data: +1Ch vertex list, +28h 16 polygon orders -> polygon records
            placement: model + [level-0 part data +0Ch]
12f860 scene table (cars, shadows, smoke, marks): instance A (40h, same layout), B (parts)
```

## Grid and cells

* `[123720]` (verified): pointer to the row table, 256 dwords (-1 = empty row); a row is
  256 cell pointers (-1 = no cell). Index of a coordinate: `(c + 100000h) >> 13`; rows are
  z, columns x (7a2e4 reads the camera's x at +0Ch into the column and z at +14h into the
  row; the game's own lookup at 32a70 does the same). Verified: all 2260 objects' origins
  lie in the cell that holds them (`world.txt`, "object origin").
* Loading (code only, consistent with memory): `535ee` reads the grid file into one block
  ([14729c] -> [123720]); `53659` relocates the row pointers, the cell pointers and, in
  each cell, the five dwords +00..+10h if non-zero.
* Cell record (verified by address arithmetic in several cells, e.g. (107,120): header
  at +0, list 0 at +28h, the other blocks following in order):

| Offset | Content |
|---|---|
| +00h | list 0: objects, 40h bytes each (count +14h) |
| +04h | list 1: objects, 40h bytes each (count +18h) |
| +08h | 12-byte records (count +1Ch): a float and two ints (x, z?) - not read by the drawing |
| +0Ch | 1Ch-byte records (count +20h): a float and three (x, z?) pairs - not read by the drawing |
| +10h | visible cells, dwords `row << 16 | col` (count +24h) |

  The drawing reads only +00/+04/+14h/+18h (`7a76c`, `7a678`: both lists, the +4 list
  first) and +10h/+24h (`7a2e4`). The game's collision / height code (32a70 -> 32b08)
  walks list 0 of the 3 x 3 cells around a position (code only).
* China: 639 cells (rows 104..137, columns 113..147), all with objects; 300 with a
  visibility list; the lists name 884 distinct cells, of which 639 exist (the engine skips
  -1 entries). List 0: 693 objects in 253 cells, all named `ax<n>s`; list 1: 1567 objects
  in 606 cells, never `ax<n>s`. The +08h blocks: 1761 records in 182 cells; +0Ch: 656 in 317.

## Objects (40h bytes)

Cell objects and the scene table's instances share the layout (verified: the draw loop
`8246c` reads both the same way; the dump reads 2428 objects with it and the comparison
below matches them all):

| Offset | Content |
|---|---|
| +00h | model index into `[147390]` (8253f) |
| +04h | flags: bit 15 (8000h) = faces the camera (8254f -> 82c3c -> 52dc0); bit 0 see unknowns |
| +08h, +0Ch, +10h | position x, y, z (world units, i32) |
| +14h, +18h, +1Ch | angles 0, 1, 2 (11 bits, 0..7FFh; a full turn is 2048) |
| +20h | animation (smoke puffs grow it; if negative the engine uses [1473a0]); 824bf |
| +3Ch | copied to [7bb98] (8253f..; use unknown) |

* `y` grows upwards: the cars' wheels are below their bodies (player car: wheels y -139..89,
  body -70..254, instance y -156) and the camera above the car (dump: camera y 267 over a
  car at -156 in the bumper view).
* Flag 8000h (billboard): the engine replaces the angles with
  `a0 = (o.a0 - cam.a0) & 7FFh`, `a1 = -(o.a1 - cam.a1 + 400h) & 7FFh`, `a2 = 7FFh - cam.a2`
  (52dc0; `cam` = the camera structure's angles). Verified on the smoke puffs (the only
  objects with it on China: 96 scene instances, flags 8001h).

## Scene instances (moving objects)

* Scene table `12f860` (28-byte entries, see strt3fx.md): instance A at +0Ch (the object),
  instance B at +10h (= A + 40h, what the engine uses at 824ab: per-part placements of
  20h bytes: angles at +08h..+10h, position at +14h..+1Ch, in the car's model space;
  `826ec`). Verified for the cars' 12 parts.
* The frame drawing receives the instance list `12f478` (34a88: `eax = 12f478` ->
  `78d84` -> [7bb7c]): instance pointers ending with 0; an entry with bit 30 set is
  skipped this frame (`81fbc`). China race: 168 entries, 128 to 136 skipped in the frames dumped.
* China: 48 `asmoke`, 48 `a2smoke`, 30 `aztri`, 30 `az1tri` (tyre marks), 6 `ashadow`,
  6 cars (`auto1` x1, `auto2` x2, `auto3` x2, `auto4` x1). There are no wheel objects: the
  wheels are slots of the car models (`fauto`, `rauto`, `fli`, `flx`, `fri`, `frx`, `rli`,
  `rri`, `blx`, `brx`, `fai`). Unused puffs and marks sit at (0, 0, 0).

## Models

* `[147390]` (verified): pointer to the model pointer table, one pointer per model, -1
  after the last. `53440` loads a model file (code only): two header dwords (data size,
  count), `count` offsets (made absolute), the data; it allocates `count * 4 + 3ECh` for
  the table, and sets every model's +B4h to [14739c]. China: 1596 models; 0..1584 the
  track's (every one used by world objects), 1585..1595 smoke, smoke 2, two marks, the
  shadow and the 6 cars (heap addresses; who appends them was not traced).

| Offset | Content |
|---|---|
| +00h | bounding radius (object cull at 82240) |
| +10h | animation frames, -1 = not animated (8258b) |
| +14h | part count (8256d; <= 13 seen) |
| +18h + 34h L + 4 i | level L (0..2) of part slot i (0..12): offset of the part data from the model, 0 = none |
| +B4h | polygon material base, loaded into [14739c] per object (82567); two values on China's track models (10800000h / 10820000h), one per car |
| +C4h | sort bias (word, subtracted from the object's sort key at 8225b) |

* Level choice: `828c4` (instanced) / `82a04` (others) select +0, +34h or +68h through the
  object block's +B4h (= model + 18h + 4 i, set at 827e8) - `lod.rs` replaces the distance
  test with the highest level present. The engine takes the part's placement and the LOD
  centre from the **level-0** data of the slot before choosing (827f8: `esi = model +
  [model + 18h + 4 i]`, an entry of 0 thus reads the model itself).
* China world: every model has exactly one part, present at all three levels (290 models
  use the same part data at two levels); level 0 is never empty or smaller than level 1.
  Cars: slot 0 (`MV_mauto`) at levels 0, 1, 2 (level 2 is the 17-polygon stand-in), slots
  1..11 at levels 0 and 1 only.

## Parts (part data, one per level)

| Offset | Content |
|---|---|
| +00h | radius (near-plane test in 7ac50) |
| +04h | centre, int16 x, y, z (LOD distance, 828c4) |
| +0Ch | placement, offset from the model (read from the level-0 data only) |
| +18h | flags -> [7bb90]; bit 2000h calls bb380 (7acd4; unknown) |
| +1Ch | vertex list, offset from the model: int16 x, y, z, ends with x = 7FFFh (81f87) |
| +20h, +24h | offsets copied to [7bad0] / [7bad4]; no reader found |
| +28h | 16 dwords: polygon orders, offsets from the part, one per viewing octant (81eac) |
| +68h | name (the LOD selectors test its first characters) |

* Polygon order: int16 polygon offsets from the part data, ended by a value <= 0
  (7ad80..). The 16 orders do not hold the same polygons: on China 1780 of 4342 part data
  differ between orders (per-octant precomputed culling). Every path of `7ac50` walks one
  order, so the union of the 16 is every polygon the engine can draw; the export takes it.
* Polygon record (topic B owns it): +04h material word, +06h vertex count, +08h vertex
  offsets (6 n into the vertex list), +14h..+1Ch normal (dwords; back-face test in 7ad80).
  China: 7189 triangles, 38444 quads, nothing else.
* Placement record (model + [level-0 part +0Ch]): angles at +04h, +08h, +0Ch, position at
  +10h, +14h, +18h (copied to the part draw record +10h..+24h, 827d8). Animated models
  (model +10h >= 0, code only): from +1Ch, frames of 30h bytes, each two 3 x 4 int16
  matrices (rotation Q14 + translation, then its transpose) loaded by 826b4, frame offset
  `((frames * [1473a0]) >> 16) * 30h` (8258b).

## Placement (model -> world)

```
world = Ro (Rp v + tp) + to          v = int16 model vertex
to, Ro: object position, R(object angles)            (8246c -> 82d04)
tp, Rp: part placement position, R(placement angles) (827d8 -> 82d04)
        cars: instance B part entry instead          (826ec)
        animated models: the frame's matrix          (826b4)
x' = r0 x + r3 y + r6 z + t0,  y' = r1 x + r4 y + r7 z + t1,  z' = r2 x + r5 y + r8 z + t2
```

`R(a0, a1, a2)` (82d04 -> 793b0; verified): with `A, B = sin, cos (a0)`, `C, D = sin, cos
(a1)`, `E, F = sin, cos (a2)` from the table at `c48e0` (2048 entries of 8 bytes: sine
int16 at +0, cosine at +4, Q15: entry 0 = (0, 32767), 512 = (32767, 0)):

```
r = [ F D - A C E,  B E,  A D E + C F,
      -(A C F + D E),  B F,  A D F - C E,
      -B C,  -A,  B D ]
```

The engine works in Q14 integers (table >> 1, products >> 14, compositions in `79638`)
and brings the result into the view with the camera's transposed matrix (camera +48h)
after subtracting the camera position ([82454..5c]; 82338 clears both matrices'
translations): `view = Rc^T (world - campos)`. On China every world object has a = (0, a1,
0) and a zero part placement, so `world = (D x - C z + px, y + py, C x + D z + pz)`; the
general formula (all three angles, part placements) is verified with the cars and their
shadows.

## What moves

| Objects | Where | Moves | Verified |
|---|---|---|---|
| track sections, scenery (2260) | cells, lists 0 and 1 | no | a lap: position, angles and model never changed (`verify.txt`) |
| cars (6, 12 parts each, wheels included) | scene table | yes, per part (instance B) | positions and angles change |
| shadows (6), smoke (96), tyre marks (60) | scene table | yes | positions change; smoke faces the camera |
| animated scenery | model +10h >= 0 | per frame | none on China (all models -1) |

Object +04h bit 0 does change during the race (see unknowns); nothing else of a world
object does.

## The draw loop's per-frame lists (reference)

`78d84` (main view; `78eb0` mirror, `78d10` variant): `7b218`, then `7a52c` collects the
objects of the listed cells (or of the frustum box, `7a640`) as (distance^2 / 8, object)
into `57d90`; `81fbc` appends the visible scene instances with bit 30 set; `82130` culls
by bounding sphere into `5af90` (14h bytes: sort key, object, view centre) and sort keys
into `5e190` (word offset into 5af90, key = -sqrt(d)/4, +12Ch for instances, - model
+C4h); `7a8c0` sorts; per object `825f8`: set-up `8246c` (object block `7bab4`: +0 model,
+4 part count, +8 part index, +0Ch part draw record, +10h/+14h part data, +18h vertex
list, +30h angles / position / matrix in the camera structure's layout, +B4h level-0
slot, +B8h model, +C8h instance B entry or -1, +D4h animation frame offset, +D8h object
as listed), each part `826ec` (instances) / `827d8` (others) into a part draw record
(1F4h bytes at 7bb9c + 1F4h n: +0 chosen part data, +10h sort key, +28h part matrix, +58h
its transpose, +118h model -> view matrix for `7a920`), sorted; then per part `81f3c`,
`7aa00` (transform), `7ac50` (polygon order), `7ba0c` (polygons).

## Enumerating the whole world

Every row of `[123720]`, every cell of a row, both object lists of a cell; plus the scene
table up to its -1 index. Each object: model `[[147390] + 4 * o.model]`, slots 0 ..
min(parts, 13) - 1, for each the level wanted, its vertex list and the union of its 16
orders. This is what `native/world.rs` does (`read_world`, `part_xf`).

## Counts (Rally China)

| | objects | parts | polygons | vertices |
|---|---|---|---|---|
| world, level 0 | 2260 | 2260 | 43 700 | 69 984 |
| world, level 1 | 2260 | 2260 | 20 248 | 39 828 |
| world, level 2 | 2260 | 2260 | 9 716 | 24 973 |
| cars (level 0, highest) | 6 | 72 | 1 609 | 2 453 |
| shadows / smoke / marks | 6 / 96 / 60 | 1 each | 12 / 192 / 120 | |

Models 1596 (1585 track), distinct part data 4342. World by distance of its cell from the
nearest track cell (Chebyshev, cells): 0: 1342 objects / 35 235 polygons, 1: 693 / 7147,
2: 189 / 1139, 3: 36 / 179, beyond: none.

## Verification

* Hook (only with `SR_DUMP_WORLD`): observer at `78e58`, right after the part transform
  (78e41 `call 7aa00`) in the main view (mirror skipped). For the part being drawn
  (object [7bb8c], slot `word [81d44 + 4 [7babc]]`, vertex list [7bacc]) the export's
  world positions are brought into the view with the camera of that moment and compared,
  per vertex and axis, with the engine's integer view positions at `68fc8 + 12 n`.
* `SR_DETERMINISTIC=1 SR_AUTOPILOT=1 SR_VISIBILITY=2 SR_DUMP_WORLD=<dir>,1700,2500`
  (China, a lap, all cells drawn, draw distance 300 %): 1 511 492 parts, 45 975 134
  vertices; all 2260 world objects and 2448 distinct (object, part) pairs; largest
  difference **4.8 units** (world list 1; cars 3.0, smoke 1.2, shadows 1.7, marks 1.5;
  independent of distance: 2.6 under 1000 units, 3.0 under 10 000, 4.8 beyond). A wrong
  angle step (2pi / 2048) would be 300 units at 100 000. The remaining differences are
  the engine's Q14 truncation and `fistp`.
* 6 166 766 of those vertices the engine placed a multiple of **2^18** away (counted and
  then compared modulo 2^18): the 32-bit `imul` sums of `79638` overflow when a part's
  view translation exceeds 2^31 / 2^14 = 131 072 in a dot product; the nearest such vertex
  was 122 275 units away. The same check at the original draw distance (`SR_DRAW_DISTANCE=
  100`, original lists, 1500 frames, 1035 world objects): **0** overflows, largest
  difference 4.8. So at 300 % the current Glide path draws far objects at wrong positions
  (probably clipped away: not checked on screen).
* A static object check runs with it (fields re-read at every comparison): world objects'
  position, angles and model never changed; +04h changed 1 -> 0 on 385 list-0 objects.
* Outputs (git-ignored): `work/agents/world/china/{world.obj, world.txt, verify.txt,
  world_top.bmp}`, `china_dd100/` for the 100 % run.

## Completeness: is there terrain beyond the track corridor?

No. Seen from above (`world_top.bmp`: highest surface per 128-unit pixel, colour = height;
steep polygons as yellow lines; cell outlines):

* the ground is a ribbon along the track, about one to two cells (8 000-16 000 units)
  wide, with its banks; 27 % of the bounding rectangle of the grid's cells has any polygon
  from above, the rest is void (no polygon at any height);
* the ribbon's edges and the void are lined with vertical panels (13 911 steep triangles:
  forest and mountain backdrops `a)<n>for`, `a)<n>pep`, `a)<n>bd`, crossed-quad trees
  `a<n>tree`);
* outside the ribbon stand isolated objects: conical karst peaks (`a<n>vetta`, open
  underneath), pagodas, towers, the Great Wall (`a<n>gmurag`), villages, houses, all over
  the void; nothing lies more than 3 cells from a track cell.

Ground test (other objects' surfaces at each object's lowest vertices, `world.txt`): of
2260 world objects, 792 have no other surface within 1000 units above or below any of
their lowest vertices (track sections: 0 of 693; terrain pieces `ar/am<n>s`: 2 of 221;
`a<n>tower` 37/49, `a<n>gmurag` 30/30, `a)<n>bd` 27/27, `a<n>casa` 11/11, `a)<n>vil`
15/16, `a<n>vetta` 44/130, `a<n>tree` 102/180). By distance from the track: 222 of 1342
at 0 cells, 416/693 at 1, 128/189 at 2, 26/36 at 3. 464 objects have no other surface
above or below them at all.

Consequence for the native renderer: drawing everything with a depth buffer hides a
building only where the corridor's own banks, peaks or panels lie on the line of sight;
there are no hills between the corridor's stretches to hide the rest, and the objects
outside it have no ground under them. Where the void is visible below such an object it
will float exactly as in the extended lists (what fills the void in the original is the
background: sky picture and the constant-colour base quad, strt3fx.md "Graphics").
Screenshots of the same deterministic lap with `SR_VISIBILITY=1` and `2` (frames 1800 -
3600 every 150) differ in at most 2.4 % of samples: "everything" adds trees, a tower on
the wall and a pagoda through gaps between banks; no frame of that sample showed an object
floating, so the visual effect depends on the viewpoint (not measured further).

## Unknowns

* Object +04h bit 0: 1 at load on list-0 objects and scene instances (first dump in the
  attract mode: 693 + 72 objects, list 1 all 0; 53731 sets it on created instances), set
  to 0 by the cell collection (7a6a5 / 7a790) for every object it visits; its reader and
  meaning are unknown. Other object fields (+24h..+38h, +3Ch's use) unknown.
* Cell +08h (12-byte) and +0Ch (1Ch-byte) records: layout guessed from hex (a float and
  int pairs that look like x, z coordinates along the road); not used by the drawing.
* Model +04h..+0Fh; part +20h, +24h, +18h bit 2000h (bb380).
* Animated models and flag-8000h world objects: code read, none on China.
* Other tracks were not dumped: the key scripts tried (DOWN / RIGHT / UP in the track menu,
  deterministic and real time) always started China. Multi-part world models, non-zero
  part placements and pitch / roll of world objects are therefore only known absent on
  China.
* Whether the 2^18 overflow shows on screen at 300 % (objects popping or appearing in odd
  places far away) was not checked.

## Interfaces needed

* **B (polygons)**: the export uses +04h (material word), +06h, +08h of a polygon record
  and the union of the 16 orders; B to confirm that the union is the right set for a
  depth-buffered renderer (the orders' per-octant culling plus the +14h normal test), and
  the meaning of part +18h flags.
* **C (textures)**: model +B4h is the base the material word is relative to (7ba1e: `ebx
  & ~0Fh + [14739c]`); it differs between track models and per car.
* **D (transforms)**: assumed here: y up; `R(a0, a1, a2)` above with x' = r0 x + r3 y +
  r6 z; view = camera +48h rotation of (world - [82454..]); billboard angles from the
  camera angles. D owns the exact Q14 behaviour, the mirror and the projection; the
  2^18 overflow limits the Glide path to about 122 000 units (draw distance above about
  240 % on China's 50 000).
* **E (renderer)**: statics = cell objects (2260 instances of 1585 one-part meshes on
  China: upload one mesh per model at the highest level); dynamics = scene instances drawn
  this frame (12f478 without bit 30): cars as 12 part instances (instance B), shadows,
  smoke (billboards), marks.
* A public extraction API (models -> meshes, objects -> instances) can grow out of
  `read_world` / `part_xf` in phase 2.

## Proposed changes to other documents

* `docs/04-native-renderer.md`: in the glossary, "level 0, 1 or 2 (table entries at +0,
  +34h, +68h)" -> "model +18h + 34h L + 4 i"; the scene table holds no wheel objects (the
  wheels are car model slots); add the 2^18 overflow to the far-plane / draw distance
  notes.
* `docs/re/strt3fx.md` "Level of detail": the level table is in the model (+18h), reached
  through the object block's +B4h (not "part data +B4h"); the car stand-in is slot 0's
  level 2, slots 1-11 have no level 2 (not "its level-0 entry is 0"); "some parts have no
  level 0" was not observed on China (every world part has all three levels).
  "Camera and scene objects": instance B is the per-part placement (angles +08h, position
  +14h, 20h per part).
