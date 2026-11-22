# The Claude Renderer: design

The design of the renderer that replaces the 1997 3D drawing (in the program's settings:
Graphics > Renderer > Claude Renderer, the default; "native renderer" in the code), how it
was built in waves, and what each wave verified.

## Goal

Keep the game as close as possible to the original, with an updated engine that gives us
full control of the drawing and fixes the graphic problems at their root.

* **Kept from the original:** the game's own code runs everything except the 3D drawing:
  physics, AI, timing, rules, menus, HUD and 2D screens, sound, input. The look stays the
  original: the same models, textures, palettes, camera views and art.
* **Replaced:** the 3D drawing of the world (track, scenery, cars, wheels, shadows, smoke,
  marks, sky), in the main view, the rear-view mirror and the menus' 3D scenes. Our
  renderer reads the game's data from its memory after the game has loaded it from the
  user's disc, uploads it to the GPU once and draws it every frame.
* **Fixed by construction:**
  * precision: positions, camera and transforms in floating point (no whole-unit view
    coordinates, no 640 x 400 grid, no fixed-point matrices): no jumping textures near the
    camera;
  * visibility: the whole world is available and drawn with frustum culling and a real
    depth buffer: terrain hides what is behind it (no floating buildings), no hand-made
    lists, no pop-in;
  * performance: static geometry lives on the GPU; per frame only the camera and the
    moving objects change. The target is 400+ frames per second on modern hardware, the
    mirror and the menus included;
  * interpolation in floating point between the game's simulation steps.

## Decisions

