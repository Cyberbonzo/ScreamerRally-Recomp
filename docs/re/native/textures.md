# Textures and palettes (wave 1, topic C)

Where the game keeps every texture and palette of a race, how a polygon names its texture,
how the texture manager feeds the Voodoo's texture memory, and how to build every texture
of a track up front. Link addresses as in `docs/re/strt3fx.md`. Code:
`crates/sr-runtime/src/native/textures.rs` (`SR_DUMP_TEXTURES`).

**Verified** = seen in the disassembly *and* checked at run time (the check is named);
**inferred** = from the disassembly only. All run-time checks are from deterministic runs
(`SR_DETERMINISTIC=1`) of the attract-mode race and a player race on Rally China with the
default car, the rear-view mirror on and a camera change (1.29 million texture binds, see
"How it was verified").

## Summary for the renderer

* Every texture of a race is in RAM for the whole race, already converted to its Glide
  format (P8 with a 256-entry palette, RGB565 or ARGB4444), one level, power-of-two size.
  Nothing is streamed from disc during the race; the only exception is the background
  picture (sky), whose RAM copy is freed once it is in texture memory. (Verified: every
  texture download and bind of the run came from a texture record's RAM data, or was the
  background.)
* A polygon names a **material** (a 20h-byte entry of its library's directory); the material
  names a **frame list** of texture **records** (one per animation frame or car state); the
  record (38h bytes, table at 1479d4) holds the `GrTexInfo`, the palette pointer and the
  chroma colour. Reflection polygons (mode 5) take the environment-map library's material
  instead of their own.
* The game's texture manager only decides what is resident in the TMU (a ring); for a
  renderer that uploads everything once it can be ignored.

## Loading (where the data comes from)

* A race loads several **libraries** (`%s.MLB`, "material library", read from the packed
  archives in `GFX\`), each into one of 32 slots at **147420** (2Ch bytes each, filled from
  slot 31 downwards): +00 data block, +04 temporary pointer during the load, +08 name
  (16 bytes), +18h archive name (16 bytes), +28h a flag (meaning unknown). Rally China
  (verified, slot names logged by the dump):

  | slot | +08 | +18h | content |
  |---|---|---|---|
  | 31 | `map3` | `map32.dat` | the track |
  | 30 | `misc` | `map32.dat` | smoke, dust, two 8x8 flat textures |
  | 29..26 | `auto1`..`auto4` | `cars102.dat` | the four cars |
  | 25 | `env` | `envp302.dat` | the environment (reflection) map |

* The library loader `53fd8` (inferred: eax = name, edx = flags: 1 materials, 4 materials
  of the environment map (also stored in [aa6a8]), 2 the background picture; ebx = a
  resolution class, index into the table at 123750, -1 = 1.0; the archive name at 128e58)
  reads `<name>.MLB`, calls `cc714` (size pass) and `ccae4` (conversion) and keeps the
  result in the "MATERIALS" block ([1479cc], then slot +00); with flags 1 or 4 it first
  reads the source palettes `<name>.HIC` (`54a1d`). Slots are unloaded by `53b90` /
  `53d52`, which also free the slot's records (record +10h = slot).
* **Data block layout** (verified for every library of China: directory and lists walked
  by the dump, texture data offsets in `index.tsv`): the material directory (20h-byte
  entries from offset 0, ending with an entry whose first four dwords are 0), the frame
  lists, then, from the next multiple of 32 after them, the converted textures (each: its
  texels, then for P8 its 1 KB palette). E.g. `map3`: 195 materials, textures from +2300h
  to +375780h. The file's original texel pages (256 bytes per row) and its RGB565 source
  palettes (`%s.HIC`, "HI PAL", [147348]) are only used during the conversion (inferred).
* The **converter** `ccae4` (inferred, results verified below) for each directory entry
  with a frame list:
  * size: the entry's width (+08) and height (+0C) times the factor [12374c] (= 1.0 on
    China; table at 123750: 0.9, 1.0, 0.85, 0.73, 0.9, 0.9, 1.0, 1.0, 1.0 by load class,
    from a peek), rounded to a power of two (>= E0h -> 256, >= 70h -> 128, >= 38h -> 64,
    >= 1Ah -> 32, >= 0Ch -> 16, >= 6 -> 8, else 4), aspect at most 8:1; the texels are
    resampled to that size (`db470`, not followed); +08 / +0C are overwritten with the new
    size;
  * one record per frame-list entry (entries with the same source share a record); the
    frame list's entries, file offsets in the file, are overwritten with record ids;
  * the format from the entry's type (+00), see "Formats".
* Records are allocated from the first free one (BF8h at most: "Superato limite texture
  cache"), so ids follow the load order: on China `map3` 0-212, `misc` 213-224, `auto1`
  225-250, `auto2` 251-274, `auto3` 275-298, `auto4` 299-322, `env` 323 (verified). Ids are
  reused after an unload: a renderer builds its textures per race load.

## Structures

### Material (directory entry, 20h bytes, at data block + 20h n)

| off | content |
|---|---|
| +00 | type: 10h opaque, 40h colour-keyed, A0h reflection, 100h alpha, 120h alpha (special colours), 80h (no frame list, size 0: no texture), E0h (not on China) |
| +04 | offset of its frame list from the data block (0 = no texture) |
| +08, +0C | texture width, height (after loading: the converted power-of-two size; equal to the records' sizes for all 8,679 frame-list entries on China) |
| +10 | offset of its RGB565 source palette in the `%s.HIC` buffer (multiples of 200h; inferred) |
| +14..+1F | 0 on China |

Observed (verified, bind check): every bind of a polygon of mode 0 had a type-10h
material, mode 1 -> 40h, mode 5 -> A0h, mode 6 -> 100h, mode 7 -> 120h (counts in "How it
was verified"). Modes 3 and 4 never bound a texture in the runs.

### Frame list (at data block + material +04)

`{count, word1, id[count]}`: `count` 1 (most track materials), 5, 6 or 10 (animations),
128 (every textured car material: a table indexed by the car's state). `word1` is an
unknown byte value (0..FFh). See "Resolution path" for how a frame is chosen.

### Texture record (1479d4 + 38h id, up to BF8h)

| off | content | verified by |
|---|---|---|
| +00 | in use (1) | all 324 used records named by materials and exported |
| +04 | texture-manager slot (index into 171814) while resident in the TMU, else -1 | slot check |
| +08 | bytes in texture memory = texels x bytes per texel | 324 / 324 |
| +0C | P8 palette: 256 dwords 0x00RRGGBB, right after the texels | bind check; 261 / 261 right after the texels |
| +10 | library slot | ids by library, unload code (53cfe) |
| +14 | chroma-key colour 0x00RRGGBB | grChromakeyValue check |
| +18 | `GrTexInfo`: small LOD, large LOD (always equal), aspect, format, data pointer | it is the `info` pointer of every grTexSource / grTexDownloadMipMap |
| +2C, +30 | s and t scale: width / larger side, height / larger side (1.0, 0.5, 0.25, 0.125) | 324 / 324 |
| +34 | 1 = type-120h texture converted to a grey RGB565 ramp (texels 30h-3Fh present) | 0 on China (code only) |

The vertex builder multiplies the polygon's integer s, t by +2C / +30 (cc683, cc68f) before
Glide's 1/w, so a polygon's texture coordinates are in 1/256 of the texture on both axes
whatever its size and aspect: **u = s / 256, v = t / 256** (inferred; consistent with the
Glide layer's aspect ranges, with which the current renderer draws correctly).

### Texture manager state (inferred from `cb6af`, `54d58`; values from a peek at a race frame)

| address | content |
|---|---|
| 171814 + 8 n | 400h slots: TMU address, record id (-1 = free) |
| 173f80 | TMU address of the background picture (grTexMinAddress, 0) |
| 173f84 / 173f88 | ring start (173f80 + 20000h) / end (grTexMaxAddress: 3FFFF8h with the runtime's 4 MB) |
| 173f8c | slots used; 173f90 eviction cursor (-1: no wrap yet); 173f98 next free TMU address |
| 173fb4 | id of the last bound record (bind skipped when the same) |
| 173fb8 | last polygon mode (mode state skipped when the same) |
| 173f9c, 173fa0, 173fa4 | RGB444 colours baked into type-120h textures at load (0EDCh, 0EDCh, 0 on China) |
| 173fa4.. | table of constant colours for polygons whose phase carries the flags 60000h (cbd86; topic B) |

### Background picture

`GrTexInfo` at **173fd4** (256x256, RGB565, one level), loaded from `%s.BCK` (the low words
of 65,536 dwords, inferred) into [14734c], downloaded once to TMU address [173f80] (544fe),
then freed ([14734c] = 0, verified at race time; the info's data pointer then points to
reused memory). The dump reads it back from the Glide layer's TMU memory. It is drawn by
`cb6af` type 8 as a 2D quad, wrapped, bilinear (topic B).

## Formats (converter `ccae4`, results verified on China)

| material type | texels | Glide format | conversion |
|---|---|---|---|
| 10h, 40h | > 1024 | P8 (5) | indices kept; palette entries 0x00RRGGBB with the 5/6-bit channels shifted, low bits 0 (261 / 261 palettes); type 40h: entry 0 = the chroma colour (98 / 98) |
| 10h, 40h | <= 1024 | RGB565 (0Ah) | palette looked up; type 40h: index 0 = the file palette's entry 0 |
| 100h | any | ARGB4444 (0Ch) | index 0 -> 0 (transparent), indices >= F0h alpha 6, else alpha F; colours 5/6 -> 4 bits |
| 120h | any | ARGB4444 | index 0 -> 0; 1-0Fh / 10h-1Fh / 20h-2Fh: alpha = low nibble with colour [173fa4] / [173f9c] / [173fa0]; 30h-5Fh: white with that alpha; others palette colour, alpha F. With any texel 30h-3Fh: grey RGB565 ramp instead (+34 = 1) |
| A0h, E0h, other | any | RGB565 | palette looked up (E0h: index 0 = palette entry 0) |

* Chroma colour (+14h) = `54e2b(file palette entry 0)`: RGB565 -> 888 through the tables
  12378c (5 bit) and 12380c (6 bit), which are bit replication except 6-bit entry 40
  (= 171, not 162): a data quirk (verified by reading the tables). The current renderer
  keys texels whose decoded colour equals it exactly; all type-40h textures on China have
  such texels.
* No mipmaps: small LOD = large LOD in every record and every bind (verified); the game never
  calls grTexMipMapMode in a traced race frame. It sets `grTexFilterMode(0, bilinear,
  bilinear)` once (returning to cb7c3) and the clamp per mode: wrap for the background
  (returning to cb763) and mode 5 (cc557), clamp in every other mode (cc3b1, cc47e,
  cc4e2, cc5a6) (trace of frame 1500, by return address).

## Resolution path: polygon -> texels + palette

1. Polygon record +04 (int16) = offset of its material from the model's data block
   ([model + B4h], stored in [14739c] by the object set-up 8246c). The set-up routines do
   `movsx eax, word [esi+4]; add eax, [14739c]` (e.g. ca670) and keep the result in
   [7bb80]; each also has a second entry that takes the offset as a dword at +00 of
   another structure (e.g. ca4a0: `mov eax, [esi]; add esi, 4`), and two routines take it
   from a list they build first (ced5f, d4df4). Which polygons go through which entry is
   topic B's. Verified at every bind: [14739c] is a loaded library's data block
   and [7bb80] an entry of its directory.
2. Frame list = data block + material +04; `count` = its first dword.
3. Frame (verified at every bind: the result equals the id the game hands to `cb6af` in
   [aa5c0], which equals the record bound):
   * modes 0, 1, 3, 4, 6, 7: `frame = count * p >> 14` (signed), `p` = [7bb94] (low 16
     bits only when it has the flags 60000h); [7bb94] = object instance +20h >> 2, or, for
     instances whose +20h is negative, the animation clock [1473a0] & 3FFFh (8246c);
   * mode 5 (d59c0): when an environment library is loaded ([aa6a8] != 0, its data
     block), material 0 of that library instead of the polygon's own; `frame = count *
     [7bb98] >> 16`, [7bb98] = instance +3Ch.
4. Record = 1479d4 + 38h id: texels at +28h (its `GrTexInfo` data), palette at +0Ch (P8),
   chroma colour at +14h.

`textures::polygon_texture(mode, base, material, phase, env_base, phase2)` implements 3.

## Texture memory (the game's manager, `cb6af` from cbd40)

Per textured polygon: if the record is not resident (+04 < 0) it gets the next TMU range of
the ring (evicting slots it overlaps when it wraps, setting their records' +04 to -1),
`grTexDownloadMipMap(0, addr, 3, rec + 18h)`; then, unless the record is the last one
bound, `grTexDownloadTable(0, GR_TEXTABLE_PALETTE, rec + 0Ch)` for P8 and
`grTexSource(0, addr, 3, rec + 18h)`; slot and record +04 are written after the
grTexSource. Then the mode's state (cc388..: clamp, chroma key with rec + 14h for modes 1
and 3, blending), then the vertices (cc5e8). On China the whole race fits: at Glide frame
1500, 127 slots, next free address 161480h (1.26 MB after the background's 128 KB), no
wrap (peek). The palette is downloaded at almost every texture change (820 of 933
grTexSource calls in a traced frame): the cache in the Glide layer keys paletted textures
by palette contents.

## Special textures

* **Background** (sky): above.
* **Reflection**: mode-5 polygons (material type A0h: car windows) draw material 0 of the
  `env` library (record 323 on China, 128x128 RGB565, wrapped), not the 128x128 sky texture
  each car library carries for its own A0h material (records 234, 272, 277, 316): in a race
  every mode-5 bind was of record 323 (material path check).
* **Colour-keyed**: type 40h (mode 1, chroma key on, value = record +14h).
* **Alpha**: type 100h (ARGB4444, mode 6: car glass and overlays) and 120h (mode 7: smoke
  215-218, dust 219, spray 220-224 by appearance; colours baked per track).
* **Animated**: `map3` materials at +680h and +6C0h (10 frames each: the two waving flags,
  records 52-61 and 63-72), all 10 frames bound in the race; `misc` +40h (smoke, 6 entries
  over 4 records) and +80h (5 frames), all frames bound (verified: frames seen at binds).
* **Car states**: every car material has a 128-entry list; in the runs entries 0, 1, 2, 3,
  66 and 67 were used. Records differ by entry bits, e.g. `auto1` material +1E0h (the
  wheels): entry bit 1 selects record 247 (blurred wheel) instead of 246 (sharp). What each
  bit of instance +20h means (wheel speed, lights, damage) is unknown.
* **Flat polygons** (mode 2): no texture; colour from the polygon (topic B). Material
  type 80h (one per car) has no frame list; whether mode 2 uses it is not verified.

## Rally China inventory (verified, `SR_DUMP_TEXTURES`)

324 records + the background; identical texels in the attract-mode race and the player's
race (same hashes, different addresses).

| library | records | formats | materials (types) |
|---|---|---|---|
| map3 (track) | 213 | 200 P8, 13 RGB565 | 195 (135 x 10h, 60 x 40h), 2 animated |
| misc | 12 | 10 ARGB4444 64x64, 2 RGB565 8x8 | 5 (2 x 10h, 3 x 120h), 2 animated |
| auto1..auto4 | 26 / 24 / 24 / 24 | 15-16 P8, 6-7 RGB565, 3 ARGB4444 each | 18-19 each (10h, 40h, 100h, A0h, 80h), 128-entry lists |
| env | 1 | RGB565 128x128 | 1 (A0h) |
| background | - | RGB565 256x256 (TMU only) | - |

Totals: P8 261 (3,606 KB texels + 261 KB palettes, 205 distinct palettes), RGB565 41
(213 KB), ARGB4444 22 (144 KB); sizes from 8x8 to 256x256 (24 at 256x256). About 240
distinct records were bound during the run.

## SR_DUMP_TEXTURES

`SR_DUMP_TEXTURES=<dir>`: at the first frame of every race whose libraries and records
differ from the last dumped (the attract-mode race, then the player's), writes to
`<dir>/race<n>_<track library>/`:

* `tex_<id>.png`: every record, decoded as the Glide layer's TMU decoder decodes it (5/6-bit
  channels by bit replication, 4-bit x 17, P8 through its palette, ARGB4444 with alpha);
* `sky.png`: the background picture, from the emulated TMU;
* `pal_<n>.png`: each distinct P8 palette as 16 x 16;
* `index.tsv`: per record: id, library slot, materials naming it (`slot:index#frame`),
  material type, size, format, LOD, aspect, TMU bytes, data address and offset in its
  library block, palette address and number, chroma colour, texels of the chroma colour,
  grey flag, s/t scale, manager slot, FNV-1a hash of the RGBA, file; header comments: the
  libraries, the environment library, the background, the chroma tables;
* `materials.tsv`: every directory entry of every library with its frame list.

For the whole run every texture bind, download and chroma value is checked against RAM;
the log has `textures: check ...` lines every 250 race frames. The API
(`records`, `libraries`, `materials`, `texture`, `polygon_texture`, `Texture::rgba`) is meant
for the phase-2 scene extraction. Default behaviour is unchanged: without the variable
nothing runs except an atomic flag test per grTexSource / grChromakeyValue /
grTexDownloadMipMap.

## How it was verified

Deterministic run (`SR_DETERMINISTIC=1`, keys
`12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER,44:M,50:F3`, `--seconds 60`),
attract-mode race, menus and a player race on China, `SR_DUMP_TEXTURES` set. Totals after
1,250 race frames:

* **Texels**: 1,291,533 binds of records; at each, the TMU content at the bound address
  decoded with the palette then in effect (what the renderer draws) was hashed against the
  record decoded from RAM: 0 differ. 428 downloads: the TMU bytes equal the record's RAM
  texels, 0 differ. 0 binds of anything but records and the background.
* **Polygon -> record**: [aa5c0] (the polygon's id) = bound record 1,291,533 / 0; the frame
  list path (with mode 5's environment rule) gives [aa5c0]: by mode 0: 990,820, 1: 231,884,
  5: 26,986, 6: 16,199, 7: 25,644, 0 differ (before the mode-5 rule was found, 3 % of the
  binds, all mode 5, differed). Mode / material type at binds: 0/10h 990,820, 1/40h
  231,884, 5/A0h 26,986, 6/100h 16,199, 7/120h 25,644.
* **Chroma**: grChromakeyValue from the manager = record +14h: 598,124 / 0.
* **Manager**: the record's slot holds the bound TMU address and the record: 1,291,533 / 0
  (binds right after a download checked at the next bind).
* **Records**: s/t scale and TMU bytes 324 / 324, palette placement 261 / 261, material
  size = record size for 8,679 / 8,679 frame-list entries; formats by type as tabled; no
  record with several LODs.
* **Pictures**: contact sheets of all 324 PNGs and `sky.png` inspected: walls, rock, earth,
  flags, trees, buildings, car liveries, wheels, smoke, sky, as in the game.
* Peek (`SR_PEEK`) of 123740, 173f70, 1479c0, aa6a0 at Glide frame 1500; trace
  (`SR_TRACE_EVERY=1500`) of the same frame for the Glide calls.

## Unknowns

* What the bits of the car instance +20h select in the 128-entry lists (only entries 0-3,
  66, 67 seen), and whether other phases (damage) use more of them.
* Frame list +4 (`word1`), slot +28h, material +14h..+1Fh, record +34 in practice (no grey
  texture on China).
* The resampler `db470` (filter used) and which load class uses which factor of the table
  at 123750 (all of China's textures were converted with the factor read as 1.0 at the end
  of the load; the factor is per load).
* Mode 3 and 4 textures (not bound on China), material types 80h and E0h in practice.
* The use of material +10h at draw time ([8309c], read at d66dd / d6842; not followed).
* Other tracks, the menus' libraries (seen bound: two 2-frame materials, menu car models)
  and the English version: not dumped yet.

## Interfaces needed

* **Topic A (world)**: for each object, its model's data block ([model + B4h], one of the
  libraries' blocks), its instance +20h (and +3Ch for mode 5) per simulation step, and
  which objects take the clock [1473a0] (+20h < 0); the clock's unit and update (written
  at 41b50, 42594, 4f8ed).
* **Topic B (polygons)**: confirm polygon record +04 as the material offset for every mode;
  the vertex s, t (int16 at +4, +6 of the builder's vertex list, from [7bad4]); the per-mode
  state (clamp: wrap only in mode 5, traced; chroma key in modes 1 and 3, checked; blend
  functions set in the mode 6 and 7 blocks cc468 / cc4c3, code only); the phase flags
  60000h and the colour table at 173fa4; flat polygons (mode 2, type 80h); the second
  material entry of the set-up routines (ca4a0 and the like).
* **Topic E (renderer)**: textures arrive as `Texture { width, height, format, pixels }`
  with `TexFormat::{Indexed8 { palette }, Rgb565, Argb4444}` (no other format occurs);
  decode as `Texture::rgba` (or keep P8 + palette on the GPU); chroma key = exact match of
  the decoded colour with the record's +14h in modes 1 and 3; the game asks for bilinear
  filtering; clamp except mode 5 and the background (wrap); u = s / 256, v = t / 256; a
  material is a list of textures selected per object per frame (frame = count * phase >>
  14, or the environment material for mode 5), so a draw needs a per-instance texture
  choice, not one texture per mesh range; the background is a 256x256 RGB565 texture read
  from the Glide layer's TMU at load time.
