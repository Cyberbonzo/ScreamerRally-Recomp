//! Guest memory.
//!
//! The guest is a flat-model program: its linear addresses are plain host addresses in
//! this 32-bit process. The LE objects are relocated into one reserved region; heap blocks
//! requested through DPMI are ordinary host allocations.
//!
//! Real-mode ("conventional") memory cannot live at its natural place because Windows maps
//! its own data in the bottom megabyte, so DOS memory blocks are host buffers addressed
//! through *virtual* real-mode segments whose linear addresses lie below 0x10000. Windows
//! never maps that range, so every direct access faults and the trap layer redirects it
//! (see `lowmem`).

use std::ptr;

use sr_le::LeImage;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

/// Flat selectors of a 32-bit Windows process: user code, user data/stack.
pub const FLAT_CS: u16 = 0x23;
pub const FLAT_DS: u16 = 0x2b;

/// `VirtualAlloc` flag: track writes (GetWriteWatch).
const MEM_WRITE_WATCH: u32 = 0x0020_0000;

/// Allocate committed, zeroed host memory for the guest. `hint` may be 0. Writes are
/// tracked (`MEM_WRITE_WATCH`) so extra drawn frames can be undone (`interp`).
pub fn alloc(hint: u32, size: u32, exec: bool) -> Option<u32> {
    let prot = if exec { PAGE_EXECUTE_READWRITE } else { PAGE_READWRITE };
    let at = |a: u32| unsafe { VirtualAlloc(a as usize as *const _, size as usize, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, prot) };
    let p = at(hint);
    if p.is_null() { None } else { Some(p as usize as u32) }
}

/// Whether `len` bytes at `addr` are committed, readable memory (diagnostics only).
/// The committed, readable region (start, end) that contains `addr`, if any.
pub fn readable_region(addr: u32) -> Option<(u32, u32)> {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(addr as usize as *const _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    if n == 0 || mbi.State != MEM_COMMIT || mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
        return None;
    }
    let start = mbi.BaseAddress as usize;
    let end = (start + mbi.RegionSize).min(u32::MAX as usize);
    Some((start as u32, end as u32))
}

pub fn readable(addr: u32, len: u32) -> bool {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(addr as usize as *const _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    n != 0
        && mbi.State == MEM_COMMIT
        && mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0
        && (addr as usize + len as usize) <= mbi.BaseAddress as usize + mbi.RegionSize
}

pub fn free(addr: u32) {
    // deterministic mode: blocks inside the arena go back to it
    if let Some(a) = ARENA.lock().unwrap().as_mut() {
        if let Some(len) = a.blocks.remove(&addr) {
            unsafe { VirtualFree(addr as usize as *mut _, len as usize, MEM_DECOMMIT) };
            return;
        }
    }
    unsafe {
        VirtualFree(addr as usize as *mut _, 0, MEM_RELEASE);
    }
}

/// Memory the game asks for (DPMI blocks, its environment): in deterministic mode at the
/// same addresses in every run (see `deterministic_alloc`), otherwise like `alloc(0, ..)`.
pub fn alloc_guest(size: u32) -> Option<u32> {
    if crate::time::deterministic() {
        if let Some(p) = deterministic_alloc(size, PAGE_READWRITE) {
            return Some(p);
        }
    }
    alloc(0, size, false)
}

/// Executable memory whose addresses the game may keep (hook stubs it is handed as
/// callbacks): deterministic in deterministic mode, like `alloc_guest`.
pub fn alloc_guest_code(size: u32) -> Option<u32> {
    if crate::time::deterministic() {
        if let Some(p) = deterministic_alloc(size, PAGE_EXECUTE_READWRITE) {
            return Some(p);
        }
    }
    alloc(0, size, true)
}

/// Deterministic mode: one write-watched reservation, blocks placed first-fit, so every run
/// gets the same addresses for the same sequence of requests (pointers to this memory end
/// up in the game's data, which is compared between runs). Below 40000000h: the game breaks
/// with heap addresses above that, which Windows never gives it.
struct Arena {
    base: u32,
    end: u32,
    /// Allocated blocks: address -> length (64 KB granular).
    blocks: std::collections::BTreeMap<u32, u32>,
}

static ARENA: std::sync::Mutex<Option<Arena>> = std::sync::Mutex::new(None);
static ARENA_TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The deterministic arena (base, size); the launcher keeps it free with a placeholder.
pub const DETERMINISTIC_ARENA: (u32, u32) = (0x1000_0000, 0x1000_0000);

fn deterministic_alloc(size: u32, prot: u32) -> Option<u32> {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    let (base, len) = DETERMINISTIC_ARENA;
    let mut arena = ARENA.lock().unwrap();
    if arena.is_none() && !ARENA_TRIED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        unsafe {
            // the launcher's placeholder: release it, reserve again with write watching
            let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQuery(base as usize as *const _, &mut mbi, std::mem::size_of_val(&mbi)) != 0
                && mbi.State == MEM_RESERVE
                && mbi.AllocationBase as usize as u32 == base
            {
                VirtualFree(base as usize as *mut _, 0, MEM_RELEASE);
            }
            let p = VirtualAlloc(base as usize as *const _, len as usize, MEM_RESERVE | MEM_WRITE_WATCH, PAGE_EXECUTE_READWRITE);
            if p.is_null() {
                crate::warn!("deterministic mode: the guest arena at {base:#010x} is not available; addresses will differ between runs");
                return None;
            }
            *arena = Some(Arena { base, end: base + len, blocks: Default::default() });
            crate::info!("deterministic mode: guest allocations from {base:#010x}");
        }
    }
    let a = arena.as_mut()?;
    let want = (size.max(1) + 0xffff) & !0xffff;
    // first fit
    let mut at = a.base;
    for (&b, &l) in a.blocks.iter() {
        if b >= at && b - at >= want {
            break;
        }
        at = at.max(b + l);
    }
    if at as u64 + want as u64 > a.end as u64 {
        crate::warn!("deterministic mode: arena full ({size:#x} bytes asked); this block's address will differ between runs");
        return None;
    }
    let p = unsafe { VirtualAlloc(at as usize as *const _, size as usize, MEM_COMMIT, prot) };
    if p.is_null() {
        return None;
    }
    // committed pages come back zeroed only when fresh: clear reused ones
    unsafe { std::ptr::write_bytes(at as usize as *mut u8, 0, size as usize) };
    a.blocks.insert(at, want);
    crate::debug!("deterministic mode: {size:#x} bytes at {at:#010x}");
    Some(at)
}

