//! First stage of `ScreamerRally.exe`: start the real game process suspended and reserve
//! the conventional memory region in it before Windows' loader can use that range.
//!
//! Windows places the process parameters, both thread stacks and locale tables in the
//! bottom megabyte of every 32-bit process. With the large stack reserve set in `build.rs`,
//! linear 0C0000h-0FFFFFh is still free while the process is suspended; reserving it there
//! gives the original code 256 KB of DOS memory at its true linear addresses.

use std::process::ExitCode;

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::Memory::{MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess, INFINITE, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};

const CHILD_VAR: &str = "SR_GAME_PROCESS";
const REGION: (u32, u32) = (0x000c_0000, 0x0010_0000);

/// Returns `None` in the game process itself, or the child's exit code in the launcher.
pub fn run_as_launcher() -> Option<ExitCode> {
    if std::env::var_os(CHILD_VAR).is_some() || std::env::var_os("SR_NO_LAUNCHER").is_some() {
        return None;
    }
    unsafe { std::env::set_var(CHILD_VAR, "1") };
    let exe: Vec<u16> = std::env::current_exe().ok()?.as_os_str().encode_wide_nul();
    unsafe {
        let cmdline = GetCommandLineW();
        let mut len = 0;
        while *cmdline.add(len) != 0 {
            len += 1;
        }
        let mut cmd: Vec<u16> = std::slice::from_raw_parts(cmdline, len + 1).to_vec();
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        // Address space layout varies between process starts; retry a few times if the
        // region happens to be taken.
        for attempt in 0..6 {
            if CreateProcessW(exe.as_ptr(), cmd.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 1, CREATE_SUSPENDED, std::ptr::null(), std::ptr::null(), &si, &mut pi) == 0 {
                eprintln!("cannot start the game process: {}", std::io::Error::last_os_error());
                return Some(ExitCode::FAILURE);
            }
            let size = (REGION.1 - REGION.0) as usize;
            if attempt == 5 || !VirtualAllocEx(pi.hProcess, REGION.0 as usize as *const _, size, MEM_RESERVE, PAGE_READWRITE).is_null() {
                break;
            }
            TerminateProcess(pi.hProcess, 1);
            WaitForSingleObject(pi.hProcess, INFINITE);
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
        }
        // Deterministic mode (diagnostics): keep the guest allocation arena free; the
        // runtime replaces this placeholder with its own write-watched reservation.
        if std::env::var_os("SR_DETERMINISTIC").is_some() {
            let (base, size) = sr_runtime::mem::DETERMINISTIC_ARENA;
            if VirtualAllocEx(pi.hProcess, base as usize as *const _, size as usize, MEM_RESERVE, PAGE_READWRITE).is_null() {
                eprintln!("note: deterministic arena not reserved: {}", std::io::Error::last_os_error());
            }
        }
        // One reservation, so DOS blocks can be committed anywhere inside it. If it is
        // missing the runtime falls back to virtual conventional memory (slower, still correct).
        let mut probe: windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        windows_sys::Win32::System::Memory::VirtualQueryEx(pi.hProcess, REGION.0 as usize as *const _, &mut probe, std::mem::size_of_val(&probe));
        if probe.State != MEM_RESERVE {
            eprintln!("note: conventional memory region not available: {}", std::io::Error::last_os_error());
            let mut a = 0x10000u32;
            while a < 0x200000 {
                let mut mbi: windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION = std::mem::zeroed();
                if windows_sys::Win32::System::Memory::VirtualQueryEx(pi.hProcess, a as usize as *const _, &mut mbi, std::mem::size_of_val(&mbi)) == 0 {
                    break;
                }
                if mbi.State != windows_sys::Win32::System::Memory::MEM_FREE {
                    eprintln!("  {:08x}-{:08x} state {:x} type {:x}", a, a as usize + mbi.RegionSize, mbi.State, mbi.Type);
                }
                a += mbi.RegionSize as u32;
            }
        }
        if ResumeThread(pi.hThread) == u32::MAX {
            TerminateProcess(pi.hProcess, 1);
            return Some(ExitCode::FAILURE);
        }
        let mut code = 1u32;
        if WaitForSingleObject(pi.hProcess, INFINITE) == WAIT_OBJECT_0 {
            GetExitCodeProcess(pi.hProcess, &mut code);
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        Some(ExitCode::from(code as u8))
    }
}

trait WideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl WideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}
