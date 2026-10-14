//! Window-management hotkeys: snap and move windows between monitors.
//!
//! Registers `Win+Left` / `Win+Right` / `Win+Up` on a hidden top-level window.
//! On `WM_HOTKEY` the foreground window is snapped into the work area of the
//! monitor it currently sits on. All failures degrade gracefully (a taken
//! hotkey or a missing monitor just logs; nothing panics).

use std::mem::size_of;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_WIN, RegisterHotKey, UnregisterHotKey, VK_LEFT,
    VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClassNameW, GetForegroundWindow, RegisterClassW,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, WINDOW_EX_STYLE, WM_DESTROY, WM_HOTKEY,
    WNDCLASS_STYLES, WNDCLASSW, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

/// Class name of our hidden receiver window. The `minicon_winmgr_` prefix lets
/// the snap handler skip windows the shell itself owns.
const CLASS_NAME: &str = "minicon_winmgr_snap_hotkeys";
const WINDOW_TITLE: &str = "minicon-winmgr (snap hotkeys)";

/// Prefix shared by every window this process creates.
const OWN_CLASS_PREFIX: &str = "minicon_winmgr_";

const HOTKEY_LEFT: i32 = 1;
const HOTKEY_RIGHT: i32 = 2;
const HOTKEY_UP: i32 = 3;

/// A snap target within a monitor work area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    /// Left half of the area.
    Left,
    /// Right half of the area.
    Right,
    /// Whole area.
    Maximize,
}

/// Computes the target rectangle `(x, y, w, h)` for `action` inside `area`.
///
/// Pure function (no Win32); used by tests.
///
/// `w / 2` (floor) is the left half's width and also the right half's width;
/// the right half starts at `x + w - w / 2`, so the two halves never overlap
/// (for odd `w` the right half stays flush to the right edge and leaves a
/// one-pixel seam).
pub fn snap_rect(area: (i32, i32, i32, i32), action: Snap) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = area;
    match action {
        Snap::Left => (x, y, w / 2, h),
        Snap::Right => (x + w - w / 2, y, w / 2, h),
        Snap::Maximize => area,
    }
}

/// Candidate key combinations for `action`, in preference order: the canonical
/// `Win+*` first, then a `Ctrl+Alt+*` fallback for when another shell (companion
/// mode) already owns the `Win+*` chord.
///
/// Returns `(label, modifiers, virtual-key)` triples. Pure (no Win32); used by
/// tests to verify that the fallback only swaps modifiers, never the key.
pub fn combos(action: Snap) -> [(&'static str, HOT_KEY_MODIFIERS, u32); 2] {
    let (win_label, alt_label, vk) = match action {
        Snap::Left => ("Win+Left", "Ctrl+Alt+Left", VK_LEFT.0 as u32),
        Snap::Right => ("Win+Right", "Ctrl+Alt+Right", VK_RIGHT.0 as u32),
        Snap::Maximize => ("Win+Up", "Ctrl+Alt+Up", VK_UP.0 as u32),
    };
    [
        (win_label, MOD_WIN, vk),
        (alt_label, MOD_CONTROL | MOD_ALT, vk),
    ]
}

/// Installs the window-management hotkeys and prepares their hidden window.
///
/// Registers a private window class and a hidden top-level window, then arms
/// `Win+Left` / `Win+Right` / `Win+Up`. Individual hotkey registration failures
/// are logged but not fatal.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: standard Win32 window setup; the class name / title buffers
    // outlive their calls and the wndproc is a valid `extern "system"` fn.
    unsafe {
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            return Err("RegisterClassW(hotkeys) failed".into());
        }

        // Hidden top-level window: WS_POPUP without WS_VISIBLE, no parent.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        tracing::info!(hwnd = ?hwnd, "snap hotkey window created (hidden)");

        for (id, action) in [
            (HOTKEY_LEFT, Snap::Left),
            (HOTKEY_RIGHT, Snap::Right),
            (HOTKEY_UP, Snap::Maximize),
        ] {
            let mut registered = false;
            for (label, mods, vk) in combos(action) {
                match RegisterHotKey(Some(hwnd), id, mods, vk) {
                    Ok(()) => {
                        tracing::info!(hotkey = label, "snap hotkey registered");
                        registered = true;
                        break;
                    }
                    Err(err) => {
                        tracing::warn!(hotkey = label, %err, "snap hotkey candidate taken")
                    }
                }
            }
            if !registered {
                tracing::error!(action = ?action, "snap hotkey unreachable");
            }
        }
    }
    Ok(())
}

/// Window procedure for the hidden snap-hotkey window.
///
/// The body runs under [`crate::win_util::guard_abi`] so a panic cannot unwind
/// across the FFI boundary; on panic it degrades to `DefWindowProcW`.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the guard only runs the body and degrades to the default handler
    // on a panic; all handles come straight from Win32.
    crate::win_util::guard_abi(
        "hotkeys::wndproc",
        || unsafe { wndproc_inner(hwnd, msg, wparam, lparam) },
        || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    )
}

