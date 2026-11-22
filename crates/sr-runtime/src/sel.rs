//! Selectors and segment registers.
//!
//! 64-bit Windows cannot create LDT descriptors for a 32-bit process, and it resets DS,
//! ES, GS (and keeps FS on the TEB) whenever it resumes a thread from an exception. So the
//! guest runs with the flat Win32 selectors only. Descriptors created through DPMI are
//! *emulated*: they get selector numbers that do not exist in the real GDT/LDT, so
//! loading one into a segment register traps. The trap layer then records the
//! descriptor's base as that register's "virtual base" and loads the flat selector.
//! Accesses through a register with a non-zero virtual base and a small offset land in
//! the never-mapped first 64 KB, fault, and are re-executed with the right address.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use crate::mem::{FLAT_CS, FLAT_DS};

/// Virtual selector of each segment register (index `Seg as usize`). Kept in statics at
/// fixed addresses so translated guest code (`segjit`) can read and update them directly.
pub static VSEL: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
/// Virtual base of each segment register.
pub static VBASE: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
/// Base of every known selector, indexed by `selector >> 3`; `NO_SELECTOR` = unknown.
pub static SELBASE: [AtomicU32; 8192] = [const { AtomicU32::new(NO_SELECTOR) }; 8192];
pub const NO_SELECTOR: u32 = u32::MAX;

fn publish(sel: u16, base: Option<u32>) {
    SELBASE[(sel >> 3) as usize].store(base.unwrap_or(NO_SELECTOR), Relaxed);
}

#[derive(Clone, Copy, Debug)]
pub struct Descriptor {
    pub base: u32,
    pub limit: u32,
    /// DPMI access rights word (type, DPL, present, 32-bit flag…).
    pub access: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seg {
    Es = 0,
    Cs = 1,
    Ss = 2,
    Ds = 3,
    Fs = 4,
    Gs = 5,
}

impl Seg {
    pub fn from_register(r: iced_x86::Register) -> Option<Seg> {
        use iced_x86::Register as R;
        Some(match r {
            R::ES => Seg::Es,
            R::CS => Seg::Cs,
            R::SS => Seg::Ss,
            R::DS => Seg::Ds,
            R::FS => Seg::Fs,
            R::GS => Seg::Gs,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        ["es", "cs", "ss", "ds", "fs", "gs"][self as usize]
    }
}

pub struct Selectors {
    table: BTreeMap<u16, Descriptor>,
}

/// Emulated selectors start here: LDT selectors (TI=1) with RPL 3, far above anything the
/// real Win32 LDT could hold, so loading them always faults.
const FIRST_EMULATED: u16 = 0x1007;

impl Default for Selectors {
    fn default() -> Self {
        let flat = Descriptor { base: 0, limit: u32::MAX, access: 0xc0f3 };
        let mut table = BTreeMap::new();
        table.insert(FLAT_DS, flat);
        table.insert(FLAT_CS, Descriptor { access: 0xc0fb, ..flat });
        // The Win32 TEB selector reads back from FS; to the guest it is just "flat".
        table.insert(0x0053, flat);
        // DOS/4GW's predefined selector 0040h: the BIOS data area at linear 400h.
        table.insert(0x0043, Descriptor { base: 0x400, limit: 0xff, access: 0x00f3 });
        for s in SELBASE.iter() {
            s.store(NO_SELECTOR, Relaxed);
        }
        SELBASE[0].store(0, Relaxed);
        for (sel, d) in &table {
            publish(*sel, Some(d.base));
        }
        let mut s = Selectors { table };
        s.set_virt_all([(FLAT_DS, 0), (FLAT_CS, 0), (FLAT_DS, 0), (FLAT_DS, 0), (0x53, 0), (FLAT_DS, 0)]);
        s
    }
}

impl Selectors {
    /// Virtual (selector, base) of every segment register.
    pub fn virt(&self) -> [(u16, u32); 6] {
        std::array::from_fn(|i| (VSEL[i].load(Relaxed) as u16, VBASE[i].load(Relaxed)))
    }

    pub fn set_virt_all(&mut self, v: [(u16, u32); 6]) {
        for (i, (sel, base)) in v.into_iter().enumerate() {
            VSEL[i].store(sel as u32, Relaxed);
            VBASE[i].store(base, Relaxed);
        }
    }

    pub fn set_virt(&mut self, seg: Seg, sel: u16, base: u32) {
        VSEL[seg as usize].store(sel as u32, Relaxed);
        VBASE[seg as usize].store(base, Relaxed);
    }

    pub fn virt_sel(&self, seg: Seg) -> u16 {
        VSEL[seg as usize].load(Relaxed) as u16
    }

    pub fn get(&self, sel: u16) -> Option<Descriptor> {
        self.table.get(&(sel | 3)).copied()
    }

    /// Allocate `count` consecutive descriptors (DPMI 0000h). Returns the first selector.
    pub fn alloc(&mut self, count: u16) -> Option<u16> {
        let count = count.max(1);
        let mut s = FIRST_EMULATED;
        'search: loop {
            for k in 0..count {
                let sel = s.checked_add(k * 8)?;
                if self.table.contains_key(&sel) {
                    s = sel + 8;
                    continue 'search;
                }
            }
            break;
        }
        for k in 0..count {
            self.table.insert(s + k * 8, Descriptor { base: 0, limit: 0, access: 0x40f3 });
            publish(s + k * 8, Some(0));
        }
        Some(s)
    }

    pub fn create(&mut self, d: Descriptor) -> Option<u16> {
        let s = self.alloc(1)?;
        self.table.insert(s, d);
        publish(s, Some(d.base));
        Some(s)
    }

    pub fn free(&mut self, sel: u16) -> bool {
        let sel = sel | 3;
        let removed = sel >= FIRST_EMULATED && self.table.remove(&sel).is_some();
        if removed {
            publish(sel, None);
        }
        removed
    }

    pub fn update(&mut self, sel: u16, f: impl FnOnce(&mut Descriptor)) -> bool {
        match self.table.get_mut(&(sel | 3)) {
            Some(d) if (sel | 3) >= FIRST_EMULATED => {
                f(d);
                publish(sel, Some(d.base));
                true
            }
            _ => false,
        }
    }

    pub fn is_emulated(&self, sel: u16) -> bool {
        (sel | 3) >= FIRST_EMULATED
    }

    /// Record a segment register load. Returns `Err` for selectors that do not exist.
    pub fn load(&mut self, seg: Seg, sel: u16) -> Result<Descriptor, String> {
        if sel & !3 == 0 {
            self.set_virt(seg, 0, 0);
            return Ok(Descriptor { base: 0, limit: 0, access: 0 });
        }
        let d = self.get(sel).ok_or_else(|| format!("load of unknown selector {sel:#06x} into {}", seg.name()))?;
        self.set_virt(seg, sel, d.base);
        Ok(d)
    }

    pub fn base(&self, seg: Seg) -> u32 {
        VBASE[seg as usize].load(Relaxed)
    }
}
