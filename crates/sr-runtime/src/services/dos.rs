//! DOS services (`int 21h`) as seen by a DOS/4GW program.
//!
//! DOS/4GW extends the register interface to 32 bits: buffers are flat `DS:EDX` /
//! `DS:ESI` pointers and transfer counts are in `ECX`. Files are served by `Vfs`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

use sr_disc::DirEntry;
use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;

use crate::Machine;
use crate::cpu::{Cpu, ZF};
use crate::mem::{self, FLAT_CS, FLAT_DS};
use crate::vfs::{self, ListEntry, Node};

// DOS error codes
const E_FUNC: u16 = 1;
const E_NOFILE: u16 = 2;
const E_NOPATH: u16 = 3;
const E_HANDLES: u16 = 4;
const E_ACCESS: u16 = 5;
const E_HANDLE: u16 = 6;
const E_NOMORE: u16 = 18;

pub enum OpenFile {
    /// stdin / stdout / stderr / aux / prn
    Device(u8),
    Host { file: File, writable: bool },
    Cd { entry: DirEntry, pos: u64 },
}

pub struct Dos {
    pub handles: Vec<Option<(OpenFile, String)>>,
    pub dta: u32,
    searches: HashMap<u32, (Vec<ListEntry>, usize)>,
    pub psp_sel: u16,
    pub psp_seg: u16,
    pub verify: bool,
    pub break_check: bool,
}

impl Default for Dos {
    fn default() -> Self {
        let mut handles = Vec::new();
        for d in 0..5 {
            handles.push(Some((OpenFile::Device(d), ["CON", "CON", "CON", "AUX", "PRN"][d as usize].to_string())));
        }
        Dos { handles, dta: 0, searches: HashMap::new(), psp_sel: 0, psp_seg: 0, verify: false, break_check: false }
    }
}

impl Dos {
    fn alloc_handle(&mut self, f: OpenFile, name: String) -> Option<u16> {
        let slot = self.handles.iter().position(|h| h.is_none());
        let idx = match slot {
            Some(i) => i,
            None if self.handles.len() < 255 => {
                self.handles.push(None);
                self.handles.len() - 1
            }
            None => return None,
        };
        self.handles[idx] = Some((f, name));
        Some(idx as u16)
    }
}

fn path_arg(addr: u32) -> Vec<u8> {
    unsafe { mem::cstr(addr, 260) }
}

/// Host address of a guest buffer `DS:addr` of `len` bytes. The game passes flat pointers,
/// also into conventional (DOS) memory, which lives at virtual linear addresses below
/// 64 KB: those are translated (the guest's own accesses fault and are translated by the
/// trap layer; the runtime must do the same before touching them).
fn host_buf(m: &mut Machine, addr: u32, len: u32) -> Result<u32, String> {
    let linear = m.sel.base(crate::sel::Seg::Ds).wrapping_add(addr);
    if linear >= 0x10000 {
        return Ok(linear);
    }
    let host = m.lowmem.translate(linear).ok_or_else(|| format!("DOS buffer at unmapped conventional memory {linear:#x}"))?;
    if len > 1 && m.lowmem.translate(linear + len - 1) != Some(host + len - 1) {
        return Err(format!("DOS buffer {linear:#x}+{len:#x} crosses a conventional memory block"));
    }
    Ok(host)
}

fn io_log() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    static LOG: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| std::env::var_os("SR_IOLOG").and_then(|p| std::fs::File::create(p).ok()).map(std::sync::Mutex::new)).as_ref()
}

fn cd_read(m: &Machine, entry: &DirEntry, pos: u64, buf: &mut [u8]) -> usize {
    let size = entry.size as u64;
    if pos >= size {
        return 0;
    }
    let len = (buf.len() as u64).min(size - pos) as usize;
    let first = pos / 2048;
    let skip = (pos % 2048) as usize;
    let Some(disc) = m.vfs.disc else { return 0 };
    match disc.read_data(entry.lba + first as u32, skip + len) {
        Ok(data) => {
            buf[..len].copy_from_slice(&data[skip..skip + len]);
            len
        }
        Err(e) => {
            crate::error!("CD read failed: {e}");
            0
        }
    }
}

