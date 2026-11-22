# Screamer Rally for Windows 10 / 11 — player guide

This program runs the original 1997 Screamer Rally (MS-DOS, 3dfx version) natively on
modern Windows: the game's own code, physics, AI, tracks, cars and music, with a new
renderer, controller support, widescreen and high frame rates. It contains **no game
data**: you need your own copy of the original game.

## What you need

* Windows 10 or 11 (64-bit), a Direct3D 11 graphics card.
* Your copy of Screamer Rally, either:
  * **an image of the original CD**, Italian or English edition (the 3dfx version
    `START0\STRT3FX.EXE`, the same program on both): Alcohol 120% `.mds` + `.mdf`, a
    `.cue` sheet with its `.bin` file(s), or an `.iso` of the data track. The `.mds` and
    `.cue` images also hold the game's **music** (the CD's audio tracks); an `.iso` has
    none; or
  * **the DOS game installed on disk**: its folder with `STRT3FX.EXE`, `DATA` and `GFX`
    (also the common version that runs without the CD). It has no CD music.

## Starting

1. Unzip the package into any folder.
2. Run `ScreamerRally.exe`.
3. The first time, a window asks where your game is: click **Choose game folder…** and
   pick the folder that holds the CD image, or the installed game's folder (or a folder
   above it: the program looks two levels down). The game found is shown, and it starts.
   A CD image next to the program is found without asking.

The choice is remembered in `data\settings.ini` (`[game] path`); if the game is moved, the
window asks again. Your game files are only read, never changed: the game's saves
(`CONFIG\CHOICE.DAT`, controls, replays) and settings are kept in `data`.

## Language

The game is in Italian or English depending on your copy: the text, the menu pictures and
the speedometer (km/h or mph) are in its data. The first-run window shows which language it
found. Progress, records and player names are kept in `data` whichever copy you use.

## Controls

* **Keyboard**: the original controls — arrows to steer, accelerate and brake, Right Shift
  / Ctrl to change gear, Enter / Esc in menus, Esc in a race for the pause menu, M for the
  rear-view mirror, **F1–F4** for the four race cameras (bumper, then chase views from
  near to far; F5–F8 for player 2 in split screen). **C** steps to the next camera.
  **Tab** switches the autopilot on and off: the game's own AI drives your car. With CRT
  emulation on, holding **F11** shows the game without the shader (see "CRT emulation").
