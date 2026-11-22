# Controller vibration: the game variables it reads

The 1997 game had no force feedback. The runtime drives the controller's two motors from the
player's car (`crates/sr-runtime/src/rumble.rs` reads the game, `crates/sr-input/src/rumble.rs`
turns it into motor levels and sends them). This note lists what is read, where it lives
(link addresses, as in `docs/re/strt3fx.md`) and how each was verified. Italian 3dfx build;
every measurement is Rally China (`MAP3`) with the default car, autopilot on.

## When

Once per race simulation step, at the end of the race frame timer's tick wait (observer
`4f793`, the same one as the autopilot and the native renderer): the previous step's physics
is complete. No step runs in the menus, the pause menu or the settings overlay (verified: a
real-time race paused with Esc had no step from 48.7 to 54.7 s, the overlay none from 60.7 to
65.7 s and 70.7 to 72.7 s), so the motor levels, posted with a time stamp, drop to 0 200 ms
after the last step. The attract mode's demo race does run race steps: it is excluded by the
mode bit below. Nothing is written to guest memory.

## Globals

| Address | Meaning | Verified |
|---|---|---|
| `1436d4` | index of player 1's car (already used by the autopilot) | `strt3fx.md` |
| `1436dc` | game mode; bit 7 = the attract mode's demo race | dump: 80h in every step of the attract race (frames 7-422), 1 in the race; the game itself skips its impact sounds and damage with this bit (`29378`, `32011`, `33ae5`) |
| `1296c8 + n * cc8h` | car records | `strt3fx.md`; the player's record's +0 holds its index (`2b29a`: `cmp edi, [esi]` with `13148c`) |
| `1469c4` | simulation step, seconds (float, smoothed) | `strt3fx.md`; 0.033 at 33 steps per second in every race step dumped |
| `12eb74 + 24h * id` | the track's road kinds (below) | `SR_PEEK` at frame 1800 against `MAP3DAT.DTR` |

## The car record

The record holds the current physics state and, 25Ch bytes further, the game's copy of the
previous step's state: in a dump of 4 251 race steps (`SR_RUMBLE_DUMP`) the copy equals the
previous step's current value in 4 239 to 4 250 steps per field (the rest: the race start).
That gives the change over one step directly.

