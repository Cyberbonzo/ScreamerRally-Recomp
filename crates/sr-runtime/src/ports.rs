//! Virtual I/O ports: 8259 PIC, 8253 PIT, 8042 keyboard controller, gameport, VGA DAC
//! and CMOS clock. Everything else reads as an empty bus (0xFF) and is logged once.

use std::collections::{HashSet, VecDeque};

#[derive(Default)]
pub struct Pic {
    pub imr: [u8; 2],
    pub eoi_count: u64,
}

#[derive(Default)]
pub struct Pit {
    /// Programmed reload value of each channel (0 = 65536).
    pub reload: [u16; 3],
    mode: [u8; 3],
    lsb_pending: [Option<u8>; 3],
    latch: [Option<u16>; 3],
    read_hi: [bool; 3],
}

impl Pit {
    pub const HZ: f64 = 1_193_182.0;

    /// Frequency of channel 0 (the system timer, IRQ0).
    pub fn ch0_hz(&self) -> f64 {
        let r = if self.reload[0] == 0 { 65536.0 } else { self.reload[0] as f64 };
        Self::HZ / r
    }

    fn counter_now(&self, ch: usize) -> u16 {
        let r = if self.reload[ch] == 0 { 65536u64 } else { self.reload[ch] as u64 };
        let ticks = (crate::time::uptime().as_nanos() as u64).wrapping_mul(1_193_182) / 1_000_000_000;
        (r - ticks % r) as u16
    }
}

/// PC keyboard: the scancode stream the game's IRQ1 handler reads from port 0x60.
#[derive(Default)]
pub struct Keyboard {
    pub scancodes: VecDeque<u8>,
    pub data: u8,
    /// BIOS keyboard buffer (scancode << 8 | ascii) for int 16h users.
    pub bios: VecDeque<u16>,
    pub shift: u8,
}

impl Keyboard {
    pub fn bios_key(&mut self) -> Option<u16> {
        self.bios.pop_front()
    }
    pub fn peek_bios_key(&self) -> Option<u16> {
        self.bios.front().copied()
    }
    pub fn shift_flags(&self) -> u8 {
        self.shift
    }
}

/// Analog gameport (0x201): four one-shot timers and four buttons.
pub struct Gameport {
    /// Axis positions as one-shot durations in microseconds (None = axis not connected).
    pub axis_us: [Option<f64>; 4],
    /// Buttons, true = pressed.
    pub buttons: [bool; 4],
    fired_at: Option<std::time::Duration>,
}

impl Default for Gameport {
    fn default() -> Self {
        Gameport { axis_us: [None; 4], buttons: [false; 4], fired_at: None }
    }
}

impl Gameport {
    fn read(&self) -> u8 {
        let mut v = 0u8;
        let elapsed_us = self.fired_at.map(|t| (crate::time::uptime() - t).as_secs_f64() * 1e6);
        for (i, a) in self.axis_us.iter().enumerate() {
            let busy = match (a, elapsed_us) {
                (Some(us), Some(e)) => e < *us,
                (None, _) => true, // a missing axis never times out
                (Some(_), None) => false,
            };
            if busy {
                v |= 1 << i;
            }
        }
        for (i, b) in self.buttons.iter().enumerate() {
            if !b {
                v |= 0x10 << i;
            }
        }
        v
    }
}

pub struct Vga {
    pub dac: [[u8; 3]; 256],
    write_index: u8,
    read_index: u8,
    write_comp: usize,
    read_comp: usize,
    pub dac_dirty: bool,
    misc_index: [u8; 4],
}

impl Default for Vga {
    fn default() -> Self {
        Vga { dac: [[0; 3]; 256], write_index: 0, read_index: 0, write_comp: 0, read_comp: 0, dac_dirty: false, misc_index: [0; 4] }
    }
}

#[derive(Default)]
pub struct Ports {
    pub pic: Pic,
    pub pit: Pit,
    pub kbd: Keyboard,
    pub joy: Gameport,
    pub vga: Vga,
    cmos_index: u8,
    logged: HashSet<u16>,
}

impl Ports {
    fn unknown(&mut self, port: u16, write: Option<u32>) -> u32 {
        if self.logged.insert(port) {
            match write {
                Some(v) => crate::debug!("unhandled port write {port:#06x} <- {v:#x}"),
                None => crate::debug!("unhandled port read {port:#06x}"),
            }
        }
        0xffff_ffff
    }

