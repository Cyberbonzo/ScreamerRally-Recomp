//! Translation of segment-dependent guest instructions into native code (no traps).
//!
//! The game's asm modules address tables through selectors with non-zero bases
//! (`mov eax, fs:[ebx]`, `cmp dword ptr fs:[eax*2], 0`) and reload those selectors many
//! times per frame (`mov fs, [147308]`). On WOW64 such selectors cannot exist, so every one
//! of these instructions used to trap and be emulated (~150 000 exceptions per second while
//! racing). This module rewrites them, the first time they trap, into small stubs in the
//! runtime arena that do the same with ordinary flat instructions:
//!
//! * a memory access through a segment with a virtual base becomes
//!   `push t; pushfd; lea t, [address]; add t, [VBASE + seg]; popfd; <op> [t]; pop t`
//!   (the live virtual base is used, so the translation is exact for any selector);
//! * a segment load becomes `push t; pushfd; movzx t, source; mov [VSEL + seg], t;
//!   shr t, 3; mov t, [SELBASE + t*4]; mov [VBASE + seg], t; popfd; pop t`
//!   (the real register stays flat, as the trap handler leaves it).
//!
//! The original instruction is replaced by `jmp stub`. When it is shorter than 5 bytes the
//! following instructions are moved into the stub too (re-encoded by iced's block encoder,
//! so relative branches keep their targets); their old bytes become `int3`, and a jump into
//! them (never observed, but possible in hand-written asm) is redirected to the matching
//! place in the stub by the trap handler. Instructions the game itself rewrites at run time
//! (self-modifying code, found by scanning for writes into the code objects) and the
//! runtime's own patch sites are never moved. Anything that cannot be translated simply
//! keeps trapping, as before.

use std::collections::{HashMap, HashSet};

use iced_x86::{
    BlockEncoder, BlockEncoderOptions, Code, Decoder, DecoderOptions, FlowControl, Instruction, InstructionBlock,
    InstructionInfoFactory, MemoryOperand, Mnemonic, OpKind, Register,
};

use crate::Machine;
use crate::mem;
use crate::sel::{SELBASE, Seg, VBASE, VSEL};

#[derive(Default)]
pub struct SegJit {
    /// Sites already considered (translated or refused).
    tried: HashSet<u32>,
    /// Old address of a moved instruction -> its place in a stub.
    pub redirect: HashMap<u32, u32>,
    /// Bytes that must not be moved: written by the game (self-modifying code), runtime
    /// patch sites, already-translated windows.
    protected: HashSet<u32>,
    pub translated: u32,
    pub refused: u32,
    /// While set, every patch written to guest code is also logged here (extra frames
    /// put guest memory back; these must survive).
    pub patch_log: Option<Vec<(u32, Vec<u8>)>>,
    pub enabled: bool,
}

const SCRATCH: [Register; 7] = [Register::ECX, Register::EDX, Register::EBX, Register::ESI, Register::EDI, Register::EAX, Register::EBP];

fn seg_of(r: Register) -> Option<Seg> {
    Some(match r {
        Register::ES => Seg::Es,
        Register::CS => Seg::Cs,
        Register::SS => Seg::Ss,
        Register::DS => Seg::Ds,
        Register::FS => Seg::Fs,
        Register::GS => Seg::Gs,
        _ => return None,
    })
}

fn vbase_addr(seg: Seg) -> u32 {
    VBASE.as_ptr() as u32 + seg as u32 * 4
}

fn vsel_addr(seg: Seg) -> u32 {
    VSEL.as_ptr() as u32 + seg as u32 * 4
}

fn abs(addr: u32) -> MemoryOperand {
    MemoryOperand::new(Register::None, Register::None, 1, addr as i64, 4, false, Register::None)
}

fn i1(code: Code, r: Register) -> Instruction {
    Instruction::with1(code, r).expect("encodable")
}