| Offset | Meaning | Verified |
|---|---|---|
| +4Ch | flags; bit 3 = in contact with another car | `3163f`/`3166b`: the car-car collision code (`303f4`) sets it on both cars of a colliding pair; in the logged lap set in 16 steps, at the start among the other cars and at 29.8-30.6 s (speed falling, below 1 g) |
| +5Ch | top speed (float; China's car 145.76) | `2ae91` divides the speed by it |
| +60h | maximum engine speed (9000.0) | `2d86a` divides the engine speed (+C7Ch) by it, right after the engine sound's selection; the engine speed never exceeded it in the dump |
| +150h | touching a wall (0 / 1) | written by the per-car ground and wall routine `3314c` (cleared at its start, set by the wall test); 1 in all four steps of the dump with more than 8 g of horizontal velocity change (12-23 g), alternating 1 / 0 while the car scrapes along a wall |
| +500h | speed (float, the game's unit; 0-127 on China) | correlates 0.99 with the speed measured from the position; the rolling sound's volume follows it (`2a9f0`) |
| +510h | fraction of the wheels on the ground (0, 0.25 .. 1) | equals the mean of the four wheels' contact flags in all 4 251 steps dumped (156 with all four up); its previous value (+76Ch) = 0 and the current one > 0 is the game's own rule for its landing sound `GROUND1` (`2b280`..`2b2bc`) |
| +52Ch | position x, y, z (float, world units, y up) | its change per step is the velocity times the step (next row) |
| +55Ch | velocity x, y, z (float, world units per second) | position change per step / step length = velocity to 1.000 (100 consecutive steps); in the air the vertical velocity drops 212.1 per 0.033 s step: gravity 6 427 units/s² |
| +574h + 70h k | wheel k (0-3) | the wheel loop `2ae44` (`ebx` = car + 574h, += 70h) |
| wheel +04h | on the ground (0 / 1) | its mean over the wheels is +510h (above) |
| wheel +48h | suspension travel, -30..30 | `2b0db`; +30 / -30 while the wheel hangs in the air |
| wheel +5Ch | road kind id under the wheel | indexes the road kinds at `273c0`, `29a38`, `2a5bc`, `2dd7b`; changes where the road's look changes (screenshots: 0 grey gravel at frame 1800, 2 brown dirt at frame 2400, 1 at the start) |
| wheel +60h | sliding beyond its grip (non-zero, `& 0Fh`) | `29cfe` sets it when the slip exceeds the grip, `29d07` clears it |
| +C7Ch | engine speed, rpm (float) | the HUD's rev needle: 4 997 at frame 1800 with the needle at 5 |
| +C80h | gear (0-4) | the HUD: 2 at frame 1800, 4 at frame 2400 |

## Road kinds

The track's data file (`DATA\MAPn.DAT`, entry `MAPnDAT.DTR`, section `ROAD KINDS`) lists
for each road kind id: grip with slick, rain and snow tyres, **"sconn."** (sconnessione,
unevenness), smoke, smoke dispersion, rolling sound, skid sound. The game keeps them at
`12eb74 + 24h * id`:

| Offset | Column | Used by the game at |
|---|---|---|
| +00h, +04h, +08h | grip (float) per tyre type (car +50h) | `273c0`, `29a5d`, `29c29`, `2a5f0` |
| +0Ch | unevenness ("sconn.") | `2b1f2`: selects the mask (`122e4c`) of the wheels' bump pattern |
| +10h, +14h | smoke kind, dispersion | `2dd82`, `2ddd9` |
| +18h | rolling sound: `TIRES` sample 12h + v, 0 = none | `2a9c5` |
| +1Ch | skid sound: `SKID` sample 7 + v | `2a97a` |

Verified on China (`SR_PEEK=1800:12eb74:120`): id 0 = 0.9 / 1.3 / 1.4, 1, 6, 63, 4, 3
(`Ghiaia`, gravel), id 1 = 1.2 / 1.3 / 1.25, 1, 1, 63, 1, 2 (`Lastricato`, cobbles), id 2 =
1.2 / 1.6 / 1.5, 2, 5, 63, 0, 3 (`Sterrato`, dirt): the file's rows by id. The sample numbers
come from the race's sound list (`SOUND.DAT`, `GAME.DTR`: 08h-0Bh `SKID1-4`, 0Ch-0Eh
`HIT1-3`, 0Fh-11h `CRASH1-3`, 12h `GROUND1`, 13h-18h `TIRES1-6`). Unevenness on the seven
tracks: 0 on asphalt (and MAP6's `Terra gola`); 1-3 on grass, gravel, cobbles, dirt, sand,
snow and the wooden bridge; 5 on MAP7's `Terra`. There is no kerb road kind.

The vibration uses the unevenness of the road under the wheels that touch the ground (the
road effect), nothing else from the table.

## What the effects use

| Effect | Signals |
|---|---|
| impacts | the horizontal velocity change in the step, `|v(+55Ch) - v(+7B8h)| / step / g`, above 2 g with a wall (+150h) or car (+4Ch bit 3) contact, above 4 g without; a new contact below that: a light pulse. A car moved further than its speed allows (a restart) is no impact |
| landings, bumps | the game's landing rule (+76Ch = 0, +510h > 0) with the fall speed before it (-vy at +7BCh); the vertical acceleration with wheels down above 3 g |
| road | unevenness of the road kinds under the wheels, speed +500h / top speed +5Ch, the suspension travel change (wheel +48h against its copy) as texture |
| light | wheels sliding (wheel +60h), engine speed +C7Ch / +60h above 0.93 |

Thresholds from the dump of a lap: in normal driving the horizontal velocity change stays
below 1.5 g in 99 % of the steps (largest 3.7 g), the vertical acceleration below 2.7 g;
wall hits are 12-23 g, landings after jumps 7-24 g (vertical).

## Verification

**A lap** (`SR_DETERMINISTIC=1 SR_AUTOPILOT=1 SR_RUMBLE_LOG=...`, `--input controller --pad
"0:LX=0"`, keys of a China race): 3 391 steps, frames 1450-4840, 101.7 s. Events annotated
from the log (motor levels at Medium; the plot of both motors is `work/agents/rumble/lap.svg`,
the log `lap.csv`):

| time (s) | event | game flags / measured | low | high |
|---|---|---|---|---|
| 8.0 | road: gravel (id 0) | 4 wheels on id 0 | 0.08 | 0.05 |
| 9.3 | car contact | car flag, 0.8 g | 0.26 | 0.23 |
| 14.5 | wall hit | wall flag, 23.6 g, speed 96 -> 69 | 1.00 | 0.85 |
| 14.7, 15.1 | scraping the wall | wall flag, < 1 g | 0.27 | 0.34 |
| 22.6 | road: dirt (id 2) | | 0.14 | 0.06 |
| 27.3 | landing | all wheels were up, fall 0.43 g s | 0.66 | 0.24 |
| 29.8-30.6 | rubbing another car | car flag, < 0.5 g | 0.31 | 0.27 |
| 46.5, 52.2 | road: gravel, dirt | | 0.08, 0.12 | 0.05, 0.07 |
| 56.3 | landing after the big jump | fall 0.85 g s, 20 g on touchdown | 1.00 | 0.42 |
| 60.9 | landing | fall 0.65 g s | 0.94 | 0.34 |
| 74.0, 75.1 | landing, bumps | fall 0.32 / 0.29 g s, 11 g | 0.52-0.58 | 0.19-0.27 |
| 78.4 | wall hit (two contacts) | wall flag, 17.6 g and 13.6 g, speed 111 -> 67 | 1.00 | 0.84 |
| 80.4-92.2 | four landings | fall 0.17-0.86 g s | 0.37-1.00 | 0.14-0.40 |
| 99.9 | landing | fall 0.46 g s | 0.69 | 0.25 |
| 101.5 | road: cobbles (id 1, start line) | | 0.08 | 0.05 |

Steady driving (no event in the 10 steps before, four wheels on one road kind, above 30 % of
top speed), low / high motor medians: gravel 0.079 / 0.048, cobbles 0.066 / 0.050, dirt
0.111 / 0.066 (95th percentiles at most 0.131 / 0.138). Above 0.3 in 79 of 3 391 steps: the
hits and landings. The race start's wheelspin shows on the high motor (0.10-0.13, wheels
sliding). The attract race before it produced no step in the log (mode bit 7).

**Pause, overlay, levels, focus, exit** (real time, the user's controller in slot 0,
`SR_RUMBLE_OUT`, script in `work/agents/rumble/live_steps.txt`): every value sent was
`round(logged level x gain x 65535)` (136 of 136 at High, gain 1.6); Esc -> "no race step"
and 0 sent within 200 ms, the same for the overlay; the overlay's Vibration item changed the
level live (Medium -> High -> Low, saved to `settings.ini`); with the game's window not in
the foreground 0 was sent ("focus lost"); at exit all four slots were set to 0 (XInput result
0 for slot 0, 1167 = not connected for 1-3). `XInputSetState` returned 0 (success) for every
value sent to the controller. Whether the motors actually turned was not felt: verified
through the logs only. The level scaling, the effects and the timeout have unit tests
(`sr-input`), the setting and the overlay item too (`screamer`).

## Unknown, not used

* Damage: `29378` adds to the car's +08h, +0Ch..+18h and +1Ch..+28h (per side, capped at
  1 / 2) and sets +B9h bits, but only when damage is on (`[143720]`); not used.
* The game's own collision strengths (the wall routine's `[ebp+72h]` in `3314c`, the
  car-car relative velocity at `[ebp-3F2h]` in `303f4`, which choose the `HIT` / `CRASH`
  samples) are stack values: reading them would need observers on the collision code; the
  velocity change measures the same hits.
* Replays: not checked whether they run race steps with the player's car (they would
  vibrate like the race).
* The HUD's km/h from +500h: not needed, not derived.
* Other tracks: the same table and fields are read (the ids index the track's own table);
  only China was run.
