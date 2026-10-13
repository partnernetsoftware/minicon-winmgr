//! Small shared helpers for Win32 interop.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, HWND_TOP, IsIconic, SW_MINIMIZE, SW_RESTORE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
    ShowWindow,
};

/// Encodes `s` as a null-terminated UTF-16 buffer for Win32 `PCWSTR` arguments.
///
/// The returned buffer must outlive any `PCWSTR` built from it.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Brings `hwnd` to the foreground and top of the z-order, restoring it first
/// if minimized. Uses a layered strategy because a plain `SetForegroundWindow`
/// is frequently refused by the Windows foreground lock (our taskbar/settings
/// windows are top-most tool windows that never own the foreground):
///
/// 1. raise in the z-order + `SetForegroundWindow`;
/// 2. attach our input queue to the current foreground thread, try again, detach;
/// 3. last resort: `SW_MINIMIZE` then `SW_RESTORE`, which reliably activates a
///    window even when the foreground lock would otherwise refuse.
///
/// Returns whether `hwnd` ended up as the foreground window.
///
/// # Safety
/// `hwnd` must be a valid window handle; call on the message thread.
pub unsafe fn force_foreground(hwnd: HWND) -> bool {
    // SAFETY: pure window/thread queries plus the documented foreground
    // workarounds; all handles come from Win32.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        // 1) Raise in z-order (allowed without foreground rights) + try to focus.
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
        let _ = SetForegroundWindow(hwnd);
        if GetForegroundWindow() == hwnd {
            return true;
        }

        // 2) AttachThreadInput dance.
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let cur_tid = GetCurrentThreadId();
        if fg_tid != 0 && fg_tid != cur_tid {
            let _ = AttachThreadInput(fg_tid, cur_tid, true);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(fg_tid, cur_tid, false);
        }
        if GetForegroundWindow() == hwnd {
            return true;
        }

        // 3) Last resort: a minimize/restore cycle activates the window.
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        GetForegroundWindow() == hwnd
    }
}
