//! Taskbar host windows — the shell's UI surface.
//!
//! Design language: `docs/ui-ux.md`. A minimal, text-first, 36px "efficiency
//! bar": a brand dot, task buttons (hover + active states, click to focus),
//! and a read-only right zone holding just the clock.
//!
//! Multi-monitor: one bar is created per monitor, docked to that monitor's work
//! area. The task list is shared; layout, hover, and keyboard cursor are kept
//! per bar (keyed by HWND).
//!
//! Opt-out: shown by default, disabled with `MINICON_NO_TASKBAR=1` or
//! `MINICON_TASKBAR=0`. The default run never touches the Shell registry.
//!
//! GDI/HWND handles are stored as `isize` in process-lifetime statics; the
//! message thread is the only user, so this is safe by construction.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DrawTextW, EndPaint, FillRect, FrameRect, HBRUSH, HDC, HGDIOBJ, InvalidateRect,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, RegisterHotKey, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    UnregisterHotKey, VK_ESCAPE, VK_LEFT, VK_OEM_3, VK_RETURN, VK_RIGHT,
};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABN_POSCHANGED, APPBARDATA,
    SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetClientRect, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect,
    HWND_TOPMOST, IsWindow, KillTimer, MF_STRING, PostMessageW, RegisterClassW, SM_CXSCREEN,
    SM_CYSCREEN, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE, SetForegroundWindow,
    SetTimer, SetWindowPos, ShowWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_CLOSE,
    WM_DESTROY, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEMOVE,
    WM_PAINT, WM_RBUTTONDOWN, WM_SETFOCUS, WM_TIMER, WNDCLASS_STYLES, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWORKAREA, SPI_SETWORKAREA, SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

// --- layout tokens --------------------------------------------------------
const BAR_H: i32 = 36;
const PAD: i32 = 10;
const GAP: i32 = 4;
const GRIP: i32 = 12;
/// Width of the settings button (left of the menu button).
const SETTINGS_W: i32 = 26;
/// Horizontal space reserved on the right edge for the clock. Task buttons may
/// never intrude into it. The notification-icon strip is reserved on top of
/// this (see `layout`), so it never overlaps the clock.
const RIGHT_W: i32 = CLOCK_W + PAD;
const BTN_MIN: i32 = 120;
const BTN_MAX: i32 = 200;
const BTN_TOP: i32 = 6;
const CLOCK_W: i32 = 44;
/// Width of the window-list button that sits between the pins and the tasks.
const LIST_W: i32 = 26;

// --- palette defaults live in `config::Theme`; loaded once at startup --------
fn theme() -> crate::config::Theme {
    static T: OnceLock<crate::config::Theme> = OnceLock::new();
    *T.get_or_init(|| crate::config::load().theme)
}

const COLORREF_BLACK: COLORREF = COLORREF(0);

// WM_MOUSELEAVE lives in Win32::UI::Controls, which we don't pull in.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Global hotkey id for summoning the bar into keyboard-navigation mode.
const HOTKEY_ID: i32 = 1;

/// AppBar callback message (WM_APP + 1) for docked bars.
const APPBAR_CALLBACK: u32 = 0x8000 + 1;

/// One-shot timer that gates the hover window preview (debounce).
const HOVER_TIMER_ID: usize = 2;
/// Delay before a hover shows the preview, in milliseconds.
const HOVER_DELAY_MS: u32 = 350;

/// Whether the acrylic backdrop is active (paint glass-black instead of bg).
static ACRYLIC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Original work area captured before we shrink it ourselves (used when AppBar
/// docking is unavailable, i.e. no explorer AppBar broker). Restored on teardown.
static ORIG_WORKAREA: Mutex<Option<RECT>> = Mutex::new(None);

/// Reserves the bottom strip by shrinking the work area so maximized windows
/// stop above our bar when AppBar docking failed. Captures the original area
/// once so it can be restored faithfully.
fn reserve_workarea(area: (i32, i32, i32, i32)) {
    let (x, y, w, h) = area;
    let mut guard = ORIG_WORKAREA.lock().unwrap();
    // SAFETY: `SystemParametersInfoW` reads/writes the RECT we point at, which
    // outlives the call; these are process-global work-area queries.
    unsafe {
        if guard.is_none() {
            let mut current = RECT::default();
            let ok = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut current as *mut RECT as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_ok();
            if !ok {
                tracing::warn!("SPI_GETWORKAREA failed; not reserving work area");
                return;
            }
            *guard = Some(current);
        }
        let mut target = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h - BAR_H,
        };
        let _ = SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut target as *mut RECT as *mut core::ffi::c_void),
            SPIF_SENDCHANGE,
        );
    }
    tracing::info!(bottom = y + h - BAR_H, "work area reserved above the bar");
}

