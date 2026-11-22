# Polygons and materials (topic B)

How the 3dfx build of `STRT3FX.EXE` stores a polygon, how it turns one into Glide
triangles, and what each of its polygon modes means for a renderer. Link addresses as in
`docs/re/strt3fx.md`. "Verified" means checked by the diagnostic `SR_POLY_CHECK` (see
"How verified") on a deterministic run up to a race on Rally China (default car, chase
camera): 1200 frames with 3D drawing up to frame 1682 (menus' 3D scenes and the race), about
1.5 million polygons and 2.5 million triangles. "Code" means read from the disassembly only.

## Summary for the renderer

* A polygon record is 24h bytes: material offset, vertex count (3 or 4), the vertex
  offsets (a closed loop), a model-space face normal and one more distance. Texture
  coordinates are **not** in the record: they are per model vertex, in a table of the part
  (`[7bad4]`, the level's +24h), indexed by the same vertex offset.
* The material (20h bytes, model data) names the polygon **type**, which alone selects the
  set-up routine and with it the builder **mode** (0-7), and an animation list of texture
  indices. Topic C owns what a texture index points to.
* Texture coordinates are integers in 1/256 of the texture (0..255 across the whole
  texture on both axes); the Glide s, t are `u * S`, `v * T` with the texture entry's
  scales (1, 0.5 for a 2:1 texture). For a modern renderer: `uv = (u, v) / 256`.
* Vertex colours never vary. Textured polygons do not use the vertex colour at all; flat
  polygons (mode 2, the car bodies) have one 5:6:5 colour per polygon in the record (baked
  shading). There is no lighting and no gouraud shading in this build. An optional tint
  (texture times a constant colour) exists but was not seen in a race.
* Mode 5 (car windows) gets texture coordinates computed every frame from the vertex
  normals: a reflection vector's x and z, 127 texels per unit, scrolled by the car's motion.
  Exact formula below; reproduced bit for bit.
* The sky is a screen-aligned 256 x 256 RGB565 texture quad whose s follows the camera
  heading and whose horizon follows the camera pitch, plus a constant-colour ground quad.

## Where polygons come from

Per model part (frame loop `78e15`..`78e90`, docs/re/strt3fx.md "Hidden surfaces"):

1. `81f3c` part set-up: loads the matrix (`7a920`) and, from the chosen level entry
   (`[7bac4]`, topic A), `[7bb90]` = level+18h (flags), `[7bacc]` = vertex list (+1Ch),
   `[7bad0]` = normal list (+20h), `[7bad4]` = texture-coordinate table (+24h), each
   relative to the model (`[7bab4]`).
2. `7aa00` transform: view positions `68fc8 + 12 i`, packed screen positions `6ed94 + 6 i`.
3. `7ac50` builds the part's polygon list at `67088` from the octant list (level +28h + 4
   octant, a list of int16 polygon offsets relative to the level, ending with a value <= 0)
   (`81eac`): each entry is two dwords, `(material offset | variant, record pointer)`,
   ending with -1. All three of its paths (`7ad8a`, `7ae3a`, `7b0fa`) first drop back
   faces in model space: the record is kept only if `(v0 - c) . n >= 0` (v0 = the first
   vertex's model coordinates, c = the camera in model coordinates, patched into the code
   as immediates, n = record +14h; code). Then the near / far plane tests (variants 8 / C).
4. `7ba0c` draws the list: `call [798e0 + material.type + variant]`, eax = the record.
5. The set-up routine copies the vertices into its buffer (`cf4b0`, 8 bytes per vertex:
   packed position, u, v) and their view z into `aa5e0`, tests the screen bounds and
   facing, picks the texture, sets `[aa5c8]` = mode and calls the Glide vertex builder
   `[aa6ac]` = `cb6b0` (eax = vertex count, edx = vertex buffer, ebx = `aa5b8`).

## Polygon record (24h bytes)

Verified layout (all fields read by the code; statistics over every polygon that reached a
set-up routine):

| Offset | Type | Meaning |
|---|---|---|
| +00 | u16 | unknown; 0 in every record seen |
| +02 | u16 | flat colour, RGB 5:6:5 (mode 2 only, `d02ce`); 0 in every textured record seen |
| +04 | i16 | material offset, relative to the model's data base `[14739c]` (= `[model + B4h]`, set per object at `82570`); a multiple of 20h, so the list entry's low 4 bits are free for the variant |
| +06 | u16 | vertex count n: 3 or 4 (the only values seen; the textured set-ups load at most 4 vertices, and the normal's fixed offset leaves room for at most 5) |
| +08 | u16 x (n + 1) | vertex offsets (6 x vertex index) into every per-vertex table of the part: view position `68fc8 + 2 off`, packed screen position `6ed94 + off`, model vertex `[7bacc] + off`, texture coordinates `[7bad4] + off`, reflection coordinates `bcf94 + off`. The list is closed: entry n repeats entry 0 (all 1.5 million records); a triangle's +10h is 0 |
| +12 | u16 | padding (not read) |
| +14 | i32 x 3 | face normal in model space, Q14 (length 16384). It points **away from** the visible side: it is opposite to (v1 - v0) x (v2 - v0) in model space in every record except 401 non-planar mode-1 quads, and for 99.1 % of the polygons reaching a set-up (v0 - eye) . n > 0 in view space (eye = view origin; the rest are back faces of objects drawn without the screen-space test, or nearly edge-on). Used by the list builder's back-face test |
| +20 | i32 | a distance used by the list builder's near test (`7ae87`: view z of each vertex minus this value indexes the table `aa3a4`); for smoke, shadows and marks (mode 7) it equals twice the largest distance of the polygon's (and the part's) vertices from the model origin (all 22633); for other polygons it is not (see "Unknowns") |

The list entry's variant (low 4 bits) selects the column of the dispatch table: 0 = record
with all vertices in front of the camera (the routines below), 4 = polygon rebuilt in screen
space by the near-plane clipper (a buffer at `70510`: material offset, n, then per vertex
z, ?, packed position, u | v << 16), 8 = near-plane clipper (`ced48` for all textured types,
`cfaf0` flat, `d4de8` reflection), C = the other clipper (`cf0d4`, `cfcbc`, `d50f0`; called
by the list builder's far path at `7b1bc`). The clippers are called from the list builder
(`7b07c`, `7b1bc`) and write variant-4 entries.

## Material (20h bytes, at `[14739c]` + record +04)

| Offset | Meaning |
|---|---|
| +00 | type, a multiple of 10h: `798e0 + type` is the row of set-up routines (below) |
| +04 | offset of the animation list, relative to `[14739c]`: `{u32 count, u32 unknown, u32 texture index[count]}` |
| +08, +0C | copied to `[aa5b8]` / `[aa5bc]`; equal to the texture's width and height in texels for the three materials checked against their texture entry (64 x 64 smoke, 128 x 64 track, 128 x 128 reflection). The 3dfx builder does not use them for textured polygons (a dead path at `d56ce`, behind an unconditional `jmp` at `d56c9`, scaled mode-5 coordinates by them / 32) |
| +10 | copied to `[8309c]`; not used by the 3dfx path (10h x the material offset in the scenery and smoke materials looked at, other values in car models) |

Texture selection (`ca8f4` and copies; verified, 0 texture or palette mismatches):

* modes other than 5: `frame = (count * p) >> 14` (signed), with p = `[7bb94]` = instance
  +20h >> 2 (or, when +20h is negative, the global animation clock `[1473a0] & 3FFFh`,
  `824ed`); when bits 17-18 of p are set (a tint, below) the low 16 bits of p are used;
* mode 5: the material is the reflection material `[aa6a8]` when set (the data base of a
  model loaded with flag 4, `540eb`) else the polygon's; `frame = (count * [7bb98]) >> 16`,
  `[7bb98]` = instance +3Ch;
* texture index = list[frame]; texture entry = `1479d4 + 38h * index` (topic C). The
  builder uses: +04 TMU slot (address at `171814 + 8 slot`; negative = not resident, then
  downloaded), +0C palette (downloaded with `grTexDownloadTable` for P8), +14 chroma-key
  colour (modes 1, 3), +18 `GrTexInfo` (+24 = format), +2C / +30 s and t scale (float),
  +34 mode 7's blend choice.

## Type -> set-up -> mode

The dispatch table `798e0` (runtime copy checked: unchanged), column 0. All set-up routines
except mode 2 are identical apart from the mode number they store (instruction diff) and the
mode-5 texture coordinates:

| Material type | Set-up (variant 0) | Variant 4 | Mode | Seen on China |
|---|---|---|---|---|
| 00h, 10h, 20h, 140h, 150h | `ca670` | `ca4a0` | 0 | type 10h: road, terrain, buildings, car parts |
| 30h-70h | `d5f30` | `d5d60` | 1 | type 40h: trees, fences, signs; tyres |
| 80h, 90h | `d02cc` | `d01c4` | 2 | type 80h: car bodies |
| A0h, B0h, 160h-1F0h | `d567c` | `d54bc` | 5 | type A0h: car windows |
| C0h, D0h | `d25c0` | `d23f0` | 4 | not seen |
| E0h, F0h | `ce9f0` | `ce820` | 3 | not seen |
| 100h, 110h | `ce190` | `cdfc0` | 6 | type 100h: car glass, stickers, number plates |
| 120h, 130h | `d2e20` | `d2c50` | 7 | type 120h: smoke, shadows, tyre marks |

Modes 8 and 9 are not polygon types: the sky routine `7b218` calls the builder with them.

## The set-up routines (`ca670` and copies)

Verified behaviour (code read for all; predictions matched for modes 0, 1, 2, 5, 6, 7):

* Vertices in record order into `cf4b0` (packed position, u, v from `[7bad4] + off`, or from
  `bcf94 + off` in mode 5), view z into `aa5e0`. Mode 2 copies packed positions only, to
  `668b8`, and passes them starting after the rightmost vertex (rotation only).
* Screen test on the packed positions: rejected when wholly off screen or degenerate;
  drawn directly when min x >= 0, min y >= 0, max y <= the y limit and `(max x << 16 |
  max y) <= x limit << 16` (immediates at `ca821` / `ca82a`, patched at run time: 852 and
  400 in this run, 638 and 399 in the file); otherwise through the 2D clipper (mode 0
  `ca3e0`, mode 6 `cdf00`, mode 4 `d2330`, mode 5 `d5414`, mode 2 `cfeb8`, ...), which
  rebuilds the vertex list at the screen edges.
* Back faces (polygons drawn directly only, not mode 2): when `[aa6b4] != 0` and bit 0 of
  `[aa5b0]` is clear, culled if `(x0 - x1)(y0 - y2) - (y0 - y1)(x0 - x2) < -20` (packed
  game coordinates, y down; the low word of a packed position is y): that is
  `cross(v1 - v0, v2 - v0) < -20`, i.e. **front faces are clockwise on screen**. In the
  run the test was on for every polygon of modes 2, 5, 6 and 7 and for 20 % of mode 0 and
  35 % of mode 1; of all polygons reaching a set-up 64.4 % were clockwise, 33.7 %
  degenerate (cross 0 at 640 x 400) and 1.9 % counter-clockwise.

## The vertex builder (`cb6b0`, Watcom C)

Entry: eax = n, edx = vertex buffer, ebx = `aa5b8` = `{+0 [aa5b8], +4 [aa5bc], +8 [aa5c0]
texture index, +C [aa5c4] colour, +10 [aa5c8] mode}`, `[aa5dc]` = view z list, `[b9b70]` =
tint. Every Glide state call is skipped when the cached value (`[173ff4]` colour combine,
`[173ffc]` fog, `[173fb4]` texture index, `[173fb8]` mode, `[173ff8]` filter) is unchanged;
modes 1, 3 and 7 store -1 in the mode cache so their state is sent for every polygon.

1. `grFogMode([12373c] ? GR_FOG_WITH_TABLE : GR_FOG_DISABLE)` (fog was off in the race).
2. Mode 8 / 9: sky or flat background (below), return.
3. Mode 2: chroma off, blend `(ONE, ZERO, ONE, ZERO)`, alpha combine `(SCALE_OTHER, ONE,
   CONSTANT, TEXTURE)`, colour combine `(SCALE_OTHER, ONE, CONSTANT, ITERATED)`; per vertex
   x, y, r = `(c & F800h) >> 8`, g = `(c & 7E0h) >> 3`, b = `(c & 1Fh) << 3` with c =
   `[aa5c4]` = record +02; 1/w = the same constant for all vertices: 1 / (mean of w of the
   first three vertices of the last textured polygon built) (`cbbd3`; the depth is
   meaningless, see `crate::depth`).
4. Textured modes: colour combine `(SCALE_OTHER, ONE, CONSTANT, TEXTURE)` (texture only),
   or with a tint `(SCALE_OTHER, LOCAL, CONSTANT, TEXTURE)` and `grConstantColorValue(
   [173fa4 + 4 k])`; texture download if needed, palette, `grTexSource`; the mode's state
   (table below); per vertex:

   ```
   x   = x_table[[aa6b8] + (packed >> 16)]      (float(i); widescreen-shifted; mirror table b06cc)
   y   = y_table[[aa6bc] + (int16) packed]       (1.2 * i)
   oow = -1.0 / (z - 128)                         (K = [122314] = -1.0; w = 128 - z, the same
                                                   w the projection divides by, crate::subpixel)
   sow = u * S * oow,   tow = v * T * oow         (S, T = texture entry +2C, +30)
   ```

   r, g, b, a are **not written**: the buffer keeps the last flat polygon's colour
   (observed: (40, 44, 40, 255) = 2965h), which the combine modes ignore.
5. Triangles: a fan `(v0, vi, vi+1)`, i = 1 .. n - 2, from the buffer at `1738a8` (3Ch per
   vertex), `grDrawTriangle` returning to `cc6f3`.

Texture filtering is bilinear for everything (`grTexFilterMode(0, BILINEAR, BILINEAR)`,
set by the sky path at `cb7be` and cached; verified on every triangle).

## Mode table

| Mode | Meaning | Glide state (besides texture source, bilinear) | A modern renderer must |
|---|---|---|---|
| 0 | opaque textured | clamp s, t; chroma off; blend `(ONE, ZERO)`; colour = texture; alpha = texture | draw opaque, clamp-to-edge, depth write |
| 1 | colour-keyed textured (cut-outs) | as 0 + chroma key on, key = texture entry +14 | discard texels equal to the key colour (compare after palette lookup, topic C), opaque otherwise |
| 2 | flat colour (no texture) | colour = iterated (constant per polygon from record +02); chroma off; blend `(ONE, ZERO)` | draw the record's 5:6:5 colour, opaque, real depth (the game's 1/w is garbage) |
| 3 | colour-keyed, additive-like (not seen) | as 1 + blend `(ONE, SRC_COLOR, ONE, ZERO)`: dst = src + dst * src | key as mode 1, then blend src + dst * src (code only) |
| 4 | opaque textured (not seen) | identical to mode 0 in this builder | treat as mode 0 (code only) |
| 5 | reflection (car windows) | **wrap** s, t; chroma off; blend `(ONE, ZERO)` | opaque, repeat addressing, texture coordinates computed per frame (below), negative coordinates allowed |
| 6 | alpha-blended overlay (glass, stickers) | clamp; chroma off; blend `(SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ZERO)` | alpha blend with texture alpha (ARGB4444 textures), no depth write, drawn after the opaque polygons of the same part |
| 7 | decal (smoke, shadows, tyre marks) | clamp; chroma off; blend `(SRC_ALPHA, ONE_MINUS_SRC_ALPHA)` when texture entry +34 = 0, else `(DST_COLOR, SRC_COLOR, ONE, ZERO)` = 2 x src x dst (fog off for those) | alpha blend (all seen: +34 = 0); support the multiply variant (code only) |
| 8 | sky + ground (from `7b218`) | sky: wrap, texture `173fd4` at TMU `[173f80]`, colour = texture; ground: colour = constant | below |
| 9 | flat background (when `[123740]` = 0, not seen) | colour = constant `[123744]` (5:6:5) over the whole view | fill the view with that colour (code only) |

Mode-independent: alpha combine always `(SCALE_OTHER, ONE, CONSTANT, TEXTURE)` (texture
alpha); no depth buffer, no culling in Glide (`grCullMode(0)`); fog only when `[12373c]`.

## Texture coordinates and their units

The table at `[7bad4]` (level +24h, 6 bytes per vertex: u int16, v int16, a third word, 0
where looked at) holds one (u, v) per model vertex, shared by every polygon using the
vertex. Values seen: 0..254 (e.g. a smoke quad 0/253, track sections 0/128/254). The
texture entry's scales convert to Glide's coordinate range (256 along the longer side): (1,
1) for square textures, (1, 0.5) for 2:1 (128 x 64), (1, 0.25) for 4:1 (128 x 32; all
seen in the samples), so that u = 256 is always the
texture's right edge and v = 256 its bottom edge. A renderer should use `uv = (u, v) / 256`
with clamp addressing (wrap in mode 5); the game's coordinates are exactly reproduced
(s/w, t/w equal to the last bit for 6.9 million textured vertices).

## Mode 5: reflection coordinates (`bb380`, verified exactly)

For a part whose level flags (`[7bb90]`, level +18h) have bit 2000h, `7aca2` calls `bb380`
after the transform; it fills `bcf94 + 6 i` (u, v int16) for every vertex i, and the mode-5
set-up reads its coordinates there instead of `[7bad4]`:

```
n_i   = R * N_i            N_i: vertex normal (3 floats) at [7bad0] + 4 + 12 i (count at +0);
                           R: the part's matrix (draw record +118h, 9 ints) * [bb7e4] (~1/16384),
                           same layout as the transform (x' = m0 x + m3 y + m6 z) (bb6f0)
e     = p_i / |p_i|        p_i: the vertex's view position (68fc8 + 12 i)
r     = 2 (e . n_i) n_i - e
u     = round(127 * r.x) + 127 - ox
v     = round(127 * r.z) + 127 - oz       (round to nearest even, fistp)
```

`ox`, `oz`: the object's scroll offsets, `round(acc) & FFh` of accumulators at instance
+30h / +38h (floats), which grow every frame by 0.008 x the object's displacement since the
last frame (instance +08..+10 minus +24..+2C, saved again after) rotated by the 3 x 3 float
matrix at `82420` (presumably the camera; topic D); 0 for objects that are not scene
instances (`[7bb8c]` without bit 40000000h). The texture (seen: index 143h, 128 x 128
RGB565, scales 1, 1) is drawn with wrap, so u, v outside 0..255 repeat (traces show
negative s, t). The coordinates use the x and z of the reflected view vector: a horizontal
(top-down) projection of the environment that moves with the view and the car. Verified:
101196 vertex coordinates recomputed with this formula equal the game's table, and the
offsets equal the game's (bb6ac / bb6b4) for every polygon.

A renderer can do this in a vertex shader: per-vertex normal (static, from `[7bad0]`), the
view-space position, and the object's two scroll offsets as a uniform (computed on the CPU
exactly as above, or read from the instance).

## Sky and background (`7b218`, builder mode 8; verified 596 frames)

```
s0      = ((7FFh - camera.heading) & 3FFh) * 0.25     camera = [81e0c], heading at +4 (11 bits):
                                                       the 256-texel sky repeats every 180 degrees
h       = (((-54152 * [798ac]) as i32) >> 7) / -54152 + [81e3c] + 28h - 200h + 2
                                                       ([798ac]: camera matrix element, topic D)
n1      = [81e24] - 1 (view height - 1);  yb = 1FFh - |h|
te      = yb > n1 ? (yb - n1) * 0.5 : 0;  yb = min(yb, n1)
top     = y_table[[aa6bc]];  y_h = top + y_table[trunc(yb)];  bottom = top + y_table[trunc(n1)]
left    = x_table[[aa6b8]];  right = x_table[[aa6b8] + 639] (the 9FCh at cb973, widescreen-patched)
sL, sR  = s0, s0 + 159   (swapped when [aa5b0] != 0: the mirror)
quad    = (left, top, sL, |h|/2) (right, top, sR, |h|/2) (right, y_h, sR, 255 - te) (left, y_h, sL, 255 - te)
ground  = (left, y_h)-(right, bottom) in constant colour 5:6:5 -> 888 of [81e1c] (low half; China: 4288h = 405040h)
```

The sky texture: `GrTexInfo` at `173fd4` (256 x 256, RGB565, data pointer at +10h), TMU
address `[173f80]`, wrap, bilinear; 1/w is left as is (1.0 in every frame seen). The ground
quad is drawn only when `y_table[trunc(yb)] < y_table[trunc(n1)]`. With `[123740]` = 0
(not seen) mode 9 fills the view with `[123744]` instead.

A modern renderer: draw the sky as a screen-space quad behind everything with these
coordinates (or, better for wide views, a cylinder whose s maps 256 texels to 180 degrees
of heading and whose t matches the horizon line), then the ground colour below the horizon,
then the world.

## Colours, lighting and tint

* No vertex lighting, no gouraud: textured polygons use texture colour only; flat polygons
  one colour each (record +02). Verified: 0 colour mismatches over 354166 flat vertices.
* Tint: when bits 17-18 of `[7bb94]` are set (instance +20h >= 80000h, bits 19-20 of it),
  textured polygons are drawn texture x constant colour `[173fa4 + 4 k]`, k = 1..3: CCCCCCh
  (0.8 grey, `54730`), FFBD6Bh, C3E6FFh (`546df`). Not seen in the race (0 tinted
  polygons); who sets those instance bits is unknown.

## How verified

* Disassembly: `work\glue_dis.txt` (b8000-e8000, fixups applied), `work\engine_dis.txt`,
  `work\game_dis.txt`; Glide thunk names from `work\glide_imports.txt` (thunk = `ee92c + 5 x
  index`); runtime values from an `SR_STATE_DUMP` of frame 1600 (dispatch table, builder
  pointer `[aa6ac]` = `cb6b0`, `jmp` at `d56c9` unpatched, patched screen limits, materials,
  animation lists, texture entries, tint table).
* `SR_POLY_CHECK=100 SR_POLY_SAMPLE=1600 SR_DETERMINISTIC=1`, China race keys from
  `docs/04-native-renderer.md`, `--seconds 40` (`crates/sr-runtime/src/native/polygons.rs`).
  It observes every variant-0 set-up routine, predicts from the record, material, animation
  list, texture entry and the part's tables the builder's vertices and Glide state, and
  compares them with the game's own vertex buffer just before each `grDrawTriangle` (so the
  runtime's sub-pixel and depth changes do not enter) and with the Glide state the game has
  set at that moment. Result over 1200 frames (summary at frame 1682):

  | Mode | Polygons | Triangles | Vertices compared (fan order) | Mismatches |
  |---|---|---|---|---|
  | 0 | 1159378 | 1980445 | 5760396 | 0 s/t, 0 1/w, 0 state |
  | 1 | 197887 | 301784 | 855747 | 0 |
  | 2 | 74264 | 118167 | 354166 | 0 colour, 0 state |
  | 5 | 29474 | 40357 | 120345 | 0; 101196 reflection coordinates recomputed, 0 different |
  | 6 | 46213 | 44274 | 131970 | 0 |
  | 7 | 22633 | 22750 | 63075 | 0 |
  | 8 | 596 sky + 596 ground quads | | 4 vertices each | 0 |

  State compared per triangle: colour and alpha combine, blend, chroma key mode and value,
  clamp, constant colour, texture binding (`GrTexInfo` and TMU address), palette contents,
  filter, fog. "Vertices compared" excludes polygons that went through the 2D clipper
  (about 10 % of mode 0); for those, 91696 kept vertices were found by position (458
  differed: two vertices on one whole pixel, or the clipper's own values) and 87026 were
  new. Triangles of near-clipped (variant 4) polygons are not predicted (853 in all, from
  13337 variant-4 polygons).
* Samples (frame 1600, `SR_POLY_SAMPLE`): a mode-1 track quad, record `1111a284` = `0000
  0000 0800 0004 0012 000c 0000 0006 0012 | 0000 | 081c 0 0 | 3f7a .. | 384e`, material
  `10820800` = `{40h, 1BC8h, 80h, 40h, 8000h}`, texture 52h (128 x 64 P8, scales 1 / 0.5,
  key 8C9E84h): uv (128, 0) (128, 254) (0, 254) (0, 0) -> s, t (128, 0) (128, 127) (0, 127)
  (0, 0), two triangles (v0 v1 v2) (v0 v2 v3), equal to the game's. A smoke quad (mode 7,
  model "aSMOKE", texture DBh 64 x 64 ARGB4444): uv 1/253 -> s, t 1/253; car glass (mode 6,
  material `{100h, 260h, 80h, 20h, 0}`, texture 113h 128 x 32 ARGB4444, scales 1 / 0.25): uv
  (176, 3) (180, 253) (243, 252) -> s, t (176, 0.75) (180, 63.25) (243, 63), blend (1, 5, 4,
  0); a car window (mode 5, texture 143h 128 x 128 RGB565): uv (-63, 32) (-46, 34) (-46, 33)
  (-62, 32) from the reflection table.

## Unknowns

* Record +00 (always 0) and the meaning of +20h beyond mode 7. Candidates tested (within
  1 unit): twice the largest |v| of the polygon (mode 0: 8 %), of the part (mode 0: 18 %),
  the plane distance |n . v0| / 16384 (never), the largest |v - v0| or |vi - vj| (rarely).
  The list builder's near test that uses it (`7ae3a`..`7b010`, table `aa3a4`) was not
  decoded (topic A / D); a GPU renderer does not need it.
* Modes 3, 4 and 9, the tint and the mode-7 multiply variant were never exercised on China;
  their state is from the code only. Which tracks or objects use them is unknown (check the
  other tracks and the menus with `SR_POLY_CHECK`).
* Polygons with more than 4 vertices: none seen; the textured set-ups would read only 4.
* The animation list's second dword and material +08 / +0C / +10 (width, height, 10h x
  offset in every case looked at) are not used by the 3dfx path; their meaning for topic C
  is a guess.
* The near-plane (variant 8 / 4) and far (C) clippers were not decoded beyond their entry
  points; a GPU renderer clips by itself and does not need them.
* `[798ac]` and `82420` are named from their use only (camera matrix element, camera
  rotation as floats): topic D.

## Interfaces needed

* **Topic A** (world and objects): per part and level: the polygon octant lists (level +28h,
  16 offsets relative to the level, each to an int16 list of polygon offsets relative to the
  level), the record pointer = level + offset; vertex list (+1Ch), normal list (+20h, -1 =
  none), texture-coordinate table (+24h), flags (+18h, bit 2000h = reflection coordinates);
  the model's data base `[model + B4h]` for materials; per instance: +20h (animation phase
  and tint bits), +3Ch (mode-5 animation), +24h..+38h (reflection scroll state); the global
  animation clock `[1473a0]`. A static mesh can take all levels' polygons once (the union of
  the 16 octant lists, without duplicates).
