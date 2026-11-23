//! Virtual interrupts: the HMI SOS timer service and hardware IRQs, delivered to the
//! original code asynchronously, as the PC's 8253/8259 did.
//!
//! A host thread (the "PIT thread") keeps the PIT base clock. To deliver an interrupt it
//! suspends the guest thread and checks where it stopped. If the guest is in its own code
//! with the virtual IF set, the PIT thread runs the interrupt work **itself**, on a
//! dedicated interrupt stack, while the guest stays frozen, and then resumes the guest.
//! That is exactly what a single-CPU hardware interrupt is: the program is frozen at an
//! instruction boundary, the handler runs, the program continues.
//!
//! The guest's thread context is never modified, which avoids every WOW64 race around
//! `SetThreadContext` (an exception or system call in flight).
//!
//! If the guest is anywhere else (runtime code, a system call, interrupts disabled) it is
//! resumed untouched and delivery is retried shortly after. Ticks are counted, never
//! dropped, so game time stays exact even when delivery has to wait.
//!
//! `Machine` access rule: the guest thread, or the PIT thread while the guest thread is
//! suspended in guest code (never both at once).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, GetThreadContext};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, GetCurrentThreadId, INFINITE, OpenThread,
    ResumeThread, SetWaitableTimer, SuspendThread, THREAD_ALL_ACCESS, TIMER_ALL_ACCESS, WaitForSingleObject,
};

/// CONTEXT_i386 | CONTROL | INTEGER | FLOATING_POINT
const CONTEXT_FLAGS: u32 = 0x0001_000b;

/// One periodic HMI timer event.
#[derive(Clone, Debug)]
pub struct TimerEvent {
    pub handle: u32,
    pub rate_hz: u32,
    pub callback: u32,
    /// Base ticks between calls and the countdown to the next one.
    pub divisor: u32,
    pub countdown: u32,
    /// Registered by HMI driver code rather than the game (not run until audio HLE).
    pub internal: bool,
    pub calls: u64,
}

#[derive(Default)]
pub struct Shared {
    pub events: Vec<TimerEvent>,
    /// Base ticks not yet delivered.
    pub pending_ticks: u32,
    /// Scancodes waiting for IRQ1.
    pub scancodes: VecDeque<u8>,
}

pub struct Scheduler {
    pub shared: Arc<Mutex<Shared>>,
    code: (u32, u32),
    vif: Arc<AtomicBool>,
    pub base_hz: f64,
    pub delivered: Arc<AtomicU64>,
    pub busy_retries: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    /// Address of the guest's IRQ1 handler (0 = none).
    pub irq1_handler: Arc<AtomicU32>,
    /// Interrupt stack [base, top).
    stack: (u32, u32),
}

static SHARED: std::sync::OnceLock<Arc<Mutex<Shared>>> = std::sync::OnceLock::new();

fn shared() -> Arc<Mutex<Shared>> {
    SHARED.get_or_init(Arc::default).clone()
}

/// Queue a PC scancode for the guest's keyboard interrupt (callable from any thread).
/// The game is paused (settings overlay open): no timer ticks are issued, so game time
/// does not jump when it continues.
pub static PAUSED: AtomicBool = AtomicBool::new(false);

/// Hold interrupt delivery (ticks keep counting and are delivered afterwards): the runtime
/// is running guest code of its own (an extra drawn frame).
pub static DEFER: AtomicBool = AtomicBool::new(false);

pub fn push_scancode(sc: u8) {
    shared().lock().unwrap().scancodes.push_back(sc);
}

/// Base rate of the HMI timer: the PIT reload HMI programs for a 100 Hz event.
pub const HMI_BASE_HZ: f64 = 1_193_182.0 / 11_931.0;

impl Scheduler {
    pub fn new(code_lo: u32, code_hi: u32, vif: Arc<AtomicBool>) -> Scheduler {
        const STACK: u32 = 512 << 10;
        // above the image like the guest stack, so Watcom stack checks in handlers pass
        let det = if crate::time::deterministic() { crate::mem::alloc_guest(STACK) } else { None };
        let base = det
            .or_else(|| crate::mem::alloc(0x3900_0000, STACK, false))
            .or_else(|| crate::mem::alloc(0, STACK, false))
            .expect("cannot allocate the interrupt stack");
        Scheduler {
            shared: shared(),
            code: (code_lo, code_hi),
            vif,
            base_hz: HMI_BASE_HZ,
            delivered: Arc::default(),
            busy_retries: Arc::default(),
            stop: Arc::default(),
            irq1_handler: Arc::default(),
            stack: (base, base + STACK),
        }
    }