/// Restores the work area captured by [`reserve_workarea`], if any.
fn restore_workarea() {
    let mut guard = ORIG_WORKAREA.lock().unwrap();
    if let Some(orig) = guard.take() {
        let mut r = orig;
        // SAFETY: `r` outlives the call.
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                Some(&mut r as *mut RECT as *mut core::ffi::c_void),
                SPIF_SENDCHANGE,
            );
        }
        tracing::info!("work area restored on teardown");
    }
}

fn hbrush(v: isize) -> HBRUSH {
    HBRUSH(v as *mut core::ffi::c_void)
}

fn hwnd_from(v: isize) -> HWND {
    HWND(v as *mut core::ffi::c_void)
}

/// One taskbar button: an application group collapsed to a single entry.
struct TaskItem {
    /// Group identity (owning pid), for logging/stability.
    key: String,
    /// Window to focus when the button is activated.
    hwnd: isize,
    /// All windows in the group (used for the "active" highlight).
    members: Vec<isize>,
    /// Button label (title, with `(+N)` when the group has more windows).
    label: String,
}

/// Per-bar state: layout, mouse hover, and keyboard cursor.
#[derive(Default)]
struct BarEntry {
    hwnd: isize,
    /// Monitor work area this bar is docked to.
    area: (i32, i32, i32, i32),
    rects: Vec<RECT>,
    /// Quick-launch pin rects (left of the list button).
    pin_rects: Vec<RECT>,
    /// Window-list button rect.
    list_rect: Option<RECT>,
    hover: Option<usize>,
    selected: Option<usize>,
}

fn bars() -> &'static Mutex<Vec<BarEntry>> {
    static B: OnceLock<Mutex<Vec<BarEntry>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(Vec::new()))
}

fn tasks() -> &'static Mutex<Vec<TaskItem>> {
    static T: OnceLock<Mutex<Vec<TaskItem>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

/// Mirrored notification-area icons, refreshed alongside the task list and
/// rendered by `crate::tray_bar` in the right zone.
fn tray_icons() -> &'static Mutex<Vec<crate::tray::TrayIcon>> {
    static T: OnceLock<Mutex<Vec<crate::tray::TrayIcon>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

/// Quick-launch pins (most-used shortcuts), refreshed alongside the task list.
fn pins() -> &'static Mutex<Vec<crate::quick_launch::Pin>> {
    static P: OnceLock<Mutex<Vec<crate::quick_launch::Pin>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Vec::new()))
}

struct Brushes {
    bg: isize,
    glass: isize,
    surface: isize,
    hover: isize,
    accent: isize,
}

fn brushes() -> &'static Brushes {
    static B: OnceLock<Brushes> = OnceLock::new();
    B.get_or_init(|| {
        // SAFETY: solid brushes are process-lifetime GDI objects, never freed.
        unsafe {
            Brushes {
                bg: CreateSolidBrush(COLORREF(theme().bg)).0 as isize,
                // Pure black doubles as the "glass" fill when the frame is
                // extended into the client area (DWM backdrop shows through).
                glass: CreateSolidBrush(COLORREF_BLACK).0 as isize,
                surface: CreateSolidBrush(COLORREF(theme().surface)).0 as isize,
                hover: CreateSolidBrush(COLORREF(theme().hover)).0 as isize,
                accent: CreateSolidBrush(COLORREF(theme().accent)).0 as isize,
            }
        }
    })
}

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

