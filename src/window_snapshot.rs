//! Top-level window enumeration.
//!
//! Seeds the later taskbar (M3): a read-only snapshot of every top-level
//! window, with the fields a task list needs (class, title, pid, visibility).
//! M2 only observes — it never moves, hides, or selects windows.

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

/// Read-only description of a top-level window.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: HWND,
    pub pid: u32,
    pub class: String,
    pub title: String,
    pub visible: bool,
}

/// Enumerates all top-level windows into a snapshot vector.
pub fn snapshot() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    // SAFETY: `enum_proc` only pushes into the Vec behind this pointer, and the
    // Vec outlives the synchronous EnumWindows call.
    let lparam = LPARAM(&mut out as *mut Vec<WindowInfo> as isize);
    unsafe {
        let _ = EnumWindows(Some(enum_proc), lparam);
    }
    out
}

/// Logs a snapshot: one info line with totals, then one debug line per window.
pub fn log_snapshot() {
    let windows = snapshot();
    let visible = windows.iter().filter(|w| w.visible).count();
    tracing::info!(total = windows.len(), visible, "top-level window snapshot");
    for w in &windows {
        tracing::debug!(
            hwnd = ?w.hwnd,
            pid = w.pid,
            visible = w.visible,
            class = %w.class,
            title = %w.title,
            "window"
        );
    }
}

/// Windows worth showing in a task list: visible, titled, owned by another
/// process, and either ownerless or explicitly taskbar-eligible — excluding
/// tool windows and other helper chrome.
pub fn taskbar_windows() -> Vec<WindowInfo> {
    let self_pid = std::process::id();
    snapshot()
        .into_iter()
        .filter(|w| w.visible && !w.title.is_empty() && w.pid != self_pid)
        .filter(|w| is_app_window(w.hwnd))
        .collect()
}

/// Application windows on this desktop, including settings but excluding shell surfaces.
pub fn desktop_windows() -> Vec<WindowInfo> {
    snapshot()
        .into_iter()
        .filter(|window| {
            window.visible
                && !window.title.is_empty()
                && is_app_window(window.hwnd)
                && !matches!(
                    window.class.as_str(),
                    "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
                )
        })
        .collect()
}

/// Classic "should this appear on the taskbar" test.
fn is_app_window(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` came from EnumWindows, so it is a live top-level window.
    unsafe {
        let has_owner = GetWindow(hwnd, GW_OWNER)
            .map(|h| !h.0.is_null())
            .unwrap_or(false);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let is_tool = ex & WS_EX_TOOLWINDOW.0 != 0;
        let is_app = ex & WS_EX_APPWINDOW.0 != 0;
        is_taskbar_eligible(has_owner, is_tool, is_app, is_cloaked(hwnd))
    }
}

/// Pure "belongs on the taskbar" predicate, split out so it can be unit-tested
/// without a live window.
///
/// `cloaked` is the DWM-hidden flag: cloaked windows are not on screen (they
/// are kept around by the shell for UWP apps, virtual desktops, and the Start
/// menu host). The classic owner/`WS_EX_TOOLWINDOW` test alone lets them
/// through — the Start menu's `Windows.UI.Core.CoreWindow` has neither an owner
/// nor `WS_EX_TOOLWINDOW`, so it would otherwise get a task button that cannot
/// be activated or closed.
fn is_taskbar_eligible(has_owner: bool, is_tool: bool, is_app: bool, cloaked: bool) -> bool {
    (!has_owner || is_app) && !is_tool && !cloaked
}

/// Whether DWM currently cloaks `hwnd` (hidden UWP shell window / other desktop).
///
/// Best-effort: when the attribute is unsupported the call fails and the window
/// is treated as *not* cloaked, matching the pre-existing behaviour.
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: `hwnd` is a live top-level window; `cloaked` is a `u32` the API
    // fills in when `DWMWA_CLOAKED` is supported.
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    result.is_ok() && cloaked != 0
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: the guard only runs the body; enumeration continues on a panic,
    // so one bad window is skipped rather than truncating the snapshot.
    crate::win_util::guard_abi(
        "window_snapshot::enum_proc",
        || unsafe { enum_proc_inner(hwnd, lparam) },
        || BOOL(1),
    )
}

unsafe extern "system" fn enum_proc_inner(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` was set by `snapshot` to a valid `*mut Vec<WindowInfo>`.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };
    out.push(unsafe { read_window(hwnd) });
    BOOL(1) // TRUE keeps enumeration going
}

unsafe fn read_window(hwnd: HWND) -> WindowInfo {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    WindowInfo {
        hwnd,
        pid,
        class: unsafe { class_name(hwnd) },
        title: unsafe { window_text(hwnd) },
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
    }
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

unsafe fn window_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let got = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if got <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..got as usize])
}

#[cfg(test)]
mod tests {
    use super::is_taskbar_eligible;

    #[test]
    fn plain_ownerless_window_is_eligible() {
        assert!(is_taskbar_eligible(false, false, false, false));
    }

    #[test]
    fn tool_window_is_never_eligible() {
        assert!(!is_taskbar_eligible(false, true, false, false));
        assert!(!is_taskbar_eligible(true, true, false, false));
    }

    #[test]
    fn owned_window_needs_the_appwindow_flag() {
        assert!(!is_taskbar_eligible(true, false, false, false));
        assert!(is_taskbar_eligible(true, false, true, false));
    }

    #[test]
    fn cloaked_window_is_never_eligible() {
        // The Start menu host's `Windows.UI.Core.CoreWindow`: no owner, no
        // `WS_EX_TOOLWINDOW`, but DWM-cloaked — it must not get a task button.
        assert!(!is_taskbar_eligible(false, false, false, true));
        assert!(!is_taskbar_eligible(true, false, true, true));
    }
}
