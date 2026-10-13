//! Hover preview: a thumbnail popup shown above a hovered task button.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Design constraints (from review):
//! - The taskbar must **not** call `capture` from `WM_MOUSEMOVE`; the caller
//!   throttles with a `WM_TIMER` and only then calls [`show`].
//! - One process-lifetime popup window, shown/hidden (never recreated).
//! - The captured `HBITMAP` is released on redirect and on destroy.

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetDC, GetMonitorInfoW, HBITMAP, HBRUSH, HGDIOBJ,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, PAINTSTRUCT, ReleaseDC,
    SRCCOPY, SelectObject, StretchBlt,
};
use windows::Win32::Storage::Xps::PrintWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, PW_RENDERFULLCONTENT, RegisterClassW,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow,
    WINDOW_STYLE, WM_DESTROY, WM_PAINT, WNDCLASS_STYLES, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

/// Default preview size, in logical pixels.
pub const PREVIEW_W: i32 = 240;
pub const PREVIEW_H: i32 = 160;

/// Whether the hover preview is enabled. **Off by default** — the feature is
/// low-value for most users. Toggle it in the settings window or opt in with
/// `MINICON_HOVER_PREVIEW=1` (env wins; see `config`).
pub fn enabled() -> bool {
    crate::config::current().hover_preview
}

/// Vertical gap between the button and the preview.
const GAP: i32 = 6;
/// Dark backing colour so letterboxed thumbnails read as a single card.
const BG_COLOR: COLORREF = COLORREF(0x0020_2020);

const CLASS_NAME: &str = "minicon_winmgr_hover_preview";
const WINDOW_TITLE: &str = "minicon preview";

/// `ERROR_CLASS_ALREADY_EXISTS`, tolerated on repeated `install`.
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

// --- process-lifetime state ------------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static PREVIEW: AtomicIsize = AtomicIsize::new(0);
/// The window handle currently captured into [`BITMAP`].
static TARGET: AtomicIsize = AtomicIsize::new(0);
/// The captured thumbnail (`HBITMAP` as `isize`), owned by this module.
static BITMAP: AtomicIsize = AtomicIsize::new(0);
/// Last anchor rectangle, kept only for diagnostics.
static ANCHOR: Mutex<Option<RECT>> = Mutex::new(None);

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

fn preview() -> Option<HWND> {
    existing(&PREVIEW)
}

/// Computes the preview rectangle for a hovered `btn`, preferring just above
/// the bar and flipping below when there is no room above; centred on `btn`
/// and clamped inside `work`. Pure, so it can be unit-tested without Win32.
pub fn target_rect(btn: RECT, work: RECT) -> RECT {
    let center_x = (btn.left + btn.right) / 2;
    let mut left = center_x - PREVIEW_W / 2;

    // Prefer sitting above the button; flip below if that would leave `work`.
    let above = btn.top - PREVIEW_H - GAP;
    let mut top = if above >= work.top {
        above
    } else {
        btn.bottom + GAP
    };

    // Clamp inside the work area (right/bottom edges first so the card keeps
    // its size when the work area is larger than the preview).
    if left + PREVIEW_W > work.right {
        left = work.right - PREVIEW_W;
    }
    if left < work.left {
        left = work.left;
    }
    if top + PREVIEW_H > work.bottom {
        top = work.bottom - PREVIEW_H;
    }
    if top < work.top {
        top = work.top;
    }

    RECT {
        left,
        top,
        right: left + PREVIEW_W,
        bottom: top + PREVIEW_H,
    }
}