* **Visibility (2026-11-21, the user):** the tracks are stage sets: there is no terrain
  outside the track corridor (topic A: on Rally China the ground is a ribbon one to two
  cells wide lined with backdrop panels; peaks, towers and villages stand over empty
  space, 792 of 2260 objects have no ground near their base). Drawing everything would
  show them floating. The renderer therefore draws what the game's own visibility lists
  allow (the designers' intent) and, beyond them, only objects with ground under them
  (such as the road far ahead); objects fade in and out instead of popping. Generating
  filler ground would be new art and is out of scope.
* **Visibility, revised (2026-11-21, the user, after testing):** "backed by the world".
  The first rule still showed far track pieces and trees floating in the sky (the lists
  borrowed from other track cells and the "grounded" statics include them; nothing in the
  world hides them), and objects faded out as the camera approached (the fade followed
  membership of a set that changes with the camera cell, not what is on screen). Now:
  (1) the base is exactly the designers' list of the camera's cell; (2) objects that the
  lists of the track ahead would show are candidates, drawn only when their base (their
  lowest part) lies in front of world geometry already drawn, never against the sky or
  the void (tested per frame on the GPU); (3) fades only in, at a distance, when an object
  starts being drawn; an object on screen and near is never faded out (depth hides it when
  something is in front).

## Non-goals

* No remake in another engine, no new art, no change to the game's behaviour.
* No game data in the repository, ever (see constraints).

## Architecture (target)

```
 game code (STRT3FX.EXE, native x86)            runtime (Rust)
 ─────────────────────────────────────          ─────────────────────────────────────────
 loads track, models, textures  ───────────────► sr-runtime/src/native/  extraction:
                                                   world, models, materials, textures
                                                   -> SceneData (once per track / scene)
 simulation step (33/s): camera, objects ──────► per step: camera + object transforms
                                                   (float), kept for interpolation
 world drawing 349b8 ── skipped when native ──►   (the game's own 3D drawing is not run)
 HUD, menus' 2D, LFB  ─────────────────────────► existing Glide path (unchanged), on top

                                                 sr-render/src/native/  GPU renderer:
                                                   upload SceneData once; per frame draw
                                                   views (main, mirror) from FrameData
```

* **sr-runtime `native/`** turns guest memory into a neutral scene description: no
  Direct3D types, no guest addresses in the output.
* **sr-render `native/`** draws that description with Direct3D 11: vertex and index buffers
  uploaded once, materials matching the game's polygon modes, depth buffer, the views'
  cameras, then the game's 2D on top through the existing path.
* The original Glide path stays as it is, as the reference to compare against and as a
  fallback setting after the switch.

### Scene description (the contract, revised after wave 1)

The contract is the crate **`crates/sr-scene`** (std only): `sr-runtime` builds its types
(re-exported as `sr_runtime::native::scene`), `sr-render` draws them
(`sr_render::native`). Its documentation is the reference; the essentials:

* **`SceneData`** once per track or menu scene: `textures` (the game's formats:
  `Indexed8` with a `0x00RRGGBB` palette, `Rgb565`, `Argb4444`; one level, power-of-two),
  `texture_frames` (animation frame lists), `materials`, `meshes` (indexed triangles,
  model space, `Vertex { pos, normal, uv, color }`), `statics` (world objects).
* **`FrameData`** per drawn frame: `views` (main, rear-view mirror), `dynamics` (cars as
  one instance per part, smoke, shadows), `meshes` built per frame (`FrameMesh` with an
  f64 origin), `static_fade` (visibility per static, decided by the runtime), `clock`
  and `env_phase` (animation); per `dynamics` entry `dynamic_fade` / `dynamic_backing`
  (moving objects' visibility), `dynamic_order` (the game's drawing order of puffs over
  cars) and `dynamic_dissolve` (puffs between two steps' pictures).
* **Spaces:** world and model space are the game's (x left, y up, z back, left-handed);
  positions that are absolute are f64 (`Transform { basis: [f32; 9], origin: [f64; 3] }`,
  `Camera::pos`). View space is fixed: x right, y down, z forward. Each `Camera` +
  `Projection` is filled from the game's camera structure so that it reproduces topic D's
  reference projection exactly (eye 128 units behind the game's view origin; main view,
  menus and mirror per `transforms.md`, "Recipe"); a runtime unit test checks it.
* **Instances** carry `phase` (the game's instance +20h, animation and car states) and
  `scroll` (the reflection offsets).
* **Materials** are orthogonal fields (`texture` + `anim`, `transparency`, `texgen`,
  `layer`, `cull`, `combine`, `wrap`, `bilinear`); the game's polygon modes map onto them
  as follows (topic B's table, verified exactly in a race):

| Game mode (material type) | Material | Notes |
|---|---|---|
| 0 (10h) opaque textured | `Opaque`, wrap `Clamp` | |
| 1 (40h) colour-keyed | `ChromaKey { key }`, `Clamp` | key = texture record +14h, compared after the palette |
| 2 (flat) car bodies | `texture: None`, `Opaque` | the polygon's 5:6:5 colour (record +02h) in `Vertex::color` |
| 3 colour-keyed additive (not seen) | `ChromaKey` + blend (ONE, SRC_COLOR) | from the code only |
| 4 (not seen) | as 0 | |
| 5 (A0h) reflection, car windows | `TexGen::Reflection`, `Repeat`, `Opaque`, `anim` source `Env` | the env library's material 0; formula in `sr-scene` |
| 6 (100h) glass, stickers | `Blended(ALPHA)`, `Clamp` | ARGB4444; no depth write, after the opaque polygons of the same part |
| 7 (120h) smoke, shadows, tyre marks | `Layer::Decal { pull, stand }`, `Blended(ALPHA)` (or 2 x src x dst when record +34h != 0) | pulls as the Glide path: shadow 250, smoke 500, marks 120; smoke `stand` 20 (stands on the ground) and the game's order over cars (`FrameData::dynamic_order`) |
| 8 sky + ground | `View::sky` | not a mesh: a screen-aligned quad computed per view |

  No lighting anywhere (textured polygons ignore vertex colour). Front faces are
  clockwise on screen. World objects: back faces culled, as the engine's list builder
  `7ac50` drops them (W2-1 measured that the back faces it still draws are 659 of 318 595
  triangles near the camera; drawing the dropped ones would add 48 %); scene instances
  (cars, shadows, smoke, marks): culled where the game culls (`depth.rs` `CULL_ON`), so
  their materials' `cull` follows the object.
* **Visibility** (the revised decision above, "backed by the world";
  `native/visibility.md`): the runtime fills `static_fade` (the designers' list of the
  camera's cell, plus candidates from the lists of the track in view) and
  `static_backing` (which candidates the GPU tests); the renderer draws the base world,
  tests the candidates' bases against it (`BackingResults`, read back a frame later) and
  draws those backed. Measured on six tracks (two laps, four cameras): main-view frames
  with a floating object 49-75 % -> 0-0.3 %; mirror 45-64 % -> 0.0 % (at most 2 frames
  per track, after its minimum of sky pixels was scaled to its smaller pixels). No extra
  distance in practice: candidate bases are almost always hidden or against the sky, so
  the designers' lists already hold what can be seen. Moving objects (cars, shadows,
  smoke, marks; `native/dynvis.rs`) are drawn only where the world under them is drawn,
  never the player's car or an object within 8192 units: frames with a car over undrawn
  world 1.7-8.2 % (main) and 0.1-1.5 % (mirror) -> 0. The original shows such cars too
  (Rally China frame 6750, a car in the sky). A camera cell without a list takes the
  nearest listed cell's list merged with the last one (before: Canada, Arizona, Sweden
  and Wales emptied the view for up to 400 frames, the cars over the ground colour).

## Glossary

* **link address**: an address in the executable's link space, as in `docs/re/strt3fx.md`
  (e.g. `349b8`); documents use link addresses, code converts with `m.img.link()`.
* **object**: something the engine's draw loop sets up (`8246c`, at `8253f` esi = its
  instance): a scene-table instance (car, shadow, smoke) or a world object in a cell.
* **model**: an object's geometry; **part**: a model's piece with its own matrix (the
  engine's part, drawn through `78e71` / `7ba0c`); **level**: a part's detail level 0, 1
  or 2 (slot i at level L: model +18h + 34h L + 4 i). Wheels are parts of the car models
  (the scene table has no wheel objects).
* **polygon record**: the engine's data for one polygon (vertex count at +6, vertex
  offsets from +8, ...).
* **mode**: the polygon builder's polygon type (0 to 7; traces print `mode`).
* **view**: one 3D drawing of the world with one camera (0 main, 2 rear-view mirror).

## Phases

1. **Map the data** (wave 1): how the world, models, levels, polygons, textures and
   transforms are held in memory, verified against the original output. In parallel: the
   GPU side of the renderer (against synthetic data) and the English version check.
2. **Static world native**: track and scenery from the GPU with the game's camera in
   float, compared with the original; frame rate on a lap of China.
3. **Cars and moving objects**: cars with parts and wheels, shadows, smoke, tyre marks,
   glass and reflections; interpolation in float.
4. **Everything else in 3D**: rear-view mirror, menus' 3D scenes (with in-between frames),
   attract mode, replays.
5. **Switch**: native by default; the Glide path stays as a setting.

## Wave 1 (done, 2026-11-21)

Every result was merged after the organiser re-ran its check on a race the agent had not
used (the Shield, close chase camera, mirror on):

| Topic | Result (verified) | Check re-run by the organiser |
|---|---|---|
| A. World and objects (`native/world.md`) | cells, objects (40h bytes), models (13 part slots x 3 levels), placement `world = Ro (Rp v + tp) + to`; world objects never move; China: 2260 objects, 1585 track models, 43 700 polygons at level 0; no terrain outside the track corridor; the engine's matrix product overflows beyond ~122 000 units (300 % draw distance) | `SR_DUMP_WORLD`: 1 431 032 vertices, max 4.7 units (engine rounding), nothing missing |
| B. Polygons and materials (`native/polygons.md`) | record layout, uv per model vertex (u / 256), mode from the material type, flat colours, no lighting, exact reflection and sky formulas | `SR_POLY_CHECK`: ~1.7 M polygons, 0 mismatches in s/t, 1/w, colour, state |
| C. Textures and palettes (`native/textures.md`) | all of a race's textures in RAM (records at 1479d4 + 38h id), material -> frame list -> record, animation and car states, the texture manager ignorable | `SR_DUMP_TEXTURES` check: 1 146 821 binds, 0 differences |
| D. Transforms (`native/transforms.md`) | `R = Ry(-a1) Rx(a0) Rz(a2)`, part -> object -> world -> view, projection, mirror, axes, a recipe for a modern projection | `SR_TRANSFORMS_CHECK`: 7742 parts, 201 499 vertices, 0 differences from the engine's own numbers |
| E. Renderer core (`native/renderer.md`) | Direct3D 11 renderer for the contract, test scene, integration point | default output unchanged (24 frames identical); test scene 2347 fps at 1920 x 1080 (~100 k triangles, two views) |
| F. English version (`docs/re/english.md`) | identical executables, language in three data files; MDS music offset fixed | executable hashes; the .mds track offsets read directly |

## Wave 2: static world native (phase 2)

Goal: in a real race, the track and scenery drawn by the native renderer from the game's
data, while cars, shadows, smoke and tyre marks still come through the Glide path (a
hybrid frame sharing one depth buffer); the camera in floating point and interpolated;
visibility per the decision; compared with the original and measured. Off by default
(`graphics.renderer = glide | native`, `SR_NATIVE=1`).

| Topic | Owns | Delivers |
|---|---|---|
| W2-1. Scene extraction | `crates/sr-runtime/src/native/extract.rs` (`build_scene`, its signature fixed): every texture of the race (track, cars, env map, sky), materials per the mode table, one mesh per world model at its highest level (uv, normals where the model has them, flat colours), the statics with their `StaticMeta` (cell, grounded, bounds); car part meshes as a stretch | a check that the scene's statics, transformed by `sr-scene` and projected with topic D's math, land on the engine's own view positions, and that polygons carry the game's texture and uv |
| W2-2. Renderer semantics | `crates/sr-render/src/native/`: everything the contract now says the renderer does: `TexAnim` (phase, clock, env), the exact `TexGen::Reflection`, `static_fade` with screen-door fades on chunked statics, `View::sky`, the decal multiply variant and mode 3; depth in the same convention as the Glide path so Glide-drawn cars are tested against the native world; flush after the main view | unit tests per feature; `SR_NATIVE_TEST` scenes showing each; measured cost |
| W2-3. Integration and visibility | the switch (setting, `SR_NATIVE`, F12 item marked experimental), the `Backend` trait methods (forwarded by the D3D backend in its own impl block), calling `build_scene` once per race, per frame `FrameData` (main and mirror views from the game's cameras with a unit test against D's projection, `static_fade` from the camera cell's lists plus grounded statics and fades, clock, env phase), skipping the game's drawing of world cells and sky when native, interpolated frames in float | the race on China drawn hybrid, side by side with the original (frames), mirror on, frame rate with VSync off |

Rules for wave 2: W2-1 owns `extract.rs` (and may add read helpers to `world.rs`,
`textures.rs`, `polygons.rs`); W2-3 owns new files `native/frame.rs` and
`native/visibility.rs`, the hooks and settings, and calls `build_scene` through its fixed
signature (it returns `None` until W2-1 is merged); W2-2 owns `crates/sr-render/src/native/`
and does not change `d3d.rs` beyond what the renderer itself needs. Contract changes go
through the organiser.

### Wave 2 results (done, 2026-11-21)

Merged after the organiser's own checks (the Shield, close chase camera, mirror on):

* **Switch off:** frames byte-identical to before (24 of 24; the agent: 38 of 38).
* **Hybrid race** (`graphics.renderer = native` or `SR_NATIVE=1`; F12 "Renderer
  (experimental)"): track, scenery and sky drawn natively from `build_scene` (China:
  built in ~120 ms, uploaded in ~55-135 ms), cars, shadows, smoke and marks through the
  Glide path into the same depth buffer. Side by side with the original frames of the
  same moments: the same picture (texel-edge noise), mirror and HUD identical.
* **Frame rate** (1280 x 720, VSync off, interpolation on, close chase camera, nothing
  else running): Glide path 240-370 presents per second, hybrid 940-1010.
* **Near the camera (fixed in wave 3, W3-2):** a wall beside the camera was missing
  natively (Shield frame 2100) and other statics showed through beside the far chase
  camera. Cause: the native vertex shader's clip-space z blew up for vertices behind the
  eye, so the depth clip cut those triangles away. Fixed in the shader; the engine's own
  near handling was decoded (`native/near.md`, predicted with 0 errors over 403 880
  polygons) and native clips at the same plane. Sweeps of 626 frames against the
  original: frames with strongly differing edges 25 + 43 -> 0. The native view left the
  window's last pixel column black (the Glide path draws it): fixed, see "Frame edges".
* **Contract notes from wave 2:** the runtime sends one `FrameData` per view (the
  mirror's camera exists only after the main view is drawn); `View::viewport` is the
  game's 640-column rectangle (the renderer widens a full-width one; the sky's `s` depends
  on it); mode 3 (key + blend) not expressible yet; car part meshes need a
  (model, slot) -> mesh map for wave 3.

## Wave 3: cars, moving objects, and near-camera fidelity (phase 3)

| Topic | Owns | Delivers |
|---|---|---|
| W3-1. Cars and moving objects | dynamics in `FrameData`: car part instances from the scene instances' per-part placements (wheels with spin and steering, suspension), shadows and tyre marks (decals), smoke (camera-facing, phase-animated), car glass and overlays in the game's per-part order, reflection scroll offsets, car detail levels (one version per car: no stand-in drawn over the detailed parts), interpolated in float; the game's drawing of scene instances skipped when native | the whole 3D world native in a race; compared with the original; frame rate |
| W3-2. Near-camera fidelity | why the native world and the game's differ beside the camera (the frame-2100 wall, the far chase camera's extra statics): what the game does with polygons crossing its near plane or overflowing its projection, and what native should do so it shows what the game shows without its errors | `docs/re/native/near.md` with evidence, the fix, before/after frames |

### Wave 3 results (done, 2026-11-21)

* **Whole race native** (W3-1): cars (parts placed per frame: wheels spin and steer,
  suspension), shadows, tyre marks, smoke (facing each camera), car glass and overlays in
  the game's per-part order; the game's own drawing of scene instances skipped (observer at
  81fbc); HUD and 2D on the Glide path. One version of each car ("maximum": every part at
  the finest level; "original": the game's rule). Checks: 2.36 M dynamic vertices against
  the engine's (max 13.6 units, its Q14 rounding); the Shield's rear window, overlay,
  sticker and plate as the original in every compared frame, its interpolated sequence
  stable (the Glide path jumps 15-17 four times in 80 frames: likely the user's
  "alternating reflections"). Organiser: switch off 24/24 frames identical; side by side
  with the original, the same picture; frame rate (1280 x 720, VSync off, interpolation
  on, close camera): native 1470-1530, native with mirror 820-900, Glide with mirror
  125-170.
* **Near camera** (W3-2): fixed, see above.
* Open: the longest gap between presents still 7-16 ms in some seconds (pacing under VSync
  to check); only Rally China verified. (The window's last pixel column: fixed, see "Frame
  edges".)
* **Smoke and dust puffs** (fix, 2026-11-21; `native/dynamics.md`, "Smoke and dust puffs"):
  the user saw them cut by the ground, hidden under the car where the original draws a cloud
  over its rear, and stepping at the simulation rate. The original has no depth buffer: it
  draws objects far to near by distance (`82130` / `7a8c0`), every scene instance as if 3 394
  units nearer than the scenery, so a puff covers the ground it stands on and a puff nearer
  than a car's position covers the car. Native now: the puff (`Layer::Decal::stand`) is
  tested below the ground plane through its base at the plane's depth (the ground ahead no
  longer cuts it, walls and hills in front still hide it); over cars it follows the game's
  order (`FrameData::dynamic_order`: places in the stencil's low bits; a puff covers the cars
  behind its position whatever the depth, never those in front); puffs are sent far to near;
  in-between frames dissolve each puff from one step's picture to the next
  (`FrameData::dynamic_dissolve`: fade in at birth, out at the end, texture frames mixed)
  instead of switching at the first in-between frame. Verified on China (close chase): frames
  next to the game's order (`SR_DEPTH=0`) match (`work/agents/smoke/sbs_dust_over_car.png`,
  `sbs_ground.png`); 804 in-between frames: 3 196 complete picture switches -> none, each
  change at most 1/4 of a switch with four frames per step (`strip_birth.png`); three GPU
  tests; mirror unchanged but for a faint puff; Glide path 48 of 48 frames byte-identical.
  Not changed: the Glide path with depth still hides the dust under the car and cuts far
  puffs (its output must stay as it is). Open: a puff behind a low wall can show its lower
  part on the ground in front of the wall's foot; the multiply-blend smoke variant (texture
  +34h != 0, not seen) does not fade in or out; only Rally China checked.
* Open, user's report (2026-11-21): **the objects' visibility is not good.** Far objects
  fade in too obviously, the user wants to see further than now, and some unwanted floating
  objects are still visible. Fixing this needs a tool to inspect and correct what each
  track cell shows; postponed until that tool exists.

### Frame edges (done, 2026-11-21)

* **White line under the fade after the name entry** (user's report; both renderers, the
  2D being the Glide path's): the game's fade (`54b4c`, `strt3fx.md`) is a constant-colour
  quad over 0..639.8 x 0..479, so it never covers the screen's last row (the 3dfx at
  640 x 480 leaves row 479 too) nor, from 2.5 target pixels per game pixel, the last
  column. After the name entry the game draws a white flash with it (alpha 08h..F8h over
  the picture), clears the screen to F8FCF8h and fades to black with it (alpha 08h..E8h,
  frames 629-645 of the default run): the uncovered rows (2 at 1280 x 720 and 1920 x 1080)
  stayed dark under the flash, then white under the fade. The Glide layer now puts the
  quad's last row and column on the screen's edges (`cover_screen`; only coverage changes,
  the colour is constant). The run's other fades (intro, main menu to track selection,
  car selection) changed the same way: their last rows now take the fade too.
