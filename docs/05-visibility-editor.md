# 05 — Visibility editor (the occluder tool)

The user's request (2026-11-22): a tool like Milestone's own from 1997. Two windows: the game,
running a single car with no timer or laps, driven with the controller; and a 2D top view of
the whole track with every object, divided in sectors, used with mouse and keyboard. Drive
around, turn off the sectors that should not be seen from where the car is, save; the game
then hides and shows track parts, buildings and trees by that data. A change in the tool
shows in the game at once. Easy to use, clear and simple UI, zoom. Frame rate is not a
concern.

## What a sector is, and what is edited

* **Sector = the game's cell**: the world is a 256 x 256 grid of 8 192-unit cells (row from
  z, column from x; `crate::visedit::cell_of`). Every static (road piece, terrain, building,
  tree) belongs to one cell.
* The designers gave many cells a **visibility list**: the cells drawn while the camera is in
  that cell. Those lists are the output of the 1997 tool; the game itself draws the camera
  cell's list (within the view frustum's box). The new tool edits exactly that: **per camera
  cell, which sectors are drawn**.
* **Default list** of a camera cell: its designers' list; a camera cell without one (the far
  chase camera on a bend, a car off the road, unlisted stretches of track) takes the
  designers' list of the nearest cell with one (within 8 cells; `visedit::default_list`).
  With no list within reach the game draws every cell of the view's box (the engine's rule,
  `7a640`; the native renderer too while no list has been seen): the default is then every
  sector holding objects (`visedit::default_cells`), and such a camera cell keeps the game's
  own rule (no effective list) until it is edited. On the six rallies every track cell has
  a list within reach; **Stadium has no list at all** (its whole map is drawn by default,
  the panel says "DRAWS ALL IN VIEW / NO LIST", the road extension is greyed out: it reaches
  beyond lists that do not exist). Before 2026-11-22 such a cell started from nothing: the
  map showed Stadium all red, a hide did nothing and a left click made a one-sector list
  where the game drew everything.
* **Road extension** (the whole track, `visedit::RoadReach`, saved with the edits): every
  camera cell also draws the road followed further than its default list goes, and what
  stands beside it there (below, "Extend"). One rule for every camera cell, so it applies by
  itself wherever the camera is while the user drives.
* **Edits** per camera cell: sectors added, sectors removed, on top of the default list and
  the road extension. A click changes the camera cells of the **edit area** around the
  editing camera cell: those within **Area** sectors of it along the road, both ways
  (`visedit::road_area`: cells whose road places lie within that distance of the editing
  cell's, round the lap; default 4, 0 to 16, 0: the editing cell only; without a road line
  the cells within that many rows and columns). The user's requests of 2026-11-22: first
  "I want the effect to be applied only when I am in that active sector" (the scopes
  "nearby" and "whole track", and cells without a list borrowing their source cell's edits,
  had spread clicks over the map; they are gone), then, with edits for the camera cell
  only, "as soon as I move a bit the edit I did goes away. the edit needs to be propagated
  with the area Im in not the exact cell". **Effective list** = default list + road
  extension - removed + added (`visedit::Edits::effective`).
* **Objects across a sector's edge**: each static belongs to the cell of its own centre, so
  a big object made of several statics can span two sectors (Rally China's pagoda: its two
  roof ornaments are in the next sector): hiding one sector leaves the other part floating
  until that sector is hidden too, as with the 1997 tool.
* Why not the automatic rule as the starting point: the native renderer's current rule
  (`docs/re/native/visibility.md`: designers' list plus "candidates" accepted by a GPU test of
  the world behind them, with fades) depends on the view direction and on the frame, so the
  tool could not show what will be drawn. With edits the game draws exactly what the tool
  shows (what you see is what you get), and the user can see as far as they choose.

## How the game uses the edits