impl SegJit {
    /// Scan the image for instructions that write into its code objects (self-modifying
    /// code); those bytes are never moved.
    pub fn init(m: &mut Machine, extra_protected: &[(u32, u32)]) {
        let mut protected = HashSet::new();
        for o in m.img.objects.iter().filter(|o| o.code) {
            let bytes = unsafe { mem::slice(o.addr, o.size) };
            let mut dec = Decoder::with_ip(32, bytes, o.addr as u64, DecoderOptions::NONE);
            let mut ins = Instruction::default();
            while dec.can_decode() {
                dec.decode_out(&mut ins);
                if ins.is_invalid() {
                    continue;
                }
                if ins.op_count() > 0 && ins.op0_kind() == OpKind::Memory && ins.memory_base() == Register::None && ins.memory_index() == Register::None {
                    let t = ins.memory_displacement32();
                    let size = ins.memory_size().size().max(1) as u32;
                    if m.img.is_code(t) {
                        for b in t..t + size {
                            protected.insert(b);
                        }
                    }
                }
            }
        }
        for &(a, len) in extra_protected {
            for b in a..a + len {
                protected.insert(b);
            }
        }
        for h in &m.hooks.list {
            for b in h.addr..h.addr + 5 {
                protected.insert(b);
            }
        }
        crate::info!("segment translation: {} protected code bytes", protected.len());
        m.segjit.protected = protected;
        m.segjit.enabled = std::env::var_os("SR_NO_SEGJIT").is_none();
    }
}

/// Bytes of the instruction at `addr` as the original program had them (segment sites
/// carry an `int3` in their first byte).
fn original_bytes(m: &Machine, addr: u32) -> [u8; 16] {
    if let Some(b) = m.seg_sites.get(&addr) {
        return *b;
    }
    let mut b = [0u8; 16];
    b.copy_from_slice(unsafe { mem::slice(addr, 16) });
    b
}

/// Registers an instruction reads or writes (as 32-bit registers).
fn used_regs(ins: &Instruction) -> HashSet<Register> {
    let mut f = InstructionInfoFactory::new();
    let info = f.info(ins);
    let mut s: HashSet<Register> = info.used_registers().iter().map(|u| u.register().full_register32()).collect();
    s.insert(ins.memory_base().full_register32());
    s.insert(ins.memory_index().full_register32());
    s
}

fn pick_scratch(ins: &Instruction) -> Option<Register> {
    let used = used_regs(ins);
    SCRATCH.iter().copied().find(|r| !used.contains(r))
}

/// Segment whose virtual base an access must add, if the instruction is a translatable
/// memory access through such a segment.
fn access_segment(ins: &Instruction) -> Option<Seg> {
    let mem_ops = (0..ins.op_count()).filter(|&i| ins.op_kind(i) == OpKind::Memory).count();
    let other_mem = (0..ins.op_count()).any(|i| matches!(ins.op_kind(i), OpKind::MemorySegSI | OpKind::MemorySegESI | OpKind::MemorySegDI | OpKind::MemorySegEDI | OpKind::MemoryESDI | OpKind::MemoryESEDI));
    if mem_ops != 1 || other_mem || ins.memory_base() == Register::ESP || ins.memory_index() == Register::ESP {
        return None;
    }
    if ins.stack_pointer_increment() != 0 || ins.mnemonic() == Mnemonic::Lea {
        return None;
    }
    let seg = seg_of(ins.memory_segment())?;
    matches!(seg, Seg::Es | Seg::Ds | Seg::Fs | Seg::Gs).then_some(seg)
}

/// `mov sreg, r/m16` with a translatable source.
fn load_target(ins: &Instruction) -> Option<Seg> {
    if ins.mnemonic() != Mnemonic::Mov || ins.op0_kind() != OpKind::Register {
        return None;
    }
    let seg = seg_of(ins.op0_register())?;
    if !matches!(seg, Seg::Es | Seg::Ds | Seg::Fs | Seg::Gs) {
        return None;
    }
    match ins.op1_kind() {
        OpKind::Register => Some(seg),
        OpKind::Memory if matches!(ins.memory_segment(), Register::CS | Register::DS | Register::SS) && ins.memory_base() != Register::ESP => Some(seg),
        _ => None,
    }
}

