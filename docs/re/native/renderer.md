# Native renderer: the Direct3D 11 side (topic E, wave 2 topic W2-2)

The GPU half of the native renderer (`docs/04-native-renderer.md`). It takes the neutral scene
description (crate `sr-scene`, the contract) and draws it into the backend's existing scene
render target before the game's 2D and HUD. Nothing here reads guest memory.

Status: everything the contract says the renderer does is implemented and tested (wave 2):
animated textures (`TexAnim`), the game's exact reflection (`TexGen::Reflection`), static
fades with screen-door dither on merged chunks, the game's sky and ground (`View::sky`), every
blend factor pair (mode 7's 2 x src x dst, mode 3's ONE, SRC_COLOR), depth shared with the
Glide path (proved bit for bit), and a flush after the main view. It was tested against
synthetic data: on a Direct3D 11 device with the real shaders (unit tests, WARP and the
hardware GPU) and inside the real program (`SR_NATIVE_TEST`). It has not drawn the game's
own data yet (W2-1 extracts it, W2-3 calls the renderer). Default behaviour is unchanged:
everything runs only with `SR_NATIVE_TEST`, or when code calls the backend's `native_*`
functions.

## Files

| File | What |
|---|---|
| `crates/sr-scene/src/lib.rs` | The contract (`SceneData`, `FrameData` and their parts), shared with the runtime; re-exported as `sr_render::native::scene`. Not changed here. |
| `crates/sr-render/src/native/prepare.rs` | CPU side: texture expansion, mip chains, chroma key, frames resampled into one array, merging statics into chunks (with the static table and normals), flattening meshes, projection and culling math, the sky quads, and the CPU references of the shader formulas (animation frame, reflection, fade dither). No D3D; unit-tested. |
| `crates/sr-render/src/native/renderer.rs` | `NativeRenderer`: GPU resources, texture packing, pipeline states, per-frame data, the drawing of a view and of its sky. |
| `crates/sr-render/src/native/native.hlsl` | Vertex and pixel shaders (world; screen-aligned sky), and the formulas shared with the tests (`anim_frame`, `reflect_texel`, `fade_draws`). |
| `crates/sr-render/src/native/gpu_tests.rs` | Tests on a real Direct3D 11 device (WARP; the formulas also on the hardware GPU) and the `feature_costs` benchmark. |
| `crates/sr-render/src/native/timing.rs` | GPU timestamp queries, read back without stalling. |
| `crates/sr-render/src/native/test_scene.rs` | The synthetic scene behind `SR_NATIVE_TEST` (every feature). |
| `crates/sr-render/src/native/mod.rs` | Module docs, re-exports, `TestDriver` (test modes and timing). |
| `crates/sr-render/src/d3d.rs` | Integration: `D3dBackend::native_*` functions, the test hooks, `SR_D3D_DEBUG`. Wave 2 changed one line: the Glide path's w-buffer depth scale is now `native::DEPTH_SCALE` (the same value, 1/256), so both paths share it by construction. |

## Architecture

```
 SceneData (once per scene) ──► NativeRenderer::upload
     textures ── expand to RGBA8 (palette, 16-bit, chroma key) ── texture arrays by (w, h, levels)
                 an animation's frames in one array (other sizes resampled); textures no
                 material names (the sky) get a plain variant
     materials ─ GPU table (flags, layer, animation's frame list, env scale) + batch key
                 (pass, blend, cull, wrap, filter, array); frame-list layers table
     statics ─── merged per (batch, chunk); vertices relative to the chunk's centre; each vertex
                 names its static instance; static table (chunk, phase, scroll) ── one VB/IB
     meshes ──── flattened per material range (for instances) ─────────────── same VB/IB (first)
     normals ─── per vertex, up to the last vertex of a reflection material ── structured buffer
 FrameData (per frame) ──► NativeRenderer::begin_frame
     transforms: chunks, dynamic instances (+ phase, scroll), frame meshes ── f64 -> camera-relative f32
     static fades (4 per uint) ── chunks whose statics are all faded out are skipped
     instance list (grouped by mesh), the frame's own geometry (+ normals) ── dynamic buffers
 per view ──► NativeRenderer::draw_view
     cull chunks / instances / frame meshes (bounding spheres, view-space frustum, fades)
     clears -> View::sky (sky quad, ground quad) -> sky layer -> opaque + chroma key (by batch)
       -> decals (as given) -> blended (far to near); flush after view 0
     into the backend's scene target (RGBA8 + D32_S8, reversed depth), its viewport; then the
     Glide path resumes and draws on top with the same depth convention
```