* Native renderer only (the Glide path keeps the game's own lists).
* **Active** in the tool, and in normal play for every track that has a saved edits file
  (`visedit::edits_active`). Tracks never edited keep the automatic rule.
* While active, per view (main and mirror): the statics of the sectors in the camera cell's
  effective list (its default list, the road extension and its own edits; read again when
  the camera cell or the edits change), inside the view frustum's box of cells (the draw
  distance setting), are
  drawn; nothing else. No GPU backing test. A static that starts being drawn while on screen
  fades in quickly (the existing list rule's fader); the moving objects' rule ("backed by the
  world", `native/dynvis.rs`) is unchanged.
* The sector under the editor's mouse **blinks** in the game's view (when blinking is on), so
  the user can tell which sector is which.

## File

`<data folder>\visibility\<track id>.vis`, text: the road extension (when on), then one
line per edited camera cell:

```
# Screamer Rally visibility edits (docs/05-visibility-editor.md)
track 1a2b3c4d5e6f7788 Rally China
road 6 2
cam 120 88 +121,88 +122,90 -130,70
```

`track id` = hash of the track's visibility lists and statics (`visedit::track_id`), so a file
can never be applied to another track. `road A W` = the road extension: A sectors further
along the road than the designers' lists, W half sectors each side of the road's line.
`+r,c` = sector added, `-r,c` = sector removed.

## Starting the tool

`ScreamerRally.exe --vis-editor` (the package ships `Visibility Editor.cmd` that does this).
Same data folder and settings as the game. The game window opens as usual and the editor
window opens next to it.

## The game window in the tool

* Renderer forced to native (the edits are a native-renderer feature), whatever the setting
  (the setting itself is kept).
* Pick any track (and car) through the game's own menus, in any mode: the race becomes the
  game's own **Time Attack with free laps** ("GIRI LIBERI", menu mode 4): **only the player's
  car**, laps keep counting and the race **never ends** (`crate::toolmode`, `docs/re/strt3fx.md`
  "Race set-up, modes, laps and the race's end"). The game has **no time limit** in any mode
  (its "TIME OVER" text is never used). The tool never writes the saved game (`CHOICE.DAT`).
  There is no routine to put a stuck car back on the road mid-race: Esc > Ricomincia restarts
  at the start line. The attract-mode race keeps its cars.
* The game keeps running and reading the controller while the editor window has the focus
  (keys held in the game window are let go when it loses the focus).
* Windows: on a single monitor the game runs **windowed** (a fullscreen setting is kept but not
  used in the tool) and the editor moves it to the left of the screen, narrowing it if needed
  so that the editor gets two fifths of the screen on the right; with two monitors the game
  stays fullscreen (borderless) and the editor opens on the other monitor.
* F12 settings overlay as usual; Tab autopilot as usual (handy: the AI drives laps while the
  user edits).

## The editor window

Own top-level window ("Screamer Rally - Visibility Editor", then the track's name and `*`
while edits are unsaved), resizable, on its own thread with its own Direct3D 11 device and
swap chain (`crates/sr-viseditor`; it never touches the game's device). `sr_viseditor::spawn`
starts it from the app's `--vis-editor` block; it waits up to 5 s for the game's window and
opens to its right (on another monitor when the game is fullscreen or the game's monitor has
no room; over the right of the game's monitor when there is no other), sized for the
monitor's scale (1200 x 900 at 100 %), shown without taking the focus. Closing it saves
unsaved edits (it asks only if saving fails) and leaves the game running.

**Layout:** the map fills the window; a panel on the right (183 font dots wide: 366 px at
100 %) shows the state and clickable buttons; a one-line status bar at the bottom. Panel and
status bar use the F12 overlay's pixel font and colours, crisp at an integer scale: 2 at
100 %, 3 at 150 %, 4 at 200 %, lower when the window is too small (a window a little short
for scale 2 cuts the panel's end rather than going down to scale 1). The panel's legend and
key list are shown when there is room.

**The map:** the race's statics seen from above (orthographic), with the game's textures:
higher surfaces over lower (depth from world y), chroma-keyed texels are holes, blended
materials blended; mipmapped (sharp texels up close, averaged far away). x to the right and
z up: checked against the game's camera, whose right vector is always 90 degrees clockwise
from its forward vector on this map (the game's world is left-handed), so turning right in
the game turns the car's arrow clockwise. One static vertex buffer per race (Rally China:
81 446 triangles, 193 texture batches), rebuilt and fitted when a new race's map arrives.
Every triangle carries its static's cell, and a 256 x 256 state texture tints it:

| What | Look |
|---|---|
| Sector of the camera cell's default list | light green tint |
| Sector the road extension adds | blue tint |
| Sector added by the user | stronger green tint |
| Sector hidden by the user | red tint, stronger |
| Sector not drawn | dark red tint |
| The camera cell | thick yellow outline (dark edge) |
| The edit area's other camera cells | thin yellow outlines |
| The road's line; the stretches the camera cell's road extension follows, with the width each side | thin light blue line; bright line and thin lines |
| Sector under the mouse | white outline, brighter; details in the panel; blinks in the game |
| Grid | thin lines once a sector is 14 px or more |
| Car / camera | white arrow at the car (its heading); blue wedge at the camera (its view) |
| Rectangle being dragged | white frame; the sectors it takes filled green (to draw) or red (to hide) |

Without an editing camera cell (no race frame published yet, nothing picked) the map is
shown untinted. Before the first race the map area says "Start a race in the game window".

**Mouse** (edits change the edit area's camera cells, `visedit::apply` / `reset` with
them):

* **Left click** a sector: drawn; **right click**: hidden. A sector without objects is left
  alone (the status line says so).
* **Left / right drag**: a rectangle; on release every sector with objects it touches is
  drawn / hidden (one `apply`, one undo step).
* **Middle click**: the sector back to the default list and the road extension
  (`visedit::reset`). **Middle drag**: pan (turns following the car off). **Wheel**: zoom
  around the cursor.
* **Ctrl + click**: edit another camera cell's list without driving there (locks it).
* Hovering sets `visedit::set_highlight(cell, blink)`; leaving the map clears it.

**Keys (also panel buttons):** `E` the road extension 2 sectors further
(`visedit::ALONG_STEP`, up to 80), `Shift+E` 2 less (down to 0: the designers' lists);
`<` `>` its width -/+ half a sector (0 to 8 sectors, default 1); `[` `]` the edit area -/+
one sector; `Backspace` drop the edits of the edit area's camera cells (`reset_cameras`); `L` lock the camera cell or follow the game's
camera cell again; `F` follow the car (on by default: the map moves, smoothly, when the car
leaves the middle half of the map); `Home` fit the track; `+` `-` zoom (`=` too, numpad
too); `B` blink in the game on or off; `G` grid; `T` textures or flat colours (each
texture's average colour, keyed holes kept); `Ctrl+Z` / `Ctrl+Y` (or `Ctrl+Shift+Z`) undo /
redo (the road extension's changes too); `Ctrl+S` save. `+` `-` `<` `>` `[` `]` are read as typed
characters, so they work on any keyboard layout. Edits are also **saved automatically** a
second after the last change (retried every 5 s after a failure).

**Extend: the road extension** (the user's requests of 2026-11-22: "imagine the track road
is a tube, and you pour water from above ... the water will extend in both ways back and
forward following the shape of the tube"; then "start from the devs default, then extend
along the path automatically from the car point and the more I click extend the more it
draws ... I want the extension to be applied automatically while I drive"):

* The road's line is the closed line the computer's cars follow, read from the game at the
  race's start (`sr_runtime::road`: the `<map>opp.spl` spline at `[139700]`, cubic Bezier
  nodes sampled every ~1000 units; every track has one, Rally China 1 228 000 units) and
  published with the map (`TrackMap::road`). Rally Italy: its line is there although the
  table's count `[139704]` reads 0 in the race, so the table's first entry is read and
  checked (a closed chain of nodes, all but 2 % of its points within two sectors of the
  statics).
* For a camera cell (`visedit::road_extension`), the water starts where the road passes
  through it (`visedit::road_sources`: the line's points inside the cell, or its nearest
  point within two sectors of the cell's centre), runs forward and back along the line as
  far as the default list covers the road (`Road::walk`: while the line's points are in
  cells of the list, or in cells without objects), then **Along** sectors further, round the
  lap (`Road::stretch_ranges`); every cell with an object whose bounding circle on the ground
  comes within **Width** of those further stretches is added. Along 0: the designers' lists
  exactly. So each press draws more, starting where the designers stopped; parts of the map
  near only in a straight line (the other leg of a hairpin, another loop) stay out unless
  within the width. A camera cell the road does not pass near gets no extension.
* It is one setting for the track (`Edits::road`, the file's `road ALONG WIDTH` line), so the
  game applies it by itself in every camera cell, in the tool and in normal play; one undo
  step per change.
* Rally China in the tool (autopilot driving): presses 1, 2, 3 add 26, 41, 50 sectors to the
  start's camera cell; then, without touching anything, the camera cells the car reaches
  draw 51 to 58 sectors more each. Normal play with `road 8 2`: the frames draw more trees
  and buildings farther along the road (frame 1800: 15 882 pixels differ from the
  designers' lists).

**Panel:** the track's name and id; the camera cell (row, column; following or locked) and
how many sectors it draws (how many by the road), with the lock button; "Road extension:
whole track": Extend, Less, how far further than the designers' lists ("+6"), the width with
- / +; "Clicks: N camera cells" (the edit area's): Area with - / +, Reset area, Save, Undo,
Redo; Fit, Follow car, Grid,
Textures; the sector under the mouse (row, column, objects, state: drawn by default / by the
road extension / added / hidden / not drawn) with the blink button; the file (saved, unsaved or the error,
how many camera cells are edited, the file's name); the legend; the mouse and key list. The
**status line** shows what the last action did (for 5 s), the help of the button under the
mouse, or the mouse's actions on the map; on its right, the sector under the mouse.

**Code** (`crates/sr-viseditor/src`): `window.rs` (Win32 window, input as events,
placement), `editor.rs` (state and behaviour, layout, what is drawn), `view.rs` (zoom, pan,
follow, pixels <-> world), `scope.rs` (rectangles), `mesh.rs` (vertex
list and batches, texture variants and mipmaps), `gpu.rs` + `map.hlsl` (Direct3D),
`panel.rs` + `canvas.rs` (panel and status line), `shapes.rs` (outlines, arrow, wedge),
`action.rs` (the keys' and buttons' actions), `script.rs` (test script). Unit tests cover
picking, zoom, fit, follow, rectangles, batching, keying and mipmaps, the panel's
buttons and layout, the script parser, and a whole session of clicks, drags, keys and
buttons against `visedit` (clicks for the camera cell, then for the edit area along the
road but not the loop's far side, the road extension, undo, autosave to the file);
`visedit`'s own tests the road extension and the edit area on a long loop (the road beyond
the designers' list, not the other leg). In the tool (Rally China, autopilot): a sector
hidden from camera cell 129,118 (34 camera cells in the area of 4) stayed hidden while the
camera passed 129,117, 129,116, 128,115 and 127,115, and was drawn again from 125,116 on.
Stadium (no lists; before 2026-11-22 the race did not even load, `crate::conditions`): the
map all drawn, "DRAWS ALL IN VIEW"; 29 sectors hidden from the area of camera cell 124,119
(29 camera cells) gone from the game's frame 1900 (the road ahead, the stands of other
sectors kept); frames 1800 and 2000-2400 (the camera outside the area) identical to a run
without the clicks, but for 204 pixels of a distant barrier at 2300 (cause not found).

**Testing without a person** (diagnostics): `SR_VISEDIT_SHOT=DIR[,EVERY]` saves every
EVERY-th frame drawn as PNG (0: only the script's shots); `SR_VISEDIT_SCRIPT` (text, or a
file) runs timed steps after the N-th track arrives (`start N`): `world X Z`, `cell R C`,
`px X Y`, `center R C [PX]`, `click`, `rclick`, `ctrlclick`, `down` / `up`, `drag R1 C1 R2
C2`, `rdrag ...`, `wheel N`, `key K` (`ctrl+z`, `shift+e`, `home`, `>` ...), `button NAME`, `shot NAME`,
`size W H`, `check R C drawn|hidden|added|notdrawn|default`, `checkfile`, `log`, `close`
(`script.rs` documents them). While a script runs, the real mouse and keyboard are ignored.
Checks log `ok` or `FAILED`, and a summary at the end.

## Status

Done (2026-11-22), verified end to end (Rally China, deterministic runs in the tool driven by
a script): the race from the menus has one
car and free laps; the editor names the track; a click or a rectangle in the editor changes
the game's frames from then on (sectors hidden for the whole track gone from frames
1850-1930); camera cells without a list follow their source cell's edits; normal play is
byte-identical to the build before (Glide 19 of 19 but for the wall-clock "F12 / BACK" hint
box, native 18 of 18); the windows sit side by side for windowed, borderless and exclusive
settings on a 3840 x 2160 monitor at 150 %. The renderer side's verification:
`docs/re/native/visibility.md` ("Edited lists"). The editor is a developer tool: it is not in
the player package (`tools/package.ps1` builds without the `vis-editor` feature).

Known limits: an added sector beyond the draw distance setting is not drawn although the map
shows it drawn; objects split across sectors (above); split-screen races not tried in the
tool; the per-object level (hide one tree, not its sector) does not exist.