/// Whether the taskbar host should be shown. On by default; disable with
/// `MINICON_NO_TASKBAR=1` or `MINICON_TASKBAR=0`.
pub fn enabled() -> bool {
    if std::env::var_os("MINICON_NO_TASKBAR").is_some() {
        return false;
    }
    match std::env::var("MINICON_TASKBAR") {
        Ok(v) => !matches!(
            v.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => crate::config::load().show_taskbar,
    }
}

/// Whether task buttons are grouped per process. **Off by default** (one button
/// per window, matching Alt+Tab); opt in with `MINICON_GROUP_TASKS=1`.
fn grouping_enabled() -> bool {
    std::env::var("MINICON_GROUP_TASKS")
        .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// Work areas to dock a bar to: one per monitor (primary first), or the full
/// screen when monitor enumeration is unavailable.
fn bar_areas() -> Vec<(i32, i32, i32, i32)> {
    let monitors = crate::monitor::enumerate();
    if !monitors.is_empty() {
        // Use the full monitor bounds (not the work area) so the bar docks flush
        // to the physical bottom edge; AppBar `ABM_QUERYPOS` trims it away from
        // any other AppBar.
        return monitors.into_iter().map(|m| m.bounds).collect();
    }
    // SAFETY: pure metric queries.
    unsafe {
        vec![(
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )]
    }
}

/// Creates one taskbar window per monitor.
pub fn create(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: standard Win32 window creation; buffers outlive their calls.
    unsafe {
        let class_name = wide("minicon_winmgr_taskbar");
        let class_title = wide("minicon taskbar");
        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            // No class brush: we fill the whole client in WM_PAINT and handle
            // WM_ERASEBKGND ourselves, so the acrylic "glass" fill isn't erased
            // by an opaque background.
            hbrBackground: HBRUSH::default(),
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            return Err(format!(
                "RegisterClassW(taskbar) failed (Win32 error {})",
                GetLastError().0
            )
            .into());
        }

        let areas = bar_areas();
        let acrylic_wanted = acrylic_requested();
        let mut acrylic_ok = acrylic_wanted;

        for (i, (x, y, w, h)) in areas.into_iter().enumerate() {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(class_title.as_ptr()),
                WS_POPUP | WS_VISIBLE,
                x,
                y + h - BAR_H,
                w,
                BAR_H,
                None,
                None,
                Some(hinstance),
                None,
            )?;

            bars().lock().unwrap().push(BarEntry {
                hwnd: hwnd.0 as isize,
                area: (x, y, w, h),
                ..Default::default()
            });
            tracing::info!(hwnd = ?hwnd, index = i, "taskbar window created");

            // Dock as an AppBar so maximized windows stop above the bar.
            if register_appbar(hwnd, (x, y, w, h)) {
                tracing::info!(hwnd = ?hwnd, "taskbar docked as AppBar");
            } else {
                tracing::warn!(hwnd = ?hwnd, "AppBar registration failed; reserving work area instead");
                // No AppBar broker (e.g. no explorer): shrink the work area
                // ourselves so maximized windows still stop above the bar.
                if i == 0 {
                    reserve_workarea((x, y, w, h));
                }
            }

            // Rounded corners are safe everywhere; acrylic is opt-in.
            crate::dwm::apply_rounded_corners(hwnd);
            if acrylic_wanted {
                acrylic_ok &= crate::dwm::enable_acrylic(hwnd);
            }

            // The primary (first) bar owns the keyboard-navigation hotkey.
            if i == 0 {
                match RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_ALT, VK_OEM_3.0 as u32) {
                    Ok(()) => tracing::info!("keyboard nav hotkey registered (Alt+`)"),
                    Err(err) => {
                        tracing::warn!(%err, "hotkey registration failed (Alt+` already taken?)")
                    }
                }
            }
        }

        ACRYLIC_ACTIVE.store(acrylic_ok, Ordering::SeqCst);
        if acrylic_wanted && !acrylic_ok {
            tracing::warn!("MINICON_ACRYLIC requested but unavailable; using opaque bar");
        }
    }
    refresh();
    Ok(())
}

/// Registers `hwnd` as an AppBar docked to the bottom edge, then positions it
/// within `area` (a monitor work area). Returns false if registration failed.
fn register_appbar(hwnd: HWND, area: (i32, i32, i32, i32)) -> bool {
    // SAFETY: `abd` is a properly sized APPBARDATA with a live window handle.
    unsafe {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hwnd,
            uCallbackMessage: APPBAR_CALLBACK,
            ..Default::default()
        };
        if SHAppBarMessage(ABM_NEW, &mut abd) == 0 {
            return false;
        }
    }
    set_appbar_pos(hwnd, area);
    true
}