    pub fn register(&self, rate_hz: u32, callback: u32, internal: bool) -> u32 {
        let mut s = self.shared.lock().unwrap();
        let handle = (0..16).find(|h| !s.events.iter().any(|e| e.handle == *h)).unwrap_or(15);
        let divisor = ((self.base_hz / rate_hz.max(1) as f64).round() as u32).max(1);
        s.events.push(TimerEvent { handle, rate_hz, callback, divisor, countdown: divisor, internal, calls: 0 });
        handle
    }

    pub fn remove(&self, handle: u32) -> bool {
        let mut s = self.shared.lock().unwrap();
        let before = s.events.len();
        s.events.retain(|e| e.handle != handle);
        before != s.events.len()
    }

    pub fn alter_rate(&self, handle: u32, rate_hz: u32) -> bool {
        let mut s = self.shared.lock().unwrap();
        let base = self.base_hz;
        match s.events.iter_mut().find(|e| e.handle == handle) {
            Some(e) => {
                e.rate_hz = rate_hz;
                e.divisor = ((base / rate_hz.max(1) as f64).round() as u32).max(1);
                e.countdown = e.countdown.min(e.divisor);
                true
            }
            None => false,
        }
    }

    pub fn rate_of(&self, handle: u32) -> Option<u32> {
        self.shared.lock().unwrap().events.iter().find(|e| e.handle == handle).map(|e| e.rate_hz)
    }

    pub fn push_scancode(&self, sc: u8) {
        self.shared.lock().unwrap().scancodes.push_back(sc);
    }

    /// Start the PIT thread for the calling (guest) thread.
    pub fn start(&self) {
        if crate::time::cooperative_timer() {
            crate::info!("timer: delivering interrupts on the guest thread at frame waits (wall clock)");
            return;
        }
        let thread: HANDLE = unsafe { OpenThread(THREAD_ALL_ACCESS, 0, GetCurrentThreadId()) };
        assert!(!thread.is_null(), "OpenThread failed");
        let ctx = Pit {
            thread: thread as usize,
            shared: self.shared.clone(),
            code: self.code,
            vif: self.vif.clone(),
            base_hz: self.base_hz,
            stop: self.stop.clone(),
            delivered: self.delivered.clone(),
            retries: self.busy_retries.clone(),
            irq1: self.irq1_handler.clone(),
            stack: self.stack,
        };
        std::thread::Builder::new().name("pit".into()).spawn(move || ctx.run()).expect("PIT thread");
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Pit {
    thread: usize,
    shared: Arc<Mutex<Shared>>,
    code: (u32, u32),
    vif: Arc<AtomicBool>,
    base_hz: f64,
    stop: Arc<AtomicBool>,
    delivered: Arc<AtomicU64>,
    retries: Arc<AtomicU64>,
    irq1: Arc<AtomicU32>,
    stack: (u32, u32),
}

impl Pit {
    fn owed(&self) -> (bool, bool) {
        let s = self.shared.lock().unwrap();
        let timer = s.pending_ticks > 0 && s.events.iter().any(|e| !e.internal);
        let kbd = !s.scancodes.is_empty() && self.irq1.load(Ordering::Relaxed) != 0;
        (timer, kbd)
    }

    fn run(self) {
        let timer = unsafe { CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS) };
        let period = Duration::from_secs_f64(1.0 / self.base_hz);
        let mut start = Instant::now();
        let mut ticks_issued: u64 = 0;
        while !self.stop.load(Ordering::Relaxed) {
            if PAUSED.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
                // continue from the same tick count afterwards
                start = Instant::now() - period.mul_f64(ticks_issued as f64);
                continue;
            }
            // deterministic mode: ticks and key interrupts come from the tick wait instead
            if crate::time::deterministic() {
                std::thread::sleep(Duration::from_millis(5));
                start = Instant::now();
                ticks_issued = 0;
                continue;
            }
            // Issue every base tick whose time has come.
            let due = (start.elapsed().as_secs_f64() * self.base_hz) as u64;
            let new_ticks = due.saturating_sub(ticks_issued);
            ticks_issued = due;
            self.shared.lock().unwrap().pending_ticks += new_ticks as u32;
            let (want_timer, want_kbd) = self.owed();
            if (want_timer || want_kbd) && !DEFER.load(Ordering::Relaxed) {
                self.try_deliver(want_timer, want_kbd);
            }
            let (still_timer, still_kbd) = self.owed();
            let wait = if still_timer || still_kbd {
                Duration::from_micros(200)
            } else {
                (start + period.mul_f64((ticks_issued + 1) as f64)).saturating_duration_since(Instant::now())
            };
            unsafe {
                let due = -((wait.as_nanos() / 100) as i64).max(1);
                if !timer.is_null() && SetWaitableTimer(timer, &due, 0, None, std::ptr::null(), 0) != 0 {
                    WaitForSingleObject(timer, INFINITE);
                } else {
                    std::thread::sleep(wait);
                }
            }
        }
        unsafe { CloseHandle(self.thread as HANDLE) };
    }

