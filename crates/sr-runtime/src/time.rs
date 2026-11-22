//! Host time helpers.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Deterministic mode (`SR_DETERMINISTIC`, diagnostics): time is virtual. Timer ticks are
/// issued only when the game waits for them (see `events::virtual_ticks_until`), and every
/// clock the guest can read follows them, so a run with the same scripted input repeats
/// exactly.
static DETERMINISTIC: AtomicBool = AtomicBool::new(false);
/// Timer base ticks issued in deterministic mode.
static VIRTUAL_TICKS: AtomicU64 = AtomicU64::new(0);
/// DOS / BIOS clock queries in deterministic mode: each moves that clock on by 1 ms (the C
/// library calibrates a delay loop by counting queries until the seconds change).
static CLOCK_QUERIES: AtomicU64 = AtomicU64::new(0);

pub fn set_deterministic(on: bool) {
    DETERMINISTIC.store(on, Ordering::Relaxed);
}

pub fn deterministic() -> bool {
    DETERMINISTIC.load(Ordering::Relaxed)
}

pub fn virtual_ticks() -> u64 {
    VIRTUAL_TICKS.load(Ordering::Relaxed)
}

pub fn add_virtual_tick() {
    VIRTUAL_TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Monotonic time since the runtime started (virtual in deterministic mode).
pub fn uptime() -> std::time::Duration {
    if deterministic() {
        return std::time::Duration::from_secs_f64(virtual_ticks() as f64 / crate::events::HMI_BASE_HZ);
    }
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// UTC civil date/time from a Unix timestamp (Howard Hinnant's algorithm).
pub fn civil_from_unix(secs: i64) -> (i32, u8, u8, u8, u8, u8) {
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    (y, m, d, (sod / 3600) as u8, ((sod / 60) % 60) as u8, (sod % 60) as u8)
}

pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub centisecond: u8,
}

pub fn local_now() -> LocalTime {
    if deterministic() {
        // Friday 31 October 1997, 12:00, plus virtual time and 1 ms per query
        let q = CLOCK_QUERIES.fetch_add(1, Ordering::Relaxed);
        let ms = 12 * 3_600_000 + (virtual_ticks() as f64 * 1000.0 / crate::events::HMI_BASE_HZ) as u64 + q;
        let (s, cs) = (ms / 1000, (ms % 1000) / 10);
        return LocalTime {
            year: 1997,
            month: 10,
            day: 31,
            weekday: 5,
            hour: ((s / 3600) % 24) as u8,
            minute: ((s / 60) % 60) as u8,
            second: (s % 60) as u8,
            centisecond: cs as u8,
        };
    }
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    LocalTime {
        year: st.wYear,
        month: st.wMonth as u8,
        day: st.wDay as u8,
        weekday: st.wDayOfWeek as u8,
        hour: st.wHour as u8,
        minute: st.wMinute as u8,
        second: st.wSecond as u8,
        centisecond: (st.wMilliseconds / 10) as u8,
    }
}

/// BIOS timer ticks (18.2065 Hz) since local midnight.
pub fn bios_ticks_since_midnight() -> u32 {
    let t = local_now();
    let secs = t.hour as u64 * 3600 + t.minute as u64 * 60 + t.second as u64;
    let ms = secs * 1000 + t.centisecond as u64 * 10;
    (ms * 1_193_182 / 65536 / 1000) as u32
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil() {
        assert_eq!(super::civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(super::civil_from_unix(877_000_000), (1997, 10, 16, 11, 6, 40));
    }
}

/// CPU time stamp counter: a cheap clock for per-call statistics (no system call, no
/// 64-bit division); convert with `cycles_to_duration`.
pub fn cycles() -> u64 {
    // SAFETY: rdtsc is available on every CPU that runs Windows 10.
    let c = unsafe { core::arch::x86::_rdtsc() };
    CALIBRATION.get_or_init(|| (std::time::Instant::now(), c));
    c
}

static CALIBRATION: std::sync::OnceLock<(std::time::Instant, u64)> = std::sync::OnceLock::new();

/// Time stamp counter cycles as time (the rate is measured since the first `cycles` call).
pub fn cycles_to_duration(c: u64) -> std::time::Duration {
    let Some(&(t0, c0)) = CALIBRATION.get() else { return std::time::Duration::ZERO };
    let secs = t0.elapsed().as_secs_f64();
    let elapsed = cycles().saturating_sub(c0);
    if secs < 0.01 || elapsed == 0 {
        return std::time::Duration::ZERO;
    }
    std::time::Duration::from_secs_f64(c as f64 * secs / elapsed as f64)
}
