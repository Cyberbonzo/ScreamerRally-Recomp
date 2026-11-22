//! DPMI 0.9 host services (`int 31h`) as provided by DOS/4GW.

use std::collections::HashMap;

use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

use crate::Machine;
use crate::cpu::Cpu;
use crate::mem::{self, FLAT_CS};
use crate::sel::Descriptor;

impl Dpmi {
    /// Memory blocks allocated by the guest: (address, size).
    pub fn regions(&self) -> Vec<(u32, u32)> {
        self.blocks.values().copied().collect()
    }
}

#[derive(Default)]
pub struct Dpmi {
    /// handle -> (linear address, size)
    blocks: HashMap<u32, (u32, u32)>,
    next_handle: u32,
    /// Real-mode callbacks: fake real-mode address -> (pm selector, pm offset, rm struct)
    pub callbacks: Vec<(u16, u32, u32)>,
}

/// Total memory reported to the game. The original checks for "16 Mb version" vs
/// "enhanced memory version"; a generous figure selects the latter.
const REPORTED_FREE: u32 = 256 * 1024 * 1024;

fn get_cx_dx(c: &CONTEXT) -> u32 {
    (c.cx() as u32) << 16 | c.dx() as u32
}
fn get_bx_cx(c: &CONTEXT) -> u32 {
    (c.bx() as u32) << 16 | c.cx() as u32
}
fn get_si_di(c: &CONTEXT) -> u32 {
    (c.si() as u32) << 16 | c.di() as u32
}
fn set_bx_cx(c: &mut CONTEXT, v: u32) {
    c.set_bx((v >> 16) as u16);
    c.set_cx(v as u16);
}
fn set_si_di(c: &mut CONTEXT, v: u32) {
    c.set_si((v >> 16) as u16);
    c.set_di(v as u16);
}

fn descriptor_bytes(d: &Descriptor) -> [u8; 8] {
    let (limit, g) = if d.limit > 0xfffff { (d.limit >> 12, 0x80u8) } else { (d.limit, 0) };
    [
        limit as u8,
        (limit >> 8) as u8,
        d.base as u8,
        (d.base >> 8) as u8,
        (d.base >> 16) as u8,
        d.access as u8,
        ((limit >> 16) as u8 & 0x0f) | ((d.access >> 8) as u8 & 0x70) | g,
        (d.base >> 24) as u8,
    ]
}

fn descriptor_from(b: &[u8]) -> Descriptor {
    let mut limit = b[0] as u32 | (b[1] as u32) << 8 | ((b[6] & 0x0f) as u32) << 16;
    if b[6] & 0x80 != 0 {
        limit = limit << 12 | 0xfff;
    }
    Descriptor {
        base: b[2] as u32 | (b[3] as u32) << 8 | (b[4] as u32) << 16 | (b[7] as u32) << 24,
        limit,
        access: b[5] as u16 | ((b[6] & 0xf0) as u16) << 8,
    }
}