fn emit_access(out: &mut Vec<Instruction>, ins: &Instruction, seg: Seg) -> Option<()> {
    let t = pick_scratch(ins)?;
    out.push(i1(Code::Push_r32, t));
    out.push(Instruction::with(Code::Pushfd));
    let ea = MemoryOperand::new(ins.memory_base(), ins.memory_index(), ins.memory_index_scale(), ins.memory_displacement32() as i32 as i64, if ins.memory_displacement32() != 0 || (ins.memory_base() == Register::None && ins.memory_index() == Register::None) { 4 } else { 0 }, false, Register::None);
    out.push(Instruction::with2(Code::Lea_r32_m, t, ea).ok()?);
    out.push(Instruction::with2(Code::Add_r32_rm32, t, abs(vbase_addr(seg))).ok()?);
    out.push(Instruction::with(Code::Popfd));
    let mut n = *ins;
    n.set_segment_prefix(Register::None);
    n.set_memory_base(t);
    n.set_memory_index(Register::None);
    n.set_memory_index_scale(1);
    n.set_memory_displacement32(0);
    n.set_memory_displ_size(0);
    out.push(n);
    out.push(i1(Code::Pop_r32, t));
    Some(())
}

fn emit_load(out: &mut Vec<Instruction>, ins: &Instruction, seg: Seg) -> Option<()> {
    let t = pick_scratch(ins)?;
    out.push(i1(Code::Push_r32, t));
    out.push(Instruction::with(Code::Pushfd));
    match ins.op1_kind() {
        OpKind::Register => {
            let src = ins.op1_register();
            let src16 = match src {
                Register::EAX => Register::AX,
                Register::ECX => Register::CX,
                Register::EDX => Register::DX,
                Register::EBX => Register::BX,
                Register::ESP => Register::SP,
                Register::EBP => Register::BP,
                Register::ESI => Register::SI,
                Register::EDI => Register::DI,
                r if r.size() == 2 => r,
                _ => return None,
            };
            out.push(Instruction::with2(Code::Movzx_r32_rm16, t, src16).ok()?);
        }
        _ => {
            let disp = ins.memory_displacement32();
            let ea = MemoryOperand::new(ins.memory_base(), ins.memory_index(), ins.memory_index_scale(), disp as i32 as i64, if disp != 0 || (ins.memory_base() == Register::None && ins.memory_index() == Register::None) { 4 } else { 0 }, false, Register::None);
            out.push(Instruction::with2(Code::Movzx_r32_rm16, t, ea).ok()?);
        }
    }
    out.push(Instruction::with2(Code::Mov_rm32_r32, abs(vsel_addr(seg)), t).ok()?);
    out.push(Instruction::with2(Code::Shr_rm32_imm8, t, 3u32).ok()?);
    out.push(Instruction::with2(Code::Mov_r32_rm32, t, MemoryOperand::new(Register::None, t, 4, SELBASE.as_ptr() as u32 as i64, 4, false, Register::None)).ok()?);
    out.push(Instruction::with2(Code::Mov_rm32_r32, abs(vbase_addr(seg)), t).ok()?);
    // like the trap handler: the real register holds the flat selector (FS keeps the TEB)
    if seg != Seg::Fs {
        let sreg = match seg {
            Seg::Es => Register::ES,
            Seg::Ds => Register::DS,
            _ => Register::GS,
        };
        out.push(Instruction::with2(Code::Mov_r32_imm32, t, crate::mem::FLAT_DS as u32).ok()?);
        out.push(Instruction::with2(Code::Mov_Sreg_r32m16, sreg, t).ok()?);
    }
    out.push(Instruction::with(Code::Popfd));
    out.push(i1(Code::Pop_r32, t));
    Some(())
}

fn is_trapping(ins: &Instruction) -> bool {
    matches!(
        ins.mnemonic(),
        Mnemonic::Int | Mnemonic::Int1 | Mnemonic::Int3 | Mnemonic::Into | Mnemonic::In | Mnemonic::Out | Mnemonic::Insb
            | Mnemonic::Insw | Mnemonic::Insd | Mnemonic::Outsb | Mnemonic::Outsw | Mnemonic::Outsd | Mnemonic::Cli
            | Mnemonic::Sti | Mnemonic::Hlt | Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Lds | Mnemonic::Les
            | Mnemonic::Lfs | Mnemonic::Lgs | Mnemonic::Lss
    ) || (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && seg_of(ins.op_register(i)).is_some())
}

