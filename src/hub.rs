//! Process-wide hub: the one home for state that several modules must share.
//!
//! Deliberately a **leaf**: it depends on nothing in this crate. Two things live
//! here because sharing them any other way either duplicates them or closes a
//! dependency cycle:
//!
//! * the shell's message-window handle. It used to be two separate statics
//!   (one in `window`, one in `window_events`), and pointing `window_events` at
//!   `window`'s copy would make `window ↔ window_events` a cycle.
//! * the taskbar-refresh request. The settings window changes what the bar shows
//!   and used to call `taskbar::refresh` directly, while the taskbar calls back
//!   into the settings window to open it — a cycle. Now the settings window asks
//!   the hub and the assembly layer (`window::run`) wires the hub to `taskbar`.
//!
//! Everything here is written once during startup and read on the message
//! thread afterwards, so atomics and a `OnceLock` are enough; nothing here
//! allocates or locks.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::HWND;

/// Handle of the shell's message-only window, as `isize`. 0 = not created yet.
static SHELL_HWND: AtomicIsize = AtomicIsize::new(0);

/// The taskbar's rebuild entry point, installed once by `window::run`.
static REFRESH: OnceLock<fn()> = OnceLock::new();

/// Records the shell message window. Called once, from `window::run`.
pub fn set_shell_hwnd(hwnd: HWND) {
    SHELL_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
}

/// The shell message window, or `None` before it exists.
pub fn shell_hwnd() -> Option<HWND> {
    let raw = SHELL_HWND.load(Ordering::SeqCst);
    (raw != 0).then_some(HWND(raw as *mut core::ffi::c_void))
}

/// Registers the taskbar's rebuild entry point. Called once, from `window::run`.
pub fn set_taskbar_refresh(f: fn()) {
    let _ = REFRESH.set(f);
}

/// Asks the taskbar to rebuild. A no-op before the hook is installed, so a
/// caller never has to care about startup order.
pub fn request_taskbar_refresh() {
    if let Some(f) = REFRESH.get() {
        f();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_requests_reach_the_registered_hook() {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn count() {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }

        set_taskbar_refresh(count);
        request_taskbar_refresh();
        request_taskbar_refresh();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            2,
            "the hook runs every request"
        );
    }

    #[test]
    fn shell_hwnd_is_none_before_startup_sets_it() {
        // The message-only window is created later in `run()`; a caller that asks
        // too early must get `None`, not a null handle it would happily pass on.
        assert!(shell_hwnd().is_none());
    }
}