pub fn int31(m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    let ax = c.ax();
    crate::trace!("int 31h ax={ax:04x} {}", c.dump());
    match ax {
        0x0000 => match m.sel.alloc(c.cx()) {
            Some(s) => {
                c.set_ax(s);
                c.ok();
            }
            None => c.fail(0x8011),
        },
        0x0001 => {
            if m.sel.free(c.bx()) {
                c.ok()
            } else {
                c.fail(0x8022)
            }
        }
        0x0002 => {
            let sel = m.selector_for_segment(c.bx());
            c.set_ax(sel);
            c.ok();
        }
        0x0003 => {
            c.set_ax(8);
            c.ok();
        }
        0x0006 => match m.sel.get(c.bx()) {
            Some(d) => {
                c.set_cx((d.base >> 16) as u16);
                c.set_dx(d.base as u16);
                c.ok();
            }
            None => c.fail(0x8022),
        },
        0x0007 => {
            let base = get_cx_dx(c);
            if m.sel.update(c.bx(), |d| d.base = base) {
                c.ok()
            } else if base == 0 {
                c.ok() // setting base 0 on a flat selector is a no-op
            } else {
                c.fail(0x8022)
            }
        }
        0x0008 => {
            let limit = get_cx_dx(c);
            if m.sel.update(c.bx(), |d| d.limit = limit) || m.sel.get(c.bx()).is_some() {
                c.ok()
            } else {
                c.fail(0x8022)
            }
        }
        0x0009 => {
            let acc = c.cx();
            if m.sel.update(c.bx(), |d| d.access = acc) || m.sel.get(c.bx()).is_some() {
                c.ok()
            } else {
                c.fail(0x8022)
            }
        }
        0x000a => match m.sel.get(c.bx()) {
            Some(d) => match m.sel.create(Descriptor { access: (d.access & !0x0e) | 0x02, ..d }) {
                Some(s) => {
                    c.set_ax(s);
                    c.ok();
                }
                None => c.fail(0x8011),
            },
            None => c.fail(0x8022),
        },
        0x000b => match m.sel.get(c.bx()) {
            Some(d) => {
                unsafe { mem::slice_mut(c.edi(), 8).copy_from_slice(&descriptor_bytes(&d)) };
                c.ok();
            }
            None => c.fail(0x8022),
        },
        0x000c => {
            let d = descriptor_from(unsafe { mem::slice(c.edi(), 8) });
            if m.sel.update(c.bx(), |x| *x = d) {
                c.ok()
            } else {
                c.fail(0x8022)
            }
        }
        0x0100 => match m.lowmem.alloc(c.bx()) {
            Some((seg, _)) => {
                let sel = m.selector_for_segment(seg);
                c.set_ax(seg);
                c.set_dx(sel);
                crate::debug!("DPMI: DOS block of {} paragraphs at segment {seg:#06x}, selector {sel:#06x}", c.bx());
                c.ok();
            }
            None => {
                c.set_bx(0);
                c.fail(0x0008);
            }
        },
        0x0101 => {
            // free DOS memory block (DX = selector)
            match m.sel.get(c.dx()) {
                Some(d) if m.lowmem.free((d.base >> 4) as u16) => {
                    m.sel.free(c.dx());
                    c.ok();
                }
                _ => c.fail(0x0009),
            }
        }
        0x0102 => c.fail(0x0008),
        0x0200 => {
            let v = m.rm_vectors[c.bl() as usize];
            c.set_cx((v >> 16) as u16);
            c.set_dx(v as u16);
            c.ok();
        }
        0x0201 => {
            m.rm_vectors[c.bl() as usize] = get_cx_dx(c);
            c.ok();
        }
        0x0202 => {
            let (s, o) = m.exc_handlers[(c.bl() & 31) as usize];
            c.set_cx(s);
            c.Edx = o;
            c.ok();
        }
        0x0203 => {
            m.exc_handlers[(c.bl() & 31) as usize] = (c.cx(), c.edx());
            c.ok();
        }
        0x0204 => {
            let (s, o) = m.pm_vectors[c.bl() as usize];
            c.set_cx(s);
            c.Edx = o;
            c.ok();
        }
        0x0205 => {
            m.vector_set(c.bl(), c.edx());
            crate::debug!("DPMI: protected-mode vector {:02x}h -> {}", c.bl(), m.where_(c.edx()));
            c.ok();
        }
        0x0300..=0x0302 => {
            let s = c.edi();
            crate::services::realmode::call(m, ax, c.bl(), s)?;
            c.ok();
        }
        0x0303 => {
            let addr = 0xf000_0000u32 | (m.dpmi.callbacks.len() as u32 * 0x10);
            m.dpmi.callbacks.push((FLAT_CS, c.esi(), c.edi()));
            c.set_cx((addr >> 16) as u16);
            c.set_dx(addr as u16);
            c.ok();
        }
        0x0304 => c.ok(),
        0x0305 => {
            c.set_ax(0);
            c.ok();
        }
        0x0400 => {
            c.set_ax(0x005a); // DPMI 0.90
            c.set_bx(0x0005); // 32-bit host, interrupts reflected to protected mode
            c.set_cl(5); // Pentium
            c.set_dh(0x08);
            c.set_dl(0x70);
            c.ok();
        }
        0x0500 => {
            let pages = REPORTED_FREE / 4096;
            let info: [u32; 12] = [REPORTED_FREE, pages, pages, pages * 2, pages, pages, pages * 2, pages, pages * 2, u32::MAX, u32::MAX, u32::MAX];
            unsafe {
                for (i, v) in info.iter().enumerate() {
                    mem::wr32(c.edi() + i as u32 * 4, *v);
                }
            }
            c.ok();
        }
        0x0501 => {
            let size = get_bx_cx(c);
            match mem::alloc_guest(size.max(1)) {
                Some(addr) => {
                    m.dpmi.next_handle += 1;
                    let h = m.dpmi.next_handle;
                    m.dpmi.blocks.insert(h, (addr, size));
                    set_bx_cx(c, addr);
                    set_si_di(c, h);
                    crate::debug!("DPMI: allocated {size:#x} bytes at {addr:#010x} (handle {h})");
                    c.ok();
                }
                None => c.fail(0x8012),
            }
        }
        0x0502 => {
            let h = get_si_di(c);
            match m.dpmi.blocks.remove(&h) {
                Some((addr, _)) => {
                    mem::free(addr);
                    c.ok();
                }
                None => c.fail(0x8023),
            }
        }
        0x0503 => {
            let h = get_si_di(c);
            let size = get_bx_cx(c);
            match m.dpmi.blocks.get(&h).copied() {
                Some((old, old_size)) => match mem::alloc_guest(size.max(1)) {
                    Some(new) => {
                        unsafe { std::ptr::copy_nonoverlapping(mem::ptr(old), mem::ptr(new), old_size.min(size) as usize) };
                        mem::free(old);
                        m.dpmi.blocks.insert(h, (new, size));
                        set_bx_cx(c, new);
                        crate::debug!("DPMI: resized handle {h} to {size:#x} bytes at {new:#010x}");
                        c.ok();
                    }
                    None => c.fail(0x8012),
                },
                None => c.fail(0x8023),
            }
        }
        0x0600..=0x0603 | 0x0700..=0x0703 => c.ok(),
        0x0604 => {
            c.set_bx(0);
            c.set_cx(4096);
            c.ok();
        }
        0x0800 => {
            let phys = get_bx_cx(c);
            match m.map_physical(phys, get_si_di(c)) {
                Some(lin) => {
                    set_bx_cx(c, lin);
                    c.ok();
                }
                None => {
                    crate::warn!("DPMI: physical mapping of {phys:#x} refused");
                    c.fail(0x8021)
                }
            }
        }
        0x0801 => c.ok(),
        0x0900 => {
            c.set_al(m.vif() as u8);
            m.set_vif(false);
            c.ok();
        }
        0x0901 => {
            c.set_al(m.vif() as u8);
            m.set_vif(true);
            c.ok();
        }
        0x0902 => {
            c.set_al(m.vif() as u8);
            c.ok();
        }
        0x0a00 => c.fail(0x8001),
        0x0e00 => {
            c.set_ax(0x004d); // coprocessor present and enabled, 80387+
            c.ok();
        }
        0x0e01 => c.ok(),
        _ => {
            crate::warn!("DPMI function {ax:04x}h not implemented ({})", c.dump());
            c.fail(0x8001);
        }
    }
    Ok(())
}