/// Inner body of [`wndproc`]; every unhandled message falls back to
/// `DefWindowProcW`.
unsafe extern "system" fn wndproc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            let action = match wparam.0 as i32 {
                HOTKEY_LEFT => Some(Snap::Left),
                HOTKEY_RIGHT => Some(Snap::Right),
                HOTKEY_UP => Some(Snap::Maximize),
                _ => None,
            };
            if let Some(action) = action {
                // SAFETY: `hwnd` is our live window; the helper only uses the
                // foreground window and monitor APIs.
                unsafe { snap_foreground(action) };
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            for id in [HOTKEY_LEFT, HOTKEY_RIGHT, HOTKEY_UP] {
                // SAFETY: best-effort cleanup of our own hotkeys.
                let _ = unsafe { UnregisterHotKey(Some(hwnd), id) };
            }
            tracing::info!("snap hotkeys unregistered");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Snaps the foreground window into `action`'s rectangle on its monitor's work
/// area. No-op (with a debug log) for missing or self-owned windows.
///
/// # Safety
/// Calls Win32 window/monitor APIs; must run on the message thread.
unsafe fn snap_foreground(action: Snap) {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        tracing::debug!(?action, "no foreground window to snap");
        return;
    }

    let class = unsafe { class_name(fg) };
    if class.starts_with(OWN_CLASS_PREFIX) {
        tracing::debug!(%class, "ignoring snap for shell-owned window");
        return;
    }

    let monitor: HMONITOR = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        tracing::warn!("MonitorFromWindow returned null; skipping snap");
        return;
    }

    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        tracing::warn!("GetMonitorInfoW failed; skipping snap");
        return;
    }

    let work = info.rcWork;
    let area = (
        work.left,
        work.top,
        work.right - work.left,
        work.bottom - work.top,
    );
    let (x, y, w, h) = snap_rect(area, action);
    // SAFETY: `fg` is a live top-level window; the rect is within its monitor.
    let result = unsafe { SetWindowPos(fg, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) };
    match result {
        Ok(()) => tracing::info!(?action, x, y, w, h, %class, "snapped foreground window"),
        Err(err) => {
            tracing::warn!(?action, %err, %class, "SetWindowPos failed; window not snapped")
        }
    }
}

/// Reads a window's class name (empty string on failure).
///
/// # Safety
/// `hwnd` must be a valid window handle.
unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

#[cfg(test)]
mod tests {
    use super::{Snap, combos, snap_rect};
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_WIN};

    #[test]
    fn combo_primary_uses_win_and_fallback_uses_ctrl_alt() {
        for action in [Snap::Left, Snap::Right, Snap::Maximize] {
            let [primary, fallback] = combos(action);
            assert_eq!(
                primary.1, MOD_WIN,
                "primary must use MOD_WIN for {action:?}"
            );
            assert_eq!(
                fallback.1,
                MOD_CONTROL | MOD_ALT,
                "fallback must use MOD_CONTROL|MOD_ALT for {action:?}"
            );
        }
    }

    #[test]
    fn combo_fallback_shares_the_primary_virtual_key() {
        for action in [Snap::Left, Snap::Right, Snap::Maximize] {
            let [primary, fallback] = combos(action);
            assert_eq!(
                primary.2, fallback.2,
                "fallback must reuse the primary VK for {action:?}"
            );
        }
    }

    #[test]
    fn maximize_returns_area_unchanged() {
        let area = (10, 20, 1920, 1080);
        assert_eq!(snap_rect(area, Snap::Maximize), area);
    }

    #[test]
    fn left_and_right_split_even_width_without_overlap() {
        let area = (100, 50, 1000, 800);
        let left = snap_rect(area, Snap::Left);
        let right = snap_rect(area, Snap::Right);
        assert_eq!(left, (100, 50, 500, 800));
        assert_eq!(right, (600, 50, 500, 800));
        assert!(left.0 + left.2 <= right.0, "halves must not overlap");
        assert_eq!(left.2 + right.2, area.2, "halves cover the full width");
    }

    #[test]
    fn odd_width_keeps_halves_disjoint_and_flush() {
        let area = (0, 0, 5, 10);
        let left = snap_rect(area, Snap::Left);
        let right = snap_rect(area, Snap::Right);
        // Left width is floor(5/2) = 2.
        assert_eq!(left, (0, 0, 2, 10));
        // Right half starts at x + w - w/2 = 3 and stays flush to the right edge.
        assert_eq!(right, (3, 0, 2, 10));
        assert!(left.0 + left.2 <= right.0, "halves must not overlap");
        assert_eq!(
            right.0 + right.2,
            area.0 + area.2,
            "right half flush to edge"
        );
    }
}
