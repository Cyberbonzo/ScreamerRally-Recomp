# The English version (wave 1, topic F)

What the English CD is, how it differs from the Italian one, and what switching the port's
language would take. Every statement says how it was checked; what was not checked is listed
under "Unknowns".

## Summary

* The English 3dfx build is **byte-identical** to the Italian one: `START0\STRT3FX.EXE`
  SHA-256 `9eac6e12…5d1` on both discs, and so are all 16 game executables in `START0` and
  `START1`. The port's profile (found by that hash) and every patched link address apply
  unchanged: nothing moves.
* The language lives in **data**, in three files that the 3dfx build reads:
  `DATA\LOCAL.DAT` (all text), `GFX\GRAF2.DAT` (menu headings and buttons, loading screens)
  and `GFX\GAME2.DAT` (the speedometer face). Sound, music, fonts, tracks and cars are
  identical. The speed unit follows the text: English `GAME.TXT` says `MPH`, and the
  executable then shows speed x 0.625.
* The port runs the English image today without changes (`disc = ...\srally.cue`), and the
  Italian image with those three English files in the data folder's `C\SR` also plays in
  English (both verified by running, below).
* **Recommendation:** switch language by choosing which disc image is mounted (an optional
  second image and a `language` setting); about half a day to a day of work. Details under
  "Recommendation".
