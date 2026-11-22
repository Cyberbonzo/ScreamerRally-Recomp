# Scene extraction (wave 2, topic W2-1)

`build_scene(m: &Machine) -> Option<SceneBuild>` in `crates/sr-runtime/src/native/extract.rs`
turns the race just loaded into the scene description of `crates/sr-scene` (textures,
animation frame lists, materials, meshes, statics) plus a `StaticMeta` per static and the
sky's texture index. The integration (W2-3) calls it once per race at its first race frame.
Link addresses as in `docs/re/strt3fx.md`; every number below is Rally China (Italian 3dfx
build, default car), and says how it was obtained. **Verified** = compared with the engine
while it draws, by the check `SR_EXTRACT_CHECK` ("Verification").

## Summary

* Built in about 0.12 s at the first race frame (log line `extract: scene built in ...`):
  325 textures (4.25 MB, the sky included), 6 943 frame-list entries, 263 materials,
  1 585 world meshes (70 584 triangles) plus 77 part meshes of the scene-table models (cars,
  shadow, smoke, tyre marks: for wave 3), 61 052 vertices (2.93 MB of vertex and index data),
  2 260 statics placing 81 446 triangles.
* Verified while the engine draws (deterministic China races of 1 500 race frames: default
  settings; every cell drawn, all 2 260 statics; chase camera with the rear-view mirror):
  every vertex of every static the engine transforms lands on the engine's own view position
  within its rounding (default settings: rms 3.6 units, max 24 units at 132 000 units'
  distance; on screen rms 0.51 grid pixels, max 4.3 where the engine's own projection
  truncates); every polygon the engine sets up (up to 14.4 million world and 1.1 million
  car, smoke, shadow and mark polygons per run) is in the scene, with 0 differences in
  texture coordinates (up to 58 million vertices), texture (animated frames and car states
  included), mode, chroma key, blending and clamping.
* Decisions: world materials are back-face culled (the engine's list builder drops world
  back faces, measured); rotations in true sine and cosine (topic D's float model); no
  mipmaps (the game has none); `phase` = the engine's [7bb94]; grounded = connected to the
  road through supporting surfaces (946 of 2 260 on China: the road and its terrain; none of
  the Great Wall, villages, houses, towers, peaks and backdrop pieces `world.md` found
  floating, 4 of 180 trees).

## Use

* Call `build_scene(m)` on the guest thread at the race's first race frame (the race frame
  observer at 4f793 is a good place: the world, the libraries and the background picture are
  all loaded, and nothing is drawn meanwhile). It reads memory only, takes about 0.12 s and
  logs two lines (`extract: scene built ...`, `extract: ground test ...`). `None`: the world
  cannot be read (no model table or grid) or has no objects.
* Build it again whenever a new race is loaded (the attract-mode race, then the player's: the
  check detects that by the grid pointer [123720], the model table [147390] and the last
  library's data block changing); texture record ids, materials and model indices are only
  valid for the race they were read in.
* `FrameData::clock` must be [1473a0] & 3FFFh for `phase` to give the game's frames (below).

## What is built from where

### Textures

* Every texture record in use (`textures::records()`, table at 1479d4, `textures.md`), in
  record order, as the game holds it: `Indexed8` with the record's palette (+0Ch: the
  palette the game downloads before every bind of the record, so "the palette in effect"),
  `Rgb565`, `Argb4444`; one level (`Mipmaps::None`: the game has no mipmaps and never asks
  for mipmapping, `textures.md`). China: 324 records (261 P8, 41 RGB565, 22 ARGB4444).
* Then the background picture: its `GrTexInfo` at 173fd4 (256 x 256 RGB565), its texels read
  from the emulated texture memory at [173f80] (the RAM copy [14734c] is freed after the
  download). Its index is `SceneBuild::sky_texture`. Checked: the texture memory there kept
  the same bytes for the whole race (30 checks, 0 differences).

### Materials

