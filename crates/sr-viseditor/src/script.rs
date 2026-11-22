//! The test script (`SR_VISEDIT_SCRIPT`, diagnostics): timed mouse and key actions on the
//! editor window, screenshots and checks, so the editor can be tested without a person.
//!
//! The script is the variable's value or, when that names a file, the file's text: steps
//! separated by `;` or new lines (`#` starts a comment), each `TIME VERB ARGS` with TIME in
//! seconds after the first track appears in the editor (a line `start N`: after the N-th, e.g.
//! 2 to skip the attract mode's race). While a script runs, the real mouse and keyboard are
//! ignored. Verbs:
//!
//! | Verb | Does |
//! |---|---|
//! | `world X Z` / `cell ROW COL` / `px X Y` | move the mouse to a world point, a cell's centre, a window pixel |
//! | `center ROW COL [PX]` | centre the map on a cell (PX pixels per sector), follow car off |
//! | `click` `rclick` `mclick` `ctrlclick` | a click where the mouse is |
//! | `down` `up` `rdown` `rup` | the left / right button |
//! | `drag R1 C1 R2 C2` | left drag from a cell's centre to another's (`rdrag`: right, pans) |
//! | `wheel N` | N wheel notches where the mouse is (positive: zoom in) |
//! | `key K` | a key: a letter or digit, `+ - [ ] , . < >`, `home`, `back`, `ctrl+z` ... |
//! | `button NAME` | click a panel button (`Action::id`: extend, less, width+, reset, ...) |
//! | `shot NAME` | save the window as `NAME.png` in the shot folder (`SR_VISEDIT_SHOT`) |
//! | `size W H` | the window's client size |
//! | `check ROW COL STATE` | log whether the sector is `drawn`, `hidden`, `added`, `notdrawn` or `default` (drawn by default) from the camera cell |
//! | `checkfile` | log whether the saved file holds exactly the edits |
//! | `log` | the editor's state and the edits in the log |
//! | `close` | close the editor window (as its close button does: unsaved edits are saved) |

use crate::action::Action;
use crate::editor::{Btn, VK_BACK, VK_HOME};

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    World(f64, f64),
    Cell(u16, u16),
    /// Centre the map on a cell (pixels per sector, 0: keep the zoom); follow car off.
    Center(u16, u16, f64),
    Px(f64, f64),
    Click { btn: Btn, ctrl: bool },
    Down(Btn),
    Up(Btn),
    Drag { from: (u16, u16), to: (u16, u16), btn: Btn },
    Wheel(f64),
    Key { vk: u32, ctrl: bool, shift: bool },
    Char(char),
    Button(Action),
    Shot(String),
    Size(u32, u32),
    Check { cell: (u16, u16), state: String },
    CheckFile,
    Log,
    /// Close the editor window (as its close button does).
    Close,
}

/// A parsed script.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    /// In time order (steps at the same time in their written order).
    pub steps: Vec<(f64, Step)>,
    /// The steps' times count from the editor's this-th track (`start N`; 1: the first).
    pub start_race: usize,
}

/// Read a script.
pub fn parse(text: &str) -> Result<Script, String> {
    let mut steps = Vec::new();
    let mut start_race = 1;
    // comments first (they may hold a ';'), then the steps of each line
    for raw in text.lines().flat_map(|l| l.split('#').next().unwrap_or("").split(';')) {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        let bad = |what: &str| format!("{line:?}: {what}");
        if w[0].eq_ignore_ascii_case("start") {
            start_race = w.get(1).and_then(|n| n.parse().ok()).filter(|&n: &usize| n >= 1).ok_or_else(|| bad("start N: the steps count from the N-th track"))?;
            continue;
        }
        let t: f64 = w[0].parse().map_err(|_| bad("a time in seconds first"))?;
        let verb = w.get(1).ok_or_else(|| bad("no action"))?.to_ascii_lowercase();
        let num = |i: usize| -> Result<f64, String> { w.get(i).and_then(|s| s.parse().ok()).ok_or_else(|| bad("a number missing")) };
        let cell = |i: usize| -> Result<(u16, u16), String> {
            let (r, c) = (num(i)?, num(i + 1)?);
            if !(0.0..256.0).contains(&r) || !(0.0..256.0).contains(&c) {
                return Err(bad("cells are 0 to 255"));
            }
            Ok((r as u16, c as u16))
        };
        let step = match verb.as_str() {
            "world" => Step::World(num(2)?, num(3)?),
            "cell" => {
                let c = cell(2)?;
                Step::Cell(c.0, c.1)
            }
            "px" => Step::Px(num(2)?, num(3)?),
            "center" => {
                let c = cell(2)?;
                Step::Center(c.0, c.1, if w.len() > 4 { num(4)? } else { 0.0 })
            }
            "click" => Step::Click { btn: Btn::Left, ctrl: false },
            "ctrlclick" => Step::Click { btn: Btn::Left, ctrl: true },
            "rclick" => Step::Click { btn: Btn::Right, ctrl: false },
            "mclick" => Step::Click { btn: Btn::Middle, ctrl: false },
            "down" => Step::Down(Btn::Left),
            "up" => Step::Up(Btn::Left),
            "rdown" => Step::Down(Btn::Right),
            "rup" => Step::Up(Btn::Right),
            "drag" => Step::Drag { from: cell(2)?, to: cell(4)?, btn: Btn::Left },
            "rdrag" => Step::Drag { from: cell(2)?, to: cell(4)?, btn: Btn::Right },
            "wheel" => Step::Wheel(num(2)?),
            "key" => key(w.get(2).ok_or_else(|| bad("which key"))?).ok_or_else(|| bad("unknown key"))?,
            "button" => Step::Button(w.get(2).and_then(|n| Action::from_id(n)).ok_or_else(|| bad("unknown button"))?),
            "shot" => Step::Shot(w.get(2).map_or_else(|| format!("shot_{t}"), |s| s.to_string())),
            "size" => Step::Size(num(2)? as u32, num(3)? as u32),
            "check" => {
                let state = w.get(4).ok_or_else(|| bad("which state"))?.to_ascii_lowercase();
                if !["drawn", "hidden", "added", "notdrawn", "default"].contains(&state.as_str()) {
                    return Err(bad("states: drawn, hidden, added, notdrawn, default"));
                }
                Step::Check { cell: cell(2)?, state }
            }
            "checkfile" => Step::CheckFile,
            "log" => Step::Log,
            "close" => Step::Close,
            v => return Err(bad(&format!("unknown action {v}"))),
        };
        steps.push((t, step));
    }
    steps.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(Script { steps, start_race })
}

