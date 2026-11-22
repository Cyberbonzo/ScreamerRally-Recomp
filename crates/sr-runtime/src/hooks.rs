//! Function-level replacement of original code ("hooks") and the executable arena that
//! holds hook stubs, return gadgets and the trap layer's re-execution buffers.
//!
//! Installing a hook verifies the original bytes at the address (so a hook can never be
//! applied to an unexpected executable), then overwrites them with `jmp stub`.


use crate::Machine;
use crate::cpu::HookFrame;
use crate::mem;

/// What a hook handler wants to happen next.
pub enum HookResult {
    /// Continue executing guest code at this (loaded) address.
    Resume(u32),
    /// Return from the hooked function to its caller, popping `n` argument bytes
    /// (0 for cdecl/Watcom register calls, the argument size for stdcall).
    Return(u16),
}

/// Hook handler; `arg` is the value given when the hook was installed (lets one handler
/// serve many hooks, e.g. all Glide imports).
pub type Handler = fn(&mut Machine, &mut HookFrame, arg: u32) -> HookResult;

pub struct Hook {
    pub name: &'static str,
    pub addr: u32,
    pub handler: Handler,
    pub arg: u32,
    pub calls: u64,
}

/// Executable scratch memory owned by the runtime.
pub struct Arena {
    base: u32,
    size: u32,
    used: u32,
    /// `ret n` gadgets indexed by n (0 = not made yet).
    ret_gadgets: Vec<u32>,
}

impl Arena {
    pub fn new(size: u32) -> Arena {
        let base = mem::alloc_guest_code(size).expect("cannot allocate executable arena");
        Arena { base, size, used: 0, ret_gadgets: Vec::new() }
    }

    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.base && addr < self.base + self.size
    }

    pub fn emit(&mut self, bytes: &[u8]) -> u32 {
        let at = self.base + self.used;
        assert!(self.used + bytes.len() as u32 <= self.size, "arena exhausted");
        unsafe { mem::slice_mut(at, bytes.len() as u32).copy_from_slice(bytes) };
        self.used += (bytes.len() as u32 + 15) & !15;
        at
    }

    /// Address the next `emit` / `reserve` will return.
    pub fn peek(&self) -> u32 {
        self.base + self.used
    }

    /// Address of a `ret n` instruction.
    pub fn ret_gadget(&mut self, n: u16) -> u32 {
        // n is a multiple of 4 for every stdcall import; others get their own slot too
        let slot = n as usize;
        if let Some(&a) = self.ret_gadgets.get(slot) {
            if a != 0 {
                return a;
            }
        }
        let a = if n == 0 { self.emit(&[0xc3]) } else { self.emit(&[0xc2, n as u8, (n >> 8) as u8]) };
        if self.ret_gadgets.len() <= slot {
            self.ret_gadgets.resize(slot + 1, 0);
        }
        self.ret_gadgets[slot] = a;
        a
    }

    /// Reserve a fixed-size block (e.g. a re-execution buffer).
    pub fn reserve(&mut self, len: u32) -> u32 {
        let at = self.base + self.used;
        assert!(self.used + len <= self.size, "arena exhausted");
        self.used += (len + 15) & !15;
        at
    }
}

pub fn jmp_rel32(from: u32, to: u32) -> [u8; 5] {
    let rel = to.wrapping_sub(from + 5).to_le_bytes();
    [0xe9, rel[0], rel[1], rel[2], rel[3]]
}

#[derive(Default)]
pub struct Hooks {
    pub list: Vec<Hook>,
}

impl Machine {
    /// Replace the code at `link_addr` (a link-time address) with a call to `handler`.
    /// `expect` must match the original bytes there; at least 5 bytes are overwritten.
    pub fn install_hook(&mut self, name: &'static str, link_addr: u32, expect: &[u8], handler: Handler) -> Result<(), String> {
        self.install_hook_arg(name, link_addr, expect, handler, 0)
    }