    pub fn read(&mut self, port: u16, width: u32) -> u32 {
        let v: u32 = match port {
            0x20 | 0xa0 => 0,
            0x21 => self.pic.imr[0] as u32,
            0xa1 => self.pic.imr[1] as u32,
            0x40..=0x42 => {
                let ch = (port - 0x40) as usize;
                let val = self.pit.latch[ch].unwrap_or_else(|| self.pit.counter_now(ch));
                let byte = if self.pit.read_hi[ch] {
                    self.pit.latch[ch] = None;
                    (val >> 8) as u8
                } else {
                    val as u8
                };
                if self.pit.mode[ch] & 0x30 == 0x30 {
                    self.pit.read_hi[ch] = !self.pit.read_hi[ch];
                }
                byte as u32
            }
            0x60 => {
                if let Some(s) = self.kbd.scancodes.pop_front() {
                    self.kbd.data = s;
                }
                self.kbd.data as u32
            }
            0x61 => 0x20,
            0x64 => {
                if self.kbd.scancodes.is_empty() {
                    0x1c
                } else {
                    0x1d
                }
            }
            0x71 => {
                let t = crate::time::local_now();
                let bcd = |v: u8| ((v / 10) << 4 | v % 10) as u32;
                match self.cmos_index & 0x7f {
                    0x00 => bcd(t.second),
                    0x02 => bcd(t.minute),
                    0x04 => bcd(t.hour),
                    0x07 => bcd(t.day),
                    0x08 => bcd(t.month),
                    0x09 => bcd((t.year % 100) as u8),
                    0x0a => 0x26,
                    0x0b => 0x02,
                    _ => 0,
                }
            }
            0x201 => self.joy.read() as u32,
            0x3c7 => 3, // DAC in write mode
            0x3c8 => self.vga.write_index as u32,
            0x3c9 => {
                let v = self.vga.dac[self.vga.read_index as usize][self.vga.read_comp];
                self.vga.read_comp += 1;
                if self.vga.read_comp == 3 {
                    self.vga.read_comp = 0;
                    self.vga.read_index = self.vga.read_index.wrapping_add(1);
                }
                v as u32
            }
            0x3da => {
                // Input status 1: report vertical retrace in a ~70 Hz pattern from host time
                let us = crate::time::uptime().as_micros() as u64 % 14_286;
                if us < 1400 { 0x09 } else { 0x00 }
            }
            0x3c4 | 0x3c5 | 0x3ce | 0x3cf | 0x3d4 | 0x3d5 | 0x3cc => 0,
            p => self.unknown(p, None),
        };
        match width {
            1 => v & 0xff,
            2 => v & 0xffff,
            _ => v,
        }
    }

    pub fn write(&mut self, port: u16, _width: u32, v: u32) {
        let b = v as u8;
        match port {
            0x20 | 0xa0 => {
                if b & 0x20 != 0 {
                    self.pic.eoi_count += 1;
                }
            }
            0x21 => self.pic.imr[0] = b,
            0xa1 => self.pic.imr[1] = b,
            0x40..=0x42 => {
                let ch = (port - 0x40) as usize;
                match self.pit.mode[ch] & 0x30 {
                    0x10 => self.pit.reload[ch] = b as u16,
                    0x20 => self.pit.reload[ch] = (b as u16) << 8,
                    _ => match self.pit.lsb_pending[ch].take() {
                        None => self.pit.lsb_pending[ch] = Some(b),
                        Some(lo) => {
                            self.pit.reload[ch] = (b as u16) << 8 | lo as u16;
                            crate::debug!("PIT channel {ch} reload {} ({:.2} Hz)", self.pit.reload[ch], self.pit.ch0_hz());
                        }
                    },
                }
            }
            0x43 => {
                let ch = (b >> 6) as usize;
                if ch < 3 {
                    if b & 0x30 == 0 {
                        self.pit.latch[ch] = Some(self.pit.counter_now(ch));
                        self.pit.read_hi[ch] = false;
                    } else {
                        self.pit.mode[ch] = b;
                        self.pit.lsb_pending[ch] = None;
                        self.pit.read_hi[ch] = false;
                    }
                }
            }
            0x60 => {
                // Commands to the keyboard itself are acknowledged like a real AT keyboard.
                match b {
                    0xff => self.kbd.scancodes.extend([0xfa, 0xaa]),
                    0xee => self.kbd.scancodes.push_back(0xee),
                    _ => self.kbd.scancodes.push_back(0xfa),
                }
            }
            0x61 | 0x64 => {}
            0x70 => self.cmos_index = b,
            0x201 => self.joy.fired_at = Some(crate::time::uptime()),
            0x3c7 => {
                self.vga.read_index = b;
                self.vga.read_comp = 0;
            }
            0x3c8 => {
                self.vga.write_index = b;
                self.vga.write_comp = 0;
            }
            0x3c9 => {
                self.vga.dac[self.vga.write_index as usize][self.vga.write_comp] = b & 0x3f;
                self.vga.write_comp += 1;
                if self.vga.write_comp == 3 {
                    self.vga.write_comp = 0;
                    self.vga.write_index = self.vga.write_index.wrapping_add(1);
                }
                self.vga.dac_dirty = true;
            }
            0x3c4 | 0x3ce | 0x3d4 => self.vga.misc_index[0] = b,
            0x3c5 | 0x3cf | 0x3d5 | 0x3c0 | 0x3c2 => {}
            p => {
                self.unknown(p, Some(v));
            }
        }
    }
}