### Spaces and precision (unchanged from wave 1 except the chunk key)
* **Frame space**: world minus the *frame origin* (the first view's camera position, f64).
  Every transform slot (a chunk, a dynamic instance, a frame mesh) is a 3x4 model→frame
  matrix whose translation is computed in f64 and rounded to f32.
* **Chunks**: static instances are grouped by the 8192-unit square column (world x and z;
  world y is up per the contract) their origin falls in, numbered in Morton order; a chunk's
  origin height is the middle of its instances' heights (whole units). Wave 1 used a 3D key,
  which split every chunk whose objects straddle y = 0 (the game's world does: cars at y -13
  to -41, the camera at 270 to 3000): the test scene went from 136 chunks to 79, and from
  about 210 to 176-194 draws per frame. `statics_merge_into_precise_chunks` (0.25-unit
  offsets 3·10⁷ units from the origin survive exactly) and `chunks_are_columns` test it.
* **View space** (contract): x right, y down, z forward. `view = rot · (frame + (frame origin − camera))`.
* **Projection** (the game's): `x_s = cx + sx·x/z`, `y_s = cy + sy·y/z` in game pixels, mapped
  through the (widened) viewport; clip space `(px0·x + px1·z, py0·y + py1·z, DEPTH_SCALE, z)`.
  `projection_matches_the_game_formula`.

## What the renderer does (wave 2)

### Animated textures (`Material::anim`, `TexAnim`)
* The frame of a draw is `min(count − 1, (count · v) >> shift)` with `v` and `shift` from the
  source: `Phase`: the instance's `phase` when not negative, else `FrameData::clock`, shift
  14; `Env`: `FrameData::env_phase`, shift 16. Computed in the vertex shader per vertex (so
  per instance, also inside merged chunks), exactly: `anim_frame` avoids 64-bit products by
  returning `count − 1` whenever `v >> shift != 0` (then the product is at least
  `count << shift`), and otherwise multiplies below 2³² (`count` is capped at 65535).
* Where `v` comes from: merged statics: the static table (their `phase` is fixed at upload,
  as the contract says statics cannot change); dynamic instances: their transform slot (4th
  row: scroll, phase); frame meshes: phase −1, i.e. the clock (`FrameMesh` has no phase; see
  "Proposed contract changes").
* All frames of one material live in one texture array, so the frame is just a layer index
  (`frame_slices[anim_first + frame]`): instanced draws and merged chunks keep batching.
  Frames of another size than frame 0 are resampled to frame 0's size (nearest texel; uv are
  relative to the texture, so the mapping is unchanged), and their mip chain is generated
  or cut to frame 0's level count (`texture_levels_as`). On China every frame list entry has
  its material's size (`textures.md`), so this is a safety net. A texture repeated in a
  list (car states: 128 entries over a few records) is uploaded once.
* Verified: `anim_frame_is_exact` (CPU reference, products above 2³²);
  `anim_and_fade_shaders_match_the_references` runs the shader's `anim_frame` on the GPU
  (WARP and the RTX 3090) for 8256 cases (counts 1 to 65535, both shifts, edge values,
  random values): all equal to the 64-bit reference; `animation_picks_the_frame_per_instance`
  draws statics with phases 0, 5000, −1 (clock) and 16383, dynamic instances with a phase,
  the env source and a negative phase, and a frame mesh, with frame 2 at another size, and
  reads back the colour of each: all the expected frame.

### Reflection (`TexGen::Reflection`, the game's mode 5)
* Per vertex in the vertex shader, exactly as `sr-scene` documents it: the normal is
  `Vertex::normal` rotated by the instance basis (statics: at upload) and the camera
  rotation (not renormalised, as the game's `R · N`), the position is the vertex's view
  position. The renderer's view space is converted to the game's: position
  `(−x, −y, 128 − z)` (the game's view origin is 128 units in front of the eye, `GAME_EYE`),
  direction `−n`; then `e = p / |p|`, `r = 2 (e · n) n − e`, `u = round(127 r.x) + 127 −
  scroll[0]`, `v = round(127 r.z) + 127 − scroll[1]` (HLSL `round` is round-half-to-even,
  like `fistp`), `uv = (u, v) / 256 · scale + offset`, wrapped by the material's `Repeat`.
  The coordinates are interpolated perspective-correctly across the polygon, like the
  game's s/w, t/w.
* `scroll` comes from the instance (dynamic: transform slot; static: static table).
* **Mirror views:** the game negates x of the view position after the transform in the
  mirror (`7ab44`, `fchs`) but its normals come from the part matrix without that flip
  (`bb380` reads draw record +118h). In a mirror view the renderer therefore uses position
  `(x, −y, 128 − z)` and the same `−n`. This is inferred from the code descriptions, not
  verified against the game (see "Unknowns").
* Normals are read from a structured buffer by `SV_VertexID` only for reflection
  materials (no extra vertex data for anything else; the vertex stays 32 bytes). The scene's
  buffer holds the normals up to the last vertex a reflection material uses; meshes are now
  placed before the statics in the vertex buffer, so a scene whose statics have no
  reflection (the game's mode 5 is car windows) uploads normals for the meshes only.
* The instance basis must be a rotation (like the game's part matrices): a scaled basis
  scales the normal and changes the formula's result (the test scene bakes its mirror
  spheres' radii into the meshes).
* Verified: `reflection_reference_cases` (CPU reference: straight-back reflection, scroll,
  the 128-unit origin, mirror symmetry); `reflection_shader_matches_the_reference` runs the
  shader's `reflect_texel` on the GPU for 60 000 random cases (positions 17 to 60 000 units
  away, unit and slightly non-unit normals, scrolls, a quarter in mirror views) against the
  f64 reference: WARP and the RTX 3090 both give 0 differences (the test would allow one
  texel only where 127 r is within 10⁻³ of a half; none occurred);
  `reflection_through_the_pipeline` draws a mirror quad through the whole pipeline and reads
  back the texel drawn (v exact with scroll and wrap, u within one texel of the interpolated
  value).

### Static fades (`FrameData::static_fade`)
* Every merged static vertex names its static instance (`GpuVertex::slot` = `STATIC_BIT` |
  instance), and the shader reads the instance's chunk slot, phase and scroll from the static
  table and its fade from a per-frame buffer (four fades per uint, 2121 statics = 2.1 KB per
  frame in the test scene). Statics stay merged in chunks; no draw is split.
* Fade 0: the vertex shader puts every corner outside a clip distance, so the triangles are
  clipped before rasterisation. Fade 255 (or no entry: the list is shorter, or empty):
  drawn. Between: screen-door dither in the pixel shader, a 4 x 4 ordered pattern on target
  pixels, a pixel drawn when `16 · fade > 255 · level + 128` (16 levels; 0 draws nothing, 128
  half, 255 all). Faded statics stay in their pass (opaque ones write depth where drawn).
* A chunk whose statics are all at fade 0 is culled on the CPU like an invisible chunk.
* Verified: `fade_dither_coverage` (CPU reference), `anim_and_fade_shaders_match_the_references`
  (the shader's test for all 256 fades × 16 pixels, WARP and RTX 3090: equal),
  `static_fade_dithers_and_hides` (a wall drawn with fades 0 to 255 through the whole
  pipeline: every pixel drawn exactly where the reference says; fade 0 culls the chunk
  (0 draws); two statics of one chunk fade independently; a missing entry is drawn).

### The game's sky and ground (`View::sky`)
* Drawn right after the view's clears, before everything else, no depth test or write, no
  culling, no fog: a screen-aligned textured quad from the top of the viewport to the
  horizon and a flat ground-colour quad from the horizon to the bottom (when `ground` is
  set), generated in the vertex shader from four corners (`vs_screen` / `ps_screen`, no vertex
  buffer).
* `s` is given at the left and right edge of the view's own viewport (`View::viewport`: the
  game's 640-pixel width in the main view, the mirror's rectangle in the mirror) and extends
  proportionally over a widened viewport (the same rule as the Glide path's `widen_sky`);
  `t` runs from `t[0]` at the top to `t[1]` at the horizon. The texture is sampled wrapped,
  bilinear unless the filter setting is "nearest" (the game asks bilinear).
* The quad is drawn as given on screen, not flipped in a mirror view: the game's own pair is
  already swapped there (`polygons.md`: sL, sR swapped when `[aa5b0] != 0`), and the
  contract has the runtime pass the game's values.
* The sky texture is a scene texture no material uses (the background, `173fd4`); every
  such texture gets a plain layer at upload. If a sky texture were also used colour-keyed
  by a material and nowhere plain, its keyed layer would be used (rgb only).
* Verified: `sky_quads_extend_s_over_wide_views` (CPU: rectangles, s extension, mirror pair,
  horizon above or below the view); `sky_and_ground_fill_the_view` draws a texture whose
  texel colour is its own (s, t) and reads back s at the edges and the middle, t above the
  horizon and the ground colour below, in a plain view, a view widened by 160 game pixels
  per side (s extended and wrapped: 224 at the left edge) and a mirror viewport (s as given,
  nothing drawn outside the viewport).

### Blending, flat colour, chroma key
* Any `Blend { src, dst }` pair maps to the Direct3D 11 factors directly, including mode 7's
  multiply variant `Blend { src: DstColor, dst: SrcColor }` = 2 x src x dst and mode 3's
  `Blend { src: One, dst: SrcColor }` = src + src x dst. The alpha channel now uses the game's
  `(ONE, ZERO)` in every mode (`polygons.md`, mode table; wave 1 used the colour factors'
  alpha equivalents; nothing displayed reads the target's alpha).
  `decal_multiply_and_mode_3_blend` draws both over a known colour and reads back the
  result (within 1 of the formula in every channel).
* Mode 3 needs the chroma key **and** the blend; the contract's `Transparency` is either
  `ChromaKey` or `Blended`, so mode 3 cannot be expressed exactly (see "Proposed contract
  changes"). The renderer already supports a keyed blended material internally (the key is
  baked into texture alpha and the shader discards keyed texels in any pass): only the
  material's key needs a place in the contract.
* Flat colour (`texture: None`, mode 2) draws the vertex colour; chroma key compares the
  palette-expanded colour with the key at upload (`chroma_key_makes_holes_with_neighbour_colours`),
  as in wave 1.

### Depth shared with the Glide path (hybrid frames)
For a point at view depth z (the contract's camera is the game's eye, so z is the game's
w = 128 − z'), **both paths write `DEPTH_SCALE / z = (1/256) / z`** into the D32 buffer,
reversed (larger is nearer), compared GREATER_EQUAL:
* native: clip `(…, DEPTH_SCALE · z / z, z)` → `DEPTH_SCALE / z` (decals: `DEPTH_SCALE /
  max(z − pull, z/4)`), then linear in screen space;
* Glide path (`voodoo.hlsl`): `oow · tex_range.z` with `oow = 1 / (128 − z')` from the game
  (`transforms.md` step 7; exact view positions with `precise_geometry`) and `tex_range.z =
  native::DEPTH_SCALE` (`d3d.rs`), linear in screen space; its depth-buffered opaque
  polygons add a constant rasteriser bias of 64 units of `2^(e − 23)`, e = the exponent of
  the primitive's largest depth (`depth_raster`).
`DEPTH_SCALE · z / z` is exactly `DEPTH_SCALE` (a power of two), so the per-vertex values are
the same float, and both interpolate linearly in screen space.

Verified on the GPU with the Glide path's real vertex shader (`voodoo.hlsl` compiled in the
test, the backend's input layout, constants, depth-stencil state and rasteriser state):
`glide_and_native_depth_agree` draws the same triangles through both paths and compares the
depth buffers pixel by pixel:

| Shape | Pixels | Native vs Glide (bias off) | Glide's bias |
|---|---|---|---|
| wall at 300 / 1000 / 40 000 units | 76 800 each | 0 ulps (bit for bit) | +64 ulps |
| floor 1000 to 20 000 units, corners on screen | 3 834 | 0 ulps | +64 to +1024 ulps |
| floor 200 to 20 000 units, corners at ±48 000 game px | 38 080 | at most 5 ulps (5.8·10⁻⁷ relative; clipping far outside the screen) | +64 to +4096 ulps |

`glide_cars_are_hidden_behind_native_walls`: a native wall at 1000 units, then a Glide
"car" quad (opaque car state, stencil reference 81h) 5, 1, 0.1, 0.01 units in front, at the
same depth, and 0.01 to 5 units behind: in front and coplanar it is drawn (and writes its
stencil); from 0.01 units behind it is hidden, and the wall's pixels keep stencil 0, which is
"scenery" for the Glide path's decal tests. The bias means a Glide polygon wins against a
native surface at the same depth and within about `z · 7.6·10⁻⁶` behind it (0.0076 units at
1000) for small polygons; on a polygon spanning a large depth range the far end is pulled
nearer by more (64 units at the exponent of its nearest point: about 15 units at 20 000 on
the 200-to-20 000 floor). Car polygons are small, so this does not matter for them; it is
the Glide path's existing behaviour.

Not yet verified in a real race (W2-3 makes the hybrid frames): that the runtime's cameras
put the native world where the game's Glide cars are (W2-3's unit test against topic D).

### Flush after the main view
`draw_view` calls `Flush()` after view 0 (`flush_after_main`, on; `SR_NATIVE_FLUSH=0`
turns it off for measurements). In the game the CPU keeps emulating for milliseconds after
the world is drawn; without the flush the commands wait for the present (wave 1 measured
GPU time per frame 0.6–2.9 ms including that wait, 0.11–0.35 ms with a flush). The test
mode still flushes after its whole frame too. Its cost, measured: 0.04 ms of CPU per frame
in the benchmark, 0.07 ms in a race (see "Measured cost"). Its benefit shows only when the
game works between the main view and the present, i.e. in W2-3's hybrid frames, where it
can be compared with `SR_NATIVE_FLUSH=0`.

## API in brief

```rust
// the backend (crate::d3d::D3dBackend): the integration point, unchanged
fn native_upload(&mut self, &SceneData) -> Result<SceneStats, String>     // once per scene
fn native_begin_frame(&mut self, &FrameData) -> Result<(), String>        // once per frame
fn native_draw_view(&mut self, &FrameData, view: usize) -> Result<DrawStats, String>
fn native_draw_frame(&mut self, &FrameData) -> Result<DrawStats, String>  // begin + all views
// NativeRenderer (crate::native)
pub flush_after_main: bool   // Flush() after view 0 (default on)
SceneStats { .., animated /* materials with frames */, normals /* vertices with a normal */, .. }
```

### Integration point (unchanged)
Call `native_draw_view(frame, i)` where the game would draw its world for view i: after the
frame's clear and `set_view` (`Backend::begin_view`, which clears depth and stencil), before
anything 2D. It clears depth and stencil (whole target) and the viewport's colour if
`View::clear` is set, draws the view's sky and world, unbinds its extra slots and restores
the Glide pipeline (`bound = None`, `bind_scene`). Depth and stencil are left as the native
world wrote them (stencil 0) for the Glide-drawn cars, shadows and smoke that follow.

## Decisions and why

* **Palette expanded at upload, chroma key baked into alpha with the neighbours' colour,
  texture arrays by size, material table on the GPU, instancing through a per-instance slot,
  CPU culling by bounding spheres, blended sorting per chunk or instance, the mirror flips
  clip x**: as in wave 1 (reasons there: hardware filtering, clean keyed edges, batching).
* **Frame selection on the GPU, frames in one array.** The alternative (choose the texture
  per draw on the CPU) would split instanced draws and merged chunks by frame. A layer index
  per vertex costs one structured-buffer read for animated materials only.
* **The static instance index in the vertex's slot field** instead of a new vertex
  attribute: the vertex stays 32 bytes; a static vertex costs one extra (cached) read of the
  static table, plus the fade read when fades are given.
* **Normals by `SV_VertexID` from a separate buffer**, read only by reflection materials:
  no bandwidth for anything else, and full float precision (the formula rounds to whole
  texels; packed normals would move some by one).
* **Reflection per vertex, not per pixel**: the game computes texture coordinates per vertex
  and interpolates them; doing the same reproduces its look (wave 1's per-pixel placeholder
  is gone).
* **Screen-door fade, not alpha**: faded statics stay in the opaque pass with depth (the
  contract's choice); 4 x 4 ordered dither on target pixels, so at 4K the pattern is fine.
* **Sky as screen quads from four constants**, not a mesh: the game's sky is a screen-space
  quad whose coordinates the runtime computes exactly as the game; a cylinder would change
  the look.

## SR_NATIVE_TEST and diagnostics

* `SR_NATIVE_TEST=1` (replace), `=over`, `SR_NATIVE_BENCH=n`, the log every 5 s and
  `SR_D3D_DEBUG=1`: as in wave 1.
* `SR_NATIVE_TEST_OFF=fade,anim,reflect,sky,multiply` (new, any of them): the test scene
  without those features (for measurements).
* `SR_NATIVE_FLUSH=0` (new): no flush after the main view.

The test scene (`test_scene.rs`): 32 x 32 ground tiles of 2048 units, 420 static boxes
(every fifth one blinks: fades from 0 through the dither to 255 and back), 320
chroma-keyed trees (fading out between 24 000 and 30 000 units from the camera), 70 glass
panes, 50 additive glows, 36 mirror spheres (the game's mode 5: opaque, repeat, reflection
from per-vertex normals with a fixed scroll each, the env map animated by the env phase
over 4 tinted frames), 80 flags (8 frames, one of another size; half with fixed phases,
half on the clock), fences, road markings, 140 moving objects (boxes, some with the flag
texture and their own phase; every tenth a mirror sphere whose scroll follows its motion),
their shadows (alternately alpha and 2 x src x dst multiply decals), the game-style sky and
ground in both views (s from the heading at 256 texels per 180 degrees, the mirror's pair
swapped), fog, and the camera loop with a rear-view mirror. Upload: 22 textures in 9 arrays
(1.3 MB), 14 materials in 14 batches, 79 chunks, 168 560 static + 137 340 mesh triangles,
208 584 vertices, about 58 ms.

## Verification

| Claim | How it was checked |
|---|---|
| CPU math (expansion, chroma key, mips, frames resampled, chunks as columns, static table, normals, projection, culling, sky quads, the CPU references of the shader formulas, test scene consistency) | 16 unit tests in `sr-render` (`cargo test --release --offline -p sr-render`) |
| The shaders' formulas equal the references | `anim_and_fade_shaders_match_the_references`, `reflection_shader_matches_the_reference`: compute shaders compiled from `native.hlsl` itself, on WARP and on the RTX 3090 (numbers above) |
| Each feature through the whole pipeline | 7 tests drawing with `NativeRenderer` on WARP and reading back colour and depth (see each feature above) |
| Depth agrees with the Glide path | `glide_and_native_depth_agree`, `glide_cars_are_hidden_behind_native_walls` (table above) |
| Draws correctly inside the real program | `SR_NATIVE_TEST=over` in the menus (1280 x 720) and `SR_NATIVE_TEST=1` in a race on China (1280 x 720, 1920 x 1080), frames captured with `SR_SHOT` and inspected (enlarged crops): the sky quad with its clouds and the ground colour in the main view and the mirror, the mirror spheres with the reflected env map (sky in the middle, ground at the rim, the sun) in its tints, flags (the frame bar of the drawn frame visible), boxes with the flag texture moving, blinking boxes in the 4 x 4 dither, alpha and multiply shadows, fog; in the race the HUD and the game's mirror frame on top |
| No Direct3D API misuse | `SR_D3D_DEBUG=1` with `SR_NATIVE_TEST=1`, menus and a race on China (`SR_DETERMINISTIC=1`): "0 messages so far" at frames 0, 1000 and 2000 |
| Default behaviour unchanged | The only change outside `native/` is `d3d.rs` using `native::DEPTH_SCALE` (1/256, the same f32 value as the literal it replaces) for the w-buffer scale; all new code runs only with `SR_NATIVE_TEST` or the `native_*` calls. Not re-checked by a frame comparison against the base build |

All 26 tests pass (10 of them on a GPU device, about 0.7 s in all); `feature_costs` is a
benchmark, ignored unless asked for.

## Measured cost

Machine: Intel i9-10900K, NVIDIA RTX 3090, Windows 11, 32-bit process, release build, vsync
off. Not controlled: during these measurements another agent's copy of the game and a
compiler were running (seen in the process list), which is why the in-program figures
spread widely. The per-feature figures come from a benchmark built to be insensitive to that.

**Per feature** (`feature_costs`: the test scene's two views at 1920 x 1080 on the hardware
device, about 114 000 triangles and 190 draws per frame; 8 configurations interleaved in 24
rounds of 200 frames, a different one first in each round; medians per frame):

| Configuration | CPU build (test scene) | CPU submit | GPU (both views) |
|---|---|---|---|
| all features | 0.0470 ms | 0.1574 ms | 0.0952 ms |
| fades off (`static_fade` empty) | 0.0419 | 0.1558 | 0.0952 |
| animation off | 0.0468 | 0.1562 | 0.0952 |
| reflection off (`TexGen::Vertex`) | 0.0472 | 0.1595 | 0.0942 |
| sky off (`View::sky` None) | 0.0468 | 0.1532 | 0.0819 |
| multiply decals off | 0.0463 | 0.1584 | 0.0942 |
| all five off | 0.0409 | 0.1529 | 0.0819 |
| all features, no flush after view 0 | 0.0467 | 0.1169 | 0.0727 |

* All features together: **+0.013 ms GPU and +0.005 ms CPU submit per frame**. The GPU part
  is the sky: two viewport-filling quads per view (about 2 million pixels at 1080p). Fades
  (dither, buffer read, static table), animation (frame lookup), the reflection (normals
  read, per-vertex formula) and the multiply blend are each within the timestamps' ±0.001 ms.
* The CPU "build" column is the test scene's own work (its fades cost it 0.005 ms for 2121
  statics); the runtime's frame building will differ.
* The flush after the main view costs 0.040 ms of CPU per frame here (0.07 ms in the game
  below). With it the timestamps also bracket the GPU's wait for the second view's
  commands (+0.022 ms), which is idle time, not work.

**Native frames alone inside the program** (`SR_NATIVE_TEST=over`, `SR_NATIVE_BENCH=5000`,
1920 x 1080 window; build, submit, present blit and `Present`, everything included; 17
runs, the series in `SR_NATIVE_TEST_OFF` variations): 1756 to 2860 fps (0.35 to 0.57 ms per
frame), GPU 0.080 to 0.128 ms; all features on: 1840, 2598 and 2823 fps. The configurations
did not order the runs (all five off: 1832 and 2223 fps): the spread is the machine's.
Wave 1 measured 2190 fps on the previous test scene (136 chunks, about 210 draws).

**Inside the game** (`SR_NATIVE_TEST=1`, a race on China in real time, 1920 x 1080,
game-paced at 33 frames per second): CPU build 0.05 ms, CPU submit 0.22–0.26 ms, GPU
0.48–0.51 ms per frame. The same race with `SR_NATIVE_FLUSH=0`: submit 0.14–0.18 ms, GPU
0.16–0.65 ms. Wave 1 measured 0.19–0.39 ms GPU in the game; game-paced frames leave the GPU
at lower clocks between bursts (wave 1, `nvidia-smi`), and the other agent's game was
running: these GPU figures are not controlled.

The 400+ fps target is not touched: the features add about 0.013 ms of GPU and 0.005 ms of
CPU per frame, the flush 0.04–0.07 ms of CPU.

## Proposed contract changes (for `docs/04-native-renderer.md`, through the organiser)

1. **Mode 3 (chroma key and blend).** `Transparency` is `ChromaKey` or `Blended`, never
   both, so mode 3 (key, then `(ONE, SRC_COLOR)`) is not expressible. Proposed: a separate
   `Material::key: Option<u32>` (or `Transparency::KeyedBlend { key, blend }`). The
   renderer needs one line (`key_of` in `upload`); the shader already discards keyed texels
   in any pass. Mode 3 was never seen on China, so this is low priority.
2. **Animation values: the game's or the raw ones?** The contract says `Instance::phase` is
   "the game's instance +20h" and `FrameData::clock` is `[1473a0]`, with the frame
   `(count · v) >> 14`. But the game's `v` is `[7bb94]` = instance +20h **>> 2** (its low 16
   bits when the tint flags 60000h are set) and, for a negative +20h, `[1473a0] & 3FFFh`
   (`polygons.md` "Texture selection", `textures.md` step 3). With the raw values the frames
   run 4 times too fast and the clock saturates at the last frame. Proposed wording:
   `phase` = the game's `[7bb94]` value for the instance (+20h >> 2, masked as the game
   does), `clock` = `[1473a0] & 3FFFh`; the renderer applies the contract's formula as
   written.
3. **The env phase is per object in the game** (`[7bb98]` = instance +3Ch, `world.md`), but
   one value per frame in the contract (`FrameData::env_phase`). If cars carry different
   values, add `Instance::env_phase` (the renderer has room: the 4th row of a dynamic
   instance's transform slot has a free component).
4. **`FrameMesh` has no phase or scroll**: per-frame geometry animates with the clock and has
   no reflection scroll. If smoke or marks built per frame need their own phase, add
   `FrameMesh::phase` (or send them as dynamic instances).
5. **Name the eye offset.** The reflection formula works in the game's view space, whose
   origin is 128 units in front of the contract's camera; the renderer hard-codes
   `GAME_EYE = 128` (`native.hlsl`, `prepare.rs`). Proposed: `sr_scene::GAME_EYE` (or a field
   of `TexGen::Reflection`), so both sides use one constant.
6. **Reflection details to state in `TexGen::Reflection` / `Vertex::normal`:** the normal
   goes through the instance basis unnormalised (the game's `R · N`), so instances with
   reflection materials need a rotation basis; in a mirror view the position's x is the
   game's negated one (`(x, −y, 128 − z)`) while the normal is not flipped (to be verified,
   see "Unknowns").
7. **`Sky::s` on screen:** state that `s[0]` / `s[1]` are drawn at the left / right edge of
   `View::viewport` as displayed, also in a mirror view (the runtime passes the game's
   swapped pair; the renderer does not flip the sky), and extend proportionally over a
   widened viewport.
8. Still open from wave 1: a `View` flag for a full-width view that must not widen (menus
   with `wide_menus = off`); today a full-width viewport always widens.

## Unknowns

* **Reflection in the mirror view**: the position's x negated, the normal not, is inferred
  from the descriptions of `7ab44` (`fchs` after the transform) and `bb380` (normals from
  draw record +118h). Topic B's check (`SR_POLY_CHECK`, 101 196 coordinates exact) does not
  say whether mirror frames were included. To verify: `SR_POLY_CHECK` with the mirror on,
  or compare a native and a Glide mirror frame once W2-3 draws real data.
* **Modes 3 and 7's multiply variant** were never seen in the game (topic B); they are
  tested here on synthetic data only.
* **The animation values' scaling** (proposal 2) depends on what the runtime puts in `phase`
  and `clock`; the renderer follows the contract literally.
* **Hybrid frames in a real race**: the depth agreement is proved with the Glide path's own
  shader and states on synthetic triangles; whether the runtime's cameras put the native
  world exactly where the game's Glide cars are is W2-3's check.
* **Exactness against 1997**: the game rounds view positions to whole units before the
  reflection (`fistp` at `68fc8`) and its sky spans x 0 to 639.8 (the contract: 640); the
  renderer uses exact positions and the contract's 640. Differences of a texel near the
  camera and 0.03 % of the sky's s are expected and intended (the same choice as
  `precise_geometry`).
* The tint (texture x constant colour, `polygons.md`) is not in the contract and not
  implemented; never seen in a race.

## What remains

* Real data: draw W2-1's scene of China and compare with the Glide frames (W2-3's hybrid
  frames); measure on a lap.
* Per-polygon sorting of large blended surfaces, and the game's per-part polygon order for
  car glass (phase 3). On China the blended mode 6 is car glass, stickers and number plates
  and mode 7 the decals (`polygons.md`, "Type -> set-up -> mode"), so the world does not
  need it now.
* Texture updates during a race: not needed (topic C: textures do not change during a race;
  animation is by frame lists, which the renderer now does).
* Contract proposals above (mode 3's key, the animation values, a non-widened full-width
  view).