    pub fn install_hook_arg(
        &mut self,
        name: &'static str,
        link_addr: u32,
        expect: &[u8],
        handler: Handler,
        arg: u32,
    ) -> Result<(), String> {
        assert!(expect.len() >= 5, "hook {name}: need at least 5 expected bytes to patch");
        let addr = self.img.link(link_addr);
        let actual = unsafe { mem::slice(addr, expect.len() as u32) };
        if actual != expect {
            return Err(format!(
                "hook {name} at {link_addr:#x}: original bytes differ (expected {}, found {})",
                hex(expect),
                hex(actual)
            ));
        }
        let id = self.hooks.list.len() as u32;
        let stub = self.arena.reserve(10);
        let mut code = [0u8; 10];
        code[0] = 0x68;
        code[1..5].copy_from_slice(&id.to_le_bytes());
        code[5..10].copy_from_slice(&jmp_rel32(stub + 5, crate::guest::hook_common as *const () as usize as u32));
        unsafe { mem::slice_mut(stub, 10).copy_from_slice(&code) };
        unsafe { mem::slice_mut(addr, 5).copy_from_slice(&jmp_rel32(addr, stub)) };
        // poison the rest of the overwritten instruction(s) so a stray jump into them traps
        for i in 5..expect.len() as u32 {
            unsafe { mem::wr8(addr + i, 0xcc) };
        }
        self.hooks.list.push(Hook { name, addr, handler, arg, calls: 0 });
        crate::trace!("hook {name} installed at {addr:#x} (link {link_addr:#x})");
        Ok(())
    }

    /// Diagnostic hook that runs `handler` and then the original function unchanged: the
    /// overwritten prologue instructions are relocated into a trampoline (they must not be
    /// relative branches). `handler` receives the trampoline address as its argument and
    /// should return `HookResult::Resume(arg)`.
    /// Replace original bytes at `link_addr` (verified first) with `new`.
    pub fn patch(&mut self, what: &str, link_addr: u32, expect: &[u8], new: &[u8]) -> Result<(), String> {
        let addr = self.img.link(link_addr);
        let actual = unsafe { mem::slice(addr, expect.len() as u32) };
        if actual != expect {
            return Err(format!("{what} at {link_addr:#x}: original bytes differ ({})", hex(actual)));
        }
        unsafe { mem::slice_mut(addr, new.len() as u32).copy_from_slice(new) };
        Ok(())
    }

    pub fn install_observer(&mut self, name: &'static str, link_addr: u32, handler: Handler) -> Result<(), String> {
        use iced_x86::{Decoder, DecoderOptions, FlowControl};
        let addr = self.img.link(link_addr);
        let bytes = unsafe { mem::slice(addr, 32) }.to_vec();
        let mut dec = Decoder::with_ip(32, &bytes, addr as u64, DecoderOptions::NONE);
        let mut len = 0usize;
        while len < 5 {
            let ins = dec.decode();
            if ins.is_invalid() || ins.flow_control() != FlowControl::Next {
                return Err(format!("observer {name} at {link_addr:#x}: prologue has a branch"));
            }
            len += ins.len();
        }
        let tramp = self.arena.reserve(len as u32 + 5);
        unsafe {
            mem::slice_mut(tramp, len as u32).copy_from_slice(&bytes[..len]);
            mem::slice_mut(tramp + len as u32, 5).copy_from_slice(&jmp_rel32(tramp + len as u32, addr + len as u32));
        }
        let expect = bytes[..len.max(5)].to_vec();
        self.install_hook_arg(name, link_addr, &expect, handler, tramp)
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

/// Called from `guest::hook_common` with the saved guest registers.
pub fn dispatch(frame: &mut HookFrame) -> u32 {
    let m = crate::machine();
    let id = frame.slot as usize;
    let (handler, name, arg) = {
        let h = &mut m.hooks.list[id];
        h.calls += 1;
        (h.handler, h.name, h.arg)
    };
    crate::trace!("hook {name}: {}", crate::cpu::Cpu::dump(frame));
    match handler(m, frame, arg) {
        HookResult::Resume(addr) => addr,
        HookResult::Return(n) => m.arena.ret_gadget(n),
    }
}