fn fill_dta(dta: u32, e: &ListEntry) {
    unsafe {
        mem::wr8(dta + 0x15, if e.is_dir { 0x10 } else { 0x00 } | if e.read_only { 0x01 } else { 0 });
        mem::wr16(dta + 0x16, e.time);
        mem::wr16(dta + 0x18, e.date);
        mem::wr32(dta + 0x1a, e.size);
        let name = e.name.as_bytes();
        for i in 0..13u32 {
            mem::wr8(dta + 0x1e + i, *name.get(i as usize).unwrap_or(&0));
        }
        mem::wr8(dta + 0x1e + 12, 0);
    }
}

fn find_next(m: &mut Machine, c: &mut CONTEXT) {
    let dta = m.dos.dta;
    let Some((list, idx)) = m.dos.searches.get_mut(&dta) else {
        c.fail(E_NOMORE);
        return;
    };
    if *idx >= list.len() {
        m.dos.searches.remove(&dta);
        c.fail(E_NOMORE);
        return;
    }
    let e = list[*idx].clone();
    *idx += 1;
    fill_dta(dta, &e);
    c.ok();
}

pub fn int21(m: &mut Machine, c: &mut CONTEXT) -> Result<(), String> {
    let ah = c.ah();
    crate::trace!("int 21h ah={ah:02x} {}", c.dump());
    match ah {
        0x02 => {
            crate::info!("[dos con] {}", c.dl() as char);
        }
        0x06 => {
            if c.dl() == 0xff {
                c.set_flag(ZF, true);
                c.set_al(0);
            }
        }
        0x09 => {
            let s = unsafe { mem::cstr(c.edx(), 4096) };
            let s: Vec<u8> = s.into_iter().take_while(|&b| b != b'$').collect();
            crate::info!("[dos con] {}", String::from_utf8_lossy(&s));
        }
        0x0e => {
            if m.vfs.drive_exists(c.dl()) {
                m.vfs.cur_drive = c.dl();
            }
            c.set_al(26);
        }
        0x19 => c.set_al(m.vfs.cur_drive),
        0x1a => m.dos.dta = c.edx(),
        0x25 => {
            m.vector_set(c.al(), c.edx());
            crate::debug!("int 21h/25h: vector {:02x}h -> {}", c.al(), m.where_(c.edx()));
        }
        0x2a => {
            let t = crate::time::local_now();
            c.set_cx(t.year);
            c.set_dh(t.month);
            c.set_dl(t.day);
            c.set_al(t.weekday);
        }
        0x2c => {
            let t = crate::time::local_now();
            c.set_ch(t.hour);
            c.set_cl(t.minute);
            c.set_dh(t.second);
            c.set_dl(t.centisecond);
        }
        0x2f => {
            c.Ebx = m.dos.dta;
            m.sel.set_virt(crate::sel::Seg::Es, FLAT_DS, 0);
        }
        0x30 => {
            // DOS 6.22. The upper half of EAX must not read 'DX'/'BC' (Phar Lap / other
            // extenders), so it is cleared.
            c.Eax = 0x1606;
            c.Ebx = 0;
            c.Ecx = 0;
        }
        0x33 => match c.al() {
            0 => c.set_dl(m.dos.break_check as u8),
            1 => m.dos.break_check = c.dl() != 0,
            5 => c.set_dl(3), // boot drive C:
            6 => {
                c.set_bx(0x1606);
                c.set_dx(0);
            }
            _ => {}
        },
        0x35 => {
            let (_, off) = m.pm_vectors[c.al() as usize];
            c.Ebx = off;
            m.sel.set_virt(crate::sel::Seg::Es, FLAT_CS, 0);
        }
        0x36 => {
            if c.dl() == 0 || m.vfs.drive_exists(c.dl() - 1) {
                c.set_ax(64); // sectors per cluster
                c.set_bx(0xffff); // free clusters
                c.set_cx(512); // bytes per sector
                c.set_dx(0xffff); // total clusters
            } else {
                c.set_ax(0xffff);
            }
        }
        0x39 | 0x3a => {
            let abs = m.vfs.absolute(&path_arg(c.edx()));
            let r = m.vfs.writable(&abs, false).and_then(|p| {
                if ah == 0x39 { fs::create_dir(&p).map_err(|_| E_ACCESS) } else { fs::remove_dir(&p).map_err(|_| E_ACCESS) }
            });
            match r {
                Ok(()) => c.ok(),
                Err(e) => c.fail(e),
            }
        }
        0x3b => {
            let abs = m.vfs.absolute(&path_arg(c.edx()));
            match m.vfs.lookup(&abs) {
                Some(n) if n.is_dir() || abs.len() == 3 => {
                    let drive = abs.as_bytes()[0] - b'A';
                    m.vfs.cwd[drive as usize] = abs[2..].to_string();
                    c.ok();
                }
                _ => c.fail(E_NOPATH),
            }
        }
        0x3c | 0x5b => {
            let abs = m.vfs.absolute(&path_arg(c.edx()));
            if ah == 0x5b && m.vfs.lookup(&abs).is_some() {
                c.fail(80);
                return Ok(());
            }
            match m.vfs.writable(&abs, false).and_then(|p| File::create(&p).map_err(|_| E_ACCESS)) {
                Ok(file) => match m.dos.alloc_handle(OpenFile::Host { file, writable: true }, abs.clone()) {
                    Some(h) => {
                        crate::debug!("create {abs} -> handle {h}");
                        c.Eax = h as u32;
                        c.ok();
                    }
                    None => c.fail(E_HANDLES),
                },
                Err(e) => c.fail(e),
            }
        }
        0x3d => {
            let abs = m.vfs.absolute(&path_arg(c.edx()));
            let mode = c.al() & 7;
            let node = m.vfs.lookup(&abs);
            let opened = match (&node, mode) {
                (None, _) => Err(E_NOFILE),
                (Some(n), _) if n.is_dir() => Err(E_ACCESS),
                (Some(Node::HostFile(p)), 0) => File::open(p).map(|f| OpenFile::Host { file: f, writable: false }).map_err(|_| E_ACCESS),
                (Some(Node::CdFile(e)), 0) => Ok(OpenFile::Cd { entry: e.clone(), pos: 0 }),
                (Some(_), _) => m
                    .vfs
                    .writable(&abs, true)
                    .and_then(|p| OpenOptions::new().read(true).write(true).open(p).map_err(|_| E_ACCESS))
                    .map(|f| OpenFile::Host { file: f, writable: true }),
            };
            match opened {
                Ok(f) => match m.dos.alloc_handle(f, abs.clone()) {
                    Some(h) => {
                        crate::debug!("open {abs} mode {mode} -> handle {h}");
                        c.Eax = h as u32;
                        c.ok();
                    }
                    None => c.fail(E_HANDLES),
                },
                Err(e) => {
                    crate::debug!("open {abs} mode {mode} failed ({e})");
                    c.fail(e)
                }
            }
        }
        0x3e => {
            let h = c.bx() as usize;
            match m.dos.handles.get_mut(h) {
                Some(slot @ Some(_)) => {
                    if h >= 5 {
                        *slot = None;
                    }
                    c.ok();
                }
                _ => c.fail(E_HANDLE),
            }
        }
        0x3f => {
            let h = c.bx() as usize;
            let (buf, len) = (c.edx(), c.ecx());
            let mut tmp = vec![0u8; len as usize];
            let n = match m.dos.handles.get_mut(h).and_then(|s| s.as_mut()) {
                Some((OpenFile::Host { file, .. }, _)) => file.read(&mut tmp).unwrap_or(0),
                Some((OpenFile::Cd { entry, pos }, _)) => {
                    let (e, p) = (entry.clone(), *pos);
                    let n = cd_read(m, &e, p, &mut tmp);
                    if let Some((OpenFile::Cd { pos, .. }, _)) = m.dos.handles[h].as_mut() {
                        *pos += n as u64;
                    }
                    n
                }
                Some((OpenFile::Device(_), _)) => 0,
                None => {
                    c.fail(E_HANDLE);
                    return Ok(());
                }
            };
            let dst = host_buf(m, buf, n as u32)?;
            unsafe { mem::slice_mut(dst, n as u32).copy_from_slice(&tmp[..n]) };
            // SR_IOLOG=FILE (diagnostics): every read with the file, position and a checksum
            if let Some(log) = io_log() {
                use std::io::Write;
                let (name, pos) = match m.dos.handles.get(h).and_then(|s| s.as_ref()) {
                    Some((OpenFile::Cd { pos, .. }, path)) => (path.clone(), *pos - n as u64),
                    Some((OpenFile::Host { .. }, path)) => (path.clone(), u64::MAX),
                    _ => (String::new(), 0),
                };
                let sum = tmp[..n].iter().fold(0u32, |a, &b| a.rotate_left(5) ^ b as u32);
                let _ = writeln!(log.lock().unwrap(), "{name} {pos} {len} {n} {sum:08x}");
            }
            crate::trace!("read handle {h}: {n}/{len} bytes");
            c.Eax = n as u32;
            c.ok();
        }
        0x40 => {
            let h = c.bx() as usize;
            let src = host_buf(m, c.edx(), c.ecx())?;
            let data = unsafe { mem::slice(src, c.ecx()) }.to_vec();
            match m.dos.handles.get_mut(h).and_then(|s| s.as_mut()) {
                Some((OpenFile::Device(d), _)) => {
                    let s = String::from_utf8_lossy(&data);
                    for line in s.lines() {
                        crate::info!("[guest {}] {line}", if *d == 2 { "stderr" } else { "stdout" });
                    }
                    c.Eax = data.len() as u32;
                    c.ok();
                }
                Some((OpenFile::Host { file, writable: true }, _)) => {
                    if data.is_empty() {
                        let pos = file.stream_position().unwrap_or(0);
                        let _ = file.set_len(pos);
                        c.Eax = 0;
                    } else {
                        c.Eax = file.write(&data).unwrap_or(0) as u32;
                    }
                    c.ok();
                }
                Some(_) => c.fail(E_ACCESS),
                None => c.fail(E_HANDLE),
            }
        }
        0x41 => {
            let abs = m.vfs.absolute(&path_arg(c.edx()));
            match m.vfs.lookup(&abs) {
                Some(Node::HostFile(p)) => match fs::remove_file(p) {
                    Ok(()) => c.ok(),
                    Err(_) => c.fail(E_ACCESS),
                },
                Some(_) => c.fail(E_ACCESS),
                None => c.fail(E_NOFILE),
            }
        }
        0x42 => {
            let h = c.bx() as usize;
            let off = ((c.cx() as u32) << 16 | c.dx() as u32) as i32 as i64;
            let whence = c.al();
            let r: Option<u64> = match m.dos.handles.get_mut(h).and_then(|s| s.as_mut()) {
                Some((OpenFile::Host { file, .. }, _)) => {
                    let sf = match whence {
                        0 => SeekFrom::Start(off.max(0) as u64),
                        1 => SeekFrom::Current(off),
                        _ => SeekFrom::End(off),
                    };
                    file.seek(sf).ok()
                }
                Some((OpenFile::Cd { entry, pos }, _)) => {
                    let base = match whence {
                        0 => 0,
                        1 => *pos as i64,
                        _ => entry.size as i64,
                    };
                    let np = (base + off).max(0) as u64;
                    *pos = np;
                    Some(np)
                }
                Some((OpenFile::Device(_), _)) => Some(0),
                None => None,
            };
            match r {
                Some(p) => {
                    c.set_ax(p as u16);
                    c.set_dx((p >> 16) as u16);
                    c.ok();
                }
                None => c.fail(E_HANDLE),
            }
        }
        0x43 => {
            let abs = m.vfs.absolute(&path_arg(c.edx()));
            match m.vfs.lookup(&abs) {
                Some(n) => {
                    if c.al() == 0 {
                        let ro = matches!(n, Node::CdFile(_) | Node::CdDir(_)) && abs.starts_with('D');
                        c.set_cx(if n.is_dir() { 0x10 } else { 0x20 } | ro as u16);
                    }
                    c.ok();
                }
                None => c.fail(E_NOFILE),
            }
        }
        0x44 => {
            let h = c.bx() as usize;
            match c.al() {
                0x00 => match m.dos.handles.get(h).and_then(|s| s.as_ref()) {
                    Some((OpenFile::Device(d), _)) => {
                        c.set_dx(match d {
                            0 => 0x80d1,
                            1 | 2 => 0x80d2,
                            _ => 0x80c0,
                        });
                        c.ok();
                    }
                    Some((_, name)) => {
                        let drive = name.as_bytes()[0] - b'A';
                        c.set_dx(drive as u16 & 0x3f);
                        c.ok();
                    }
                    None => c.fail(E_HANDLE),
                },
                0x01 => c.ok(),
                0x08 => {
                    let drive = if c.bl() == 0 { m.vfs.cur_drive } else { c.bl() - 1 };
                    c.set_ax((drive != vfs::CD_DRIVE) as u16);
                    c.ok();
                }
                0x09 => {
                    c.set_dx(0);
                    c.ok();
                }
                0x0e => {
                    c.set_al(0);
                    c.ok();
                }
                f => {
                    crate::warn!("int 21h/44h subfunction {f:02x} not implemented");
                    c.fail(E_FUNC);
                }
            }
        }
        0x45 | 0x46 => {
            // dup / dup2: share the same host file through a clone
            let h = c.bx() as usize;
            let dup = match m.dos.handles.get(h).and_then(|s| s.as_ref()) {
                Some((OpenFile::Device(d), n)) => Some((OpenFile::Device(*d), n.clone())),
                Some((OpenFile::Host { file, writable }, n)) => {
                    file.try_clone().ok().map(|f| (OpenFile::Host { file: f, writable: *writable }, n.clone()))
                }
                Some((OpenFile::Cd { entry, pos }, n)) => Some((OpenFile::Cd { entry: entry.clone(), pos: *pos }, n.clone())),
                None => None,
            };
            match dup {
                Some((f, n)) if ah == 0x45 => match m.dos.alloc_handle(f, n) {
                    Some(nh) => {
                        c.Eax = nh as u32;
                        c.ok();
                    }
                    None => c.fail(E_HANDLES),
                },
                Some((f, n)) => {
                    let target = c.cx() as usize;
                    while m.dos.handles.len() <= target {
                        m.dos.handles.push(None);
                    }
                    m.dos.handles[target] = Some((f, n));
                    c.ok();
                }
                None => c.fail(E_HANDLE),
            }
        }
        0x47 => {
            let drive = if c.dl() == 0 { m.vfs.cur_drive } else { c.dl() - 1 };
            if !m.vfs.drive_exists(drive) {
                c.fail(15);
                return Ok(());
            }
            let cwd = m.vfs.cwd[drive as usize].trim_start_matches('\\').to_string();
            unsafe {
                for (i, b) in cwd.bytes().chain(std::iter::once(0)).enumerate() {
                    mem::wr8(c.esi() + i as u32, b);
                }
            }
            c.ok();
        }
        0x48 => {
            // DOS/4GW: allocate conventional memory, returns a selector in AX.
            match m.lowmem.alloc(c.bx()) {
                Some((seg, _)) => {
                    let sel = m.selector_for_segment(seg);
                    c.Eax = sel as u32;
                    c.ok();
                }
                None => {
                    c.set_bx(0);
                    c.fail(8);
                }
            }
        }
        0x49 => c.ok(),
        0x4a => c.ok(),
        0x4c => {
            crate::info!("guest exit with status {}", c.al());
            m.exit_code = Some(c.al() as u32);
            c.Eax = c.al() as u32;
            c.Eip = crate::guest::exit_trampoline as *const () as usize as u32;
        }
        0x4e => {
            let spec = m.vfs.absolute(&path_arg(c.edx()));
            let (dir, pattern) = spec.rsplit_once('\\').unwrap_or((&spec, "*.*"));
            let dir = if dir.len() == 2 { format!("{dir}\\") } else { dir.to_string() };
            let attr = c.cx();
            let list: Vec<ListEntry> = m
                .vfs
                .list(&dir)
                .into_iter()
                .filter(|e| vfs::wildcard_match(pattern, &e.name) && (!e.is_dir || attr & 0x10 != 0))
                .collect();
            crate::debug!("findfirst {spec} -> {} matches", list.len());
            if list.is_empty() {
                c.fail(E_NOFILE);
            } else {
                m.dos.searches.insert(m.dos.dta, (list, 0));
                find_next(m, c);
            }
        }
        0x4f => find_next(m, c),
        0x56 => {
            let from = m.vfs.absolute(&path_arg(c.edx()));
            let to = m.vfs.absolute(&path_arg(c.edi()));
            let r = match (m.vfs.lookup(&from), m.vfs.writable(&to, false)) {
                (Some(Node::HostFile(src)), Ok(dst)) => fs::rename(src, dst).map_err(|_| E_ACCESS),
                (None, _) => Err(E_NOFILE),
                _ => Err(E_ACCESS),
            };
            match r {
                Ok(()) => c.ok(),
                Err(e) => c.fail(e),
            }
        }
        0x57 => {
            if c.al() == 0 {
                let (d, t) = vfs::dos_datetime(1997, 10, 24, 10, 0, 0);
                c.set_cx(t);
                c.set_dx(d);
            }
            c.ok();
        }
        0x59 => {
            c.set_ax(0);
            c.set_bx(0);
        }
        0x62 => c.Ebx = m.dos.psp_sel as u32,
        0x68 => c.ok(), // commit file
        _ => {
            crate::warn!("int 21h function {ah:02x}h not implemented ({})", c.dump());
            c.fail(E_FUNC);
        }
    }
    Ok(())
}
