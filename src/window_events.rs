//! Process-wide window events → debounced taskbar refresh.
//!
//! A shell's task list must react when windows appear, disappear, hide, or
//! change foreground — waiting for the 30s heartbeat makes a closed window's
//! button linger ("close takes ages"). We install an out-of-context
//! `SetWinEventHook` (delivered to our message loop, same thread — no new
//! thread, per project invariants) and, on any relevant event, arm a short
//! one-shot timer on the shell window. Re-arming the same timer coalesces
//! bursts into a single refresh.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    SetTimer, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// Timer id (on the shell message window) used to debounce refreshes.
pub const REFRESH_TIMER_ID: usize = 4;
/// Debounce delay after the last event, in milliseconds.
const REFRESH_DEBOUNCE_MS: u32 = 120;

/// HWND of the window that will receive the debounce timer. Stored as `isize`
/// (message thread is the sole user).
static SHELL_HWND: AtomicIsize = AtomicIsize::new(0);
/// Hook handles, kept alive for the process lifetime so they are never dropped.
static HOOK_OBJECT: AtomicIsize = AtomicIsize::new(0);
static HOOK_SYSTEM: AtomicIsize = AtomicIsize::new(0);

/// Installs the window-event hooks. Safe to call once; repeats are no-ops.
pub fn install(hwnd: HWND) -> Result<(), Box<dyn std::error::Error>> {
    SHELL_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    // SAFETY: `callback` is a valid `extern "system"` fn; out-of-context hooks
    // deliver events to this thread's message loop, so no module handle is
    // needed. Two hooks because the object (0x8xxx) and system (0x00xx) event
    // ranges are far apart and `eventmin` must be <= `eventmax`.
    let object = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_HIDE,
            None,
            Some(callback),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    let system = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZEEND,
            None,
            Some(callback),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if object.is_invalid() || system.is_invalid() {
        tracing::warn!(
            object_ok = !object.is_invalid(),
            system_ok = !system.is_invalid(),
            "SetWinEventHook partially failed; taskbar refresh falls back to heartbeat"
        );
    } else {
        HOOK_OBJECT.store(object.0 as isize, Ordering::SeqCst);
        HOOK_SYSTEM.store(system.0 as isize, Ordering::SeqCst);
        tracing::info!("window-event hooks installed (taskbar refresh)");
    }
    Ok(())
}

/// Hook callback: arm (or re-arm) the debounce timer on the shell window.
unsafe extern "system" fn callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only top-level object events (id_object == OBJID_WINDOW == 0, no child)
    // matter for the task list.
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
        return;
    }
    // Ignore our own shell windows (belt and braces with SKIPOWNPROCESS).
    let raw = SHELL_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    tracing::trace!(event, ?hwnd, "window event; arming taskbar refresh");
    let shell = HWND(raw as *mut core::ffi::c_void);
    // SAFETY: `shell` is our live message window; re-arming the same timer id
    // resets it, coalescing bursts.
    let _ = unsafe { SetTimer(Some(shell), REFRESH_TIMER_ID, REFRESH_DEBOUNCE_MS, None) };
}