/// A key name: `e`, `3`, `+`, `[`, `.`, `<`, `home`, `back`, `ctrl+z`, `ctrl+shift+z`.
fn key(name: &str) -> Option<Step> {
    let lower = name.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('+').collect();
    // "+" alone, or "ctrl++"
    if lower.ends_with('+') && parts.last() == Some(&"") {
        parts.pop();
        if let Some(l) = parts.last_mut() {
            *l = "+";
        }
        if parts.is_empty() {
            parts.push("+");
        }
    }
    let (mods, k) = parts.split_at(parts.len() - 1);
    let ctrl = mods.contains(&"ctrl");
    let shift = mods.contains(&"shift");
    let k = k[0];
    match k {
        "home" => Some(Step::Key { vk: VK_HOME, ctrl, shift }),
        "back" | "backspace" => Some(Step::Key { vk: VK_BACK, ctrl, shift }),
        "+" | "-" | "[" | "]" | "=" | "," | "." | "<" | ">" => Some(Step::Char(k.chars().next().unwrap())),
        "plus" => Some(Step::Char('+')),
        "minus" => Some(Step::Char('-')),
        _ if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => Some(Step::Key { vk: k.to_ascii_uppercase().as_bytes()[0] as u32, ctrl, shift }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let script = parse("start 2; 1 cell 120 88; 0.5 shot start\n2 click # hide it; a comment\n2 key ctrl+z;3 key +;3 key [;3 key home;4 drag 1 2 3 4;5 button extend;6 check 120 88 hidden").unwrap();
        assert_eq!(script.start_race, 2);
        assert_eq!(parse("1 log").unwrap().start_race, 1);
        assert!(parse("start 0").is_err());
        let s = script.steps;
        assert_eq!(s[0], (0.5, Step::Shot("start".into())));
        assert_eq!(s[1], (1.0, Step::Cell(120, 88)));
        assert_eq!(s[2], (2.0, Step::Click { btn: Btn::Left, ctrl: false }));
        assert_eq!(s[3], (2.0, Step::Key { vk: 0x5a, ctrl: true, shift: false }));
        assert_eq!(s[4].1, Step::Char('+'));
        assert_eq!(s[5].1, Step::Char('['));
        assert_eq!(s[6].1, Step::Key { vk: VK_HOME, ctrl: false, shift: false });
        assert_eq!(s[7].1, Step::Drag { from: (1, 2), to: (3, 4), btn: Btn::Left });
        assert_eq!(s[8].1, Step::Button(Action::Extend));
        assert_eq!(s[9].1, Step::Check { cell: (120, 88), state: "hidden".into() });
        assert_eq!(parse("1 center 10 20 64").unwrap().steps[0].1, Step::Center(10, 20, 64.0));
        assert_eq!(parse("1 center 10 20").unwrap().steps[0].1, Step::Center(10, 20, 0.0));
        assert_eq!(key("ctrl+shift+z"), Some(Step::Key { vk: 0x5a, ctrl: true, shift: true }));
        assert_eq!(key("e"), Some(Step::Key { vk: 0x45, ctrl: false, shift: false }));
        assert_eq!(key("."), Some(Step::Char('.')));
        assert_eq!(key(">"), Some(Step::Char('>')));
        assert_eq!(parse("1 button less").unwrap().steps[0].1, Step::Button(Action::Less));
        assert_eq!(key("shift+e"), Some(Step::Key { vk: 0x45, ctrl: false, shift: true }));
        assert!(parse("x click").is_err());
        assert!(parse("1 fly").is_err());
        assert!(parse("1 cell 300 2").is_err());
        assert!(parse("1 check 1 2 maybe").is_err());
        assert!(parse("1 button nothing").is_err());
    }
}