/// Raw guest memory access. Guest addresses are host addresses; callers must only pass
/// addresses the guest legitimately owns.
#[inline]
pub unsafe fn ptr(addr: u32) -> *mut u8 {
    addr as usize as *mut u8
}

#[inline]
pub unsafe fn rd8(a: u32) -> u8 {
    unsafe { ptr::read_volatile(ptr(a)) }
}
#[inline]
pub unsafe fn rd16(a: u32) -> u16 {
    unsafe { ptr::read_unaligned(ptr(a) as *const u16) }
}
#[inline]
pub unsafe fn rd32(a: u32) -> u32 {
    unsafe { ptr::read_unaligned(ptr(a) as *const u32) }
}
#[inline]
pub unsafe fn wr8(a: u32, v: u8) {
    unsafe { ptr::write_volatile(ptr(a), v) }
}
#[inline]
pub unsafe fn wr16(a: u32, v: u16) {
    unsafe { ptr::write_unaligned(ptr(a) as *mut u16, v) }
}
#[inline]
pub unsafe fn wr32(a: u32, v: u32) {
    unsafe { ptr::write_unaligned(ptr(a) as *mut u32, v) }
}
pub unsafe fn slice<'a>(a: u32, len: u32) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(ptr(a), len as usize) }
}
pub unsafe fn slice_mut<'a>(a: u32, len: u32) -> &'a mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(ptr(a), len as usize) }
}
/// Read a NUL-terminated string (at most `max` bytes).
pub unsafe fn cstr(a: u32, max: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..max {
        let c = unsafe { rd8(a + i) };
        if c == 0 {
            break;
        }
        v.push(c);
    }
    v
}

/// One LE object after loading.
#[derive(Clone, Debug)]
pub struct LoadedObject {
    pub number: u32,
    pub link_base: u32,
    pub addr: u32,
    pub size: u32,
    pub code: bool,
}

/// The relocated original executable.
pub struct LoadedImage {
    pub base: u32,
    pub size: u32,
    pub objects: Vec<LoadedObject>,
    pub entry: u32,
    pub stack_top: u32,
    /// First byte after the auto-data object (Watcom's `_end` lies inside it).
    pub data_end: u32,
}