* Found on the way: `sr-disc` read every CD-audio track of the Italian MDS image 2 seconds
  late (the MDF leaves out track 2's pregap). Fixed in this branch, with tests.

## Sources

| | Italian | English |
|---|---|---|
| Image | `Screamer Rally.mds` + `.mdf` (Alcohol 120%) | `srally.cue` + `.img` + `.ccd` (CloneCD; `.sub` not extracted) |
| Size | 695 785 104 bytes = 295 827 raw sectors | 560 650 944 bytes = 238 372 raw sectors |
| Volume | `SRALLY`, created 1999-01-28, CeQuadrat formatter | `SRALLY`, no dates, preparer `HAY` |
| Extra | `VIVA\` (a 1999 VIVA Football demo, 716 files) | none |

* The English files come from the user's archive `Screamer-Rally_DOS_EN_ISO-Version.zip`
  (entries `Screamer_Rally_ISO/srally.*`), extracted with .NET `ZipFileExtensions.ExtractToFile` into `work\disc-en\` (the earlier,
  truncated extraction was deleted first). Checked: every extracted file's CRC-32 equals the
  zip's (Python `zlib.crc32`); `srally.img` is 560 650 944 bytes, SHA-256
  `d27bc54aee4627087fb06b631a80f96460b0f42ea04e1831423a3b9b340488d1`. The size is exactly
  238 372 x 2352, the lead-out (`Point=0xa2`, `PLBA=238372`) of `srally.ccd`.
* `srally.sub` (subchannel data) is not needed: the port answers the game's Q-channel
  requests from the table of contents, and the runs below used no `.sub`.
* Volume fields: sector 16 read with `srtool raw` (added in this branch), fields at their
  ISO 9660 offsets. The Italian disc is a 1999 re-issue (the demo's files are dated 1999);
  the files identical on both discs carry the same 1997 dates (`srtool ls`). The language
  files are dated 1997-10-19 / 20 on the English disc and 1997-10-22 on the Italian one, and
  the text files' comments are in English (`* not to be translated`): the Italian data looks
  like a translation of the English (inferred from dates and comments).
* Italian image: `Screamer Rally.mdf` holds 295 827 raw sectors; the disc itself has
  295 977 (see the MDF pregap below).

### `srtool` reads the CUE / IMG pair

Yes, unchanged: `srtool disc|ls|extract` work on `work\disc-en\srally.cue` (sheet with
`TRACK 1`, `INDEX 1` without leading zeros and an `.img` file name; the parser already
accepted both). A test with that syntax was added (`cue::tests::clonecd_style_sheet`).
`srtool extract` wrote 155 entries (167 602 211 bytes) to `work\disc-en\files\`.

## Track layout

From `srtool disc` (Italian after the fix below; the English sheet agrees with the `.ccd`):

| Track | Italian LBA | length | English LBA | length |
|---|---|---|---|---|
| 1 data | 0 | 116 140 | 0 | 82 316 |
| 2 audio | 116 140 | 20 096 (4:27.95) | 82 316 | 19 944 (4:25.92) |
| 3 | 136 236 | 18 385 | 102 260 | 18 233 |
| 4 | 154 621 | 19 285 | 120 493 | 19 133 |
| 5 | 173 906 | 19 096 | 139 626 | 18 944 |
| 6 | 193 002 | 13 434 | 158 570 | 13 282 |
| 7 | 206 436 | 19 689 | 171 852 | 19 537 |
| 8 | 226 125 | 21 054 | 191 389 | 20 902 |
| 9 | 247 179 | 26 233 (5:49.77) | 212 291 | 26 081 (5:47.75) |
| 10 | 273 412 | 22 565 (5:00.87) | – | – |

* **Same music.** For each English track, a probe 30 s in was searched in the Italian image
  and then the whole English track compared at that alignment: tracks 2-9 are identical
  byte for byte over their full length (Python over the raw images). The alignments differ
  per track by at most 226 samples (5 ms): mastering, not audible.
* Each Italian track ends with 152 sectors (2 s) of digital silence that the English disc
  does not have (peak 0 over the whole gap), hence the 152-sector difference per track.
* The Italian disc has a **10th track** (5:00) that the English disc lacks and that is not
  on it anywhere (probe search). The game does not use it for races: the race music table
  `CD_TRACKS` in `DATA\GAME.DAT` (`MISC.DTR`, identical on both discs; loaded at 36ec4 into
  13149c, read at 3795f) lists tracks 3, 7, 5, 8, 2, 4, 9 for the seven courses; the attract
  mode plays 2 (424f7) and the menus 3 (4325c, 489ce). The pause menu's CD player steps
  between the lowest and highest track of the disc's TOC ([51076] / [5107a], set at 50c97
  from the audio-disc-info request), so it offers track 10 only with the Italian disc.

### The Italian MDF leaves out track 2's pregap (fixed in `sr-disc`)

`srtool disc` shows per track the LBA and the start offset in the MDF:
track 2 is at LBA 116 140 but at file sector 115 990, and every later track too sits 150
sectors before its LBA; track 10's MDS length (22 565) runs 150 sectors past the end of the
file. The MDF stores track 1 from LBA 0, leaves out the 150-sector pregap of track 2 and
then continues, so from track 2 on file sector = LBA - 150. `sr-disc` read the MDF as file
sector = LBA, so every audio track started 2 s into its music and ended with the next
track's first 2 s; track 10 was cut by 2 s (Italian disc: 295 827 sectors instead of
295 977).

Evidence: the peak level per half second of the first 3 s of each Italian track read at
file sector = LBA equals the levels read at the MDS start offset shifted by 2 s, and the
start-offset reading begins like the English track (silence, then the music), e.g. track 5
`[0, 0, 17680, 18852, ...]` at the start offset and on the English disc, `[19922, 17065,
...]` at file = LBA.

Fix (`crates/sr-disc/src/mds.rs`): the descriptor's 64-bit start offset (track block +28h)
is read and each track becomes its own segment, from its start offset to the next track's
(the last to the end of the file); sectors no segment covers (the left-out pregap) read as
silence, as for CUE pregaps. Tests: `contiguous_image`, `pregap_left_out`,
`overlapping_offsets_rejected`. Verified on the real images: `srtool disc` now gives 295 977
sectors and track 10 22 565 sectors (the MDS's own length); the first 400 sectors of tracks
2-9 read through the `Disc` API (`srtool raw`) contain the English track's audio at
-0.38 .. +0.37 sectors from the start (it was 150 sectors before the fix). The data track
and the TOC the game sees are unchanged (same LBAs). This changes the port's music for every
MDS user (it now starts where the CD's track starts), which is why it is a separate commit.

## Files

`srtool extract` of both images, SHA-256 of every file (Python `hashlib`):

| | count |
|---|---|
| Italian files | 859 (716 of them `VIVA\`) |
| English files | 143 |
| identical | 133 |
| different | 10 |
| only on the Italian disc | 716, all `VIVA\` |
| only on the English disc | 0 |

The ten that differ, with what the 3dfx build does with them (file opens logged with
`--log debug` in a run through the menus and a race on Rally China, both languages):

| File | Italian / English bytes | What | Read by `STRT3FX.EXE` |
|---|---|---|---|
| `DATA\LOCAL.DAT` | 3 064 / 2 948 | all game text | yes |
| `GFX\GRAF2.DAT` | 5 125 559 / 5 114 024 | menu sprites, loading screens | yes |
| `GFX\GAME2.DAT` | 674 276 / 674 211 | race sprites (65K colours) | yes |
| `GFX\GAME1.DAT` | 384 673 / 384 600 | race sprites (256 colours) | no (software builds) |
| `GFX\MENU11.DAT` | 498 046 / 498 066 | menu data (256 colours) | no (`MENU12.DAT` is read; identical) |
| `SRAUTO.EXE` | 349 696 both | CD autorun program | no (not examined further) |
| `CONFIG\SETUP.DAT` | 1 606 692 / 1 606 985 | installer data | no |
| `DRV\SETUP.BAT` | 895 / 861 | installer batch | no |
| `FILES\INSTALL.TXT` | 3 632 / 3 376 | installer text | no |
| `S2\S2PATCH.BAT` | 2 678 / 2 561 | Screamer 2 patch (other game) | no |

Everything else the run opened is identical on both discs: `DATA\GAME.DAT`, `GEN.DAT`,
`MAP3.DAT`, `SOUND.DAT` (all sound effects: there is no language-specific speech),
`GFX\MAP32.DAT`, `CARS102.DAT`, `ENVP302.DAT`, `ENVM12.DAT`, `LOGO2.DAT`, `MENU12.DAT`,
`TABS\VIS.TAB`; so are all other course, car and table files, `CONFIG\DEFCTRL.CFG`,
`DRV\ST.EXE` (the setup program) and the Glide overlays.

## Executables

| File | SHA-256 (both discs) |
|---|---|
| `START0\STRT3FX.EXE` | `9eac6e12ab309389bdda019cfa695bfbbc791fbda1a4634464a444fce35165d1` |
| `START1\STRT3FX.EXE` | `2f07ebd95c2090efae6ee697b9932e18eb180a6237cda2ea4e1e405da186274f` |

All 16 executables in `START0` and `START1` (software, 65K, 3dfx, CGL, MSI builds) have the
same hash on both discs (`cmp` agrees for `START0\STRT3FX.EXE`). There is no separate
English build: no LE or disassembly comparison is needed, the profile in
`crates/sr-runtime/src/profile/` already matches the English disc's executable, and none of
the patched addresses moves. (The profile's name says "Italian CD"; it is the same file on
the English CD.)

Italian text inside the executable: `PREMI ENTER PER GIOCARE` at 1216b8 is used only at
484e2, inside a function (48418) that nothing calls and no fixup points to (searched: `call`
/ `jmp` in `work\game_dis.txt`, and every fixup target of the LE) — dead code, never shown.
The other Italian strings are error messages (`errore !! %s`, an out-of-memory message),
the same in both versions.

## Where the language lives

### The `.DAT` archives

Read by `52ec9` / `52f21` (open, find entry) and `c9c40` / `c9cac` (decompress):

* The **last dword** of the file is the directory size; the directory lies just before it,
  32-byte records: name (16 bytes, each XOR 3Ah, NUL-terminated after the XOR), data offset,
  stored size, size, flags. An empty name ends the list.
* Flag 2: the entry is **LZW** compressed: codes packed least significant bit first, 9 bits
  growing to 12, 100h = clear, 101h = end, first free code 102h, the width grows when the
  next free code reaches 2^width (`c9d30`, bit reader `c9fb0`). Flag 1 (set with flag 2 on
  the `.TXT` / `.DTR` text entries): the entry is unpacked whole into a buffer when opened
  (5304f..53083); other compressed entries are decoded as they are read (inferred from the
  code; not needed here). Flags 0: stored.

A Python unpacker following this (kept in the scratchpad, not in the repository) was run
over every compressed entry of both languages' `LOCAL.DAT`, `GAME2.DAT`, `GRAF2.DAT` and the
Italian `GAME.DAT`, `MENU12.DAT`, `LOGO2.DAT` (162 entries): each stream starts with the
clear code and decodes to exactly the directory's size; the text entries read as plain
text, the pictures render correctly (below).

### Text: `DATA\LOCAL.DAT`

Three entries: `ATTRACT.TXT` (intro credits), `STRINGS.TXT` (menus), `GAME.TXT` (race).
The loader `3ad50(name, table)` builds `%s\data\local.dat`, finds `<name>.txt`, skips lines
starting with `*` (the files' comments, e.g. `*max chars 16`, `* not to be translated`) and
stores a pointer per line in the table. Callers: `strings` (373ab, 4aa4f, table 13a524),
`game` (38278, table 136938), `attract` (430eb, table 13acf4).

* The strings are addressed **by line index**. `STRINGS.TXT` (178 strings) and `GAME.TXT`
  (42) line up one to one between the languages (side-by-side listing; the English
  `STRINGS.TXT` has two extra empty lines at the end, after the last used index).
  `ATTRACT.TXT` is a credits script read in sequence (blocks separated by `#`), 99 lines in
  Italian, 98 in English.
* Examples: `MENU PRINCIPALE` / `MAIN MENU`, `IMPOSTAZIONI` / `GAME SETTINGS`,
  `RIPRENDI` / `RESUME`, `CARICANDO...` / `LOADING...`.
* **Units follow the text.** Right after loading `game` (3827d), the code tests the first
  character of string 30 (`KMH` / `MPH`): `m` or `M` sets [136a04] = 1, and the speed shown
  in the race (4c3ce, 4c4b2) is then multiplied by the double at 121a4a = 0.625. These are
  the only references to [136a04] (two writes, two reads), so only the race display
  changes. Checked in two deterministic autopilot runs on Rally China (same frames, same
  gear): Italian 153 / 147 / 90 km/h, English 95 / 92 / 56 mph.

### Pictures with words: `GFX\GRAF2.DAT`, `GFX\GAME2.DAT`

Formats (from the unpacked entries; sizes add up exactly): `.BKK` = `VK56`, width, height,
256 x RGB565 palette, 8-bit pixels; `.SPR` = sprite count, palette count, pixel bytes, 24
bytes per sprite {?, ?, width, height, pixel offset, palette index}, the palettes, the
pixels. Rendered to PNG with Python (`zlib` PNG writer) in `work\en-compare\png\`.

* `GRAF2.DAT` (all builds read this one file): `MENUH.SPR` / `MENUL.SPR` (high / low
  resolution): 38 of 138 sprites differ: screen headings (`Selezione Principale` /
  `Main Selection`, `Inserimento Nome` / `Enter Your Name`, ...), buttons (`Salva` / `Save`),
  the car plates (`180 Kmh` / `112 Mph`) and the track panels (`Terreno` / `Terrain`); the
  English sprites have their own widths. `LOAD_1H` .. `LOAD_7L.BKK`: the loading screen of
  each course (`Caricando...` / `Loading...`). The other 34 entries (backgrounds, fonts
  `MENUH.FNT` / `MENUL.FNT`) are identical.
* `GAME2.DAT`: `GAMEH.SPR` / `GAMEL.SPR`: one sprite, the speedometer face (`Km/h` /
  `MPH`).
* `GAME1.DAT` / `MENU11.DAT`: the same for the 256-colour builds (`MENU11.DAT`: one entry,
  `map1.unr`); not read by the 3dfx build.

### Sound

`DATA\SOUND.DAT` is identical; the CD music is identical (above). Nothing to switch.

## Runs through the port

All with this branch's build (runs 1 and 2 before the `sr-disc` commit, 3 and 4 after it;
that change only affects the Italian image's CD audio), `SR_DETERMINISTIC=1`, keys
`12:ENTER,16:ENTER,20:ENTER,24:ENTER,28:ENTER,32:ENTER` (menus, then a race on Rally
China; runs 1, 2 and 4 also press Esc for the pause menu), screenshots with `SR_SHOT`;
data folders copied from `work\agents\template-data` into `work\agents\`.

1. **English image, unchanged port** (`english-data`, `disc = ...\work\disc-en\srally.cue`):
   the runtime identifies the executable by its hash, the game passes its CD check (sector
   16), plays the English disc's track 2 in attract mode (`CD audio: play LBA 82316`) and
   shows English everywhere: `Enter Your Name`, `Main Selection`, `Track Selection`,
   `Car Selection` (`112 Mph`), `Select Transmission`, the race HUD (`MPH`), the pause menu
   `RESUME / RESTART / CD PLAYER / EXIT`.
2. **Italian image, same script** (`english-itref-data`): the same screens at the same
   frame numbers in Italian (`Inserimento Nome`, `Selezione Principale`, `180 Kmh`,
   `RIPRENDI / RICOMINCIA / LETTORE CD / ESCI`).
3. **Autopilot runs** (`SR_AUTOPILOT=1`) of 1 and 2: the speed readings above.
4. **Italian image + three English files** (`english-overlay-data`: `DATA\LOCAL.DAT`,
   `GFX\GRAF2.DAT`, `GFX\GAME2.DAT` from the English disc copied into the data folder's
   `C\SR`, which the port's file system serves before the CD, `vfs.rs` `lookup`), with the
   autopilot: every screen is English, as in run 1 (`Enter Your Name` ... `Select
   Transmission`, `112 Mph`, the pause menu), and the race shows 95 MPH at frame 1750 like
   the English image (Italian: 153 km/h). So these three files are all the 3dfx build needs.

Screenshots and logs: `work\en-compare\` (`contact-en*.png`, `contact-it.png`,
`contact-overlay.png`, `png\speedo2.png`, `png\speedo-overlay.png`, `runtime-*.log`).

## Recommendation

The executable is the same, so a language is a choice of data, not of code. Two ways, both
needing the user's English image (the Italian disc holds no English data):

* **A. Mount the chosen image (recommended).** Settings: the existing `disc` plus an
  optional `disc_english` (or a list of images) and `language = italian | english`; the
  runtime mounts the image of the chosen language as drive D: and the `C:\SR` overlay, as
  today. No file list to maintain, music and data come from one consistent disc, every
  runtime module works as is (verified by run 1). Work: settings keys and migration, the
  first-run dialog (ask for the second image when English is chosen, check that its
  `START0\STRT3FX.EXE` matches the profile), a language item in the F12 overlay that takes
  effect at the next start (the game loads its text at start-up and the pictures per
  screen), the profile's name. The saves (`CONFIG\CHOICE.DAT`, replays) are written by the
  same executable, so one data folder can serve both languages (tested across a switch
  since: "Language switching", below). About half a day to a day with testing.
* **B. Italian image plus the English language files.** Mount the Italian image and serve
  `DATA\LOCAL.DAT`, `GFX\GRAF2.DAT`, `GFX\GAME2.DAT` (for completeness also `GFX\GAME1.DAT`,
  `GFX\MENU11.DAT`) from the English image: a second `IsoFs` consulted first for those paths
  in `vfs.rs`. Run 4 checks the file list. Only worth it if the Italian disc's 10th audio
  track should stay available in the CD player; otherwise it adds a file list for no gain.
  Similar effort.

Not recommended: copying the language files into the user's data folder (it works, run 4,
but it puts game files where the port's own settings and saves live, and they would
silently win over a later disc change).

No address profile work is needed in either case.

## Language switching (implemented, approach A)

* **Settings** (`apps/screamer/src/settings.rs`): `[game] language = italian | english`
  (empty until an image is known), `disc_italian`, `disc_english`. The single `disc = ...`
  of earlier versions is still read and moved to the key of its image's language at the
  next start; `language` then takes that image's language. While that image cannot be read
  (moved, drive not connected) the key is kept and the start asks for a disc as before.
* **Identification** (`apps/screamer/src/language.rs`): `DATA\LOCAL.DAT` is unpacked with
  the game's archive format and LZW (`crates/sr-disc/src/dat.rs`, following `c9d30`;
  `srtool dat` lists and unpacks entries); its `STRINGS.TXT` and `GAME.TXT` are searched for
  eight common menu words per language as whole lines (main menu, championship, settings,
  loading, save, exit, resume, restart). At least three and more than the other language
  decide; anything else (another language, a modified text) is "unknown" and is treated as
  Italian. Line positions, sizes and hashes are not used, so a re-issue with small text
  changes is still recognised. Both real images: 8 of 8 words each, none of the other
  language (counted over the entries unpacked with `srtool dat`; the `real_images` test,
  ignored by default, checks the result with the image paths in `SR_DISC_ITALIAN` /
  `SR_DISC_ENGLISH`).
* **Start** (`apps/screamer/src/firstrun.rs`, `choose`): `--disc` for one run; otherwise
  the remembered image of `language`, else an image of that language next to the program,
  else the question and file dialog as for the first disc. Every image is identified; one of
  the other language is said so ("That is the image of the Italian CD") and remembered under
  its own key if that key has no usable image. Cancel plays the other language's image and
  sets `language` to it, so the next start does not ask again. Unit tests with a scripted
  environment cover each path.
* **F12 > Gameplay > Language**: saved at once, applies at the next start.

### Runs (this branch's build, data folders copied from `work\agents\template-data`)

1. **Italian only, old settings** (`disc = ...mds`): no question; settings rewritten with
   `language = italian`, `disc_italian`; seven deterministic frames (500..1100, name and
   main menus) byte-identical to the build before this change (PNG hashes).
2. **F12**: Language set to English: `language = english` saved, status "Saved: the
   language changes at the next start" (screenshot).
3. **English chosen, no English image**: the question names English and offers Italian on
   Cancel (text read with UI Automation); the Italian image given: "That is the image of the
   Italian CD", asked again; the English image given: English menus (`Enter Your Name`,
   `Main Selection`), `disc_english` remembered; later starts do not ask.
4. **Both images, English**: no question; menus, `112 Mph` car plate, speedometer `MPH`,
   pause menu `RESUME ... EXIT`.
5. **Back to Italian** in F12: next start Italian (`Inserimento Nome`, `180 Kmh`, `Km/h`,
   `RIPRENDI ... ESCI`), no question.
6. **Cancel** with English chosen and only the Italian image: plays Italian,
   `language = italian` saved, the next start does not ask.
7. **Old `disc =` with the English image**: moved to `disc_english`, `language = english`,
   English menus. **First start without settings**: the original question; the English image
   selected is remembered as `disc_english`, `language = english`. **`--disc`** with the
   English image and `language = italian`: English for that run, `settings.ini` unchanged
   (hash).

### Saves across languages

One data folder (`C:\SR`) serves both languages. `CONFIG\CHOICE.DAT` (the 8500h-byte block
at 13b03c, decoded as in docs/re/strt3fx.md) holds numbers and the typed player names only:
its only printable strings are the profile name `MIL` (1 577 times, 105 of them with two
printable bytes of a neighbouring value in front): no menu or setup texts that differ
between the CDs. The game writes it only after a race (one caller, 4152c). Runs in real
time, races left from the pause menu (with `SR_DETERMINISTIC=1` the keys scripted after
Esc never arrived and the run did not end: the pause menu seems to hold virtual time):

* the template's file, written by Italian runs, loads in English (the name `MIL` on
  `Enter Your Name`);
* after an English race the file differs from the template in one dword (13b0a4: 0 -> 1);
  the next Italian race, started from that file, wrote it with 13b0a4 = 2 and nothing else
  changed: the Italian game loaded the English save and carried it on.

The record tables are in the same block (their names are the profile names above); the
high-score screens were not compared on screen. Replays were not tested.

## Unknowns

* What the Italian disc's 10th audio track is (it is not used by the races, the attract
  mode or the menus; perhaps belongs to the 1999 re-issue). Not listened to.
* `SRAUTO.EXE` and the installer files were only hashed, not examined (the port does not
  use them).
* Only Rally China was raced; the other six loading screens and courses were compared as
  files (all seven `LOAD_*` pictures differ, the course data is identical), not run. The
  loading screen itself was not caught in a screenshot (it shows briefly); it was compared
  as a picture.
* The file list of approach B is the one run's (menus, one race, pause menu); another
  screen opening another language file would show up as Italian. All differing files are
  listed above, so the risk is limited to `GAME1.DAT` / `MENU11.DAT`.
* Whether the 256-colour builds' files (`GAME1.DAT`, `MENU11.DAT`) need switching is
  irrelevant to the port (3dfx only); not run.
* The first two dwords of an `.SPR` sprite record were 0 in the entries looked at; their
  meaning is unknown (not needed).

## Reproduce

```powershell
$T = "target\i686-pc-windows-msvc\release\srtool.exe"
& $T disc "D:\GitHub_Projects\MyGitHub\ScreamerRally\work\disc-en\srally.cue"
& $T extract "D:\GitHub_Projects\MyGitHub\ScreamerRally\work\disc-en\srally.cue" "D:\GitHub_Projects\MyGitHub\ScreamerRally\work\disc-en\files"
& $T hash "D:\GitHub_Projects\MyGitHub\ScreamerRally\work\disc-en\files\START0\STRT3FX.EXE"
& $T raw "D:\GitHub_Projects\MyGitHub\ScreamerRally\Screamer Rally.mds" 136236 400 it3.raw
python -I tools\research\mds_info.py "D:\GitHub_Projects\MyGitHub\ScreamerRally\Screamer Rally.mds"
```