/// Try to translate the guest instruction at `site` (which just trapped). On success the
/// guest should continue at the returned stub address (which performs the instruction).
pub fn translate(m: &mut Machine, site: u32) -> Option<u32> {
    if !m.segjit.enabled || !m.segjit.tried.insert(site) || !m.img.contains(site) {
        return None;
    }
    let r = build(m, site);
    match r {
        Some(stub) => {
            m.segjit.translated += 1;
            crate::debug!("segment translation: {} -> stub {stub:#x}", m.where_(site));
            Some(stub)
        }
        None => {
            m.segjit.refused += 1;
            crate::debug!("segment translation refused at {}", m.where_(site));
            None
        }
    }
}

fn build(m: &mut Machine, site: u32) -> Option<u32> {
    // the window: the site plus following instructions up to at least 5 bytes
    let mut window: Vec<Instruction> = Vec::new();
    let mut addr = site;
    while addr - site < 5 {
        if m.segjit.redirect.contains_key(&addr) {
            return None;
        }
        let bytes = original_bytes(m, addr);
        let ins = Decoder::with_ip(32, &bytes, addr as u64, DecoderOptions::NONE).decode();
        if ins.is_invalid() {
            return None;
        }
        let len = ins.len() as u32;
        if (addr..addr + len).any(|b| m.segjit.protected.contains(&b)) {
            return None;
        }
        window.push(ins);
        addr += len;
        match ins.flow_control() {
            FlowControl::Next => {}
            FlowControl::UnconditionalBranch | FlowControl::Return | FlowControl::IndirectBranch => break,
            _ if addr - site >= 5 => break,
            _ => return None,
        }
    }
    if addr - site < 5 {
        return None;
    }
    let end = addr;

    // the stub
    let mut out: Vec<Instruction> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for (k, ins) in window.iter().enumerate() {
        starts.push(out.len());
        let explicit = matches!(ins.segment_prefix(), Register::FS | Register::GS | Register::ES);
        let based = |s: Seg| VBASE[s as usize].load(std::sync::atomic::Ordering::Relaxed) != 0;
        if let Some(seg) = load_target(ins) {
            emit_load(&mut out, ins, seg)?;
        } else if let Some(seg) = access_segment(ins).filter(|&s| explicit || (k == 0 && based(s))) {
            emit_access(&mut out, ins, seg)?;
        } else if k == 0 || is_trapping(ins) {
            // the trapping instruction itself must be one we can translate
            return None;
        } else {
            out.push(*ins);
        }
    }
    let mut back = Instruction::with_branch(Code::Jmp_rel32_32, end as u64).ok()?;
    back.set_ip(0);
    out.push(back);
    // generated instructions need distinct IPs that are not branch targets
    let mut fake = 0xfff0_0000u64;
    for ins in out.iter_mut() {
        if ins.ip() == 0 {
            ins.set_ip(fake);
            fake += 16;
        }
    }
    let rip = m.arena.peek();
    let block = InstructionBlock::new(&out, rip as u64);
    let res = BlockEncoder::encode(32, block, BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS).ok()?;
    let code = res.code_buffer;
    if code.len() > 512 {
        return None;
    }
    let stub = m.arena.emit(&code);
    debug_assert_eq!(stub, rip);

    // patch the original: jmp stub, int3 over the rest of the window
    unsafe {
        mem::slice_mut(site, 5).copy_from_slice(&crate::hooks::jmp_rel32(site, stub));
        for b in site + 5..end {
            mem::wr8(b, 0xcc);
        }
    }
    if let Some(log) = m.segjit.patch_log.as_mut() {
        log.push((site, unsafe { mem::slice(site, end - site) }.to_vec()));
    }
    let offsets = res.new_instruction_offsets;
    for (k, ins) in window.iter().enumerate() {
        let a = ins.ip() as u32;
        m.seg_sites.remove(&a);
        if k > 0 {
            m.segjit.redirect.insert(a, stub + offsets[starts[k]]);
        }
    }
    for b in site..end {
        m.segjit.protected.insert(b);
    }
    Some(stub)
}
