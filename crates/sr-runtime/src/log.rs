//! Minimal logger: everything goes to `logs/runtime.log`, warnings and errors also to stderr.
//!
//! Kept dependency-free on purpose; the runtime logs from inside exception handlers,
//! so the logger must never allocate unboundedly or take locks that the guest could hold.

use std::fmt::Arguments;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn init(path: &std::path::Path, level: Level) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    *FILE.lock().unwrap() = File::create(path).ok();
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

pub fn write(level: Level, args: Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let tag = match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN ",
        Level::Info => "INFO ",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    };
    let line = format!("{tag} {args}\n");
    if let Ok(mut g) = FILE.try_lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.write_all(line.as_bytes());
            if level <= Level::Warn {
                let _ = f.flush();
            }
        }
    }
    if level <= Level::Warn {
        eprint!("{line}");
    }
}

pub fn flush() {
    if let Ok(mut g) = FILE.lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.flush();
        }
    }
}

// The level is checked before the arguments are evaluated (some build strings).
#[macro_export]
macro_rules! error { ($($t:tt)*) => { if $crate::log::enabled($crate::log::Level::Error) { $crate::log::write($crate::log::Level::Error, format_args!($($t)*)) } } }
#[macro_export]
macro_rules! warn { ($($t:tt)*) => { if $crate::log::enabled($crate::log::Level::Warn) { $crate::log::write($crate::log::Level::Warn, format_args!($($t)*)) } } }
#[macro_export]
macro_rules! info { ($($t:tt)*) => { if $crate::log::enabled($crate::log::Level::Info) { $crate::log::write($crate::log::Level::Info, format_args!($($t)*)) } } }
#[macro_export]
macro_rules! debug { ($($t:tt)*) => { if $crate::log::enabled($crate::log::Level::Debug) { $crate::log::write($crate::log::Level::Debug, format_args!($($t)*)) } } }
#[macro_export]
macro_rules! trace { ($($t:tt)*) => { if $crate::log::enabled($crate::log::Level::Trace) { $crate::log::write($crate::log::Level::Trace, format_args!($($t)*)) } } }