One scene material per (game material, culling, decal pull), created when a polygon names
it. The game material is the 20h-byte directory entry at `[model + B4h]` + record +04h;
its type (+00h) gives the builder mode through the game's dispatch table
(`[798e0 + type]`, the variant-0 set-up routine, mapped to its mode:
`polygons::mode_of_type`). The textures come from its frame list (data block + material
+04h: `{count, ?, record ids}`); mode 5 takes the frame list of material 0 of the
environment library ([aa6a8]) instead, as the set-up does (d59c0).

| Mode | Scene material |
|---|---|
| 0, 4 | texture, `Opaque`, `Combine::Replace`, clamp, bilinear |
| 1 | `ChromaKey { key }`, key = record +14h of frame 0 (all frames of a material have the same key on China), clamp |
| 2 | `texture: None` (flat), `Combine::Modulate`; the colour is in the vertices |
| 3 | `Blended(ONE, SRC_COLOR)`, clamp: the key cannot be expressed together with a blend (contract change proposed); not seen on China |
| 5 | `TexGen::Reflection { (1, 1), (0, 0) }`, `Opaque`, repeat |
| 6 | `Blended(ALPHA)`, clamp |
| 7 | `Layer::Decal { pull }`, `Blended(ALPHA)`, or `Blended(DST_COLOR, SRC_COLOR)` when record +34h != 0 (2 x src x dst); pull by the object's kind: shadow 250, smoke 500, other 120 |

* `cull`: `CounterClockwise` (front faces are clockwise on screen) for world objects and
  scene-table instances alike, see "Culling".
* Animated materials (more than one frame) get `anim = TexAnim { first, count, source }`
  over `texture_frames`, source `Phase` (`Env` for mode 5); `texture` is frame 0. China:
  the two waving flags (10 frames), smoke (6 and 5) and every textured car material (128
  entries: the car-state table).
* Combine: textured materials use `Replace` (the game's colour combine is the texture
  alone); vertex colours of textured polygons are white anyway.
* China: 263 materials (167 mode 0, 73 mode 1, 4 mode 2, 4 mode 5, 12 mode 6, 3 mode 7);
  every keyed texture has texels of its key colour (the chroma table quirk of
  `textures.md` does not hit China).

### Meshes

* **World meshes**: one per world model used by a cell object (China: 1 585, all used),
  every part slot at its highest detail level present (`world::best_level`, as `lod.rs`
  forces in the engine), the part's placement (model placement block through the slot's
  level-0 data, `827d8`; the current frame's matrix for an animated model) baked into the
  vertices, so mesh space = the object's space. On China every world model is one part with
  a zero placement: the vertices are the model's integers.
* **Scene-table part meshes** (wave 3, kept out of `statics`): one per part slot of every
  model a scene-table instance uses (cars, shadow, smoke, marks: China 77 = 6 cars x 12 + 5),
  in the part's own space (placed per frame from instance B, `transforms.md`), appended
  after the world meshes in model-index then slot order. There is no field in `SceneBuild`
  to find them yet (proposal below); the check finds them by (model, slot).
* Vertices: vertex `vbase + i` of a part is its model vertex `i` (`[part +1Ch]`, int16):
  `pos` = placement applied (f32), `uv` = the part's table (`[part +24h]`, 6 bytes per
  vertex) (u, v) / 256 (exact in f32), `normal` = the part's normal table (`[part +20h]`:
  count, then 3 floats per vertex, used only when the count equals the vertex count and the
  values are unit vectors; China: 6 parts, the car windows) rotated by the placement, else
  zero; `color` white. Flat polygons (mode 2) get their own copies of their vertices with
  the record's 5:6:5 colour as the builder iterates it (r = (c & F800h) >> 8, g = (c & 7E0h)
  >> 3, b = (c & 1Fh) << 3, alpha 255).
* Triangles: the polygon records of the union of the part's 16 polygon orders (`world.md`)
  in address order, each a fan (v0, vi, vi+1) of its vertex loop, as the game's builder
  draws it, so **the record's winding is kept: front faces are clockwise on screen** (the
  contract's `Cull::CounterClockwise` removes back faces). Index ranges are grouped by
  material (one range per material per mesh).