    fn try_deliver(&self, timer: bool, kbd: bool) {
        let thread = self.thread as HANDLE;
        unsafe {
            if SuspendThread(thread) == u32::MAX {
                return;
            }
            let mut ctx: CONTEXT = std::mem::zeroed();
            ctx.ContextFlags = CONTEXT_FLAGS;
            let ok = GetThreadContext(thread, &mut ctx) != 0
                && ctx.Eip >= self.code.0
                && ctx.Eip < self.code.1
                && self.vif.load(Ordering::Relaxed);
            if ok {
                let cw = ctx.FloatSave.ControlWord as u16;
                let old = crate::guest::set_stack_bounds(self.stack.1, self.stack.0);
                // The handlers' segment loads are tracked in the shared virtual segment
                // state; their final `pop` restores do not trap. Like the CPU restoring the
                // interrupted program's registers, put the frozen thread's state back.
                let m = crate::machine();
                let saved_segments = m.sel.virt();
                if kbd {
                    self.raise_irq1(cw);
                }
                if timer {
                    self.run_ticks(cw);
                }
                crate::machine().sel.set_virt_all(saved_segments);
                crate::guest::set_stack_bounds(old.0, old.1);
                self.delivered.fetch_add(1, Ordering::Relaxed);
            } else {
                self.retries.fetch_add(1, Ordering::Relaxed);
            }
            ResumeThread(thread);
        }
    }

    /// Run every pending tick's due callbacks, in order.
    fn run_ticks(&self, cw: u16) {
        loop {
            let due: Vec<u32> = {
                let mut s = self.shared.lock().unwrap();
                if s.pending_ticks == 0 {
                    break;
                }
                s.pending_ticks -= 1;
                let mut due = Vec::new();
                for e in s.events.iter_mut() {
                    e.countdown -= 1;
                    if e.countdown == 0 {
                        e.countdown = e.divisor;
                        if !e.internal {
                            e.calls += 1;
                            due.push(e.callback);
                        }
                    }
                }
                due
            };
            for cb in due {
                unsafe { crate::guest::call_on_stack(cb, self.stack.1, 0, cw as u32) };
            }
        }
    }