impl LoadedImage {
    /// Map an LE image at `base` (0 = anywhere) and apply all fixups.
    pub fn load(le: &LeImage, base: u32) -> Result<LoadedImage, String> {
        let span = le.span();
        // deterministic mode: the first block of the guest arena (the same in every run)
        let det = if crate::time::deterministic() { deterministic_alloc(span, PAGE_EXECUTE_READWRITE) } else { None };
        let region = det
            .or_else(|| alloc(base, span, true))
            .or_else(|| alloc(0, span, true))
            .ok_or_else(|| format!("cannot reserve {span:#x} bytes for the game image"))?;
        let bases = le.layout_at(region);
        let images = le
            .relocate(&bases, |obj| {
                let o = le.object(obj).unwrap();
                if o.is_code() { FLAT_CS } else { FLAT_DS }
            })
            .map_err(|e| e.to_string())?;
        let mut objects = Vec::new();
        for ((o, img), addr) in le.objects.iter().zip(images.iter()).zip(bases.iter()) {
            unsafe { ptr::copy_nonoverlapping(img.as_ptr(), ptr(*addr), img.len()) };
            objects.push(LoadedObject {
                number: o.number,
                link_base: o.link_base,
                addr: *addr,
                size: o.mapped_size(le.page_size),
                code: o.is_code(),
            });
        }
        let at = |obj: u32, off: u32| bases[(obj - 1) as usize].wrapping_add(off);
        let auto = &objects[(le.auto_data_object - 1) as usize];
        Ok(LoadedImage {
            base: region,
            size: span,
            entry: at(le.entry_object, le.entry_offset),
            stack_top: at(le.stack_object, le.stack_offset),
            data_end: auto.addr + auto.size,
            objects,
        })
    }

    /// Translate a link-time address (as used in reverse-engineering notes) to the loaded one.
    pub fn link(&self, link_addr: u32) -> u32 {
        for o in &self.objects {
            if link_addr >= o.link_base && link_addr < o.link_base + o.size {
                return o.addr + (link_addr - o.link_base);
            }
        }
        panic!("link address {link_addr:#x} is outside the image");
    }

    /// Translate a loaded address back to its link-time address, if it is in the image.
    pub fn to_link(&self, addr: u32) -> Option<u32> {
        self.objects
            .iter()
            .find(|o| addr >= o.addr && addr < o.addr + o.size)
            .map(|o| o.link_base + (addr - o.addr))
    }

    /// Expected original bytes of an instruction as stored at link time, with its absolute
    /// displacement / immediate relocated if it points into the image (as DOS/4GW's fixups
    /// would have done). Exact: only the instruction's real constant fields are touched.
    pub fn relocate_expected(&self, link_addr: u32, link_bytes: &[u8]) -> Vec<u8> {
        use iced_x86::{Decoder, DecoderOptions};
        let mut out = link_bytes.to_vec();
        let mut decoder = Decoder::with_ip(32, link_bytes, link_addr as u64, DecoderOptions::NONE);
        let ins = decoder.decode();
        let offs = decoder.get_constant_offsets(&ins);
        let mut fix = |off: usize, size: usize| {
            if size == 4 && off + 4 <= out.len() {
                let v = u32::from_le_bytes(out[off..off + 4].try_into().unwrap());
                if let Some(o) = self.objects.iter().find(|o| v >= o.link_base && v < o.link_base + o.size) {
                    out[off..off + 4].copy_from_slice(&(o.addr + (v - o.link_base)).to_le_bytes());
                }
            }
        };
        if offs.has_displacement() {
            fix(offs.displacement_offset(), offs.displacement_size());
        }
        if offs.has_immediate() {
            fix(offs.immediate_offset(), offs.immediate_size());
        }
        out
    }

    pub fn is_code(&self, addr: u32) -> bool {
        self.objects.iter().any(|o| o.code && addr >= o.addr && addr < o.addr + o.size)
    }

    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.base && addr < self.base + self.size
    }
}

