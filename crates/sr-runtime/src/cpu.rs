//! Guest register access shared by the trap layer (Windows `CONTEXT`) and hooks
//! (`HookFrame` pushed by the hook stub), so every service works from either entry path.

use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

pub const CF: u32 = 1 << 0;
pub const ZF: u32 = 1 << 6;
pub const IF: u32 = 1 << 9;
pub const DF: u32 = 1 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum R32 {
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
}

pub trait Cpu {
    fn get(&self, r: R32) -> u32;
    fn set(&mut self, r: R32, v: u32);
    fn eflags(&self) -> u32;
    fn set_eflags(&mut self, v: u32);
    fn eip(&self) -> u32;

    fn eax(&self) -> u32 {
        self.get(R32::Eax)
    }
    fn ebx(&self) -> u32 {
        self.get(R32::Ebx)
    }
    fn ecx(&self) -> u32 {
        self.get(R32::Ecx)
    }
    fn edx(&self) -> u32 {
        self.get(R32::Edx)
    }
    fn esi(&self) -> u32 {
        self.get(R32::Esi)
    }
    fn edi(&self) -> u32 {
        self.get(R32::Edi)
    }
    fn esp(&self) -> u32 {
        self.get(R32::Esp)
    }
    fn ebp(&self) -> u32 {
        self.get(R32::Ebp)
    }
    fn set_eax(&mut self, v: u32) {
        self.set(R32::Eax, v)
    }
    fn set_ebx(&mut self, v: u32) {
        self.set(R32::Ebx, v)
    }
    fn set_ecx(&mut self, v: u32) {
        self.set(R32::Ecx, v)
    }
    fn set_edx(&mut self, v: u32) {
        self.set(R32::Edx, v)
    }
    fn set_esi(&mut self, v: u32) {
        self.set(R32::Esi, v)
    }
    fn set_edi(&mut self, v: u32) {
        self.set(R32::Edi, v)
    }

    // 16/8-bit views
    fn ax(&self) -> u16 {
        self.eax() as u16
    }
    fn bx(&self) -> u16 {
        self.ebx() as u16
    }
    fn cx(&self) -> u16 {
        self.ecx() as u16
    }
    fn dx(&self) -> u16 {
        self.edx() as u16
    }
    fn si(&self) -> u16 {
        self.esi() as u16
    }
    fn di(&self) -> u16 {
        self.edi() as u16
    }
    fn al(&self) -> u8 {
        self.eax() as u8
    }
    fn ah(&self) -> u8 {
        (self.eax() >> 8) as u8
    }
    fn bl(&self) -> u8 {
        self.ebx() as u8
    }
    fn bh(&self) -> u8 {
        (self.ebx() >> 8) as u8
    }
    fn cl(&self) -> u8 {
        self.ecx() as u8
    }
    fn ch(&self) -> u8 {
        (self.ecx() >> 8) as u8
    }
    fn dl(&self) -> u8 {
        self.edx() as u8
    }
    fn dh(&self) -> u8 {
        (self.edx() >> 8) as u8
    }
    fn set16(&mut self, r: R32, v: u16) {
        let old = self.get(r);
        self.set(r, (old & 0xffff_0000) | v as u32);
    }
    fn set_lo8(&mut self, r: R32, v: u8) {
        let old = self.get(r);
        self.set(r, (old & 0xffff_ff00) | v as u32);
    }
    fn set_hi8(&mut self, r: R32, v: u8) {
        let old = self.get(r);
        self.set(r, (old & 0xffff_00ff) | (v as u32) << 8);
    }
    fn set_ax(&mut self, v: u16) {
        self.set16(R32::Eax, v)
    }
    fn set_bx(&mut self, v: u16) {
        self.set16(R32::Ebx, v)
    }
    fn set_cx(&mut self, v: u16) {
        self.set16(R32::Ecx, v)
    }
    fn set_dx(&mut self, v: u16) {
        self.set16(R32::Edx, v)
    }
    fn set_si(&mut self, v: u16) {
        self.set16(R32::Esi, v)
    }
    fn set_di(&mut self, v: u16) {
        self.set16(R32::Edi, v)
    }
    fn set_al(&mut self, v: u8) {
        self.set_lo8(R32::Eax, v)
    }
    fn set_ah(&mut self, v: u8) {
        self.set_hi8(R32::Eax, v)
    }
    fn set_bl(&mut self, v: u8) {
        self.set_lo8(R32::Ebx, v)
    }
    fn set_bh(&mut self, v: u8) {
        self.set_hi8(R32::Ebx, v)
    }
    fn set_cl(&mut self, v: u8) {
        self.set_lo8(R32::Ecx, v)
    }
    fn set_ch(&mut self, v: u8) {
        self.set_hi8(R32::Ecx, v)
    }
    fn set_dl(&mut self, v: u8) {
        self.set_lo8(R32::Edx, v)
    }
    fn set_dh(&mut self, v: u8) {
        self.set_hi8(R32::Edx, v)
    }

