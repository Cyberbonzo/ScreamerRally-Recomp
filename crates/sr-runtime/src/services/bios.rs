//! BIOS services: video (10h), keyboard (16h), clock (1Ah), mouse driver (33h),
//! multiplex (2Fh, incl. MSCDEX) and Virtual DMA Services (4Bh).

use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

use crate::Machine;
use crate::cpu::{Cpu, ZF};
use crate::vfs::CD_DRIVE;

pub fn int10(m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    match c.ah() {
        0x00 => {
            crate::info!("BIOS: set video mode {:02x}h", c.al());
            m.video_mode = c.al() & 0x7f;
        }
        0x02 | 0x01 | 0x05 | 0x06 | 0x07 | 0x09 | 0x0b | 0x10 | 0x11 => {}
        0x03 => {
            c.set_dx(0);
            c.set_cx(0x0607);
        }
        0x0e => crate::trace!("BIOS tty: {}", c.al() as char),
        0x0f => {
            c.set_al(m.video_mode);
            c.set_ah(80);
            c.set_bh(0);
        }
        0x12 if c.bl() == 0x10 => {
            c.set_bh(0); // color
            c.set_bl(3); // 256 KB
            c.set_cx(0);
        }
        0x1a => {
            c.set_al(0x1a);
            c.set_bx(0x0008); // VGA with analog color display
        }
        0x4f => {
            crate::debug!("VESA function {:04x}h not supported", c.ax());
            c.set_ax(0x0100);
        }
        _ => crate::warn!("int 10h function {:02x}h not implemented ({})", c.ah(), c.dump()),
    }
    Ok(())
}

pub fn int16(m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    match c.ah() {
        0x00 | 0x10 => {
            let k = loop {
                if let Some(k) = m.ports.kbd.bios_key() {
                    break k;
                }
                m.idle();
            };
            c.set_ax(k);
        }
        0x01 | 0x11 => match m.ports.kbd.peek_bios_key() {
            Some(k) => {
                c.set_ax(k);
                c.set_flag(ZF, false);
            }
            None => c.set_flag(ZF, true),
        },
        0x02 | 0x12 => c.set_al(m.ports.kbd.shift_flags()),
        _ => crate::warn!("int 16h function {:02x}h not implemented", c.ah()),
    }
    Ok(())
}

pub fn int1a(_m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    match c.ah() {
        0x00 => {
            let t = crate::time::bios_ticks_since_midnight();
            c.set_cx((t >> 16) as u16);
            c.set_dx(t as u16);
            c.set_al(0);
        }
        0x02 => {
            let t = crate::time::local_now();
            let bcd = |v: u8| (v / 10) << 4 | v % 10;
            c.set_ch(bcd(t.hour));
            c.set_cl(bcd(t.minute));
            c.set_dh(bcd(t.second));
            c.set_dl(0);
            c.ok();
        }
        0x04 => {
            let t = crate::time::local_now();
            let bcd = |v: u8| (v / 10) << 4 | v % 10;
            c.set_ch(bcd((t.year / 100) as u8));
            c.set_cl(bcd((t.year % 100) as u8));
            c.set_dh(bcd(t.month));
            c.set_dl(bcd(t.day));
            c.ok();
        }
        _ => crate::warn!("int 1Ah function {:02x}h not implemented", c.ah()),
    }
    Ok(())
}

pub fn int33(_m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    // No mouse driver: the game is keyboard/joystick driven.
    if c.ax() == 0 {
        c.set_ax(0);
        c.set_bx(0);
    }
    Ok(())
}

pub fn int2f(m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    match c.ax() {
        0x1500 => {
            // MSCDEX installation check: one CD-ROM drive, D: (none without a CD: a game
            // folder)
            let drives = m.vfs.disc.is_some() as u16;
            c.set_bx(drives);
            c.set_cx(if drives > 0 { CD_DRIVE as u16 } else { 0 });
            crate::debug!("MSCDEX installation check: {drives} drive(s)");
        }
        0x150b => {
            let is_cd = c.cx() == CD_DRIVE as u16 && m.vfs.disc.is_some();
            c.set_ax(is_cd as u16);
            c.set_bx(0xadad);
        }
        0x150c => c.set_bx(0x0215),
        0x1600 => c.set_al(0), // no Windows enhanced mode
        0x1686 => c.set_ax(0), // running in protected mode under DPMI
        0x1687 => c.set_ax(0x8001), // no raw DPMI entry needed
        ax => crate::warn!("int 2Fh function {ax:04x}h not implemented ({})", c.dump()),
    }
    let _ = m;
    Ok(())
}

pub fn int4b(_m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    // Virtual DMA Services are not present (no DMA in this runtime).
    crate::debug!("VDS call {:04x}h refused", c.ax());
    c.set_flag(crate::cpu::CF, true);
    c.set_al(0x0f);
    Ok(())
}
