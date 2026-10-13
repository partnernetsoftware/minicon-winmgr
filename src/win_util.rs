//! Small shared helpers for Win32 interop.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};

/// Encodes `s` as a null-terminated UTF-16 buffer for Win32 `PCWSTR` arguments.
///
/// The returned buffer must outlive any `PCWSTR` built from it.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Raises `hwnd` to the foreground, restoring it if minimized.
///
/// A plain `SetForegroundWindow` is often refused by the Windows foreground
/// lock when the caller is not already the foreground process (e.g. clicking a
/// top-most tool window such as our taskbar). We temporarily attach our input
/// queue to the current foreground thread so the call is permitted, then detach.
///
/// # Safety
/// `hwnd` must be a valid window handle; must be called on the message thread.
pub unsafe fn force_foreground(hwnd: HWND) {
    // SAFETY: pure window/thread queries plus the documented AttachThreadInput
    // foreground dance; all handles come from Win32.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let cur_tid = GetCurrentThreadId();
        let attach = fg_tid != 0 && fg_tid != cur_tid;
        if attach {
            let _ = AttachThreadInput(fg_tid, cur_tid, true);
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attach {
            let _ = AttachThreadInput(fg_tid, cur_tid, false);
        }
    }
}
