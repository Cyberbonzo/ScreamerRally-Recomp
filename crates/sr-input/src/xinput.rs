//! XInput polling and a high-resolution period timer.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows_sys::Win32::UI::Input::XboxController::{XINPUT_STATE, XINPUT_VIBRATION, XInputGetState, XInputSetState};

use crate::PadState;

/// State of controller `slot` (0-3), `None` if not connected.
pub fn get(slot: u32) -> Option<PadState> {
    let mut st: XINPUT_STATE = unsafe { std::mem::zeroed() };
    if unsafe { XInputGetState(slot, &mut st) } != ERROR_SUCCESS {
        return None;
    }
    let g = st.Gamepad;
    Some(PadState { buttons: g.wButtons, lt: g.bLeftTrigger, rt: g.bRightTrigger, lx: g.sThumbLX, ly: g.sThumbLY, rx: g.sThumbRX, ry: g.sThumbRY })
}

/// Motor speeds of controller `slot`: left = low-frequency (heavy), right = high-frequency
/// (light). Returns XInput's result (0 = done, 1167 = not connected).
pub fn set_vibration(slot: u32, left: u16, right: u16) -> u32 {
    let mut v = XINPUT_VIBRATION { wLeftMotorSpeed: left, wRightMotorSpeed: right };
    unsafe { XInputSetState(slot, &mut v) }
}

/// Sleeps with sub-millisecond resolution (a plain `Sleep` rounds up to the system tick).
pub struct Timer(HANDLE);

unsafe impl Send for Timer {}

impl Timer {
    pub fn new() -> Timer {
        let h = unsafe { CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS) };
        Timer(h)
    }

    pub fn wait(&self, d: std::time::Duration) {
        if self.0.is_null() {
            std::thread::sleep(d);
            return;
        }
        let due: i64 = -((d.as_nanos() / 100) as i64);
        unsafe {
            SetWaitableTimer(self.0, &due, 0, None, std::ptr::null(), 0);
            WaitForSingleObject(self.0, INFINITE);
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}
