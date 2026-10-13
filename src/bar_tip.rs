//! Icon-only hover tip: a small label bubble shown above a hovered icon-only
//! task square, so a title-less button can still be identified.
//!
//! The taskbar schedules [`show`] from its debounced hover timer (the same
//! `WM_TIMER` slot the thumbnail preview uses) and [`hide`]s it on move/leave/
//! click. One process-lifetime popup window is created by [`install`] and
//! shown/hidden (never recreated); the message thread is the sole user, so the
//! stored handles are safe by construction. The window is
//! `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST`, so it never takes
//! focus and never appears in Alt+Tab.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
    EndPaint, FillRect, FrameRect, GetDC, GetMonitorInfoW, GetTextExtentPoint32W, HBRUSH, HGDIOBJ,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetSystemMetrics, IDC_ARROW, LoadCursorW,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetWindowPos, ShowWindow, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

const CLASS_NAME: &str = "minicon_winmgr_bar_tip";
const WINDOW_TITLE: &str = "minicon tip";

/// `ERROR_CLASS_ALREADY_EXISTS`, tolerated on a repeated `install`.
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

/// Bubble height, in logical pixels.
const TIP_H: i32 = 24;
/// Horizontal padding inside the bubble (both sides).
const PAD_X: i32 = 10;
/// Vertical gap between the button and the bubble.
const GAP: i32 = 6;
/// Minimum bubble width.
const MIN_W: i32 = 40;

// --- process-lifetime state ------------------------------------------------
static TIP: AtomicIsize = AtomicIsize::new(0);
/// The label currently shown (kept so `WM_PAINT` can redraw without the caller).
static TEXT: Mutex<String> = Mutex::new(String::new());

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn tip() -> Option<HWND> {
    let raw = TIP.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

/// The bubble rectangle for a `w`×`h` bubble anchored to `anchor` (a screen
/// rect): centred on the anchor, preferring just above and flipping below when
/// there is no room, then clamped inside `work`. Pure, so it is unit-testable.
pub fn tip_rect(w: i32, h: i32, anchor: RECT, work: RECT) -> RECT {
    let center_x = (anchor.left + anchor.right) / 2;
    let mut left = center_x - w / 2;

    let above = anchor.top - h - GAP;
    let mut top = if above >= work.top {
        above
    } else {
        anchor.bottom + GAP
    };

    // Clamp inside the work area (right/bottom edges first so the bubble keeps
    // its size when the work area is larger than it).
    if left + w > work.right {
        left = work.right - w;
    }
    if left < work.left {
        left = work.left;
    }
    if top + h > work.bottom {
        top = work.bottom - h;
    }
    if top < work.top {
        top = work.top;
    }

    RECT {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}

/// The tip font, created once and never freed (process-lifetime GDI object).
fn font() -> isize {
    static F: OnceLock<isize> = OnceLock::new();
    *F.get_or_init(|| {
        let face = wide("Segoe UI");
        // SAFETY: `face` outlives the call; the font is process-lifetime.
        unsafe {
            CreateFontW(
                -12,
                0,
                0,
                0,
                400, // FW_NORMAL
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                PCWSTR(face.as_ptr()),
            )
            .0 as isize
        }
    })
}

/// Theme colours, loaded once at first paint (mirrors `taskbar::theme`), so a
/// repaint never touches the config file.
fn theme() -> crate::config::Theme {
    static T: OnceLock<crate::config::Theme> = OnceLock::new();
    *T.get_or_init(|| crate::config::load().theme)
}

/// Width of `text` in the tip font, in logical pixels. Best effort (`0` on
/// failure), so the bubble simply falls back to [`MIN_W`].
fn measure(text: &str) -> i32 {
    let buf: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `buf`/`size` outlive their calls; a borrowed screen DC is released
    // before returning.
    unsafe {
        let hdc = GetDC(None);
        if hdc.0.is_null() {
            return 0;
        }
        let old = SelectObject(hdc, HGDIOBJ(font() as *mut core::ffi::c_void));
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &buf, &mut size);
        SelectObject(hdc, old);
        ReleaseDC(None, hdc);
        size.cx
    }
}

/// Creates the (hidden) tip popup window class + window.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if tip().is_some() {
        return Ok(()); // already installed
    }
    // SAFETY: standard Win32 window setup; every buffer outlives its call and
    // `wndproc` is a valid `extern "system"` function.
    unsafe {
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);
        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // No class brush: the whole client is filled in `WM_PAINT`.
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            let code = GetLastError().0;
            if code != ERROR_CLASS_ALREADY_EXISTS {
                return Err(format!("RegisterClassW(bar_tip) failed (Win32 error {code})").into());
            }
        }

        // Hidden on creation: `WS_POPUP` without `WS_VISIBLE`; `NOACTIVATE` so
        // showing it never steals focus from the bar.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            MIN_W,
            TIP_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        TIP.store(hwnd.0 as isize, Ordering::SeqCst);
        tracing::info!(hwnd = ?hwnd, "bar tip window created (hidden)");
    }
    Ok(())
}