    /// One scancode into the keyboard controller, then the guest's IRQ1 handler.
    fn raise_irq1(&self, cw: u16) {
        let handler = self.irq1.load(Ordering::Relaxed);
        let Some(sc) = self.shared.lock().unwrap().scancodes.pop_front() else { return };
        let m = crate::machine();
        // The 8042 output buffer holds one byte, and this interrupt is for this scancode.
        // Bytes nobody read (command ACKs the game polled for and ignored) would otherwise
        // shift every later scancode by one interrupt, so a held key's make code would
        // only arrive with the next key event.
        if !m.ports.kbd.scancodes.is_empty() {
            crate::debug!("keyboard: dropping {} unread byte(s) {:02x?}", m.ports.kbd.scancodes.len(), m.ports.kbd.scancodes);
            m.ports.kbd.scancodes.clear();
        }
        m.ports.kbd.scancodes.push_back(sc);
        m.set_vif(false);
        unsafe { crate::guest::call_on_stack(handler, self.stack.1, 1, cw as u32) };
        m.set_vif(true);
        static OBSERVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *OBSERVE.get_or_init(|| std::env::var_os("SR_OBSERVE_INPUT").is_some()) {
            let live = unsafe { crate::mem::rd8(m.img.link(0x50330 + (sc as u32 & 0x7f))) };
            // the game's key table (50330h) entry for this key after its handler ran
            crate::info!("IRQ1 {sc:#04x} -> {}: key[{:#x}] = {live}", m.where_(handler), sc & 0x7f);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Deterministic mode: virtual ticks and scheduled key events.

static SCHEDULED_KEYS: Mutex<Vec<(u64, u8)>> = Mutex::new(Vec::new());

/// Deterministic mode: queue a scancode for virtual tick `at` (the key script).
pub fn schedule_scancode(at: u64, sc: u8) {
    let mut k = SCHEDULED_KEYS.lock().unwrap();
    let i = k.partition_point(|e| e.0 <= at);
    k.insert(i, (at, sc));
}

fn guest_fpu_cw() -> u16 {
    let mut cw: u16 = 0;
    unsafe { core::arch::asm!("fnstcw word ptr [{}]", in(reg) &mut cw as *mut u16) };
    cw
}

impl Scheduler {
    /// Some screens, including the original pause menu, keep swapping frames without
    /// entering the usual tick wait. Service the clock and keyboard there as well.
    pub fn cooperative_frame(&self) {
        use std::cell::Cell;
        thread_local! { static LAST: Cell<Option<(Instant, u64)>> = const { Cell::new(None) }; }
        LAST.with(|last| {
            let now = Instant::now();
            let ticks = crate::time::virtual_ticks();
            let mut anchor = now;
            if let Some((previous, delivered)) = last.get() {
                if ticks == delivered {
                    let period = Duration::from_secs_f64(1.0 / HMI_BASE_HZ);
                    let elapsed = now.duration_since(previous).min(Duration::from_millis(100));
                    let due = (elapsed.as_nanos() / period.as_nanos()) as u32;
                    if due > 0 {
                        self.virtual_ticks_until(|| false, due);
                    }
                    anchor = now - (elapsed - period * due);
                }
            }
            last.set(Some((anchor, crate::time::virtual_ticks())));
        });
    }

    /// Deterministic mode, called on the guest thread where the game waits for timer ticks:
    /// issue base ticks (running the due timer callbacks and the key interrupts of that
    /// moment) until `done()` says the wait is over. At most `limit` ticks.
    pub fn virtual_ticks_until(&self, mut done: impl FnMut() -> bool, limit: u32) {
        let cw = guest_fpu_cw();
        let m = crate::machine();
        let saved_segments = m.sel.virt();
        // handlers run on the interrupt stack: exception dispatch must accept it meanwhile
        let stack = self.stack;
        let call = |func: u32, mode: u32| unsafe {
            let old = crate::guest::set_stack_bounds(stack.1, stack.0);
            crate::guest::call_on_stack(func, stack.1, mode, cw as u32);
            crate::guest::set_stack_bounds(old.0, old.1);
        };
        for _ in 0..limit {
            if done() {
                break;
            }
            crate::time::add_virtual_tick();
            let now = crate::time::virtual_ticks();
            // keys due now
            let due_keys: Vec<u8> = {
                let mut k = SCHEDULED_KEYS.lock().unwrap();
                let n = k.partition_point(|e| e.0 <= now);
                k.drain(..n).map(|e| e.1).collect()
            };
            for sc in due_keys {
                self.shared.lock().unwrap().scancodes.push_back(sc);
            }
            // timer callbacks due on this tick
            let due: Vec<u32> = {
                let mut s = self.shared.lock().unwrap();
                let mut due = Vec::new();
                for e in s.events.iter_mut() {
                    e.countdown -= 1;
                    if e.countdown == 0 {
                        e.countdown = e.divisor;
                        if !e.internal {
                            e.calls += 1;
                            due.push(e.callback);
                        }
                    }
                }
                due
            };
            if PAUSED.load(Ordering::Relaxed) {
                continue;
            }
            let handler = self.irq1_handler.load(Ordering::Relaxed);
            while handler != 0 {
                let Some(sc) = self.shared.lock().unwrap().scancodes.pop_front() else { break };
                m.ports.kbd.scancodes.clear();
                m.ports.kbd.scancodes.push_back(sc);
                m.set_vif(false);
                call(handler, 1);
                m.set_vif(true);
            }
            for cb in due {
                call(cb, 0);
            }
            self.delivered.fetch_add(1, Ordering::Relaxed);
        }
        crate::machine().sel.set_virt_all(saved_segments);
    }
}
