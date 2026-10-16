//! Hover preview: a thumbnail popup shown above a hovered task button.
//!
//! DWM composes one live thumbnail into our popup. No WM_PRINT requests,
//! screenshots, worker threads, bitmap copies, or unbounded capture retries.
//! Registration is released when hidden, redirected, or destroyed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DwmQueryThumbnailSourceSize,
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DrawTextW, EndPaint, FillRect,
    GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect,
    PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN,
    SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, WINDOW_STYLE, WM_DESTROY,
    WM_PAINT, WNDCLASS_STYLES, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::win_util::{guard_abi, lock_or_recover, wide};

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
const PREVIEW_GAP: i32 = 6;
/// Dark backing colour so letterboxed thumbnails read as a single card.
fn bg_color() -> COLORREF {
    COLORREF(crate::config::theme().surface)
}

const CLASS_NAME: &str = "minicon_winmgr_hover_preview";
const WINDOW_TITLE: &str = "minicon preview";

// --- process-lifetime state ------------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static PREVIEW: AtomicIsize = AtomicIsize::new(0);
/// Source window associated with the current thumbnail.
static TARGET: AtomicIsize = AtomicIsize::new(0);
/// One DWM registration owned by this module (zero means unavailable).
static THUMBNAIL: AtomicIsize = AtomicIsize::new(0);
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

/// Aspect-fit with a small border; invalid source dimensions produce no thumbnail.
fn thumbnail_rect(width: i32, height: i32) -> Option<RECT> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let available_w = PREVIEW_W - 8;
    let available_h = PREVIEW_H - 8;
    let scale = (available_w as f64 / width as f64).min(available_h as f64 / height as f64);
    let w = (width as f64 * scale).round().max(1.0) as i32;
    let h = (height as f64 * scale).round().max(1.0) as i32;
    let left = (PREVIEW_W - w) / 2;
    let top = (PREVIEW_H - h) / 2;
    Some(RECT {
        left,
        top,
        right: left + w,
        bottom: top + h,
    })
}

fn register_thumbnail(destination: HWND, target: isize) -> Option<isize> {
    // DWM reads its compositor surface rather than requesting rendering from
    // the source application, including when that application's UI is wedged.
    unsafe {
        let registration = DwmRegisterThumbnail(destination, hwnd_from(target)).ok()?;
        let configured = (|| {
            let size = DwmQueryThumbnailSourceSize(registration).ok()?;
            let rect = thumbnail_rect(size.cx, size.cy)?;
            DwmUpdateThumbnailProperties(
                registration,
                &DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION
                        | DWM_TNP_VISIBLE
                        | DWM_TNP_OPACITY
                        | DWM_TNP_SOURCECLIENTAREAONLY,
                    rcDestination: rect,
                    opacity: 255,
                    fVisible: true.into(),
                    fSourceClientAreaOnly: false.into(),
                    ..Default::default()
                },
            )
            .ok()?;
            Some(registration)
        })();
        if configured.is_none() {
            let _ = DwmUnregisterThumbnail(registration);
        }
        configured
    }
}

/// Creates the (hidden) preview popup window class + window.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if !enabled() {
        tracing::info!("hover preview disabled (set MINICON_HOVER_PREVIEW=1 to enable)");
        return Ok(());
    }
    prepare(hinstance)
}

