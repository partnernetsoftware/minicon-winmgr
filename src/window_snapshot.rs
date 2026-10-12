//! Top-level window enumeration.
//!
//! Seeds the later taskbar (M3): a read-only snapshot of every top-level
//! window, with the fields a task list needs (class, title, pid, visibility).
//! M2 only observes — it never moves, hides, or selects windows.

use windows::Win32::Foundation::{HWND, LPARAM};
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
        (!has_owner || is_app) && !is_tool
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
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
