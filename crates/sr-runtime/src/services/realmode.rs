//! Real-mode calls made through DPMI 0300h–0302h.
//!
//! There is no real mode here. The few real-mode services the game reaches this way
//! (MSCDEX device requests, VESA) are served directly from the register structure.

use crate::Machine;
use crate::mem;

/// Offsets in the DPMI real-mode call structure.
pub mod rm {
    pub const EDI: u32 = 0x00;
    pub const ESI: u32 = 0x04;
    pub const EBP: u32 = 0x08;
    pub const EBX: u32 = 0x10;
    pub const EDX: u32 = 0x14;
    pub const ECX: u32 = 0x18;
    pub const EAX: u32 = 0x1c;
    pub const FLAGS: u32 = 0x20;
    pub const ES: u32 = 0x22;
    pub const DS: u32 = 0x24;
}

pub fn call(m: &mut Machine, func: u16, int: u8, s: u32) -> Result<(), String> {
    let (eax, ebx, ecx, es) = unsafe { (mem::rd32(s + rm::EAX), mem::rd32(s + rm::EBX), mem::rd32(s + rm::ECX), mem::rd16(s + rm::ES)) };
    if func != 0x0300 {
        return Err(format!("DPMI real-mode far call ({func:04x}h) is not supported"));
    }
    let ax = eax as u16;
    match int {
        0x2f if ax == 0x1510 => {
            // MSCDEX: send device driver request, ES:BX = request header (real mode)
            let req = (es as u32) * 16 + (ebx & 0xffff);
            let host = m.lowmem.translate(req).ok_or("MSCDEX request outside DOS memory")?;
            crate::cdaudio::device_request(m, ecx as u8, host);
        }
        0x2f if ax == 0x1501 => crate::cdaudio::drive_device_list(m, (es as u32) * 16 + (ebx & 0xffff)),
        0x2f => {
            crate::warn!("real-mode int 2Fh {ax:04x}h not implemented");
        }
        _ => {
            crate::warn!("real-mode int {int:02x}h ax={ax:04x} not implemented");
            unsafe { mem::wr16(s + rm::FLAGS, mem::rd16(s + rm::FLAGS) | 1) };
        }
    }
    Ok(())
}