pub(crate) fn prepare(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if preview().is_some() {
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
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )
            .unwrap_or_default(),
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            let code = GetLastError().0;
            if code != crate::win_util::ERROR_CLASS_ALREADY_EXISTS {
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

/// Release before redirect/hide/destroy, including every failed registration.
fn release_thumbnail() {
    let raw = THUMBNAIL.swap(0, Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = DwmUnregisterThumbnail(raw);
        }
    }
}

pub(crate) fn clear_invalid_target() {
    let raw = TARGET.load(Ordering::SeqCst);
    if raw != 0
        && !unsafe {
            windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(raw as *mut _))).as_bool()
        }
    {
        hide();
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
/// (a screen rectangle). Registers only when the target changes.
pub fn show(target: isize, anchor: RECT) {
    let Some(hwnd) = preview() else {
        tracing::debug!("hover preview: show ignored (not installed)");
        return;
    };

    // A failed source gets a placeholder until the next hover. Never fall back
    // to PrintWindow or create another worker to wait on a stalled application.
    if TARGET.load(Ordering::SeqCst) != target {
        release_thumbnail();
        if let Some(registration) = register_thumbnail(hwnd, target) {
            THUMBNAIL.store(registration, Ordering::SeqCst);
        }
        TARGET.store(target, Ordering::SeqCst);
    }

    let work = work_area(anchor);
    let rect = crate::win_util::anchored_card(anchor, work, PREVIEW_W, PREVIEW_H, PREVIEW_GAP);

    let mut slot = lock_or_recover(&ANCHOR);
    tracing::debug!(previous = ?*slot, current = ?anchor, "hover preview: anchor moved");
    *slot = Some(anchor);

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

/// Hides the preview and unregisters the compositor relationship immediately.
pub fn hide() {
    release_thumbnail();
    TARGET.store(0, Ordering::SeqCst);
    *lock_or_recover(&ANCHOR) = None;
    let Some(hwnd) = preview() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    tracing::debug!("hover preview hidden");
}

/// Paint the backing and fallback; DWM draws available thumbnails itself.
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
        if let Some(background) = crate::win_util::Brush::solid(bg_color()) {
            FillRect(hdc, &client, background.handle());
        }

        if THUMBNAIL.load(Ordering::SeqCst) == 0 {
            let mut text = wide(crate::i18n::text("预览暂不可用"));
            let mut rect = client;
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(crate::config::theme().muted));
            DrawTextW(
                hdc,
                &mut text[..],
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Window procedure for the preview popup.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the guard only runs the body and degrades to the default handler
    // on a panic; all handles come straight from Win32.
    guard_abi(
        "hover_preview::wndproc",
        || unsafe { wndproc_inner(hwnd, msg, wparam, lparam) },
        || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    )
}

/// Un-guarded window procedure body; only reached through [`wndproc`].
unsafe extern "system" fn wndproc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // The preview never owns the message loop, so no PostQuitMessage.
            release_thumbnail();
            TARGET.store(0, Ordering::SeqCst);
            PREVIEW.store(0, Ordering::SeqCst);
            let mut slot = lock_or_recover(&ANCHOR);
            *slot = None;
            tracing::info!("hover preview window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows DWM desktop; run resources layer"]
    fn compositor_preview_survives_stalled_source_and_releases_registrations() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::*;
        static STALLED: AtomicBool = AtomicBool::new(false);
        static RELEASE: AtomicBool = AtomicBool::new(false);
        static PRINTED: AtomicBool = AtomicBool::new(false);
        unsafe extern "system" fn source_proc(
            hwnd: HWND,
            msg: u32,
            w: WPARAM,
            l: LPARAM,
        ) -> LRESULT {
            if msg == WM_APP + 1 {
                STALLED.store(true, Ordering::SeqCst);
                while !RELEASE.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                return LRESULT(0);
            }
            if msg == WM_PRINT || msg == WM_PRINTCLIENT {
                PRINTED.store(true, Ordering::SeqCst);
            }
            if msg == WM_DESTROY {
                unsafe {
                    PostQuitMessage(0);
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        struct Fixture {
            source: HWND,
            destination: HWND,
            worker: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                RELEASE.store(true, Ordering::SeqCst);
                unsafe {
                    let _ = PostMessageW(Some(self.source), WM_CLOSE, WPARAM(0), LPARAM(0));
                    if !self.destination.is_invalid() {
                        let _ = DestroyWindow(self.destination);
                    }
                }
                if let Some(worker) = self.worker.take() {
                    worker.join().unwrap();
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || unsafe {
            let class = wide("winmgr_preview_stalled_fixture");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(source_proc),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let source = CreateWindowExW(
                Default::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                640,
                480,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            tx.send(source.0 as isize).unwrap();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                DispatchMessageW(&msg);
            }
            let _ = UnregisterClassW(PCWSTR(class.as_ptr()), None);
        });
        let source = hwnd_from(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        let mut fixture = Fixture {
            source,
            destination: HWND::default(),
            worker: Some(worker),
        };
        unsafe {
            fixture.destination = CreateWindowExW(
                Default::default(),
                windows::core::w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                PREVIEW_W,
                PREVIEW_H,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            PostMessageW(Some(source), WM_APP + 1, WPARAM(0), LPARAM(0)).unwrap();
        }
        let started = Instant::now();
        while !STALLED.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(STALLED.load(Ordering::SeqCst), "fixture did not stall");
        let started = Instant::now();
        for _ in 0..100 {
            let registration = register_thumbnail(fixture.destination, source.0 as isize)
                .expect("DWM thumbnail must work without source message processing");
            unsafe {
                DwmUnregisterThumbnail(registration).unwrap();
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "preview waited for stalled source"
        );
        assert!(!PRINTED.load(Ordering::SeqCst));
        assert!(register_thumbnail(fixture.destination, 0).is_none());
    }

    #[test]
    fn aspect_fit_handles_wide_tall_and_invalid_sources() {
        for (w, h) in [(1920, 1080), (400, 2000), (1, i32::MAX), (i32::MAX, 1)] {
            let rect = thumbnail_rect(w, h).unwrap();
            assert!(rect.left >= 4 && rect.top >= 4);
            assert!(rect.right <= PREVIEW_W - 4 && rect.bottom <= PREVIEW_H - 4);
            assert!(rect.right > rect.left && rect.bottom > rect.top);
        }
        assert!(thumbnail_rect(0, 100).is_none());
        assert!(thumbnail_rect(100, -1).is_none());
    }
}
