//! Small shared helpers for Win32 interop.

use std::sync::{Mutex, MutexGuard};

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, HWND_TOP, IsIconic, SW_MINIMIZE, SW_RESTORE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
    ShowWindow,
};
use windows::core::PWSTR;

/// Locks `m`, recovering instead of panicking if the mutex has been poisoned.
///
/// Every mutex in this crate guards state touched from the single message
/// thread. A panic while a guard is held poisons the mutex, and the next
/// `.lock().unwrap()` would then panic as well — on the message thread that
/// means unwinding out of a `wndproc`, which the `extern "system"` ABI does not
/// allow, so the process aborts and the user's desktop goes with it. A poisoned
/// lock here holds no torn invariant worth hiding: the guarded values are plain
/// data (window lists, rects, config), so reading through the poison is strictly
/// better than losing the shell.
///
/// Use this everywhere instead of `.lock().unwrap()`.
pub fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs `body` with a panic guard, falling back to `fallback` if it panics.
///
/// Every callback registered with Win32 (`WNDPROC`, `WINEVENTPROC`,
/// `ENUMWINDOWSPROC`, console handlers) is declared `unsafe extern "system"`.
/// Unwinding out of such a callback is undefined behaviour and, with the
/// standard `extern "system"` shim, aborts the process — i.e. the user's
/// desktop disappears because one message hit a bad branch.
///
/// So the body of every FFI callback goes through here. A caught panic is
/// reported at `error!` with the callback's name; it is never swallowed
/// silently, because a panic still means a real bug that has to be found.
///
/// `what` names the callback (e.g. `"taskbar::wndproc"`).
pub fn guard_abi<R>(what: &str, body: impl FnOnce() -> R, fallback: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => {
            tracing::error!(
                callback = what,
                "panic crossed an FFI callback; using the fallback to keep the shell alive"
            );
            fallback()
        }
    }
}

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
        let activated = GetForegroundWindow() == hwnd;
        if !activated {
            tracing::warn!(
                ?hwnd,
                "force_foreground: every strategy was refused; the click will look like nothing happened"
            );
        }
        activated
    }
}

/// Longest image path `QueryFullProcessImageNameW` can return here, in UTF-16
/// units. Windows caps paths well below this.
const MAX_IMAGE_PATH: usize = 1024;

/// Returns the full image path of the process owning `hwnd`, or `None`.
///
/// The window is first mapped to its process id, then the image path is read
/// with `PROCESS_QUERY_LIMITED_INFORMATION`, which is granted across integrity
/// levels for ordinary processes; protected or elevated targets may still
/// refuse, in which case the caller should treat the window as not pinnable.
///
/// # Safety
/// `hwnd` must be a valid window handle.
pub unsafe fn window_exe_path(hwnd: HWND) -> Option<String> {
    // SAFETY: all handles are released before returning; the buffer outlives
    // the calls that write into it.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            tracing::warn!(?hwnd, "window_exe_path: no process id for window");
            return None;
        }
        let process = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::warn!(pid, %err, "window_exe_path: OpenProcess denied");
                return None;
            }
        };
        let mut buf = [0u16; MAX_IMAGE_PATH];
        let mut len = buf.len() as u32;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        match queried {
            Ok(()) if len > 0 => Some(String::from_utf16_lossy(&buf[..len as usize])),
            Ok(()) => {
                tracing::warn!(pid, "window_exe_path: empty image path");
                None
            }
            Err(err) => {
                tracing::warn!(pid, %err, "window_exe_path: QueryFullProcessImageNameW failed");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_or_recover_reads_through_a_poisoned_mutex() {
        let m = Mutex::new(41);

        // Poison it for real: panic while holding the guard.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = m.lock().unwrap();
            panic!("poison the mutex on purpose");
        }));
        std::panic::set_hook(previous);

        assert!(poisoned.is_err(), "the closure must have panicked");
        assert!(
            m.is_poisoned(),
            "the mutex must actually be poisoned, else this test proves nothing"
        );
        assert_eq!(
            *lock_or_recover(&m),
            41,
            "a poisoned lock still yields the value"
        );
    }

    #[test]
    fn lock_or_recover_behaves_like_a_plain_lock_when_healthy() {
        let m = Mutex::new(String::from("ok"));
        assert_eq!(lock_or_recover(&m).as_str(), "ok");
        *lock_or_recover(&m) = String::from("updated");
        assert_eq!(lock_or_recover(&m).as_str(), "updated");
    }

    #[test]
    fn wide_appends_a_terminator() {
        assert_eq!(wide("hi"), vec![u16::from(b'h'), u16::from(b'i'), 0]);
        assert_eq!(wide(""), vec![0]);
    }

    #[test]
    fn guard_abi_returns_the_body_value_when_it_does_not_panic() {
        assert_eq!(guard_abi("test::ok", || 7, || 0), 7);
    }

    #[test]
    fn guard_abi_swallows_a_panic_and_returns_the_fallback() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let got = guard_abi(
            "test::panic",
            || -> i32 { panic!("boom, as if from a wndproc") },
            || 0,
        );
        std::panic::set_hook(previous);
        assert_eq!(got, 0, "a panicking callback must degrade, not unwind");
    }
}