/// Work area (taskbar-excluded bounds) of the monitor containing `anchor`.
fn work_area(anchor: RECT) -> RECT {
    // SAFETY: pure monitor queries; `info` is a valid out-parameter.
    unsafe {
        let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            }
        }
    }
}

/// Shows the tip carrying `label`, anchored under `anchor` (a screen rect).
pub fn show(label: &str, anchor: RECT) {
    let Some(hwnd) = tip() else {
        tracing::debug!("bar tip: show ignored (not installed)");
        return;
    };
    let w = (measure(label) + 2 * PAD_X).max(MIN_W);
    let work = work_area(anchor);
    let rect = tip_rect(w, TIP_H, anchor, work);

    if let Ok(mut slot) = TEXT.lock() {
        slot.clear();
        slot.push_str(label);
    }

    // SAFETY: `hwnd` is our live popup; `rect` is within the monitor work area.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            w,
            TIP_H,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Hides the tip (the window is kept for the next hover).
pub fn hide() {
    let Some(hwnd) = tip() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// Paints the bubble: a surface fill, an accent border, and the label.
fn paint(hwnd: HWND) {
    // SAFETY: `hwnd` is our live popup; every GDI object created here is deleted
    // before the function returns (the cached font is the exception — it is
    // process-lifetime and only ever selected in and back out).
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);

        let t = theme();
        let bg = CreateSolidBrush(COLORREF(t.surface));
        FillRect(hdc, &client, bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));
        let border = CreateSolidBrush(COLORREF(t.accent));
        FrameRect(hdc, &client, border);
        let _ = DeleteObject(HGDIOBJ(border.0));

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(t.text));
        let old = SelectObject(hdc, HGDIOBJ(font() as *mut core::ffi::c_void));
        let label = TEXT.lock().map(|s| s.clone()).unwrap_or_default();
        let mut buf: Vec<u16> = label.encode_utf16().collect();
        let mut r = client;
        DrawTextW(
            hdc,
            &mut buf,
            &mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Window procedure for the tip popup.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // The tip never owns the message loop, so no PostQuitMessage.
            TIP.store(0, Ordering::SeqCst);
            if let Ok(mut slot) = TEXT.lock() {
                slot.clear();
            }
            tracing::info!("bar tip window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::{TIP_H, tip_rect};
    use windows::Win32::Foundation::RECT;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn prefers_above_the_button() {
        let anchor = rect(100, 1000, 140, 1036);
        let work = rect(0, 0, 1920, 1040);
        let out = tip_rect(120, TIP_H, anchor, work);
        assert!(out.bottom <= anchor.top, "tip must sit above the button");
        assert_eq!(out.bottom - out.top, TIP_H);
    }

    #[test]
    fn flips_below_when_no_room_above() {
        let anchor = rect(100, 4, 140, 40);
        let work = rect(0, 0, 1920, 1040);
        let out = tip_rect(120, TIP_H, anchor, work);
        assert!(out.top >= anchor.bottom, "tip must flip below");
        assert!(out.bottom <= work.bottom);
    }

    #[test]
    fn centres_on_the_button_and_keeps_its_size() {
        let anchor = rect(100, 1000, 140, 1036);
        let work = rect(0, 0, 1920, 1040);
        let out = tip_rect(120, TIP_H, anchor, work);
        assert_eq!(out.left, (anchor.left + anchor.right) / 2 - 60);
        assert_eq!(out.right - out.left, 120);
    }

    #[test]
    fn clamps_to_the_work_area_edges() {
        let work = rect(0, 0, 1920, 1040);
        let left_anchor = rect(0, 1000, 40, 1036);
        let right_anchor = rect(1900, 1000, 1920, 1036);

        let l = tip_rect(120, TIP_H, left_anchor, work);
        assert!(l.left >= work.left && l.right <= work.right);
        let r = tip_rect(120, TIP_H, right_anchor, work);
        assert!(r.left >= work.left && r.right <= work.right);
    }

    #[test]
    fn respects_a_non_primary_monitor_origin() {
        let anchor = rect(2000, 1000, 2040, 1036);
        let work = rect(1920, 0, 3840, 1040);
        let out = tip_rect(120, TIP_H, anchor, work);
        assert!(out.left >= work.left && out.right <= work.right && out.top >= work.top);
    }
}