/// Captures `hwnd` into a fresh `w`×`h` bitmap via `PrintWindow`
/// (`PW_RENDERFULLCONTENT`). Returns `None` on failure so the caller can
/// degrade gracefully. The caller owns the bitmap and must delete it.
///
/// SAFETY: `hwnd` should be a live top-level window; must be called on the
/// message thread.
pub unsafe fn capture(hwnd: isize, w: i32, h: i32) -> Option<HBITMAP> {
    if hwnd == 0 || w <= 0 || h <= 0 {
        return None;
    }
    // SAFETY: standard GDI capture sequence. `src_dc` is a borrowed screen DC
    // released below; `mem_dc` is created and deleted by us; the bitmap is
    // either returned to the caller or deleted on failure.
    unsafe {
        let src_dc = GetDC(None);
        if src_dc.0.is_null() {
            tracing::warn!("hover preview: GetDC failed");
            return None;
        }

        let mem_dc = CreateCompatibleDC(Some(src_dc));
        if mem_dc.0.is_null() {
            ReleaseDC(None, src_dc);
            tracing::warn!("hover preview: CreateCompatibleDC failed");
            return None;
        }

        let bitmap = CreateCompatibleBitmap(src_dc, w, h);
        if bitmap.0.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, src_dc);
            tracing::warn!("hover preview: CreateCompatibleBitmap failed");
            return None;
        }

        // Select the bitmap so `PrintWindow` renders into it.
        let previous = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
        let ok = PrintWindow(
            hwnd_from(hwnd),
            mem_dc,
            windows::Win32::Storage::Xps::PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
        )
        .as_bool();
        // Restore the original object before deleting the DC; otherwise the
        // bitmap stays owned by the DC and leaks.
        SelectObject(mem_dc, previous);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, src_dc);

        if ok {
            Some(bitmap)
        } else {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            tracing::warn!(hwnd, "hover preview: PrintWindow failed");
            None
        }
    }
}

/// Creates the (hidden) preview popup window class + window.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if !enabled() {
        tracing::info!("hover preview disabled (set MINICON_HOVER_PREVIEW=1 to enable)");
        return Ok(());
    }
    if preview().is_some() {
        return Ok(()); // already installed
    }
    // SAFETY: standard Win32 window setup; every buffer outlives its call and
    // `wndproc` is a valid `extern "system"` function.
    unsafe {
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);
        let background = CreateSolidBrush(BG_COLOR);

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )
            .unwrap_or_default(),
            hbrBackground: background,
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            let code = GetLastError().0;
            if code != ERROR_CLASS_ALREADY_EXISTS {
                return Err(
                    format!("RegisterClassW(hover_preview) failed (Win32 error {code})").into(),
                );
            }
        }

        // Hidden on creation: `WS_POPUP` without `WS_VISIBLE`.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(WS_POPUP.0),
            0,
            0,
            PREVIEW_W,
            PREVIEW_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        PREVIEW.store(hwnd.0 as isize, Ordering::SeqCst);
        tracing::info!(hwnd = ?hwnd, "hover preview window created (hidden)");
    }
    Ok(())
}

/// Releases the captured bitmap, if any (leaves [`TARGET`] untouched).
fn release_bitmap() {
    let raw = BITMAP.swap(0, Ordering::SeqCst);
    if raw != 0 {
        // SAFETY: `raw` came from `CreateCompatibleBitmap` and is not selected
        // into any DC (we always restore before returning from `capture`).
        unsafe {
            let _ = DeleteObject(HGDIOBJ(raw as *mut core::ffi::c_void));
        }
        tracing::debug!("hover preview: released captured bitmap");
    }
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
            // Fall back to the virtual screen so the preview still appears.
            RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            }
        }
    }
}

/// Shows the preview for `target` (a window handle) anchored under `anchor`
/// (a screen rectangle). Captures only when the target changes.
pub fn show(target: isize, anchor: RECT) {
    let Some(hwnd) = preview() else {
        tracing::debug!("hover preview: show ignored (not installed)");
        return;
    };

    // Capture only when the hovered target actually changed; a re-hover over
    // the same window reuses the existing thumbnail.
    if TARGET.load(Ordering::SeqCst) != target {
        release_bitmap();
        match unsafe { capture(target, PREVIEW_W, PREVIEW_H) } {
            Some(bitmap) => {
                BITMAP.store(bitmap.0 as isize, Ordering::SeqCst);
                tracing::debug!(target, "hover preview: captured target");
            }
            None => tracing::warn!(target, "hover preview: capture failed"),
        }
        TARGET.store(target, Ordering::SeqCst);
    }

    let work = work_area(anchor);
    let rect = target_rect(anchor, work);

    if let Ok(mut slot) = ANCHOR.lock() {
        tracing::debug!(previous = ?*slot, current = ?anchor, "hover preview: anchor moved");
        *slot = Some(anchor);
    }

    // SAFETY: `hwnd` is our live popup; `rect` is within the monitor work area.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            PREVIEW_W,
            PREVIEW_H,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
    tracing::debug!(target, ?rect, "hover preview shown");
}