* **Topic C** (textures): texture entry `1479d4 + 38h i`: palette +0C, chroma key +14,
  `GrTexInfo` +18, scales +2C / +30, +34; the sky texture (`173fd4`, `[173f80]`); the
  reflection material `[aa6a8]`. Chroma keying compares the palette-expanded colour with the
  key (Glide semantics) and must be done per texel before filtering.
* **Topic D** (transforms): view positions and the projection with w = 128 - z (1/w as the
  builder computes it); the part matrix for normals (draw record +118h); camera heading
  (`[81e0c]` +4) and `[798ac]` for the sky; the float matrix at `82420` for the reflection
  scroll.
* **Topic E** (renderer): materials as in the mode table (opaque, colour key, flat colour,
  additive-like, reflection with wrap and per-vertex dynamic uv, alpha blend, 2x multiply,
  tint = texture x constant), front faces clockwise in screen space (y down) or the record
  normal test, bilinear filtering as the game asked (the runtime's filter setting may
  override), the sky as a screen-space or cylindrical background.

Corrections for `docs/re/strt3fx.md` ("Hidden surfaces and depth"): the builder's entry is
`cb6b0` (`[aa6ac]`; `cc5e8` is a label inside it); the addresses listed per mode there
(`ce47b`, `ceceb`, `d0430`, ...) are the set-ups' `call [aa6ac]` sites, the entries are in
the table above; mode 2's 1/w is the reciprocal of the mean w of the last textured
polygon's first three vertices.

Proposed additions to `docs/04-native-renderer.md` (scene contract): `Vertex.uv` in units of
1/256 of the texture (or normalised, from u / 256); `Material` needs `tint: Option<u32>` (per
instance, dynamic), the mode list `Opaque, ChromaKey, Flat, AdditiveKeyed (3), EnvMap
(mode 5, wrap, dynamic uv), Blended, Decal { multiply: bool }` and a flat colour; `Mesh`
needs per-vertex normals for mode 5; `FrameData` needs per-instance animation frame (texture
index per material) and the mode-5 scroll offsets; a `Sky { heading_s, horizon_y,
ground_rgb }` per view.