/// Conventional ("DOS") memory.
///
/// Preferred: **real** conventional memory. The launcher starts the game process suspended
/// and reserves linear 0C0000h-0FFFFFh before Windows' loader fills the bottom megabyte
/// (see `apps/screamer`), so DOS blocks get their true linear address `segment * 16` and
/// the game's flat accesses to them run natively.
///
/// Fallback (and always for the IVT/BIOS data area at 0000h-05FFh): *virtual* blocks whose
/// segments lie below 1000h. Windows never maps that range, so every access faults and the
/// trap layer redirects it to the host buffer.
pub mod lowmem {
    use std::collections::BTreeMap;

    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualAlloc, VirtualQuery};

    pub const REAL_BASE: u32 = 0x000c_0000;
    pub const REAL_END: u32 = 0x0010_0000;

    pub struct LowMem {
        /// segment -> (host address, size in bytes); host == linear for real blocks
        blocks: BTreeMap<u16, (u32, u32)>,
        virt_next: u16,
        /// Free ranges of the real region, in paragraphs: (segment, count).
        real_free: Option<Vec<(u32, u32)>>,
    }

    impl Default for LowMem {
        fn default() -> Self {
            // segments 0000h-005Fh stay reserved for the IVT/BDA image.
            LowMem { blocks: BTreeMap::new(), virt_next: 0x0060, real_free: None }
        }
    }

    /// Is the launcher's reservation of the real conventional memory region present?
    pub fn real_region_reserved() -> bool {
        let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { VirtualQuery(REAL_BASE as usize as *const _, &mut mbi, std::mem::size_of_val(&mbi)) } != 0;
        ok && mbi.State == MEM_RESERVE && mbi.AllocationBase as usize as u32 == REAL_BASE
    }

    impl LowMem {
        pub fn enable_real(&mut self) {
            self.real_free = Some(vec![(REAL_BASE >> 4, (REAL_END - REAL_BASE) >> 4)]);
        }

        pub fn is_real(&self) -> bool {
            self.real_free.is_some()
        }

        /// Allocate `paras` paragraphs. Returns (segment, host address).
        pub fn alloc(&mut self, paras: u16) -> Option<(u16, u32)> {
            let paras = (paras as u32).max(1);
            if let Some(free) = &mut self.real_free {
                let i = free.iter().position(|&(_, n)| n >= paras)?;
                let (seg, n) = free[i];
                if n == paras {
                    free.remove(i);
                } else {
                    free[i] = (seg + paras, n - paras);
                }
                let addr = seg * 16;
                let size = paras * 16;
                let p = unsafe { VirtualAlloc(addr as usize as *const _, size as usize, MEM_COMMIT, PAGE_READWRITE) };
                if p.is_null() {
                    return None;
                }
                unsafe { std::ptr::write_bytes(addr as usize as *mut u8, 0, size as usize) };
                self.blocks.insert(seg as u16, (addr, size));
                return Some((seg as u16, addr));
            }
            let size = paras * 16;
            let seg = self.virt_next;
            let end = seg as u32 + paras;
            if end > 0x1000 {
                return None; // the unmapped window is 64 KB
            }
            let host = super::alloc(0, size.div_ceil(4096) * 4096, false)?;
            self.blocks.insert(seg, (host, size));
            self.virt_next = end as u16;
            Some((seg, host))
        }

        /// Place a virtual block at a fixed segment (the IVT/BIOS data area at 0000h).
        pub fn alloc_at(&mut self, seg: u16, paras: u16) -> Option<u32> {
            let size = paras as u32 * 16;
            if self.blocks.range(..seg + paras).next_back().is_some_and(|(&s, &(_, sz))| s as u32 * 16 + sz > seg as u32 * 16) {
                return None;
            }
            let host = super::alloc(0, size.div_ceil(4096) * 4096, false)?;
            self.blocks.insert(seg, (host, size));
            Some(host)
        }

        pub fn free(&mut self, seg: u16) -> bool {
            let Some((host, size)) = self.blocks.remove(&seg) else { return false };
            if host == seg as u32 * 16 {
                if let Some(free) = &mut self.real_free {
                    free.push((seg as u32, size / 16));
                    free.sort_unstable();
                    // merge neighbours
                    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(free.len());
                    for &(s, n) in free.iter() {
                        match merged.last_mut() {
                            Some(last) if last.0 + last.1 == s => last.1 += n,
                            _ => merged.push((s, n)),
                        }
                    }
                    *free = merged;
                }
            } else {
                super::free(host);
            }
            true
        }

        /// Translate a linear conventional-memory address into host memory.
        pub fn translate(&self, linear: u32) -> Option<u32> {
            if self.real_free.is_some() && (REAL_BASE..REAL_END).contains(&linear) {
                return Some(linear);
            }
            let seg = (linear >> 4) as u16;
            let (&bseg, &(host, size)) = self.blocks.range(..=seg).next_back()?;
            let off = linear.checked_sub(bseg as u32 * 16)?;
            (off < size).then_some(host + off)
        }

        pub fn host_of_segment(&self, seg: u16) -> Option<u32> {
            self.blocks.get(&seg).map(|b| b.0)
        }
    }
}