/// Hides the preview and keeps the captured bitmap for the next hover.
pub fn hide() {
    let Some(hwnd) = preview() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    tracing::debug!("hover preview hidden");
}

/// Paints the dark card and blits the thumbnail, if one is available.
fn paint(hwnd: HWND) {
    // SAFETY: `hwnd` is our live popup; all GDI objects created here are
    // deleted before the function returns.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);

        let client = RECT {
            left: 0,
            top: 0,
            right: PREVIEW_W,
            bottom: PREVIEW_H,
        };
        let background: HBRUSH = CreateSolidBrush(BG_COLOR);
        FillRect(hdc, &client, background);
        let _ = DeleteObject(HGDIOBJ(background.0));

        let raw = BITMAP.load(Ordering::SeqCst);
        if raw != 0 {
            let bitmap = HBITMAP(raw as *mut core::ffi::c_void);
            let mem_dc = CreateCompatibleDC(Some(hdc));
            if !mem_dc.0.is_null() {
                let previous = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
                let _ = StretchBlt(
                    hdc,
                    0,
                    0,
                    PREVIEW_W,
                    PREVIEW_H,
                    Some(mem_dc),
                    0,
                    0,
                    PREVIEW_W,
                    PREVIEW_H,
                    SRCCOPY,
                );
                // Restore before deleting the DC so the DC does not free our
                // still-owned bitmap.
                SelectObject(mem_dc, previous);
                let _ = DeleteDC(mem_dc);
            }
        }

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Window procedure for the preview popup.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // The preview never owns the message loop, so no PostQuitMessage.
            release_bitmap();
            TARGET.store(0, Ordering::SeqCst);
            PREVIEW.store(0, Ordering::SeqCst);
            if let Ok(mut slot) = ANCHOR.lock() {
                *slot = None;
            }
            tracing::info!("hover preview window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::{PREVIEW_H, PREVIEW_W, target_rect};
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
    fn prefers_above_the_bar() {
        let btn = rect(100, 1000, 220, 1036);
        let work = rect(0, 0, 1920, 1040);
        let out = target_rect(btn, work);
        assert!(out.bottom <= btn.top, "preview must sit above the button");
    }

    #[test]
    fn flips_below_when_no_room_above() {
        let btn = rect(100, 4, 220, 40);
        let work = rect(0, 0, 1920, 1040);
        let out = target_rect(btn, work);
        assert!(
            out.top >= btn.bottom,
            "preview must flip below when above is out of room"
        );
        assert!(out.bottom <= work.bottom, "flipped preview stays in work");
    }

    #[test]
    fn centres_horizontally_on_the_button() {
        let btn = rect(100, 1000, 220, 1036);
        let work = rect(0, 0, 1920, 1040);
        let out = target_rect(btn, work);
        assert_eq!(out.left, (btn.left + btn.right) / 2 - PREVIEW_W / 2);
        assert_eq!(out.right - out.left, PREVIEW_W);
        assert_eq!(out.bottom - out.top, PREVIEW_H);
    }

    #[test]
    fn clamps_to_work_area_edges() {
        let work = rect(0, 0, 1920, 1040);
        let left_btn = rect(0, 1000, 40, 1036);
        let right_btn = rect(1900, 1000, 1920, 1036);

        let left_out = target_rect(left_btn, work);
        assert!(left_out.left >= work.left);
        assert!(left_out.right <= work.right);

        let right_out = target_rect(right_btn, work);
        assert!(right_out.left >= work.left);
        assert!(right_out.right <= work.right);
    }

    #[test]
    fn respects_a_non_primary_monitor_origin() {
        let btn = rect(2000, 1000, 2120, 1036);
        let work = rect(1920, 0, 3840, 1040);
        let out = target_rect(btn, work);
        assert!(out.left >= work.left);
        assert!(out.right <= work.right);
        assert!(out.top >= work.top);
    }
}