* **Native view: last column and row undrawn.** Direct3D clips to the viewport's whole
  pixels, and in f32 the full-width view's viewport came out at 1279.9999 x 719.9999 at
  1280 x 720 (the widescreen extension's arithmetic, the runtime's rectangle ending at
  y 479.99994). A right or bottom edge less than 0.01 px short of a whole pixel is now
  put on it, by the smallest f32 step that reaches it; start edges and other rectangles
  (the mirror's, at half pixels) keep their exact values (`viewport_in_target`; GPU test
  `full_width_view_reaches_every_edge`: 7 window sizes, WARP and the hardware device).
  The right edge was short at 1280 x 720 and 1920 x 1080 (whole at 1366 x 768,
  2560 x 1080, 1024 x 768 and with the 4:3 preset), the bottom one at every size, but the
  bottom row is mostly hidden anyway: the game writes its row 479 black through the LFB in
  races and most menus (640 zero pixels per frame; the Glide path shows the same black
  row, as the original). Side effect: where an edge moves, the projection moves by one f32
  step (6e-5 px), which flips nearest-texel and coverage decisions at a few texel
  boundaries and polygon edges: see the comparison below.
* **Widescreen side clear one column short** (both renderers): `clear_sides` rounded the
  4:3 picture's left edge down (159.99998 at 1280 x 720) and cleared the same width on the
  right, so columns 159 and 1120 were touched neither by the 640 x 480 pictures nor by the
  clear: the name entry's spinning letters left trails in column 159 (311 rows). It now
  clears every column whose centre is outside the picture (`side_columns`).
