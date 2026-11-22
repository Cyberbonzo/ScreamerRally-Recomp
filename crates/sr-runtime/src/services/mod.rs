//! Software-interrupt services the original code calls: DOS, DPMI, BIOS, multiplex.

pub mod bios;
pub mod dos;
pub mod dpmi;
pub mod realmode;

use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

use crate::Machine;

pub fn interrupt(m: &mut Machine, c: &mut CONTEXT, vector: u8) -> Result<(), String> {
    m.stats.ints[vector as usize] += 1;
    let ah = (c.Eax >> 8) as u8 as usize;
    match vector {
        0x21 => m.stats.dos_fn[ah] += 1,
        0x31 => m.stats.dpmi_fn[ah] += 1,
        _ => {}
    }
    match vector {
        0x21 => dos::int21(m, c),
        0x31 => dpmi::int31(m, c),
        0x10 => bios::int10(m, c),
        0x16 => bios::int16(m, c),
        0x1a => bios::int1a(m, c),
        0x2f => bios::int2f(m, c),
        0x33 => bios::int33(m, c),
        0x4b => bios::int4b(m, c),
        v => Err(format!("unhandled software interrupt {v:02x}h")),
    }
}