### Statics

* Every object of both lists of every grid cell (`world::read_world`; China 2 260), in grid
  order: `Instance { mesh: its model's mesh, transform, phase, scroll: 0 }`.
* `transform`: `basis` = R(object angles) (`R = Ry(-a1) Rx(a0) Rz(a2)` in true sine and
  cosine, topic D's float model), row-major f32; `origin` = the object's position (f64). With
  the part placement in the mesh this is `world = Ro (Rp v + tp) + to` (`world.md`).
* `phase` = what the engine puts in [7bb94] (`824bf`): instance +20h >> 2 (its low 16 bits
  when the tint flags 60000h are set, as the set-ups read it), or -1 when +20h is negative
  (the clock). With `FrameData::clock` = [1473a0] **& 3FFFh** the contract's frame formula
  then gives the game's frame (verified, "Verification"); W2-3 must mask the clock. This is
  the contract as the organiser corrected it on main (01f48db). On China every world object
  has +20h = -1 (2 260 of 2 260 in `world.md`'s dump, `work/agents/world/china/world.txt`):
  phase -1, the clock, which is how the flags animate.
* `StaticMeta`: `cell` = (row, column) of the cell holding it; `center` / `radius` = the
  bounding sphere of its world-space vertices (box centre, largest distance); `grounded`
  (below).
* China: no world billboards (flag 8000h), no animated or multi-part world models (the code
  handles them as described; not exercised).

## Culling

The set-ups' screen-space back-face test is on only for scene-table instances ([aa6b4] = 1
in `828c4`, 0 in `82a04`): that is what `docs/04-native-renderer.md` cites. But the
polygon list builder `7ac50` drops, for every object and on all three of its paths, the
polygons whose record normal faces away from the camera (`(v0 - c) . n < 0` in model space,
`7adb3`..`7adc3`): the game does not draw world polygons seen from behind. Measured by the
check: at 78e84 the records the list builder kept (67088, variant 0) against the winding on
screen, through the scene and the float camera, of every triangle of every polygon of the
part with all vertices between the near and far planes (main view, world parts; parts the
engine overflowed left out, their camera point is wrong too).

Run 6 (every cell drawn, chase camera, frames 1 500 - 2 249; triangles, and in brackets those
wholly on screen with their total area in grid px^2, the 640 x 400 grid being 256 000 px^2
per frame):

| eye distance | engine | clockwise (front) | counter-clockwise (back) |
|---|---|---|---|
| < 40 000 (the original far distance) | keeps | 1 783 875 (1 481 540: 222 567 653 px^2) | 5 018 (3 175: 6 796 px^2) |
| | drops | 12 928 (10 770: 12 787 px^2) | 629 723 (445 309: 106 155 949 px^2) |
| beyond (the runtime's longer draw distance) | keeps | 4 097 867 (3 917 534: 63 841 171 px^2) | 456 776 (432 566: 3 098 021 px^2) |
| | drops | 723 307 (695 179: 3 465 430 px^2) | 3 026 778 (2 901 147: 49 799 849 px^2) |

* Within the original draw distance the engine draws counter-clockwise triangles covering
  0.003 % of the area it draws (6 796 of 222.6 million px^2: slivers, the largest 540 px^2),
  and drops back faces that would cover 48 % more (106 million px^2). Culling the
  counter-clockwise winding reproduces the game's selection there; not culling would draw
  those back faces (the backs of the corridor's backdrop panels and banks among them),
  which the depth buffer would only partly hide.
* Beyond 40 000 units (only reached with the runtime's 300 % draw distance) the engine's
  test disagrees more (10 % of the triangles it keeps there are back faces, its integer
  camera point being coarse that far out: not analysed further), but the area it would add
  is still 16 times the area culling loses (49.8 against 3.1 million px^2).
* Run 3 (default settings, 1 500 frames, no distance bands; a few overflowed parts not yet
  left out): kept 6 877 368 clockwise and 39 881 counter-clockwise (0.58 %; 37 745 wholly on
  screen cover 27 537 px^2 in all), dropped 50 886 clockwise and 4 136 028
  counter-clockwise.
* Of the disagreeing triangles, few have the eye and the view origin on opposite sides of
  their plane (run 6: 770 of the drawn counter-clockwise, 12 501 of the skipped clockwise);
  the engine tests from a point patched into its code at 7ad58..7ad64 (read from the code
  only: the translation of draw record +148h, the view origin in model coordinates, plus the
  part's int16 centre at part data +4). The winding on screen agreed with the side of the
  eye for every triangle classified (10 736 272 in run 6), a check of the classification.

So world materials are culled like the instances' (`WORLD_CULL` in `extract.rs`, one line to
change back to `Cull::None` if the organiser keeps the `CULL_ON` rule).

## Grounded

`StaticMeta::grounded` is for the visibility decision: beyond the game's lists the renderer
draws only objects with ground under them. The test (`ground_test`):

* **Base points**: the object's world-space vertices within 50 units of its lowest point, at
  most 16 of them spread over that list.
* **Support**: a triangle of another static whose (x, z) footprint holds the base point and
  whose surface there lies at most `GROUND_BELOW` = 200 units below the point or at most
  `GROUND_ABOVE` = 1 000 units above it (the object sunk into it).
* **Grounded**: the track sections (cell list 0, the drivable road) are the ground; any other
  static is grounded when at least half of its base points are supported by grounded
  statics; repeated until nothing changes. Ground is what connects to the road; an object
  standing on a hill that floats floats with it.

Why these bounds (China, all 2 260 statics, the median over each object's base points of the
signed distance down to the nearest surface of another static, any distance; log line
`extract: ground test`):

| median gap (units) | < -2000 | -2000..-1000 | -1000..-500 | -500..-200 | -200..-50 | -50..0 | 0..50 | 50..200 | 200..500 | 500..1000 | 1000..2000 | >= 2000 | no surface |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| objects | 163 | 8 | 21 | 31 | 89 | 399 | 430 | 25 | 6 | 11 | 31 | 45 | 1 001 |

Objects resting on something cluster within 50 units; below them the counts fall to 6 in
200..500, so 200 separates resting from hovering; above them the gap is at -2000..-1000
(8), so 1 000 keeps objects sunk into the ground while leaving out bases buried under far
surfaces (the 163 below -2000: objects under the corridor's banks and overhangs).

Result: **946 of 2 260 grounded** (693 track sections, 253 others: ar#s 155 of 156, am#s 54
of 65, ac#s 24 of 56, a)#pep 8, trees 4, ad#s 2). Without the propagation (support by any
static) it would be 1 264: the 318 more are objects on hills that are not connected to the
road (84 peaks `a#vetta`, 75 hill pieces `ad#s`, 16 `a#tian`, 12 towers, ...). Spot checks of
the classes `world.md` lists as floating over the void: `a#gmurag` (Great Wall) 0 of 30
grounded, `a)#bd` 0 of 27, `a#casa` 0 of 11, `a)#vil` 0 of 16, `a#tower` 0 of 49, `a#vetta`
0 of 130, `a#tree` 4 of 180 (three `a4tree` in cells (117, 144), (118, 139), (118, 140) and
an `a1tree` in (128, 141), each with 6 of 12 base points on grounded terrain; none of them in
`world.md`'s list of the tallest floating objects). The giant `a3tree` backdrop trees that
list starts with (cell (108, 130) and the others) are all ungrounded. Per-object results:
`grounded1.tsv` of the check (`work/agents/extract/run3/`).

## Verification

`SR_EXTRACT_CHECK=FIRST[,LAST[,DIR]]` (off by default; observers at 78e62 after the part
transform, 78e84 after the part's polygons, the eight variant-0 set-ups and the clipped
ones, the sky routine, the builder's triangle loop cc6df; not combinable with
`SR_POLY_CHECK`, which observes the same set-ups). At every new race from game frame FIRST
it builds the scene with `build_scene`'s code and, until LAST:

* **Transforms**: for every part the engine transforms (main view and mirror), the scene's
  vertices of that part (`vbase..vbase + n` of the object's mesh) through the static's
  `Transform` (scene-table instances: topic D's recipe from instance A and B with the part
  mesh), then topic D's camera (`transforms::world_to_view` from the camera structure's
  angles and position, x negated in the mirror), against the engine's view positions
  (`68fc8 + 12 n`); and projected (`transforms::project`) against the engine's packed screen
  position where both are on screen. Vertices of parts the engine misplaced by its 32-bit
  overflow (`world.md`: 2^18 multiples beyond ~122 000 units, only at 300 % draw distance) are
  counted apart.
* **Polygons**: at each variant-0 set-up (eax = the record) the object's mesh must hold the
  record; the record's vertex offsets, the game's (u, v) of each vertex from [7bad4] (what
  the set-up copies, `polygons.md`) and the mode-2 colour from record +02h are compared
  with the scene's vertices, and the scene's fan with its index list; at the polygon's first
  triangle the game's texture ([aa5c0]) is compared with the scene material's frame
  (contract formula with the static's `phase` or `clock` = [1473a0] & 3FFFh, `Env`: [7bb98]),
  the builder mode ([aa5c8]) with the material's mode, and the Glide state (chroma key on and
  its value, blend factors, clamp or wrap) with the material.
* **Culling**: at 78e84, the records the list builder kept (67088, variant 0) against the
  scene's winding of each triangle (table above).

Runs: `SR_DETERMINISTIC=1 SR_AUTOPILOT=1`, data `work/agents/extract-data` (template:
1280 x 720, draw distance 300 %, extended visibility), keys
`12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER`, reports in
`work/agents/extract/run<n>/extract_check.txt` and the log.

**Run 3** (default settings, game frames 1 500 - 2 999 = 1 500 race frames, mirror off;
`run3/`). Transforms (view: scene against the engine's view position, units; grid: the
scene's projection against the engine's packed position where both are on screen, grid
pixels against the pixel's centre):

| kind | parts | vertices | view max / rms | view max by eye distance < 1000 / < 4000 / < 16 000 / more | on screen | grid max / rms |
|---|---|---|---|---|---|---|
| world (statics) | 368 082 | 10 282 369 | 24.04 / 3.58 | 9.68 / 8.36 / 12.13 / 24.04 | 9 234 767 | 4.28 / 0.51 |
| car parts | 62 064 | 2 116 934 | 9.75 / 1.42 | 2.83 / 3.07 / 3.32 / 9.75 | 2 094 462 | 2.71 / 0.32 |
| shadows | 5 092 | 20 368 | 8.38 / 1.26 | 1.78 / 2.18 / 3.05 / 8.38 | 19 984 | 1.30 / 0.31 |
| smoke | 30 939 | 123 756 | 8.20 / 1.23 | 1.34 / 1.48 / 3.24 / 8.20 | 114 543 | 2.58 / 0.31 |
| tyre marks | 7 627 | 45 762 | 3.43 / 0.78 | 1.48 / 1.70 / 2.19 / 3.43 | 45 416 | 2.05 / 0.30 |

1 440 of the 2 260 statics were drawn and compared, every part at the scene's level; 488
world vertices of parts the engine overflowed were left out. The largest differences are
the engine's: 24 units on a peak (`a13vet`) 131 900 units away (relative 1.8e-4, the Q14
rotations, `transforms.md`); 4.28 grid pixels on a track section 83 000 units away and 390
pixels left of the centre, where the engine's `r = 800000h / z` is truncated from 100.8 to
100 (1 % of 390 pixels). Topic D measured the same kinds of bounds (`transforms.md`).

Polygons (set up: variant-0 set-ups of objects in the scene; drawn: the first triangle):

| kind | set up (by mode) | uv vertices, differ | drawn | texture, mode, chroma, blend, clamp differ |
|---|---|---|---|---|
| world | 3 700 092 (0: 3 356 692, 1: 343 400) | 14 318 322, 0 | 3 490 442 | 0, 0, 0, 0, 0 |
| car parts | 654 080 (0: 179 474, 1: 163 286, 2: 165 043, 5: 58 248, 6: 88 029) | 1 426 144, 0 (flat colours: 165 043 polygons, 0 differ) | 593 969 | 0, 0, 0, 0, 0 |
| shadows | 8 924 (7) | 26 772, 0 | 8 923 | 0 |
| smoke | 60 808 (7) | 182 424, 0 | 59 408 | 0 |
| tyre marks | 7 631 (0) | 22 893, 0 | 5 212 | 0 |

No polygon of an object outside the scene, no record missing from its object's mesh, no fan
different from the game's. (A first version of the check, without the clipped set-ups
observed, paired 14 triangles of near-clipped polygons with the previous set-up's polygon:
the check's bookkeeping, gone once the clipped set-ups clear it.)

**Run 4** (as run 3 with every cell inside the view box drawn, `SR_VISIBILITY=2`; frames
1 500 - 2 999; `run4/extract_check_2999.txt`): **all 2 260 statics** drawn and compared.
World: 868 127 parts, 21 986 555 vertices, view max 32.66 units (a backdrop panel
`a)012for` 157 000 units away), rms 6.00; by eye distance max 15.38 / 11.40 / 12.96 / 32.66
(the near ones are vertices of large objects whose origin lies far away: the engine's Q14
error grows with the object's distance and size, `transforms.md`); 4 009 070 vertices of
overflowed parts left out (at 300 % with everything drawn, as `world.md` found); on screen
20 268 825 vertices, grid max 4.57 pixels, rms 0.76. Polygons: 7 958 831 world polygons set
up (7 211 045 mode 0, 747 786 mode 1), 30 808 274 uv vertices, 7 505 732 drawn: 0
differences of any kind. Animated materials, all 0 differences: 1 076 drawn polygons of the
flags (frame from the clock, `phase` -1), 368 660 car polygons (frame from the car's phase:
the 128-entry state lists), 59 408 smoke polygons (phase). The cars, shadows, smoke and
marks are the same as in run 3 (the run is deterministic and the dynamics do not depend on
the visibility). This run's culling numbers are not used: with everything drawn the
engine's list builder also works on overflowed parts (its camera point in model space is
then wrong too); runs 5 and 6 leave those parts out, run 6 splits the comparison by
distance ("Culling").

**Run 5** (every cell drawn, keys `...,36:F2,46:M`: the chase camera from frame ~1600 and
the rear-view mirror from ~1900; frames 1 500 - 3 000; `run5/`): all 2 260 statics again;
mirror views compared with x negated, as the engine's mirror transform does.

| kind | view | parts | vertices | view max / rms | on screen | grid max / rms |
|---|---|---|---|---|---|---|
| world | main | 839 639 | 21 890 920 | 31.66 / 5.71 | 20 042 614 | 4.55 / 0.81 |
| world | mirror | 634 106 | 16 863 486 | 30.27 / 5.22 | 11 633 502 | 4.31 / 0.71 |
| car parts | main / mirror | 79 344 / 24 816 | 2 692 991 / 853 725 | 9.82 / 1.41, 8.28 / 1.67 | | 2.72 / 0.35, 1.37 / 0.32 |
| shadows | main / mirror | 6 534 / 2 082 | 26 136 / 8 328 | 7.53 / 1.09, 6.64 / 1.42 | | 1.42 / 0.32, 1.26 / 0.32 |
| smoke | main / mirror | 30 780 / 27 914 | 123 120 / 111 656 | 7.74 / 1.18, 7.06 / 1.18 | | 1.92 / 0.32, 2.05 / 0.32 |
| tyre marks | main / mirror | 7 918 / 3 049 | 47 508 / 18 294 | 3.42 / 0.72, 5.51 / 1.32 | | 2.09 / 0.31, 1.16 / 0.34 |

Polygons, both views: world 14 366 208 set up (13 035 966 mode 0, 1 330 242 mode 1),
55 677 541 uv vertices, 12 007 995 drawn (2 608 of the flags); cars 1 004 062 (257 396
flat, 88 173 reflection, 125 248 glass), 837 366 drawn (515 193 with car-state frames);
shadows 12 491, smoke 115 994 (106 695 drawn with animated frames), marks 10 970: **0
differences** in uv, fan, flat colour, texture, mode, chroma key, blending, clamping. The
background picture: 30 checks, 0 differences.

## Scene size and build time

Measured at the first race frame of the runs above (release build with debug information,
`Instant` around `build`; the game waits meanwhile): 111 to 137 ms in five builds (run 3:
117 ms = textures 7, world meshes 66, part meshes and statics 4, ground test 40).

| | China |
|---|---|
| textures | 325 (324 records + the sky): 4.25 MB (texels + 1 KB per P8 palette); 261 P8, 42 RGB565, 22 ARGB4444 |
| `texture_frames` | 6 943 entries (the cars' 128-entry state lists, the flags' and the smoke's frames) |
| materials | 263 |
| meshes | 1 662: 1 585 world (70 584 triangles) + 77 part meshes (2 317 triangles) |
| vertices, indices | 61 052 vertices (36 bytes) + 72 901 triangles: 2.93 MB |
| statics | 2 260, placing 81 446 triangles; 946 grounded |

## Unknowns

* Other tracks were not built (the key scripts start China); multi-part world models,
  non-zero part placements, animated world models and world billboards are handled by the
  code but absent on China.
* Modes 3 and 4 and the mode-7 multiply variant are built from the code only (not on China);
  mode 3 loses its key (contract).
* The tint (instance +20h flags 60000h, texture x constant colour) has no field in the
  contract; not seen on China.
* `Env` animation: the game's value is [7bb98] = the drawn object's +3Ch, set per object
  (`824ba`), not a global value; with China's one-frame environment material it does not
  matter (note below).
* Why the engine's back-face selection disagrees more beyond 40 000 units (10 % of the
  kept triangles there are back faces); within the original distance the drawn back faces
  are slivers by their measured area, not looked at in pictures.
* The hybrid frame (cars by Glide over the native world) and the native picture itself were
  not compared here (W2-2 / W2-3).
* The ground test judges the geometry, not the view: an object grounded on a terrain piece
  at the corridor's edge can still show the void beyond that piece.

## Proposed contract changes (through the organiser)

* `docs/04-native-renderer.md` ("Materials"): back faces are culled for every object: the
  list builder `7ac50` drops the polygons whose normal faces away for world objects too
  (measured above); the set-ups' screen test ([aa6b4], `depth.rs` CULL_ON) only adds itself
  for scene-table instances.
* `sr-scene` `Instance::phase` = the engine's [7bb94] (instance +20h >> 2, low 16 bits with
  the tint flags; negative: the clock): already corrected by the organiser on main
  (01f48db, after this branch was cut); `phase_of` fills exactly that. Still open:
  `FrameData::clock` should say "[1473a0] & 3FFFh" (the engine's `824ed`; the raw clock
  clamps every animation to its last frame once it passes 3FFFh).
* `FrameData::env_phase` is per object in the game ([7bb98] = instance +3Ch): nothing is
  needed for the statics (China's environment material has one frame); for wave 3's car
  windows an `Instance::env_phase` or a note that each car's value is used.
* `sr-scene` `Transparency`: mode 3 is colour-keyed and blended at once (e.g. a
  `ChromaKeyBlended { key, blend }` or a separate `chroma_key: Option<u32>` field).
* `SceneBuild` (runtime side): a map from (model index, part slot) to the part meshes built
  for the scene-table models (wave 3), e.g. `part_meshes: Vec<(u32, u32, u32)>`; and
  `StaticMeta::track: bool` (cell list 0) if the visibility wants the road apart.