* **Checked** with `SR_SHOT` captures of the deterministic default run (name entry to the
  race, autopilot), main at b2c4396 against the fix, both renderers, at 1280 x 720 (every
  5th frame 400-1900), 1920 x 1080, 1366 x 768, 2560 x 1080, 1024 x 768 (4:3 window),
  1280 x 720 with the 4:3 preset and 1920 x 1080 bilinear (every 15th frame 450-1890).
  The same binary repeats exactly except for the settings overlay's start toast (timed by
  the wall clock). Before, at every size and on both renderers, the last two rows were
  white under the fade (frames 630, 645) and dark under the flash (615); after, they are
  the fade's colour. Glide path: every other differing pixel is in the columns beside the
  picture (159 and 1120 at 1280 x 720, 239 and 1680 at 1920 x 1080; none at the other
  sizes, whose picture edges are whole pixels) or in the toast. Native: also the last
  column (1279 / 1919), and the projection's one f32 step: per frame with nearest
  filtering, menus up to 865 pixels (0.03 %), races up to 1159 at 1280 x 720 (0.13 %),
  4395 at 1920 x 1080 and 5306 at 2560 x 1080 (0.2 %; medians 109-178); bilinear races
  1892-14507 (up to 0.7 %, mostly by less than 16 levels). In difference images (the
  densest cases looked at: 1280 x 720 frames 700 and 1670, 1920 x 1080 frames 1725 and
  1860): single texels and bands along texel boundaries (where a boundary runs along pixel
  centres, a band of big texels near the camera takes its neighbour's row; such bands
  fill whole 8 x 8 tiles only at 1920 x 1080 and 2560 x 1080), polygon edges, a noisy
  reflection texture; otherwise the same picture, no object drawn or left out.

## Wave 4: every track, the menus, pacing (phase 4)

| Topic | Owns | Delivers |
|---|---|---|
| W4-1. Every track | run every track natively (Canada, Italy, Arizona, Sweden, Wales, Stadium, China, the championship and time-trial variants), the extraction and dynamics checks per track, fixes for what China never exercised (animated or multi-part world models, modes 3 / 4, animated reflection maps, other skies) | per track: checks, frames next to the original, frame rate |
| W4-2. Menus' 3D scenes | the menus' 3D scenes (main menu background, car selection turntable, cabinet, letters) natively and with in-between frames so the menus run above 50 frames per second; the attract race and replays checked | menus at the display's rate, frames next to the original |
| W4-3. Pacing and polish | even frame pacing with VSync and with the frame rate limit (no long gaps around the game's own frame), the last pixel column, the mirror's cost | present-time statistics before / after, frames |
