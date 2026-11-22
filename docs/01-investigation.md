# 01 — Investigation: what Screamer Rally actually is

Everything below was checked directly against the disc image in this folder
(`Screamer Rally.mdf/.mds`) with one-off Python scripts (not kept in the repository). Statements marked
*(inferred)* are reasoned from code/strings but not yet verified by execution.

## 1. The disc

| Item | Value |
|---|---|
| Image format | Alcohol 120% MDF/MDS, raw 2352-byte sectors |
| Track 1 | Mode 1 data, 115 990 sectors, ISO 9660, volume label `SRALLY` |
| Tracks 2–10 | 9 × CD-DA (Red Book) audio, 2:59 – 5:50 each — **the in-game music** |
| Language | Italian (Milestone installer texts, `PREMI ENTER PER GIOCARE`) |
| Extra content | `VIVA\` = a 1999 *VIVA Football* Win32 demo (unrelated, ignored) |
| Data-track SHA-256 (2048-byte user data of all sectors) | `d7a4c28c56c861164647385084ee532052aee99002aeec5808dab424c8edb472` |

The data track was read with a small ISO 9660 reader, the track layout from the MDS
descriptor (both now in `crates/sr-disc`, `srtool ls` / `srtool disc`).

### Game file layout on the CD

| Path | Purpose |
|---|---|
| `START0\*.EXE` | The game executables (installed by a *normal* install) |
| `START1\*.EXE` | Same builds for a *network* install (data on a server) — not needed |
| `DRV\ST.EXE` | Setup program (sound card / controls / video), writes `SETUP.CFG`, `CONTROLS.CFG` |
| `DRV\DOS4GW.EXE` | Rational Systems DOS/4GW extender (© 1987–1993 build) |
| `DRV\HMI*.386`, `CONFIG\HMI*.386` | HMI SOS sound drivers (digital + MIDI) |
| `HOTCGL\VOODOO.OVL`, `VRUSH.OVL` | 3dfx Glide 2.x DOS DLLs (Voodoo Graphics / Voodoo Rush), copied to `glide2x.ovl` at setup |
| `HOTCGL\VRIT*.CGL` | Creative CGL drivers for the Rendition Vérité 3D Blaster |
| `S2\` | A patch for *Screamer 2* (different game) — ignored |
| `DATA\*.DAT` | Game tables, sound bank (`SOUND.DAT`, compressed archive) |
| `GFX\*1.DAT` / `GFX\*2.DAT` | Packed, compressed archives: `…1` = 256-colour assets, `…2` = 65K-colour / 3D-card assets |
| `GFX\MAP11..72.DAT` | 7 tracks × 2 colour depths (≈ 4–11 MB each) |
| `GFX\CARS1xx.DAT` | Cars |
| `TABS\*.TAB` | Precomputed tables (perspective, visibility) |

Assets are in custom packed and compressed containers ("main file" with an internal
directory, `.BKK` images, `.MCM`, `.MLB` materials, `.PTH` splines…). **We do not need
to understand them to run the game**: the original code loads and decodes them.

## 2. The executables

All eight game builds are **Linear Executables (LE)** built with **Watcom C/C++32
(1995)** for the **DOS/4GW** flat-model extender, CPU type 80386.

| File | Renderer | Notes |
|---|---|---|
| `STARTL.EXE` / `STARTH.EXE` | Software, 256 colours | low / high resolution |
| `START65L.EXE` / `START65H.EXE` | Software, 65K colours | low / high resolution |
| `STRT3FX.EXE` | **3dfx Glide 2.x** | loads `glide2x.ovl` as a DOS/4GW DLL |
| `STRT3DBL.EXE` / `STRT3DBH.EXE` | Creative CGL (Rendition Vérité) | |
| `STRTMSI.EXE` | Matrox MSI (Mystique) | |

MD5 of the supported files (this disc):

```
f383fadb8b606c40bd14dcf4e36540ac  START0/STRT3FX.EXE   (primary target)
51337c3c6b130ca571ca3b1702ef435a  START0/STARTH.EXE    (software reference)
0387cd200efb944bf02d5e29d3359976  START0/START65H.EXE
47963826e2f4b60549660763d2e5eb91  DRV/ST.EXE
```

### `STRT3FX.EXE` memory layout (preferred bases; the image is relocatable)

| Obj | Base | Size | Kind |
|---|---|---|---|
| 1 | 0x00010000 | 0x10 | 32-bit code stub |
| 2 | 0x00020000 | 0xCEDC0 | **32-bit code — game + Watcom CRT + HMI SOS + DLL loader** (≈ 830 KB) |
| 3 | 0x000F0000 | 0x9AC | 16-bit code (real-mode / callback helpers) |
| 4 | 0x00100000 | 0x48 | 16:16 alias code |
| 5 | 0x00110000 | 0x82 | data (BSS) |
| 6 | 0x00120000 | 0x77B10 | **data + stack** (auto data segment) |

Entry point `0x000BF194` (Watcom startup). 15 913 32-bit offset fixups, 15 selector
fixups, no imports. Fixups mark every absolute address in the code, which lets us
tell code pointers and jump tables apart from instructions.

## 3. Subsystems found in the code

### Graphics (3dfx build)
* Glide is not linked in. The game carries an import list of **100 Glide 2.x functions**
  (`_grDrawTriangle@12`, `_grTexDownloadMipMap@16`, `_grLfbLock@24`, …) and a table of
  5-byte lazy-binding thunks at `0xEE92C` (names at `0xEEBB8`).
* On first call a thunk enters `0xDFD90`, which loads `glide2x` through the DOS/4GW
  Professional DLL loader (`LINEXE_LOADMODULE` / `LINEXE_GETPROCADDR`, code around
  `0xE7xxx`) and patches itself.
* **Consequence:** every pixel the 3dfx build draws goes through a documented API whose
  vertices are floating point screen coordinates with `1/w`, `s/w`, `t/w`. That is the
  ideal place to put a modern renderer (higher resolution, perspective-correct, filter
  control) without touching game code.
* 2D screens (menus, HUD) are presumably written to the Voodoo frame buffer through
  `grLfbLock` / `grLfbWriteRegion` *(inferred)*.

### Timing
* The game registers a **100 Hz** callback with the HMI SOS timer service
  (`0x36F64`: `sosTIMERRegisterEvent(100, 0x5042A, …)` *(inferred from arguments)*).
* The callback (`0x5042A`) only increments two counters (`[0x50512]`, `[0x50516]`).
* Frame pacing busy-waits on the counter (`0x504DC`: `while (ticks < 1);`).
* `0x50413` programs PIT channel 0 directly (used to restore 18.2 Hz on exit).
* **Consequence:** game time is a clean 100 Hz tick, which can be generated
  deterministically by the host. This is the foundation for decoupling rendering from
  simulation.

### Input
* Own **keyboard ISR** (`0x502FC`, IRQ1 via `int 21h AX=2509h`): reads port `0x60`,
  keeps a 128-entry key-state table, sends EOI. It derives DS as `CS+8`, which happens to
  hold on Win32 (`0x23 + 8 = 0x2B`, verified).
* **Analog gameport joystick** support: `0x50474` reads buttons from port `0x201`,
  `0x504B0` times the axis one-shots. **The original game already supports analog
  steering**, so a modern stick can drive the original input path with real analog
  values instead of key presses.
* Clears the keyboard LED flags through the BIOS data area selector (`gs:[0x17]`).

### Sound and music
* **HMI SOS** library (digital: `HMI-DIGIPATCH102395`, MIDI: `HMI-MIDISONG061595`), with
  drivers loaded at runtime from `HMIDRV.386` / `HMIMDRV.386` and FM banks
  `melodic.bnk` / `drum.bnk`. It uses DPMI, Virtual DMA Services (`int 4Bh`) and the
  Windows-presence check (`int 2Fh AX=1600h`).
* **CD audio**: MSCDEX installation check at `0x505A0` (`int 2Fh AX=1500h`); config keys
  `CD_TRACKS`. Music = disc tracks 2–10.

### Operating-system interface
Software-interrupt sites in `STRT3FX.EXE` (found by a scan of the code for `int` instructions):

| Interrupt | Sites | Used by |
|---|---|---|
| `int 21h` (DOS) | 87 | Watcom CRT file/memory/time, vector hooks |
| `int 31h` (DPMI) | 73 | CRT, HMI SOS, DLL loader |
| `int 2Fh` | 7 | MSCDEX, Windows/DPMI detection |
| `int 10h/16h/1Ah/33h/4Bh` | 1–3 each | BIOS video/keyboard/clock, mouse, VDS |

Direct port I/O: keyboard (`0x60`), PIC EOI (`0x20/0xA0`), PIT (`0x40/0x43`), gameport
(`0x201`), plus sound-card / DMA ports inside the HMI code. About 400 segment-register
loads exist, almost all inside the CRT, HMI and DLL-loader libraries.

## 4. Feasibility probe (run on this PC, Windows 11 23H2)

A 32-bit test program (`scratchpad/feas`) checked the assumptions behind running the
original machine code natively:

| Check | Result |
|---|---|
| `int 21h`, `in`, `out`, `cli`, `sti` in user mode trap into a vectored exception handler and can be emulated and resumed | ✅ |
| `iretd` (used by the game's ISRs) runs natively at ring 3 | ✅ |
| Win32 `CS + 8 == DS` (keyboard ISR trick) | ✅ (`0x23`/`0x2B`) |
| Loading a DOS/4GW selector such as `0x40` traps | ✅ |
| Cost of a trapped instruction | ≈ 5 µs (fine for rare calls; hot sites get patched) |
| Conventional memory (`0xA0000`…) free at its natural address | ❌ Windows maps NLS files and stacks there |
| WOW64 keeps a guest-chosen null GS after an exception | ❌ forced back to `0x2B` |

The last two rows set design constraints (see `02-architecture.md` §4).

## 5. What is needed from the user

Only the original disc (an MDF/MDS, CUE/BIN, ISO + audio, or an installed copy with the
CD audio ripped). From it the port uses `STRT3FX.EXE`, the `DATA`, `GFX`, `TABS`,
`CONFIG` folders, and the audio tracks. No other files are needed — in particular **no
DOS/4GW, Glide or HMI drivers are used**: those are re-implemented.
