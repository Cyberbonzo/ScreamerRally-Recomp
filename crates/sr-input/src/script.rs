//! Scripted controller for automated runs and tests.
//!
//! `--pad "10:RT=1,12:LX=-0.5,14:LX=0,15:RB+,15.2:RB-"`: at each time (seconds since
//! start) set an axis (`LX LY RX RY` -1..1, `LT RT` 0..1) or press (`+`) / release (`-`)
//! a button. A button without suffix is tapped for 120 ms. The scripted controller counts
//! as connected for the whole run and replaces XInput.

use crate::{BUTTONS, PadState};

#[derive(Clone, Debug)]
enum Event {
    Axis(String, f32),
    Button(u16, bool),
}

#[derive(Clone, Debug, Default)]
pub struct Script {
    events: Vec<(f64, Event)>,
}

impl Script {
    pub fn parse(s: &str) -> Result<Script, String> {
        let mut events = Vec::new();
        for item in s.split(',').map(str::trim).filter(|x| !x.is_empty()) {
            let (t, what) = item.split_once(':').ok_or_else(|| format!("bad pad event '{item}'"))?;
            let t: f64 = t.parse().map_err(|_| format!("bad time in '{item}'"))?;
            let what = what.to_ascii_uppercase();
            if let Some((axis, v)) = what.split_once('=') {
                let v: f32 = v.parse().map_err(|_| format!("bad value in '{item}'"))?;
                if !["LX", "LY", "RX", "RY", "LT", "RT"].contains(&axis) {
                    return Err(format!("unknown axis '{axis}'"));
                }
                events.push((t, Event::Axis(axis.to_string(), v)));
                continue;
            }
            let (name, mode) = match what.as_bytes().last() {
                Some(b'+') => (&what[..what.len() - 1], 1),
                Some(b'-') => (&what[..what.len() - 1], 2),
                _ => (&what[..], 0),
            };
            let b = BUTTONS.iter().find(|(n, _)| *n == name).map(|&(_, b)| b).ok_or_else(|| format!("unknown button '{name}'"))?;
            if mode != 2 {
                events.push((t, Event::Button(b, true)));
            }
            if mode == 0 {
                events.push((t + 0.12, Event::Button(b, false)));
            }
            if mode == 2 {
                events.push((t, Event::Button(b, false)));
            }
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Script { events })
    }

    /// Controller state at `t` seconds.
    pub fn state_at(&self, t: f64) -> PadState {
        let mut p = PadState::default();
        for (et, e) in &self.events {
            if *et > t {
                break;
            }
            match e {
                Event::Axis(a, v) => {
                    let stick = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                    let trig = (v.clamp(0.0, 1.0) * 255.0) as u8;
                    match a.as_str() {
                        "LX" => p.lx = stick,
                        "LY" => p.ly = stick,
                        "RX" => p.rx = stick,
                        "RY" => p.ry = stick,
                        "LT" => p.lt = trig,
                        _ => p.rt = trig,
                    }
                }
                Event::Button(b, down) => {
                    if *down {
                        p.buttons |= b;
                    } else {
                        p.buttons &= !b;
                    }
                }
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script() {
        let s = Script::parse("1:RT=1, 2:LX=-0.5, 3:A, 4:RB+, 5:RB-").unwrap();
        assert_eq!(s.state_at(0.5), PadState::default());
        assert_eq!(s.state_at(1.5).rt, 255);
        assert_eq!(s.state_at(2.5).lx, -16383);
        assert_eq!(s.state_at(3.05).buttons, 0x1000);
        assert_eq!(s.state_at(3.5).buttons, 0);
        assert_eq!(s.state_at(4.5).buttons, 0x0200);
        assert_eq!(s.state_at(5.5).buttons, 0);
        assert!(Script::parse("1:ZZ").is_err());
    }
}