* **Controller** (Xbox and other XInput pads; plugged in at any time): left stick steers
  (analog, through the game's own analog steering), right trigger accelerates, left trigger
  brakes (both analog), RB / LB change gear up / down, **Y** (or clicking the right stick)
  changes the camera, **X** turns the rear-view mirror on and off, clicking the left stick
  switches the autopilot. D-pad, A, B and Start work the menus. All of these can be remapped in
  `settings.ini` (`[controller]`).
* **Vibration** (controller mode): the 1997 game had none; the controller's motors now follow
  your car in races, from what the game itself computes in each simulation step:
  * a short, strong jolt for hits on walls and other cars (by how suddenly the car's
    speed changes; a light tap when you only scrape a wall or rub another car);
  * a thump when the car lands after a jump (by how fast it was falling) and on hard bumps;
  * a light, steady rumble on uneven roads, stronger with speed: none on asphalt; on
    gravel, cobbles, dirt, grass, sand or snow as uneven as the track's own data makes each
    road (the game has no separate kerbs);
  * a faint buzz when the tyres slide or spin and at the rev limiter.

  Nothing vibrates in the menus, the pause menu or the settings, when the game's window is
  not in the foreground, or with the keyboard. **F12 > Controls > Vibration**: Off, Low,
  Medium (default) or High, applied at once (`controls.vibration` in `settings.ini`).
* **F12** or the controller's **Back / View** button: settings (the game pauses).
* **Alt+Enter**: window ↔ fullscreen (borderless, or exclusive if chosen in the settings).

## Settings (F12)

**F12** or the controller's **Back / View** button opens the settings over the paused game:
six tabs, each one list in sections with all its options in view.

| Tab | Sections and options |
|---|---|
| Display | *Screen*: window, borderless fullscreen or exclusive fullscreen; under it, the resolution (see "Display"). *Aspect ratio*: Auto, 16:9, 21:9, 32:9 or the original 4:3; under it, widescreen menus |
| Graphics | *Picture quality*: render scale (50-200 %, see "Display"), texture filtering (sharp / smooth / as the game asks); *Advanced*: hidden surfaces (depth buffer or the game's polygon sorting), precise geometry. *Frame rate & VSync*: race frame rate (the game's own or interpolated) with, under it, the frame rate limit; VSync; frame rate display. *World & detail*: detail (maximum or the game's distance levels), world drawn (extended or the game's own lists). *Renderer*: the Claude Renderer (default) or the original Glide drawing; under it, camera collision |
| Audio | *Volume*: master, music and sound effects. *Mixing*: sound-effect mixing (original 11 kHz, as in 1997, or full rate) |
| Controls | *Keyboard & controller*: input mode, controller menu buttons, vibration (off, low, medium, high). *Controller dead zones*: steering, trigger; *Advanced*: remove the game's own dead zone. *Controller response*: steering sensitivity, steering and pedal response curves |
| Gameplay | *Game*: unlock all content (all tracks, championships and the bonus car; your saved progress is not changed); race conditions (see "Race conditions"). *Driving*: autopilot; *Advanced*: simulation rate (33, 50 or 25 steps per second) |
| CRT | *Shader*: CRT emulation on / off, library, shader, search, favourite, random shader. *Picture*: pixel size, aspect correction (and a custom ratio), scale, flip, orientation; *Keys, snapshot & profiles*: hold to compare, take snapshot, profile, save / load profile. *Parameters*: reset all, then every parameter of the shader, a page at a time (see "CRT emulation") |

* **↑ ↓** (D-pad) select, **← →** (D-pad) change the value (**Enter** / **A**: next value),
  **PgUp / PgDn** (**LB / RB**, or Tab) change the tab, **Esc** (**B**), F12 or Back close.
* Rarely changed options are in groups marked **+ ADVANCED**, closed at first: select the
  group and press Enter / A or → to open it (← closes it). Groups stay as you leave them
  until the program closes.
* An option indented under another depends on it, and is dimmed while it has no effect
  (the frame rate limit with the game's own race frame rate, widescreen menus in 4:3).
* The yellow line at the bottom describes the selected option; **SAVED** appears at the top
  right after a change.

Changes are applied at once (the renderer from the next race, the input mode from the next
start) and saved to `data\settings.ini`, which can also be edited by hand.

## Renderer

**Graphics > Renderer** chooses who draws the 3D world:

* **Claude Renderer** (the default): a new renderer draws the track, the scenery, the cars,
  their shadows, dust and tyre marks from the game's own data, as the game describes them
  each frame: positions and camera in floating point (nothing wobbles or jitters close to
  the camera), every object at its highest detail, a depth buffer for what is in front,
  the scenery the game shows from any part of the track in view (less pop-in), the water's
  reflections, and the chase cameras kept in front of walls (*Camera collision*, under it).
  The look stays the original's: the same models, textures, colours and cameras.
* **Original (Glide)**: the game draws as in 1997, through its 3dfx Glide code, rendered by
  Direct3D at your resolution.

The switch applies from the next race.

## Race conditions

**Gameplay > Race conditions**: *Game's choice* (as in 1997: night and fog in some
championship stages), or **Day**, **Night** or **Fog** on every race. The change applies at
once, also in the middle of a race: the sky, the lights, the fog and the cars' headlights
switch as if the race had been loaded that way. Stadium has no night (the game has no night
pictures for it): there, Night keeps the day, Fog works.

## Display

* **Resolution** (Display, under Window) depends on the window mode:
  * *Windowed*: the window's size, from common 16:9, 16:10, 21:9 and 4:3 sizes that fit
    your monitor, plus the current size if you gave the window another one by dragging its
    edges. The window takes the new size at once (`width` / `height` in `settings.ini`).
  * *Exclusive fullscreen*: *Desktop* (the desktop's display mode, the default) or any
    display mode the monitor offers, resolution and refresh rate. In exclusive fullscreen the
    display switches a second after you stop changing it (so you can step through the list);
    otherwise the mode is used the next time exclusive fullscreen starts
    (`exclusive_resolution`, e.g. `2560x1440@144`).
  * *Borderless fullscreen*: always the monitor's own resolution (shown, not selectable);
    use the render scale to draw fewer or more pixels.
* **Aspect** (`aspect`): *Auto* fills the window, and wider windows show more of the scene
  to the sides (as before; earlier settings files with `aspect = wide` mean Auto). *16:9*,
  *21:9* (2.37:1, as 2560 x 1080 ultrawide monitors), *32:9* and *4:3* give the picture
  exactly that shape: black bars at the sides or above and below where the window's shape
  differs, the 3D view as wide as the preset (a 21:9 picture on a 16:9 monitor shows what a
  21:9 monitor would), the HUD at the picture's edges, the rear-view mirror and the menus as
  on a screen of that shape. Both renderers.
* **Render scale** (Graphics, `render_scale`): the 3D scene is drawn at 50 to 200 % of the
  picture's resolution; the item shows the resulting pixels (e.g. *100% (3840 x 2160)*).
  Below 100 % is faster and softer (smoothly scaled up); above 100 % smooths edges (each
  screen pixel is the average of the scene pixels it covers). The HUD, the menus' pictures
  and text and the settings overlay always stay at full resolution and sharp. Both renderers.

## CRT emulation

The CRT tab turns the picture into that of a cathode-ray tube monitor or TV with the
RetroArch CRT shaders that ShaderGlass (by mausimus) brings to Windows: **108 presets**
(crt-royale, crt-geom, crt-guest-advanced, crt-lottes, newpixie and many more: scanlines,
shadow and slot masks, aperture grilles, curvature, glow). It is a port of ShaderGlass into
the game: the shaders work on the game's own frame, before the settings are drawn over it.

* The shader library is the file `crt-shaders.pak` next to the program (in the release
  package). Without it the tab says so.
* **CRT emulation** on: the game draws its whole frame (3D, HUD, the menus' pictures) at
  the shader's input size, by default the game's own **480 lines** (640 x 480 in 4:3,
  wider for widescreen: 853 x 480 in 16:9, 1138 x 480 in 21:9), and the shader makes the
  screen-sized picture from it. Off, the game is drawn exactly as without this feature.
* **Library**: all CRT shaders, your **personal favourites**, ShaderGlass's **community
  favourites**, **recent** ones (the shader in use when you close the settings) or the
  **search results**.
  **Shader** steps through the library with ← → (ShaderGlass's *Next*); a choice applies
  when you stop on it. **Search**: type part of a name (letters, digits, `-`, `.`, `/`,
  space; Backspace deletes, Delete clears; with a controller ← → change the last letter and
  A adds one); the matches become the library's search results. **Favorite** adds or
  removes the shader. **Random shader** picks one from the library shown.
* **Pixel size**: 480 lines (the original), 240, 360, 600 or 720 lines, or ShaderGlass's
  sizes (*x1* = the screen's resolution, *x2* = half of it, ... *x10.8*). Many shaders are
  made for 240 or 480 lines; the render scale (Graphics) does not apply while CRT emulation
  is on.
* **Aspect correction**: pixels taller than wide (*x1.2* DOS and NTSC), wider (*x0.94* PAL,
  *x0.8* NES, *x0.86* SNES, *x0.5*), *x2.0*, a *Custom* ratio (the height of a pixel
  relative to its width), or *Stretch* (the whole window). **Scale**: *Free* (the largest
  picture that fits, the default) or a percentage of the game's picture, centred.
  **Flip**: horizontal, vertical or both. **Orientation** *Vertical*: the shader works on
  the picture turned a quarter (scanlines run down the screen, as on a monitor on its side).
* **Hold to compare** (F11 by default; Scroll Lock, Backspace or \` instead, or off): while
  the key is held the game is shown without the shader (its 480 lines scaled up plainly).
  While CRT emulation is on the key is not passed to the game (Backspace is then not
  available for typing names).
* **Take snapshot**: saves the shader's picture (without the settings) as PNG in
  `data\snapshots\`. **Profile** 1-8 with **Save profile** / **Load profile**: a whole CRT
  setup (shader, picture options, the shader's parameters) in `data\crt-profiles\`.
* **Parameters**: every parameter of the shader with its range and step, ← → to change it,
  Enter / A back to its default; **Reset all to defaults** above them. Long lists are shown
  a page at a time: ↑ ↓ move through them (the page follows), and ← → on the
  *Parameters* line turn the pages. Changed values are kept per shader (`[crt.params]` in
  `settings.ini`).
* Everything is saved in `settings.ini` (`[crt]`, `[crt.params]`). Light shaders cost
  little; the heaviest (Mega Bezel, crt-royale with many passes) need a capable GPU at
  high resolutions.

## What is different from the original

* Rendering at the window's full resolution (up to 4K and beyond), any window size, crisp
  texels by default, perspective-correct texturing as on the 3dfx card.
* Widescreen: wider windows show more of the race to the sides (the HUD moves to the
  screen edges), or any of the 16:9, 21:9 and 32:9 shapes on any monitor. Menus widen too:
  their 3D scenes fill the width; the screens with a pre-rendered picture (car, track and
  gearbox selection) keep it undistorted in the middle with black sides.
* Any window size or display mode, and a render scale for the 3D scene with the HUD sharp.
* Correct overlaps: a depth buffer decides what is in front, where the 1997 game sorted
  polygons and sometimes got it wrong (scenery showing through walls, a car's spoiler).
* Full detail at any distance: the game simplified far cars and track sections, and the
  track showed cracks where simplified and detailed sections met.
* Less pop-up scenery: the game drew only what hand-made lists allowed from where you are;
  now everything it shows from any part of the track in view is drawn (Graphics > World
  drawn), as far as the 3D view reaches.
* Exact geometry: no wobbling polygons or seams at high resolutions, and no jittering
  textures close to the camera.
* Car windows, stickers and number plates drawn as in the original, with the car's parts
  overlapping correctly; shadows, dust and tyre marks no longer cut by bumps in the road.
* A longer draw distance: three times the original's.
* Day, night or fog on any race, switched live (see "Race conditions").
* Controller support with analog steering and pedals, and vibration (the 1997 game had none).
* Smooth races at any frame rate: the game simulates 33 steps per second (the rate it ran
  at on 1997 PCs; its car physics depends on it, and at 50 the cars slid sideways), and
  frames in between are drawn with the camera, the cars and the smoke interpolated (at
  your display's refresh rate with VSync, or as fast as the PC allows without it, with an
  optional limit). The picture is one step (30 ms) behind; Graphics > Race frame rate >
  Original shows the game's own frames instead. Gameplay > Simulation rate offers 50 and
  25 too. The menus run at 50 frames per second.
* CD music plays from the disc image; no CD drive is needed.
* All tracks, championships and the bonus car can be unlocked without touching your saved
  progress (Gameplay > Unlock all content); an autopilot (Tab) lets the game's AI drive.

## Troubleshooting

* `logs\runtime.log` next to the program describes what happened.
* "is a version this program does not know": the 3dfx executable of the Italian and
  English CDs (the same file) and the installed DOS game's (also its version without the
  CD) are supported so far.
* No music: the music is on the CD's audio tracks; an `.iso` image and the installed DOS
  game have none. An `.mds` / `.mdf` or `.cue` / `.bin` image has it.
* To pick another copy of the game: clear `path` under `[game]` in `data\settings.ini`; the
  first-run window asks again.
* No sound: check the Windows default output device; the game keeps running silently
  without one.

## Command line (advanced)

`ScreamerRally.exe [--disc IMAGE-OR-FOLDER] [--data FOLDER] [--fullscreen] [--aspect auto|16:9|21:9|32:9|4:3]
[--filter nearest|bilinear|game] [--input auto|keyboard|controller]` — `--disc` runs that game
for this start only (not remembered). See `README.md` in the source repository for the
diagnostic options.