    fn set_flag(&mut self, flag: u32, on: bool) {
        let f = self.eflags();
        self.set_eflags(if on { f | flag } else { f & !flag });
    }
    /// DOS/DPMI convention: carry clear = success.
    fn ok(&mut self) {
        self.set_flag(CF, false);
    }
    /// DOS/DPMI convention: carry set and AX = error code.
    fn fail(&mut self, code: u16) {
        self.set_flag(CF, true);
        self.set_ax(code);
    }

    fn dump(&self) -> String {
        format!(
            "eax={:08x} ebx={:08x} ecx={:08x} edx={:08x} esi={:08x} edi={:08x} ebp={:08x} esp={:08x} eflags={:08x}",
            self.eax(),
            self.ebx(),
            self.ecx(),
            self.edx(),
            self.esi(),
            self.edi(),
            self.ebp(),
            self.esp(),
            self.eflags()
        )
    }
}

impl Cpu for CONTEXT {
    fn get(&self, r: R32) -> u32 {
        match r {
            R32::Eax => self.Eax,
            R32::Ecx => self.Ecx,
            R32::Edx => self.Edx,
            R32::Ebx => self.Ebx,
            R32::Esp => self.Esp,
            R32::Ebp => self.Ebp,
            R32::Esi => self.Esi,
            R32::Edi => self.Edi,
        }
    }
    fn set(&mut self, r: R32, v: u32) {
        match r {
            R32::Eax => self.Eax = v,
            R32::Ecx => self.Ecx = v,
            R32::Edx => self.Edx = v,
            R32::Ebx => self.Ebx = v,
            R32::Esp => self.Esp = v,
            R32::Ebp => self.Ebp = v,
            R32::Esi => self.Esi = v,
            R32::Edi => self.Edi = v,
        }
    }
    fn eflags(&self) -> u32 {
        self.EFlags
    }
    fn set_eflags(&mut self, v: u32) {
        self.EFlags = v;
    }
    fn eip(&self) -> u32 {
        self.Eip
    }
}

/// Register block built by the hook stub (`pushfd; pushad`), lowest address first.
#[repr(C)]
#[derive(Debug)]
pub struct HookFrame {
    pub edi: u32,
    pub esi: u32,
    pub ebp: u32,
    /// ESP as stored by `pushad` (points at `eflags`); ignored by `popad`.
    pub esp_pushad: u32,
    pub ebx: u32,
    pub edx: u32,
    pub ecx: u32,
    pub eax: u32,
    pub eflags: u32,
    /// On entry: hook id. On exit: the address execution resumes at.
    pub slot: u32,
}

impl HookFrame {
    /// The guest's ESP at the moment the hooked address was reached.
    pub fn guest_esp(&self) -> u32 {
        self.esp_pushad + 8
    }
    /// `n`-th dword on the guest stack at the hook point (0 = return address for a hooked
    /// function entry).
    pub fn stack(&self, n: u32) -> u32 {
        unsafe { crate::mem::rd32(self.guest_esp() + 4 * n) }
    }
}

impl Cpu for HookFrame {
    fn get(&self, r: R32) -> u32 {
        match r {
            R32::Eax => self.eax,
            R32::Ecx => self.ecx,
            R32::Edx => self.edx,
            R32::Ebx => self.ebx,
            R32::Esp => self.guest_esp(),
            R32::Ebp => self.ebp,
            R32::Esi => self.esi,
            R32::Edi => self.edi,
        }
    }
    fn set(&mut self, r: R32, v: u32) {
        match r {
            R32::Eax => self.eax = v,
            R32::Ecx => self.ecx = v,
            R32::Edx => self.edx = v,
            R32::Ebx => self.ebx = v,
            R32::Esp => panic!("hooks cannot move ESP directly; use HookResult::Return"),
            R32::Ebp => self.ebp = v,
            R32::Esi => self.esi = v,
            R32::Edi => self.edi = v,
        }
    }
    fn eflags(&self) -> u32 {
        self.eflags
    }
    fn set_eflags(&mut self, v: u32) {
        self.eflags = v;
    }
    fn eip(&self) -> u32 {
        0
    }
}