/// Repositions a registered AppBar to the bottom of `area`, reserving BAR_H.
fn set_appbar_pos(hwnd: HWND, area: (i32, i32, i32, i32)) {
    let (x, y, w, h) = area;
    // SAFETY: `abd` is a properly sized APPBARDATA for a registered AppBar.
    unsafe {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hwnd,
            uCallbackMessage: APPBAR_CALLBACK,
            uEdge: ABE_BOTTOM,
            rc: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            ..Default::default()
        };
        let _ = SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.top = abd.rc.bottom - BAR_H;
        let _ = SHAppBarMessage(ABM_SETPOS, &mut abd);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            abd.rc.left,
            abd.rc.top,
            abd.rc.right - abd.rc.left,
            abd.rc.bottom - abd.rc.top,
            SWP_NOACTIVATE,
        );
    }
}

/// Whether the user asked for the acrylic backdrop.
fn acrylic_requested() -> bool {
    std::env::var("MINICON_ACRYLIC").is_ok_and(|v| {
        !matches!(
            v.to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// Re-enumerates task windows, recomputes every bar's layout, and repaints.
pub fn refresh() {
    let windows = crate::window_snapshot::taskbar_windows();
    // Default: one button per window (matches Alt+Tab). Per-pid grouping is
    // opt-in via `MINICON_GROUP_TASKS=1`.
    let tasks_now: Vec<TaskItem> = if grouping_enabled() {
        crate::grouping::group_by_pid(windows)
            .into_iter()
            .map(|group| {
                let first = group.windows.first();
                let base = first.map(|w| w.title.clone()).unwrap_or_default();
                let extra = group.windows.len().saturating_sub(1);
                let label = if extra > 0 {
                    format!("{base} (+{extra})")
                } else {
                    base
                };
                TaskItem {
                    key: group.key,
                    hwnd: first.map(|w| w.hwnd.0 as isize).unwrap_or(0),
                    members: group.windows.iter().map(|w| w.hwnd.0 as isize).collect(),
                    label,
                }
            })
            .collect()
    } else {
        windows
            .into_iter()
            .map(|w| TaskItem {
                key: (w.hwnd.0 as isize).to_string(),
                hwnd: w.hwnd.0 as isize,
                members: vec![w.hwnd.0 as isize],
                label: w.title,
            })
            .collect()
    };

    let n = tasks_now.len();
    *tasks().lock().unwrap() = tasks_now;

    // Refresh the mirrored notification icons and their reserved strip width.
    let icons = crate::tray::enumerate();
    let tray_w =
        crate::tray_bar::strip_width(icons.len(), crate::tray_bar::CELL, crate::tray_bar::GAP);
    *tray_icons().lock().unwrap() = icons;

    // Refresh quick-launch pins; task buttons start after them + the list button.
    let pins_now = crate::quick_launch::pins();
    let pins_w = if pins_now.is_empty() {
        0
    } else {
        pins_now.len() as i32 * crate::quick_launch::CELL
            + (pins_now.len() as i32 - 1) * crate::quick_launch::GAP
    };
    *pins().lock().unwrap() = pins_now;

    let after_pins = if pins_w > 0 {
        PAD + SETTINGS_W + GRIP + pins_w + GAP
    } else {
        PAD + SETTINGS_W + GRIP
    };
    let tasks_left = after_pins + LIST_W + GAP;

    let mut bars = bars().lock().unwrap();
    if bars.is_empty() {
        return;
    }
    for bar in bars.iter_mut() {
        let hwnd = hwnd_from(bar.hwnd);
        let mut client = RECT::default();
        // SAFETY: `hwnd` is a live bar window owned by this process.
        unsafe {
            let _ = GetClientRect(hwnd, &mut client);
        }
        let pin_count = pins().lock().unwrap().len();
        bar.pin_rects = crate::quick_launch::layout(
            PAD + SETTINGS_W + GRIP,
            pin_count,
            crate::quick_launch::CELL,
            crate::quick_launch::GAP,
            BAR_H,
        );
        bar.list_rect = Some(RECT {
            left: after_pins,
            top: BTN_TOP,
            right: after_pins + LIST_W,
            bottom: BAR_H - BTN_TOP,
        });
        bar.rects = layout(tasks_left, client.right, n, tray_w);
        if bar.hover.is_some_and(|h| h >= n) {
            bar.hover = None;
        }
        if bar.selected.is_some_and(|s| s >= n) {
            bar.selected = n.checked_sub(1);
        }
        // SAFETY: same as above.
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
    }
    tracing::debug!(
        count = n,
        pins = pins().lock().unwrap().len(),
        bars = bars.len(),
        groups = ?tasks().lock().unwrap().iter().map(|t| format!("{}#{}", t.key, t.label)).collect::<Vec<_>>(),
        "taskbar refreshed"
    );
}

/// Lays out task buttons starting at `start_left` across the available width,
/// reserving `tray_w` pixels on the right for the notification-icon strip
/// (plus its trailing margin).
fn layout(start_left: i32, client_w: i32, n: usize, tray_w: i32) -> Vec<RECT> {
    if n == 0 {
        return Vec::new();
    }
    let left = start_left;
    let tray = if tray_w > 0 { tray_w + GAP } else { 0 };
    let avail = (client_w - left - PAD - RIGHT_W - tray).max(0);
    let total_gap = GAP * (n as i32 - 1);
    let bw = ((avail - total_gap) / n as i32).clamp(BTN_MIN, BTN_MAX);
    (0..n)
        .map(|i| {
            let x = left + i as i32 * (bw + GAP);
            RECT {
                left: x,
                top: BTN_TOP,
                right: x + bw,
                bottom: BAR_H - BTN_TOP,
            }
        })
        .collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let key = hwnd.0 as isize;
    match msg {
        WM_PAINT => {
            unsafe { paint(hwnd) };
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            track_leave(hwnd);
            let x = get_x(lparam);
            let y = get_y(lparam);
            let (changed, hover) = {
                let mut bars = bars().lock().unwrap();
                match bars.iter_mut().find(|b| b.hwnd == key) {
                    Some(bar) => {
                        let hover = bar
                            .rects
                            .iter()
                            .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
                        let changed = hover != bar.hover;
                        bar.hover = hover;
                        (changed, hover)
                    }
                    None => (false, None),
                }
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                // Re-arm the debounced preview timer for the new hover target.
                let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
                crate::hover_preview::hide();
                if hover.is_some() && crate::hover_preview::enabled() {
                    let _ = unsafe { SetTimer(Some(hwnd), HOVER_TIMER_ID, HOVER_DELAY_MS, None) };
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == HOVER_TIMER_ID => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            let hover = bars()
                .lock()
                .unwrap()
                .iter()
                .find(|b| b.hwnd == key)
                .and_then(|b| b.hover);
            if let Some(i) = hover {
                let btn = bars()
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|b| b.hwnd == key)
                    .and_then(|b| b.rects.get(i).copied());
                let target = tasks().lock().unwrap().get(i).map(|t| t.hwnd);
                if let (Some(r), Some(target)) = (btn, target) {
                    let mut wr = RECT::default();
                    // SAFETY: `hwnd` is our live bar; converting client→screen.
                    unsafe {
                        let _ = GetWindowRect(hwnd, &mut wr);
                    }
                    let anchor = RECT {
                        left: r.left + wr.left,
                        top: r.top + wr.top,
                        right: r.right + wr.left,
                        bottom: r.bottom + wr.top,
                    };
                    crate::hover_preview::show(target, anchor);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            let changed = {
                let mut bars = bars().lock().unwrap();
                bars.iter_mut()
                    .find(|b| b.hwnd == key)
                    .is_some_and(|b| b.hover.take().is_some())
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            let x = get_x(lparam);
            let y = get_y(lparam);
            // Settings button (leftmost) opens the settings window.
            if x < PAD + SETTINGS_W {
                crate::settings_ui::toggle();
                return LRESULT(0);
            }
            // Menu dot opens the start menu.
            if x < PAD + SETTINGS_W + GRIP {
                crate::start_menu::toggle();
                return LRESULT(0);
            }
            // Snapshot this bar's interactive geometry.
            let (pin_rects, list_rect, hover) = {
                let bars = bars().lock().unwrap();
                match bars.iter().find(|b| b.hwnd == key) {
                    Some(b) => (b.pin_rects.clone(), b.list_rect, b.hover),
                    None => (Vec::new(), None, None),
                }
            };
            // Window-list button: popup over every task group.
            if let Some(r) = list_rect
                && x >= r.left
                && x < r.right
                && y >= r.top
                && y < r.bottom
            {
                let entries: Vec<crate::task_list::Entry> = tasks()
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|t| crate::task_list::Entry {
                        label: t.label.clone(),
                        hwnd: t.hwnd,
                    })
                    .collect();
                if let Some(target) = crate::task_list::popup(hwnd, &entries) {
                    activate(hwnd_from(target));
                }
                return LRESULT(0);
            }
            // Quick-launch pins.
            if let Some(i) = crate::quick_launch::hit(&pin_rects, x, y) {
                let path = pins().lock().unwrap().get(i).map(|p| p.path.clone());
                if let Some(path) = path {
                    crate::quick_launch::launch_path(&path);
                }
                return LRESULT(0);
            }
            let target = hover.and_then(|i| tasks().lock().unwrap().get(i).map(|t| t.hwnd));
            if let Some(target) = target {
                activate(hwnd_from(target));
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            let target = {
                let bars = bars().lock().unwrap();
                let hover = bars.iter().find(|b| b.hwnd == key).and_then(|b| b.hover);
                drop(bars);
                hover.and_then(|i| tasks().lock().unwrap().get(i).map(|t| t.hwnd))
            };
            if let Some(target) = target {
                window_menu(hwnd, hwnd_from(target));
            }
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            // Summon: focus the primary bar and start the keyboard cursor at 0.
            let _ = unsafe { SetForegroundWindow(hwnd) };
            let _ = unsafe { SetFocus(Some(hwnd)) };
            let changed = {
                let has_tasks = !tasks().lock().unwrap().is_empty();
                let mut bars = bars().lock().unwrap();
                match bars.iter_mut().find(|b| b.hwnd == key) {
                    Some(bar) => {
                        bar.selected = has_tasks.then_some(0);
                        bar.selected.is_some()
                    }
                    None => false,
                }
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            let n = tasks().lock().unwrap().len();
            let mut changed = false;
            if n > 0 {
                let mut bars = bars().lock().unwrap();
                if let Some(bar) = bars.iter_mut().find(|b| b.hwnd == key) {
                    match vk {
                        k if k == VK_LEFT.0 || k == VK_RIGHT.0 => {
                            bar.selected = step_index(bar.selected, n, k == VK_RIGHT.0);
                            changed = true;
                        }
                        k if k == VK_RETURN.0 => {
                            let target = bar
                                .selected
                                .and_then(|i| tasks().lock().unwrap().get(i).map(|t| t.hwnd));
                            bar.selected = None;
                            if let Some(target) = target {
                                drop(bars);
                                activate(hwnd_from(target));
                                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                                return LRESULT(0);
                            }
                        }
                        k if k == VK_ESCAPE.0 => {
                            changed = bar.selected.take().is_some();
                        }
                        _ => {}
                    }
                }
            }
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            let changed = {
                let has_tasks = !tasks().lock().unwrap().is_empty();
                let mut bars = bars().lock().unwrap();
                match bars.iter_mut().find(|b| b.hwnd == key) {
                    Some(bar) if bar.selected.is_none() && has_tasks => {
                        bar.selected = Some(0);
                        true
                    }
                    _ => false,
                }
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            let changed = {
                let mut bars = bars().lock().unwrap();
                bars.iter_mut()
                    .find(|b| b.hwnd == key)
                    .is_some_and(|b| b.selected.take().is_some())
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // fully repainted in WM_PAINT
        m if m == APPBAR_CALLBACK => {
            // Another AppBar moved/resized: re-dock to the stored work area.
            if wparam.0 as u32 == ABN_POSCHANGED {
                let area = bars()
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|b| b.hwnd == key)
                    .map(|b| b.area);
                if let Some(area) = area {
                    set_appbar_pos(hwnd, area);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = unsafe { UnregisterHotKey(Some(hwnd), HOTKEY_ID) };
            // SAFETY: deregisters this AppBar; harmless if it was never docked.
            unsafe {
                let mut abd = APPBARDATA {
                    cbSize: std::mem::size_of::<APPBARDATA>() as u32,
                    hWnd: hwnd,
                    ..Default::default()
                };
                let _ = SHAppBarMessage(ABM_REMOVE, &mut abd);
            }
            let mut bars = bars().lock().unwrap();
            bars.retain(|b| b.hwnd != key);
            let remaining = bars.len();
            drop(bars);
            if remaining == 0 {
                restore_workarea();
            }
            tracing::info!(remaining, "taskbar window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Paints one bar: background, grip dot, task buttons, right zone.
unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let br = brushes();

    unsafe {
        let bg = if ACRYLIC_ACTIVE.load(Ordering::SeqCst) {
            br.glass
        } else {
            br.bg
        };
        FillRect(hdc, &client, hbrush(bg));

        // Settings button (left of the menu dot): a three-slider glyph.
        let set_rect = RECT {
            left: PAD,
            top: BTN_TOP,
            right: PAD + SETTINGS_W,
            bottom: BAR_H - BTN_TOP,
        };
        FillRect(hdc, &set_rect, hbrush(br.surface));
        let scx = (set_rect.left + set_rect.right) / 2;
        for (k, off) in [4, -3, 1].into_iter().enumerate() {
            let y = BAR_H / 2 - 5 + k as i32 * 5;
            let line = RECT {
                left: scx - 6,
                top: y,
                right: scx + 6,
                bottom: y + 1,
            };
            FillRect(hdc, &line, hbrush(br.accent));
            let knob = RECT {
                left: scx + off - 1,
                top: y - 2,
                right: scx + off + 2,
                bottom: y + 3,
            };
            FillRect(hdc, &knob, hbrush(br.accent));
        }

        // Brand grip: a small accent dot (the menu button), right of settings.
        let dot = RECT {
            left: PAD + SETTINGS_W + 3,
            top: BAR_H / 2 - 3,
            right: PAD + SETTINGS_W + 9,
            bottom: BAR_H / 2 + 3,
        };
        FillRect(hdc, &dot, hbrush(br.accent));

        let old_font = SelectObject(hdc, HGDIOBJ(font() as *mut core::ffi::c_void));
        SetBkMode(hdc, TRANSPARENT);

        let fg = GetForegroundWindow().0 as isize;
        let tasks = tasks().lock().unwrap();
        let (rects, pin_rects, list_rect, hover, selected) = {
            let bars = bars().lock().unwrap();
            match bars.iter().find(|b| b.hwnd == hwnd.0 as isize) {
                Some(b) => (
                    b.rects.clone(),
                    b.pin_rects.clone(),
                    b.list_rect,
                    b.hover,
                    b.selected,
                ),
                None => (Vec::new(), Vec::new(), None, None, None),
            }
        };

        for (i, item) in tasks.iter().enumerate() {
            let Some(r) = rects.get(i).copied() else {
                continue;
            };
            let active = item.members.contains(&fg);
            let is_selected = selected == Some(i);
            let brush = if hover == Some(i) {
                br.hover
            } else {
                br.surface
            };
            FillRect(hdc, &r, hbrush(brush));

            if active {
                let marker = RECT {
                    left: r.left,
                    top: r.top,
                    right: r.left + 3,
                    bottom: r.bottom,
                };
                FillRect(hdc, &marker, hbrush(br.accent));
            }

            // Keyboard cursor: a 1px accent outline around the selected button.
            if is_selected {
                FrameRect(hdc, &r, hbrush(br.accent));
            }

            SetTextColor(
                hdc,
                if active || is_selected {
                    COLORREF(theme().text)
                } else {
                    COLORREF(theme().muted)
                },
            );
            let mut text: Vec<u16> = item.label.encode_utf16().collect();
            let mut tr = r;
            tr.left += 9;
            tr.right -= 8;
            DrawTextW(
                hdc,
                &mut text,
                &mut tr,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }

        // Quick-launch pins: surface squares with the label's first letter.
        let pin_labels: Vec<String> = pins()
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.label.clone())
            .collect();
        for (i, r) in pin_rects.iter().enumerate() {
            FillRect(hdc, r, hbrush(br.surface));
            if let Some(label) = pin_labels.get(i) {
                SetTextColor(hdc, COLORREF(theme().text));
                let head: String = label.chars().take(1).collect();
                let mut text: Vec<u16> = head.encode_utf16().collect();
                let mut tr = *r;
                DrawTextW(
                    hdc,
                    &mut text,
                    &mut tr,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
            }
        }

        // Window-list button: a three-bar "menu" glyph.
        if let Some(r) = list_rect {
            FillRect(hdc, &r, hbrush(br.surface));
            let cx = (r.left + r.right) / 2;
            for k in -1..=1 {
                let bar = RECT {
                    left: cx - 5,
                    top: BAR_H / 2 + k * 4 - 1,
                    right: cx + 5,
                    bottom: BAR_H / 2 + k * 4 + 1,
                };
                FillRect(hdc, &bar, hbrush(br.accent));
            }
        }

        // Notification-icon strip, sitting just left of the clock.
        let icons = tray_icons().lock().unwrap();
        if !icons.is_empty() {
            let tray_right = client.right - PAD - CLOCK_W - PAD;
            crate::tray_bar::paint(hdc, BTN_TOP, tray_right, BAR_H - 2 * BTN_TOP, &icons);
        }
        drop(icons);

        // Right zone: the clock, right-aligned and muted.
        SetTextColor(hdc, COLORREF(theme().muted));
        let now = GetLocalTime();
        let clock = format!("{:02}:{:02}", now.wHour, now.wMinute);
        draw_right(
            hdc,
            &clock,
            client.right - PAD - CLOCK_W,
            client.right - PAD,
        );

        SelectObject(hdc, old_font);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Draws `text` right-aligned within `[left, right]`.
unsafe fn draw_right(hdc: HDC, text: &str, left: i32, right: i32) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT {
        left,
        top: 0,
        right,
        bottom: BAR_H,
    };
    unsafe {
        DrawTextW(
            hdc,
            &mut buf,
            &mut r,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
}

/// Registers for `WM_MOUSELEAVE` (idempotent per hover session).
fn track_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = unsafe { TrackMouseEvent(&mut tme) };
}

/// Taskbar right-click menu item ids.
const MENU_RESTORE: usize = 1;
const MENU_MINIMIZE: usize = 2;
const MENU_MAXIMIZE: usize = 3;
const MENU_CLOSE: usize = 4;

/// Shows the right-click window menu for `target` and applies the choice.
fn window_menu(owner: HWND, target: HWND) {
    // SAFETY: the menu is created and destroyed within this call; `owner` and
    // `target` are live windows owned by this process.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let restore = wide("还原");
        let minimize = wide("最小化");
        let maximize = wide("最大化");
        let close = wide("关闭");
        let _ = AppendMenuW(menu, MF_STRING, MENU_RESTORE, PCWSTR(restore.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, MENU_MINIMIZE, PCWSTR(minimize.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, MENU_MAXIMIZE, PCWSTR(maximize.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, PCWSTR(close.as_ptr()));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the popup dismisses correctly when clicking away.
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            owner,
            None,
        );
        let _ = DestroyMenu(menu);

        match (cmd.0 & 0xFFFF) as usize {
            MENU_RESTORE => {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            MENU_MINIMIZE => {
                let _ = ShowWindow(target, SW_MINIMIZE);
            }
            MENU_MAXIMIZE => {
                let _ = ShowWindow(target, SW_MAXIMIZE);
            }
            MENU_CLOSE => {
                let _ = PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            _ => {}
        }
    }
}

/// Brings `target` to the foreground, restoring it first if minimized.
fn activate(target: HWND) {
    // SAFETY: `target` is a live top-level window from our task list. Guard
    // against a stale handle (window closed and the value reused) so we never
    // foreground the wrong window.
    unsafe {
        if !IsWindow(Some(target)).as_bool() {
            tracing::debug!(?target, "activate: target window no longer exists");
            return;
        }
        crate::win_util::force_foreground(target);
    }
}

fn get_x(lparam: LPARAM) -> i32 {
    (lparam.0 as usize & 0xFFFF) as u16 as i16 as i32
}

fn get_y(lparam: LPARAM) -> i32 {
    ((lparam.0 as usize >> 16) & 0xFFFF) as u16 as i16 as i32
}

/// Advances the keyboard cursor by one task, wrapping around. `None` starts at
/// the first (forward) or last (backward) task; empty lists stay `None`.
fn step_index(current: Option<usize>, n: usize, forward: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    match current {
        None => Some(if forward { 0 } else { n - 1 }),
        Some(cur) => Some(if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::step_index;

    #[test]
    fn empty_list_has_no_selection() {
        assert_eq!(step_index(None, 0, true), None);
        assert_eq!(step_index(Some(0), 0, false), None);
    }

    #[test]
    fn none_selects_first_or_last() {
        assert_eq!(step_index(None, 3, true), Some(0));
        assert_eq!(step_index(None, 3, false), Some(2));
    }

    #[test]
    fn forward_wraps_around() {
        assert_eq!(step_index(Some(0), 3, true), Some(1));
        assert_eq!(step_index(Some(2), 3, true), Some(0));
    }

    #[test]
    fn backward_wraps_around() {
        assert_eq!(step_index(Some(0), 3, false), Some(2));
        assert_eq!(step_index(Some(2), 3, false), Some(1));
    }

    #[test]
    fn tray_strip_shrinks_button_area() {
        let without = super::layout(22, 420, 2, 0);
        let with = super::layout(22, 420, 2, 60);
        assert_eq!(without.len(), 2);
        assert_eq!(with.len(), 2);
        assert!(with[1].right < without[1].right);
    }
}
