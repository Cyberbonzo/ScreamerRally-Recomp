//! Scripted keyboard input for automated runs and tests.
//!
//! `--keys "8:ENTER,10.5:UP+,14:UP-,15:ESC"`: at each time (seconds since start) press a
//! key; `KEY` alone taps it (press, release 120 ms later), `KEY+` / `KEY-` press or
//! release. Keys go through the same path as the window: PC scancodes into the guest's
//! keyboard interrupt.

use std::time::{Duration, Instant};

/// (make code, extended)
fn scancode(name: &str) -> Option<(u8, bool)> {
    let n = name.to_ascii_uppercase();
    Some(match n.as_str() {
        "ESC" => (0x01, false),
        "ENTER" | "RETURN" => (0x1c, false),
        "SPACE" => (0x39, false),
        "TAB" => (0x0f, false),
        "BACKSPACE" => (0x0e, false),
        "LSHIFT" => (0x2a, false),
        "RSHIFT" => (0x36, false),
        "CTRL" => (0x1d, false),
        "ALT" => (0x38, false),
        "UP" => (0x48, true),
        "DOWN" => (0x50, true),
        "LEFT" => (0x4b, true),
        "RIGHT" => (0x4d, true),
        "F1" => (0x3b, false),
        "F2" => (0x3c, false),
        "F3" => (0x3d, false),
        "F4" => (0x3e, false),
        "F5" => (0x3f, false),
        "F10" => (0x44, false),
        "F11" => (0x57, false),
        "F12" => (0x58, false),
        "PGUP" => (0x49, true),
        "PGDN" => (0x51, true),
        "PERIOD" | "." => (0x34, false),
        "COMMA" => (0x33, false),
        s if s.len() == 4 && s.starts_with("SC") => (u8::from_str_radix(&s[2..], 16).ok()?, false),
        s if s.len() == 1 => {
            let c = s.as_bytes()[0];
            let row = |r: &[u8], base: u8| r.iter().position(|&x| x == c).map(|i| base + i as u8);
            let code = row(b"1234567890", 0x02)
                .or_else(|| row(b"QWERTYUIOP", 0x10))
                .or_else(|| row(b"ASDFGHJKL", 0x1e))
                .or_else(|| row(b"ZXCVBNM", 0x2c))?;
            (code, false)
        }
        _ => return None,
    })
}

pub fn spawn(script: &str, push: impl Fn(u8) + Send + 'static) -> Result<(), String> {
    let mut events: Vec<(f64, u8, bool, bool)> = Vec::new(); // time, code, extended, release
    for item in script.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (t, key) = item.split_once(':').ok_or_else(|| format!("bad key event '{item}'"))?;
        let t: f64 = t.parse().map_err(|_| format!("bad time in '{item}'"))?;
        let (name, mode) = match key.as_bytes().last() {
            Some(b'+') => (&key[..key.len() - 1], 1),
            Some(b'-') => (&key[..key.len() - 1], 2),
            _ => (key, 0),
        };
        let (code, ext) = scancode(name).ok_or_else(|| format!("unknown key '{name}'"))?;
        if mode != 2 {
            events.push((t, code, ext, false));
        }
        if mode == 0 {
            events.push((t + 0.12, code, ext, true));
        }
        if mode == 2 {
            events.push((t, code, ext, true));
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    if sr_runtime::time::deterministic() {
        // virtual time: each key at its timer tick
        for (t, code, ext, release) in events {
            let tick = (t * sr_runtime::events::HMI_BASE_HZ).round() as u64;
            if ext {
                sr_runtime::events::schedule_scancode(tick, 0xe0);
            }
            sr_runtime::events::schedule_scancode(tick, if release { code | 0x80 } else { code });
        }
        let _ = push;
        return Ok(());
    }
    std::thread::Builder::new()
        .name("keyscript".into())
        .spawn(move || {
            let start = Instant::now();
            for (t, code, ext, release) in events {
                let at = Duration::from_secs_f64(t);
                if let Some(wait) = at.checked_sub(start.elapsed()) {
                    std::thread::sleep(wait);
                }
                if ext {
                    push(0xe0);
                }
                push(if release { code | 0x80 } else { code });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
