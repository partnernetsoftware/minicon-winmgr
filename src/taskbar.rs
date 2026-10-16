//! Taskbar host windows — the shell's UI surface.
//!
//! Design language: `docs/prd_design_language.md`. A minimal, text-first, 36px "efficiency
//! bar": a brand dot, task buttons (hover + active states, click to focus),
//! and a right zone whose clock opens the notification panel.
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::config::theme;

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_TOP, DT_VCENTER, DrawTextW,
    EndPaint, FillRect, FrameRect, HBRUSH, HDC, HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, RegisterHotKey, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    UnregisterHotKey, VIRTUAL_KEY, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_DOWN,
    VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_OEM_3, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS,
    ABN_POSCHANGED, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DrawIconEx,
    GetClientRect, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowTextW, HICON,
    HWND_TOPMOST, IsIconic, IsWindow, KillTimer, PostMessageW, RegisterClassW, SM_CXSCREEN,
    SM_CYSCREEN, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE, SetTimer, SetWindowPos,
    ShowWindow, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETFOCUS, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETWORKAREA, SPI_SETWORKAREA, SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows::core::PCWSTR;

use crate::win_util::{guard_abi, lock_or_recover, wide};

// --- layout tokens --------------------------------------------------------
const BAR_H: i32 = 36;
const SIDE_W: i32 = 180;
fn vertical() -> bool {
    crate::config::current().position.vertical()
}
fn thickness() -> i32 {
    if vertical() { SIDE_W } else { BAR_H }
}
const PAD: i32 = 10;
/// Spacing between adjacent elements of the bar. Primarily the gap between
/// task buttons, but it is the bar's shared token: the quick-launch strip, the
/// notification strip and the `»` overflow all space themselves with it too.
/// (Named per the 0.1.3 plan's de-duplication pass — it used to be a bare `GAP`
/// in seven modules with five different values.)
const BTN_GAP: i32 = 4;
/// Width reserved for the start-menu entry slot, right of the settings button.
/// Only reserved when the feature is on — see [`grip_w`].
const GRIP: i32 = 12;
/// Width of the settings button — the leftmost control on the bar.
const SETTINGS_W: i32 = 26;
/// Horizontal space reserved on the right edge for the clock. Task buttons may
/// never intrude into it. The notification-icon strip is reserved on top of
/// this (see `layout`), so it never overlaps the clock.
const RIGHT_W: i32 = CLOCK_W + PAD;
/// Button-width tiers ("density ladder"). The bar starts at [`Density::Full`]
/// (comfortable buttons carrying a title) and drops to [`Density::Compact`]
/// (narrower, tighter padding) and finally to [`Density::IconOnly`] (the title
/// is dropped, the icon grows to fill a square) before any task is pushed
/// behind the overflow button — "shrink the padding, then the title, and only
/// then hide it".
const D0_ENTRY: i32 = 120;
const D0_FLOOR: i32 = 112;
const D0_CEIL: i32 = 200;
/// Up-threshold from `Compact`/`IconOnly` back to `Compact`.
const D1_ENTRY: i32 = 88;
const D1_FLOOR: i32 = 80;
const D1_CEIL: i32 = 119;
/// `IconOnly` square widths (the tier is entered at 40 and never narrower).
const D2_FLOOR: i32 = 40;
const D2_CEIL: i32 = 48;
/// Icon edge length inside an icon-only square (bigger than the 16px slot the
/// title-bearing tiers use, within the 20–24 range the PRD allows).
const ICON_ONLY_SIZE: i32 = 20;
const BTN_TOP: i32 = 6;
/// Width of the clock text (`yy-mm-dd HH:MM`).
const CLOCK_W: i32 = 132;
/// Width of the overflow button shown when task buttons cannot all fit.
const OVERFLOW_W: i32 = 44;
/// The identity slot at the left of every task button: the application icon's
/// size, the inset before it, and the gap between it and the title. The icon is
/// a fixed-width anchor, so the title starts at a predictable x even when two
/// applications have wildly different name lengths.
const ICON_W: i32 = 16;
const ICON_PAD: i32 = 8;
const ICON_GAP: i32 = 7;

/// Button-form tier. The first two carry the app icon and title (`Compact`
/// tightens the padding so more windows fit); `IconOnly` drops the title and
/// enlarges the icon so the *identity* still survives the squeeze.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    /// Comfortable buttons (`D0_*`); the starting tier.
    #[default]
    Full,
    /// Narrower buttons with tighter padding (`D1_*`).
    Compact,
    /// Title-less icon squares (`D2_*`), the densest tier before overflow.
    IconOnly,
}

impl Density {
    /// Width at which this tier is abandoned (hysteresis floor).
    fn floor(self) -> i32 {
        match self {
            Density::Full => D0_FLOOR,
            Density::Compact => D1_FLOOR,
            Density::IconOnly => D2_FLOOR,
        }
    }

    /// Maximum button width in this tier.
    fn ceil(self) -> i32 {
        match self {
            Density::Full => D0_CEIL,
            Density::Compact => D1_CEIL,
            Density::IconOnly => D2_CEIL,
        }
    }

    /// Icon metrics: `(icon edge, inset from the button's left edge, icon→title
    /// gap)`. Icon-only inflates the icon to fill its square and ignores the
    /// inset/gap (it is centred, with no title following it).
    fn icon_metrics(self) -> (i32, i32, i32) {
        match self {
            Density::Full => (ICON_W, ICON_PAD, ICON_GAP),
            Density::Compact => (ICON_W, 6, 5),
            Density::IconOnly => (ICON_ONLY_SIZE, 0, 0),
        }
    }

    /// Whether this tier drops the title in favour of a centred icon square.
    fn is_icon_only(self) -> bool {
        matches!(self, Density::IconOnly)
    }
}

/// Per-button width budget: the space each button gets if the `gap`s between
/// buttons are deducted from `avail`. `i32::MAX` for an empty list.
fn budget(avail: i32, n: usize, gap: i32) -> i32 {
    if n == 0 {
        return i32::MAX;
    }
    (avail - gap * (n as i32 - 1)) / n as i32
}

/// Picks the density tier for `budget`, applying hysteresis against `prev`: the
/// down-threshold is lower than the up-threshold (an 8px dead zone) so adding or
/// removing a single window cannot make the whole bar flip between tiers.
fn density_for(budget: i32, prev: Density) -> Density {
    match prev {
        // Down from Full only below the floor; up from Compact only at the entry.
        Density::Full => {
            if budget >= D0_FLOOR {
                Density::Full
            } else if budget >= D1_FLOOR {
                Density::Compact
            } else {
                Density::IconOnly
            }
        }
        Density::Compact => {
            if budget >= D0_ENTRY {
                Density::Full
            } else if budget >= D1_FLOOR {
                Density::Compact
            } else {
                Density::IconOnly
            }
        }
        // Up from IconOnly: a wide gap back to Full (skipping Compact), but
        // 88px re-enters Compact; the down-threshold stays 80 (8px dead zone).
        Density::IconOnly => {
            if budget >= D0_ENTRY {
                Density::Full
            } else if budget >= D1_ENTRY {
                Density::Compact
            } else {
                Density::IconOnly
            }
        }
    }
}

// --- palette: `crate::config::theme()`, cached process-wide ----------------

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

/// Periodic repaint so the minute-level clock stays current between `refresh`
/// runs (the heartbeat is only every 30s).
const CLOCK_TIMER_ID: usize = 3;
/// Clock repaint cadence, in milliseconds — finer than the minute it displays.
const CLOCK_TICK_MS: u32 = 5000;

/// Whether the acrylic backdrop is active (paint glass-black instead of bg).
static ACRYLIC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Original work area captured before we shrink it ourselves (used when AppBar
/// docking is unavailable, i.e. no explorer AppBar broker). Restored on teardown.
static ORIG_WORKAREA: Mutex<Option<RECT>> = Mutex::new(None);

/// Reserves the selected edge so maximized windows avoid our bar when
/// AppBar docking failed. Captures the original area
/// once so it can be restored faithfully, and persists both rectangles so a run
/// that is *killed* (task manager, `shellctl stop`) can still be recovered.
fn reserve_workarea(_area: (i32, i32, i32, i32)) {
    let mut guard = lock_or_recover(&ORIG_WORKAREA);
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
        let mut target = crate::dock::reserve(
            guard.unwrap(),
            crate::config::current().position,
            thickness(),
        );
        if let Err(err) = SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some(&mut target as *mut RECT as *mut core::ffi::c_void),
            SPIF_SENDCHANGE,
        ) {
            tracing::warn!(
                %err,
                "reserve_workarea: SPI_SETWORKAREA refused; the bar may overlap maximised windows"
            );
        }
        if let Some(orig) = *guard {
            save_workarea_state(&orig, &target);
        }
    }
    tracing::info!(position = ?crate::config::current().position, "work area reserved beside the bar");
}

/// Restores the work area captured by [`reserve_workarea`], if any.
fn restore_workarea() {
    let mut guard = lock_or_recover(&ORIG_WORKAREA);
    if let Some(orig) = guard.take() {
        let mut r = orig;
        // SAFETY: `r` outlives the call.
        unsafe {
            if let Err(err) = SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                Some(&mut r as *mut RECT as *mut core::ffi::c_void),
                SPIF_SENDCHANGE,
            ) {
                tracing::warn!(
                    %err,
                    "restore_workarea: SPI_SETWORKAREA refused; the reserved strip may stay"
                );
            }
        }
        tracing::info!("work area restored on teardown");
    }
    clear_workarea_state();
}

/// Destroys every taskbar window and restores the reserved work area.
///
/// Normal shutdown posts `WM_CLOSE` to the *message-only* window (see
/// `window`), so the bars never receive `WM_DESTROY` and `restore_workarea`
/// would not run — the work area would stay shrunk, and the next run would
/// shrink it again (the screen would creep up, one bar-height per restart).
/// Call this once the message loop has ended, on the message thread.
pub fn shutdown() {
    let handles: Vec<isize> = lock_or_recover(bars()).iter().map(|b| b.hwnd).collect();
    for raw in handles {
        // SAFETY: `DestroyWindow` runs `WM_DESTROY` synchronously here (same
        // thread that created the window), which drops the bar and, for the
        // last one, restores the work area.
        let _ = unsafe { DestroyWindow(hwnd_from(raw)) };
    }
    // Belt and braces: restore even if the windows were already gone.
    restore_workarea();
}

// --- work-area state file --------------------------------------------------
//
// A hard kill (task manager, `shellctl stop`) never runs our teardown, so the
// shrunk work area would survive into the next session. We persist the two
// rectangles and recover on the next start.

/// File recording the work area we shrank and what it was before.
const WORKAREA_STATE_FILE: &str = "minicon-workarea.state";

fn workarea_state_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(WORKAREA_STATE_FILE))
}

fn save_workarea_state(orig: &RECT, mine: &RECT) {
    let Some(path) = workarea_state_path() else {
        return;
    };
    let body = format!(
        "orig={},{},{},{}\nmine={},{},{},{}\n",
        orig.left, orig.top, orig.right, orig.bottom, mine.left, mine.top, mine.right, mine.bottom
    );
    if let Err(err) = std::fs::write(&path, body) {
        tracing::warn!(%err, "could not persist work-area state");
    }
}

fn clear_workarea_state() {
    let Some(path) = workarea_state_path() else {
        return;
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        // The goal is "the file is gone", which is already true when it was
        // never written — or was consumed by the restore just before this.
        // Warning there lit up every normal teardown, and a WARN on the happy
        // path is how real warnings start getting ignored.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%err, "clear_workarea_state: could not delete the state file"),
    }
}

fn parse_rect(s: &str) -> Option<RECT> {
    let mut it = s.split(',').map(|p| p.trim().parse::<i32>().ok());
    let (left, top, right, bottom) = (it.next()??, it.next()??, it.next()??, it.next()??);
    Some(RECT {
        left,
        top,
        right,
        bottom,
    })
}

/// Undoes a work area left shrunk by a previous run that was killed before it
/// could restore it. Only acts when the *current* work area still equals what
/// we set, so a change made by another shell is never clobbered.
pub fn recover_workarea() {
    let Some(path) = workarea_state_path() else {
        return;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut orig = None;
    let mut mine = None;
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("orig=") {
            orig = parse_rect(v);
        } else if let Some(v) = line.strip_prefix("mine=") {
            mine = parse_rect(v);
        }
    }
    let (Some(orig), Some(mine)) = (orig, mine) else {
        clear_workarea_state();
        return;
    };
    let mut current = RECT::default();
    // SAFETY: `current` outlives the call.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut current as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    let still_ours = ok
        && current.left == mine.left
        && current.top == mine.top
        && current.right == mine.right
        && current.bottom == mine.bottom;
    if still_ours {
        let mut r = orig;
        // SAFETY: `r` outlives the call.
        let _ = unsafe {
            SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                Some(&mut r as *mut RECT as *mut core::ffi::c_void),
                SPIF_SENDCHANGE,
            )
        };
        tracing::warn!(
            bottom = orig.bottom,
            "recovered work area left shrunk by a previous run"
        );
    }
    clear_workarea_state();
}

fn hbrush(dc: HDC, v: isize) -> HBRUSH {
    unsafe {
        windows::Win32::Graphics::Gdi::SetDCBrushColor(dc, COLORREF(v as u32));
        HBRUSH(
            windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::DC_BRUSH)
                .0,
        )
    }
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
    /// Application icon to draw in the button's identity slot; `0` = none.
    icon: crate::window_icon::Icon,
}

/// One rendered quick-launch pin: the stored shortcut plus its resolved icon.
struct PinItem {
    label: String,
    path: String,
    /// Icon drawn in the pin square; `0` falls back to the label's initial.
    icon: crate::window_icon::Icon,
}

/// Per-bar state: layout, mouse hover, and keyboard cursor.
#[derive(Default)]
struct BarEntry {
    hwnd: isize,
    /// Monitor work area this bar is docked to.
    area: (i32, i32, i32, i32),
    position: crate::config::Position,
    appbar: bool,
    rects: Vec<RECT>,
    /// Overflow button rect, shown when some task buttons do not fit.
    overflow: Option<RECT>,
    /// First task index hidden behind the overflow button (== rects.len()).
    hidden_from: usize,
    /// Quick-launch pin rects (left of the task strip).
    pin_rects: Vec<RECT>,
    /// Current button-width tier for this bar (hysteresis state).
    density: Density,
    hover: Option<usize>,
    /// Quick-launch pin under the cursor, if any.
    pin_hover: Option<usize>,
    /// Notification-strip icon under the cursor, if any. Hover shows the icon's
    /// tooltip but is deliberately **never** forwarded to its owner.
    tray_hover: Option<usize>,
    tray_pressed: Option<crate::tray::TrayItem>,
    tray_right_pressed: Option<crate::tray::TrayItem>,
    selected: Option<usize>,
    /// Foreground window before keyboard navigation; cleared when focus leaves.
    keyboard_return: isize,
}

fn bars() -> &'static Mutex<Vec<BarEntry>> {
    static B: OnceLock<Mutex<Vec<BarEntry>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(Vec::new()))
}

fn tasks() -> &'static Mutex<Vec<TaskItem>> {
    static T: OnceLock<Mutex<Vec<TaskItem>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

/// Notification-area icons, refreshed alongside the task list and rendered by
/// `crate::tray_bar` in the right zone.
///
/// This is the snapshot the strip is painted from **and** the one clicks are
/// resolved against, so the icon under the cursor is always the icon that was
/// drawn (0.1.3 plan §2.2 rule 4).
fn tray_icons() -> &'static Mutex<Vec<crate::tray::TrayItem>> {
    static T: OnceLock<Mutex<Vec<crate::tray::TrayItem>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

/// Where the notification strip sits inside a bar of client rect `client`.
///
/// The single source of the strip's geometry on the message thread: the painter
/// and every mouse handler call this, so their coordinate maths cannot diverge.
fn tray_layout(client: RECT) -> crate::tray_bar::Layout {
    if vertical() {
        return crate::tray_bar::Layout::new(client.bottom - BAR_H * 2, client.right - PAD, BAR_H);
    }
    let right = client.right - PAD - CLOCK_W - PAD;
    crate::tray_bar::Layout::new(BTN_TOP, right, BAR_H - 2 * BTN_TOP)
}

/// The notification icon under `(x, y)` in client coordinates, if any.
///
/// Returns a clone so no lock is held while the caller talks to another
/// process.
fn tray_item_at(client: RECT, x: i32, y: i32) -> Option<crate::tray::TrayItem> {
    let (icons, _) = notification_parts(client);
    let index = tray_layout(client).hit_test(icons.len(), x, y)?;
    icons.get(index).cloned()
}

/// Index of the notification icon under `(x, y)`, without cloning it.
fn tray_index_at(client: RECT, x: i32, y: i32) -> Option<usize> {
    let (icons, _) = notification_parts(client);
    tray_layout(client).hit_test(icons.len(), x, y)
}

/// Tooltip text for strip cell `index` plus that cell in **screen** coordinates.
///
/// `None` when the strip changed under us or the icon carries no tooltip.
fn tray_tooltip(key: isize, index: usize) -> Option<(String, RECT)> {
    let client = bar_client(key);
    let (icons, _) = notification_parts(client);
    let cell = tray_layout(client).cell_rect(index, icons.len())?;
    let tooltip = icons.get(index)?.tooltip.clone();
    drop(icons);
    if tooltip.is_empty() {
        return None;
    }

    let mut wr = RECT::default();
    // SAFETY: `key` is one of our live bar windows; converting client → screen.
    unsafe {
        let _ = GetWindowRect(hwnd_from(key), &mut wr);
    }
    Some((
        tooltip,
        RECT {
            left: cell.left + wr.left,
            top: cell.top + wr.top,
            right: cell.right + wr.left,
            bottom: cell.bottom + wr.top,
        },
    ))
}

fn notification_parts(client: RECT) -> (Vec<crate::tray::TrayItem>, Vec<crate::tray::TrayItem>) {
    let capacity = ((client.right
        - if vertical() {
            PAD * 2 + crate::tray_bar::CELL
        } else {
            RIGHT_W + 200
        })
    .max(0)
        / (crate::tray_bar::CELL + crate::tray_bar::GAP))
        .min(4) as usize;
    let mut visible = Vec::new();
    let mut hidden = Vec::new();
    for item in lock_or_recover(tray_icons())
        .iter()
        .filter(|i| i.owner.is_some())
    {
        if !item.hidden && visible.len() < capacity {
            visible.push(item.clone());
        } else {
            hidden.push(item.clone());
        }
    }
    (visible, hidden)
}
fn notification_width(client: RECT) -> i32 {
    let (visible, hidden) = notification_parts(client);
    crate::tray_bar::strip_width(visible.len(), crate::tray_bar::CELL, crate::tray_bar::GAP)
        + if hidden.is_empty() {
            0
        } else {
            crate::tray_bar::CELL + crate::tray_bar::GAP
        }
}
fn notification_arrow(client: RECT) -> Option<RECT> {
    let (visible, hidden) = notification_parts(client);
    if hidden.is_empty() {
        return None;
    }
    let right = tray_layout(client).right_x
        - crate::tray_bar::strip_width(visible.len(), crate::tray_bar::CELL, crate::tray_bar::GAP)
        - crate::tray_bar::GAP;
    Some(RECT {
        left: right - crate::tray_bar::CELL,
        top: if vertical() {
            tray_layout(client).top
        } else {
            0
        },
        right,
        bottom: if vertical() {
            tray_layout(client).top + BAR_H
        } else {
            BAR_H
        },
    })
}
fn clock_rect(client: RECT) -> RECT {
    if vertical() {
        return RECT {
            left: PAD,
            top: (client.bottom - BAR_H).max(0),
            right: client.right - PAD,
            bottom: client.bottom,
        };
    }
    let (left, right) = clock_span(client.right);
    RECT {
        left,
        top: 0,
        right,
        bottom: BAR_H,
    }
}
fn show_notifications(bar: HWND, client: RECT, hidden_only: bool) {
    let mut anchor = if hidden_only {
        notification_arrow(client).unwrap_or_else(|| clock_rect(client))
    } else {
        clock_rect(client)
    };
    let mut origin = RECT::default();
    let _ = unsafe { GetWindowRect(bar, &mut origin) };
    anchor.left += origin.left;
    anchor.right += origin.left;
    anchor.top += origin.top;
    anchor.bottom += origin.top;
    let (_, hidden) = notification_parts(client);
    let items = if hidden_only {
        hidden
    } else {
        lock_or_recover(tray_icons())
            .iter()
            .filter(|i| i.owner.is_some())
            .cloned()
            .collect()
    };
    crate::tray_popup::toggle(bar, anchor, items, !hidden_only);
}

/// Client rect of the bar with handle `key`.
fn bar_client(key: isize) -> RECT {
    let mut client = RECT::default();
    // SAFETY: `key` identifies one of our own bar windows; a stale handle just
    // leaves `client` zeroed.
    unsafe {
        let _ = GetClientRect(hwnd_from(key), &mut client);
    }
    client
}

/// Quick-launch pins (user-curated), refreshed alongside the task list. Each
/// entry carries the resolved icon so `paint` never touches the filesystem.
fn pins() -> &'static Mutex<Vec<PinItem>> {
    static P: OnceLock<Mutex<Vec<PinItem>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Vec::new()))
}

/// Per-group label anchors: group key → the window handle whose title names the
/// button. Locked to the first window seen and kept until that window closes, so
/// activating another member never renames the button (fixes the old
/// "label follows Z-order" drift).
fn group_anchors() -> &'static Mutex<HashMap<String, isize>> {
    static A: OnceLock<Mutex<HashMap<String, isize>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Width of the start-menu entry slot, which sits between the settings button
/// and the window-list button.
///
/// **Zero unless the start menu is enabled.** The entry is no longer offered in
/// the UI: the window-list button already covers "find any window", so a second
/// entry point is redundant (product decision, 2026-10-14). The feature stays
/// reachable through the config file only (`taskbar.menu` / `MINICON_START_MENU`,
/// default off), and the slot materialises again if someone turns it on — a
/// permanently reserved 12px dead gap next to settings is worse than nothing.
fn grip_w() -> i32 {
    if crate::config::current().start_menu {
        GRIP
    } else {
        0
    }
}

/// Left edge of the button that follows the settings button (the quick-launch
/// strip, or the window-list button when there are no pins).
///
/// `grip_w()` when the start-menu slot is live, otherwise the standard
/// inter-button gap — two buttons that draw the same `surface` background must
/// never touch, or they read as one two-glyph block.
fn after_settings() -> i32 {
    after_settings_for(grip_w())
}

/// Pure core of [`after_settings`], so the "never fuse two buttons" rule is
/// unit-testable without touching the process-global config.
fn after_settings_for(grip: i32) -> i32 {
    PAD + SETTINGS_W + grip.max(BTN_GAP)
}

/// Left edge of the quick-launch strip, just right of the settings button.
fn pins_left() -> i32 {
    PAD + SETTINGS_W + grip_w()
}

struct Brushes {
    bg: isize,
    glass: isize,
    surface: isize,
    hover: isize,
    accent: isize,
}

fn brushes() -> Brushes {
    let t = theme();
    Brushes {
        bg: t.bg as isize,
        glass: 0,
        surface: t.surface as isize,
        hover: t.hover as isize,
        accent: t.accent as isize,
    }
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

/// A smaller variant of [`font`], used for the `(+N)` count badge drawn on an
/// icon-only square's corner where the 12px face would not fit.
fn small_font() -> isize {
    static F: OnceLock<isize> = OnceLock::new();
    *F.get_or_init(|| {
        let face = wide("Segoe UI");
        // SAFETY: `face` outlives the call; the font is process-lifetime.
        unsafe {
            CreateFontW(
                -9,
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

/// Whether the taskbar host should be shown. On by default; disable via the
/// settings file (`taskbar.show = false`), `MINICON_NO_TASKBAR=1`, or
/// `MINICON_TASKBAR=0` (env precedence is resolved inside `config`).
pub fn enabled() -> bool {
    crate::config::current().show_taskbar
}

pub(crate) fn running() -> bool {
    !lock_or_recover(bars()).is_empty()
}

/// Cancel pending hover work on the UI thread when settings change or open.
pub(crate) fn cancel_hover() {
    let handles: Vec<isize> = lock_or_recover(bars()).iter().map(|b| b.hwnd).collect();
    for raw in handles {
        let _ = unsafe { KillTimer(Some(hwnd_from(raw)), HOVER_TIMER_ID) };
    }
    crate::hover_preview::hide();
    crate::bar_tip::hide();
}

/// The effective merge policy and grouping key, from the live configuration.
fn merge_policy() -> (crate::config::MergeStrategy, crate::config::GroupBy) {
    let cfg = crate::config::current();
    (cfg.merge, cfg.group_by)
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
            // Without a class cursor Windows shows the "app starting" (busy)
            // cursor on hover; request the plain arrow.
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )
            .unwrap_or_default(),
            // No class brush: we fill the whole client in WM_PAINT and handle
            // WM_ERASEBKGND ourselves, so the acrylic "glass" fill isn't erased
            // by an opaque background.
            hbrBackground: HBRUSH::default(),
            // `CS_DBLCLKS` is what lets the bar receive `WM_LBUTTONDBLCLK`; the
            // only double-click action is the clock (date/time settings). Every
            // other zone keys off `WM_LBUTTONDOWN`, so the extra message is inert.
            style: CS_DBLCLKS,
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
            let position = crate::config::current().position;
            let rect = crate::dock::bounds(
                RECT {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                },
                position,
                thickness(),
            );
            let hwnd = CreateWindowExW(
                // `WS_EX_NOACTIVATE` is what makes the bar behave like the stock
                // shell: clicking a button must not deactivate the app the user
                // is looking at. Without it a mouse-down activates the bar, so
                // `GetForegroundWindow` inside the click handler reports *us*
                // and the activate/minimize toggle can never see the real state.
                // Keyboard navigation is unaffected — it is entered via the
                // summon hotkey, which activates the bar with an explicit
                // `SetForegroundWindow` (allowed for this style).
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(class_title.as_ptr()),
                WS_POPUP | WS_VISIBLE,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(hinstance),
                None,
            )?;

            lock_or_recover(bars()).push(BarEntry {
                hwnd: hwnd.0 as isize,
                area: (x, y, w, h),
                position,
                ..Default::default()
            });
            tracing::info!(hwnd = ?hwnd, index = i, "taskbar window created");

            // Dock as an AppBar so maximized windows stop above the bar.
            let appbar = register_appbar(hwnd, (x, y, w, h));
            if let Some(bar) = lock_or_recover(bars())
                .iter_mut()
                .find(|bar| bar.hwnd == hwnd.0 as isize)
            {
                bar.appbar = appbar;
            }
            if appbar {
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

            // Repaint on a fixed cadence so the clock keeps up between refreshes.
            if SetTimer(Some(hwnd), CLOCK_TIMER_ID, CLOCK_TICK_MS, None) == 0 {
                tracing::warn!(
                    hwnd = ?hwnd,
                    "clock timer failed; time may lag until the next refresh"
                );
            }
        }

        ACRYLIC_ACTIVE.store(acrylic_ok, Ordering::SeqCst);
        if acrylic_wanted && !acrylic_ok {
            tracing::warn!("MINICON_ACRYLIC requested but unavailable; using opaque bar");
        }
    }
    // The icon-only hover tip is a process-lifetime popup; a failure is
    // non-fatal (icon-only squares simply show no label).
    if let Err(err) = crate::bar_tip::install(hinstance) {
        tracing::warn!(%err, "bar tip install failed");
    }
    refresh();
    Ok(())
}

/// Registers `hwnd` as an AppBar on the configured edge, then positions it
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

/// Repositions a registered AppBar on the configured edge with its thickness.
fn set_appbar_pos(hwnd: HWND, area: (i32, i32, i32, i32)) {
    thread_local! { static POSITIONING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    if POSITIONING.replace(true) {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            POSITIONING.set(false);
        }
    }
    let _reset = Reset;
    let position = crate::config::current().position;
    let (x, y, w, h) = area;
    // SAFETY: `abd` is a properly sized APPBARDATA for a registered AppBar.
    unsafe {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: hwnd,
            uCallbackMessage: APPBAR_CALLBACK,
            uEdge: match position {
                crate::config::Position::Bottom => ABE_BOTTOM,
                crate::config::Position::Top => ABE_TOP,
                crate::config::Position::Left => ABE_LEFT,
                crate::config::Position::Right => ABE_RIGHT,
            },
            rc: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            ..Default::default()
        };
        if SHAppBarMessage(ABM_QUERYPOS, &mut abd) == 0 {
            tracing::warn!(
                ?hwnd,
                "AppBar: ABM_QUERYPOS refused; using the full screen rect"
            );
        }
        abd.rc = crate::dock::bounds(abd.rc, position, thickness());
        if SHAppBarMessage(ABM_SETPOS, &mut abd) == 0 {
            tracing::warn!(
                ?hwnd,
                "AppBar: ABM_SETPOS refused; maximised windows will not stop above the bar"
            );
        }
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

/// Keeps task-button positions stable across refreshes.
///
/// `EnumWindows` yields windows in **Z-order** (top-most first), so feeding the
/// fresh enumeration straight into the button list would reshuffle the buttons
/// every time a window is activated or raised — the order would "follow"
/// whatever is currently focused. The real taskbar instead keeps a fixed order:
/// buttons already on screen stay put, and windows seen for the first time are
/// appended to the right in enumeration order.
///
/// Matching is by [`TaskItem::key`] (window handle, or the owning pid when
/// grouping is enabled).
fn stable_order(prev: &[TaskItem], current: Vec<TaskItem>) -> Vec<TaskItem> {
    let mut slots: Vec<Option<TaskItem>> = current.into_iter().map(Some).collect();
    let mut index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(slots.len());
    for (i, slot) in slots.iter().enumerate() {
        // Every slot is `Some` until it is taken below.
        index.insert(slot.as_ref().unwrap().key.clone(), i);
    }
    let mut out = Vec::with_capacity(slots.len());
    // Survivors: keep the previously-shown order.
    for p in prev {
        if let Some(&i) = index.get(p.key.as_str())
            && let Some(item) = slots[i].take()
        {
            out.push(item);
        }
    }
    // Newcomers: append in (Z-order) enumeration order.
    for slot in slots.iter_mut() {
        if let Some(item) = slot.take() {
            out.push(item);
        }
    }
    out
}

/// Builds one button per window (merge = `never`).
fn build_ungrouped(windows: Vec<crate::window_snapshot::WindowInfo>) -> Vec<TaskItem> {
    windows
        .into_iter()
        .map(|w| {
            let h = w.hwnd.0 as isize;
            TaskItem {
                key: h.to_string(),
                hwnd: h,
                members: vec![h],
                icon: crate::window_icon::for_window(w.hwnd),
                label: w.title,
            }
        })
        .collect()
}

/// The grouping key for one window when grouping by application: its exe path
/// (lower-cased), or a `pid:<n>` fallback when the image path cannot be read.
/// `cache` memoises the per-pid lookup within a single refresh.
fn group_key_for(
    w: &crate::window_snapshot::WindowInfo,
    cache: &mut HashMap<u32, String>,
) -> String {
    if let Some(v) = cache.get(&w.pid) {
        return v.clone();
    }
    // SAFETY: `w.hwnd` is a live top-level window from our snapshot.
    let key = unsafe { crate::win_util::window_exe_path(w.hwnd) }
        .map(|p| p.to_ascii_lowercase())
        .unwrap_or_else(|| format!("pid:{}", w.pid));
    cache.insert(w.pid, key.clone());
    key
}

/// Picks a group's label anchor: the previous anchor while it is still a live
/// member, otherwise the first member seen this refresh. Pure so the
/// "label must not drift" rule is unit-tested.
fn choose_anchor(prev: Option<isize>, members: &[isize]) -> isize {
    prev.filter(|a| members.contains(a))
        .or_else(|| members.first().copied())
        .unwrap_or(0)
}

/// A grouped button's label: the anchor title, with `(+N)` when the group holds
/// more than one window (the only place a count belongs in the UI).
fn group_label(base: &str, count: usize) -> String {
    let extra = count.saturating_sub(1);
    if extra > 0 {
        format!("{base} (+{extra})")
    } else {
        base.to_string()
    }
}

/// Builds one button per application group, labelling each with its anchor's
/// title (see [`group_anchors`]) plus `(+N)` when it holds several windows.
fn build_grouped(
    windows: Vec<crate::window_snapshot::WindowInfo>,
    by: crate::config::GroupBy,
) -> Vec<TaskItem> {
    let groups = match by {
        crate::config::GroupBy::Pid => crate::grouping::group_by_pid(windows),
        crate::config::GroupBy::Exe => {
            let mut cache: HashMap<u32, String> = HashMap::new();
            crate::grouping::group_by(windows, |w| group_key_for(w, &mut cache))
        }
    };

    let mut anchors = lock_or_recover(group_anchors());
    let mut next: HashMap<String, isize> = HashMap::with_capacity(groups.len());
    let items = groups
        .into_iter()
        .map(|group| {
            let members: Vec<isize> = group.windows.iter().map(|w| w.hwnd.0 as isize).collect();
            // Keep the previous anchor while it is still alive; otherwise adopt
            // the first window we see now (which then sticks).
            let anchor = choose_anchor(anchors.get(&group.key).copied(), &members);
            next.insert(group.key.clone(), anchor);
            let anchor_win = group
                .windows
                .iter()
                .find(|w| w.hwnd.0 as isize == anchor)
                .or_else(|| group.windows.first());
            let base = anchor_win.map(|w| w.title.clone()).unwrap_or_default();
            let label = group_label(&base, group.windows.len());
            TaskItem {
                key: group.key,
                hwnd: anchor,
                members,
                label,
                icon: anchor_win
                    .map(|w| crate::window_icon::for_window(w.hwnd))
                    .unwrap_or(0),
            }
        })
        .collect();
    // Forget anchors for groups that vanished; record the current set.
    *anchors = next;
    items
}

/// Client width used to judge overflow for `when_full`: the first bar with a
/// real width, else the primary screen. Pins are ignored (a slightly
/// conservative start), so the decision does not wobble as pins change.
fn reference_client_width() -> i32 {
    {
        let bars = lock_or_recover(bars());
        for b in bars.iter() {
            let mut r = RECT::default();
            // SAFETY: `b.hwnd` is a live bar window owned by this process.
            unsafe {
                let _ = GetClientRect(hwnd_from(b.hwnd), &mut r);
            }
            if r.right > 0 {
                return if vertical() { r.bottom } else { r.right };
            }
        }
    }
    // SAFETY: pure metric query.
    unsafe { GetSystemMetrics(SM_CXSCREEN) }
}

/// Room (px) available to the task strip on the reference bar, ignoring pins
/// (a slightly conservative start, per the PRD): the reference client width
/// minus the fixed left cluster, the clock zone, and the tray strip.
fn reference_task_avail(tray_w: i32) -> i32 {
    let start_left = after_settings();
    let tray = if tray_w > 0 { tray_w + BTN_GAP } else { 0 };
    (reference_client_width() - start_left - PAD - RIGHT_W - tray).max(0)
}

/// Whether `n` per-window buttons overflow the densest (icon-only) tier given
/// the strip width `avail`. This is exactly [`layout`]'s overflow test: the
/// per-button budget falls below the 40px icon-square floor.
fn overflows(avail: i32, n: usize, gap: i32) -> bool {
    budget(avail, n, gap) < Density::IconOnly.floor()
}

/// Whether `n` per-window buttons would spill past the bar even at the densest
/// tier — the trigger for `when_full` to fall back to grouping.
fn would_overflow(client_w: i32, n: usize, tray_w: i32) -> bool {
    let start_left = after_settings();
    let tray = if tray_w > 0 { tray_w + BTN_GAP } else { 0 };
    let avail = (client_w - start_left - PAD - RIGHT_W - tray).max(0);
    overflows(avail, n, BTN_GAP)
}

/// Whether laying out `n` per-window buttons in `avail` would fall to the
/// icon-only tier. Under `never` this triggers a *local* fold per application,
/// so several identical squares (same app, one window each) never appear.
fn fold_needed(avail: i32, n: usize, gap: i32) -> bool {
    budget(avail, n, gap) < D1_FLOOR
}

/// Whether the quick-launch pins must be folded this round: with the pins the
/// task strip still overflows, but without them it does not. The pin data is
/// untouched — only its reserved width is dropped.
fn hide_pins(avail_with_pins: i32, avail_without_pins: i32, n: usize, gap: i32) -> bool {
    overflows(avail_with_pins, n, gap) && !overflows(avail_without_pins, n, gap)
}

fn show_overflow(hwnd: HWND, hidden_from: usize) {
    let entries: Vec<crate::task_list::Entry> = lock_or_recover(tasks())
        .iter()
        .skip(hidden_from)
        .map(|t| crate::task_list::Entry {
            label: t.label.clone(),
            hwnd: t.hwnd,
        })
        .collect();
    if let Some(target) = crate::task_list::popup(hwnd, &entries) {
        activate(hwnd_from(target));
    }
}
fn reposition() {
    let position = crate::config::current().position;
    let changed: Vec<_> = {
        let mut bars = lock_or_recover(bars());
        bars.iter_mut()
            .enumerate()
            .filter_map(|(i, b)| {
                if b.position == position {
                    return None;
                }
                b.position = position;
                Some((i, b.hwnd, b.area, b.appbar))
            })
            .collect()
    };
    if changed.is_empty() {
        return;
    }
    cancel_hover();
    // Keep the lifetime baseline. Restoring and recapturing between edges can
    // capture an intermediate work area adjusted by another shell participant.
    // reserve_workarea replaces our reservation from that baseline atomically.
    for (i, key, area, appbar) in changed {
        if appbar {
            set_appbar_pos(hwnd_from(key), area)
        } else {
            let (x, y, w, h) = area;
            let r = crate::dock::bounds(
                RECT {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                },
                position,
                thickness(),
            );
            unsafe {
                let _ = SetWindowPos(
                    hwnd_from(key),
                    Some(HWND_TOPMOST),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE,
                );
            }
            if i == 0 {
                reserve_workarea(area)
            }
        }
    }
}
fn settings_rect(client: RECT) -> RECT {
    RECT {
        left: PAD,
        top: BTN_TOP,
        right: if vertical() {
            client.right - PAD - grip_w()
        } else {
            PAD + SETTINGS_W
        },
        bottom: BAR_H - BTN_TOP,
    }
}
fn vertical_layout(width: i32, start: i32, end: i32, n: usize) -> ButtonLayout {
    let capacity = ((end - start).max(0) / BAR_H) as usize;
    let visible = if n > capacity {
        capacity.saturating_sub(1)
    } else {
        n
    };
    let row = |i: usize| RECT {
        left: PAD,
        top: start + i as i32 * BAR_H,
        right: (width - PAD).max(PAD),
        bottom: start + i as i32 * BAR_H + BAR_H - BTN_GAP,
    };
    ButtonLayout {
        rects: (0..visible).map(row).collect(),
        overflow: if n > visible && capacity > 0 {
            Some(row(visible))
        } else {
            None
        },
        hidden_from: visible,
        density: Density::Full,
    }
}

/// Re-enumerates task windows, recomputes every bar's layout, and repaints.
pub fn refresh() {
    thread_local! { static REFRESHING:std::cell::Cell<bool>=const{std::cell::Cell::new(false)}; static PENDING:std::cell::Cell<bool>=const{std::cell::Cell::new(false)}; }
    if REFRESHING.replace(true) {
        PENDING.set(true);
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            REFRESHING.set(false);
        }
    }
    let _reset = Reset;
    loop {
        PENDING.set(false);
        refresh_inner();
        if !PENDING.get() {
            break;
        }
    }
}
fn refresh_inner() {
    reposition();
    crate::hover_preview::clear_invalid_target();
    // Window handles are recycled, so a cached icon may belong to a window that
    // has since been closed. Drop the cache before re-enumerating. Path icons
    // are ours, so they are freed rather than merely forgotten.
    crate::window_icon::begin_refresh();
    crate::window_icon::clear_path_icons();
    let windows = crate::window_snapshot::taskbar_windows();

    // Notification icons first: their reserved strip width feeds the task
    // layout *and* the `when_full` overflow decision. Hidden entirely when the
    // user turned the tray off (`taskbar.tray = false`). `tray::items()` is
    // cheap — the cross-process mirror read happens on the heartbeat instead.
    let show_tray = crate::config::current().show_tray;
    let icons = if show_tray {
        crate::tray::items()
    } else {
        Vec::new()
    };
    // Tells the log reader which shape the strip is in: all-forwardable means we
    // own the tray, none means the inert mirrored one (0.1.3 plan §2.1).
    tracing::debug!(
        total = icons.len(),
        forwardable = icons.iter().filter(|i| i.is_forwardable()).count(),
        "notification strip refreshed"
    );
    *lock_or_recover(tray_icons()) = icons;
    let reference = RECT {
        right: reference_client_width(),
        ..Default::default()
    };
    let (_, hidden) = notification_parts(reference);
    let all: Vec<_> = lock_or_recover(tray_icons())
        .iter()
        .filter(|i| i.owner.is_some())
        .cloned()
        .collect();
    crate::tray_popup::update(&all, &hidden);
    let tray_w = notification_width(reference);

    let ref_avail = reference_task_avail(tray_w);
    // Conservative fold basis: subtract the pins' width too, so the "would this
    // land in icon-only?" test matches the *actual* strip when the pins are
    // shown. Without this, a pin-heavy bar could reach icon-only with per-window
    // buttons and paint several identical squares for the same application.
    let raw_pins_w = {
        let count = crate::quick_launch::pins().len();
        if count == 0 {
            0
        } else {
            count as i32 * crate::quick_launch::CELL
                + (count as i32 - 1) * crate::quick_launch::PIN_GAP
        }
    };
    let fold_avail = (ref_avail
        - if raw_pins_w > 0 {
            raw_pins_w + BTN_GAP
        } else {
            0
        })
    .max(0);
    let (merge, group_by) = merge_policy();
    let use_grouping = match merge {
        // `never` normally keeps one button per window — but once the density
        // would drop to icon-only, identical squares for the same application
        // become indistinguishable, so the application folds locally (+N).
        crate::config::MergeStrategy::Never => {
            !vertical() && fold_needed(fold_avail, windows.len(), BTN_GAP)
        }
        crate::config::MergeStrategy::Always => true,
        // Group only once a per-window bar would overflow at the densest tier.
        crate::config::MergeStrategy::WhenFull => {
            if vertical() {
                windows.len().saturating_mul(BAR_H as usize)
                    > (reference_client_width() - 180).max(0) as usize
            } else {
                would_overflow(reference_client_width(), windows.len(), tray_w)
            }
        }
    };
    let tasks_now = if use_grouping {
        build_grouped(windows, group_by)
    } else {
        build_ungrouped(windows)
    };

    // EnumWindows is Z-ordered; preserve button positions across refreshes so
    // activating a window never reshuffles the bar (see `stable_order`).
    let tasks_now = {
        let prev = lock_or_recover(tasks());
        stable_order(prev.as_slice(), tasks_now)
    };
    let n = tasks_now.len();
    *lock_or_recover(tasks()) = tasks_now;

    // Refresh quick-launch pins; task buttons start after them + the list button.
    // Icons are resolved here (filesystem IO) rather than in `paint`.
    let pins_now: Vec<PinItem> = crate::quick_launch::pins()
        .into_iter()
        .map(|pin| PinItem {
            icon: crate::window_icon::for_path(&pin.path),
            label: pin.label,
            path: pin.path,
        })
        .collect();
    let pins_w = if pins_now.is_empty() {
        0
    } else {
        pins_now.len() as i32 * crate::quick_launch::CELL
            + (pins_now.len() as i32 - 1) * crate::quick_launch::PIN_GAP
    };
    *lock_or_recover(pins()) = pins_now;

    // Fold the quick-launch strip this round when it is the sole cause of task
    // overflow (fold the tray first — it is gone before this; the pins next;
    // the task strip last). The pin data stays; only its width is dropped.
    let hide_pins_now =
        pins_w > 0 && hide_pins(ref_avail - pins_w - BTN_GAP, ref_avail, n, BTN_GAP);
    let pins_w = if hide_pins_now { 0 } else { pins_w };

    let after_pins = if pins_w > 0 {
        pins_left() + pins_w + BTN_GAP
    } else {
        after_settings()
    };
    let tasks_left = after_pins;

    let mut bars = lock_or_recover(bars());
    if bars.is_empty() {
        return;
    }
    let mut first_density = Density::Full;
    for (index, bar) in bars.iter_mut().enumerate() {
        let hwnd = hwnd_from(bar.hwnd);
        let mut client = RECT::default();
        // SAFETY: `hwnd` is a live bar window owned by this process.
        unsafe {
            let _ = GetClientRect(hwnd, &mut client);
        }
        let bl = if vertical() {
            let count = lock_or_recover(pins()).len();
            let cols = ((client.right - 2 * PAD)
                / (crate::quick_launch::CELL + crate::quick_launch::PIN_GAP))
                .max(1) as usize;
            let stride = crate::quick_launch::CELL + crate::quick_launch::PIN_GAP;
            bar.pin_rects = (0..count)
                .map(|i| {
                    let x = PAD + (i % cols) as i32 * stride;
                    let y = BAR_H + PAD + (i / cols) as i32 * stride;
                    RECT {
                        left: x,
                        top: y,
                        right: x + crate::quick_launch::CELL,
                        bottom: y + crate::quick_launch::CELL,
                    }
                })
                .collect();
            let start = BAR_H
                + PAD
                + count.div_ceil(cols) as i32 * stride
                + if count > 0 { PAD } else { 0 };
            vertical_layout(client.right, start, client.bottom - BAR_H * 2 - PAD, n)
        } else {
            bar.pin_rects = if hide_pins_now {
                Vec::new()
            } else {
                let pin_count = lock_or_recover(pins()).len();
                crate::quick_launch::layout(
                    pins_left(),
                    pin_count,
                    crate::quick_launch::CELL,
                    crate::quick_launch::PIN_GAP,
                    BAR_H,
                )
            };
            layout(
                tasks_left,
                client.right,
                n,
                notification_width(client),
                bar.density,
            )
        };
        bar.rects = bl.rects;
        bar.overflow = bl.overflow;
        bar.hidden_from = bl.hidden_from;
        bar.density = bl.density;
        if index == 0 {
            first_density = bar.density;
        }
        if bar.hover.is_some_and(|h| h >= n) {
            bar.hover = None;
        }
        let navigable = bar.rects.len() + usize::from(bar.overflow.is_some());
        if bar.selected.is_some_and(|s| s >= navigable) {
            bar.selected = navigable.checked_sub(1);
        }
        // SAFETY: same as above.
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
    }
    tracing::debug!(
        count = n,
        pins = lock_or_recover(pins()).len(),
        bars = bars.len(),
        density = ?first_density,
        groups = ?lock_or_recover(tasks()).iter().map(|t| format!("{}#{}", t.key, t.label)).collect::<Vec<_>>(),
        "taskbar refreshed"
    );
}

/// Result of laying out the task buttons for one bar.
struct ButtonLayout {
    /// Visible task-button rects (index `i` ↔ task `i`).
    rects: Vec<RECT>,
    /// Overflow button, present when `rects.len() < n`.
    overflow: Option<RECT>,
    /// Number of tasks represented by visible buttons (== `rects.len()`).
    hidden_from: usize,
    /// Density tier chosen for this layout (fed back as the next `prev`).
    density: Density,
}

/// One task-button rect at position `i` given a fixed button width.
fn rect_at(left: i32, i: usize, bw: i32) -> RECT {
    let x = left + i as i32 * (bw + BTN_GAP);
    RECT {
        left: x,
        top: BTN_TOP,
        right: x + bw,
        bottom: BAR_H - BTN_TOP,
    }
}

/// Lays out task buttons starting at `start_left`, reserving `tray_w` pixels on
/// the right for the notification strip and `RIGHT_W` for the clock.
///
/// Product rule: the right zone (clock) must **never** be overlapped. The tier
/// is chosen from the per-button budget with hysteresis against `prev`; if even
/// the densest tier cannot hold every button, as many as fit are shown in
/// compact form plus an overflow button (`»`) opening the full list.
fn layout(start_left: i32, client_w: i32, n: usize, tray_w: i32, prev: Density) -> ButtonLayout {
    if n == 0 {
        return ButtonLayout {
            rects: Vec::new(),
            overflow: None,
            hidden_from: 0,
            density: prev,
        };
    }
    let tray = if tray_w > 0 { tray_w + BTN_GAP } else { 0 };
    let avail = (client_w - start_left - PAD - RIGHT_W - tray).max(0);
    let bud = budget(avail, n, BTN_GAP);
    let density = density_for(bud, prev);

    // The whole set fits in the chosen tier → proportional width, clamped to it.
    if bud >= density.floor() {
        let bw = bud.clamp(density.floor(), density.ceil());
        let rects = (0..n).map(|i| rect_at(start_left, i, bw)).collect();
        return ButtonLayout {
            rects,
            overflow: None,
            hidden_from: n,
            density,
        };
    }

    // Overflow: as many icon squares as fit, plus one overflow button. Keep at
    // least one button visible when that does not cross the clock, so an extreme
    // bar never degrades to a lone `»`. Only the icon-only tier can reach here
    // (`bud < floor`), so the squares are 40px wide.
    let bw = density.floor();
    let room = avail - OVERFLOW_W - BTN_GAP;
    let mut k = if room >= 0 {
        (room / (bw + BTN_GAP)) as usize
    } else {
        0
    };
    k = k.min(n);
    if k == 0 && n >= 1 && avail >= bw + BTN_GAP + OVERFLOW_W {
        k = 1;
    }
    let rects: Vec<RECT> = (0..k).map(|i| rect_at(start_left, i, bw)).collect();
    let ox = start_left + k as i32 * (bw + BTN_GAP);
    let overflow = (k < n).then(|| RECT {
        left: ox,
        top: BTN_TOP,
        right: ox + OVERFLOW_W,
        bottom: BAR_H - BTN_TOP,
    });
    ButtonLayout {
        rects,
        overflow,
        hidden_from: k,
        density,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the guard only runs the body and degrades to the default handler
    // on a panic; all handles come straight from Win32.
    guard_abi(
        "taskbar::wndproc",
        || unsafe { wndproc_inner(hwnd, msg, wparam, lparam) },
        || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    )
}

unsafe extern "system" fn wndproc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
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
            let client = bar_client(key);
            // Hover over a notification icon only ever shows *our* tooltip: the
            // event is never forwarded (0.1.3 plan §2.2 rule 3).
            let tray_hover = tray_index_at(client, x, y);
            let (changed, hover, density, tray_changed) = {
                let mut bars = lock_or_recover(bars());
                match bars.iter_mut().find(|b| b.hwnd == key) {
                    Some(bar) => {
                        let hover = bar
                            .rects
                            .iter()
                            .position(|r| crate::win_util::rect_contains(*r, x, y));
                        let pin_hover = crate::quick_launch::hit(&bar.pin_rects, x, y);
                        let changed = hover != bar.hover || pin_hover != bar.pin_hover;
                        bar.hover = hover;
                        bar.pin_hover = pin_hover;
                        let tray_changed = tray_hover != bar.tray_hover;
                        bar.tray_hover = tray_hover;
                        (changed, hover, bar.density, tray_changed)
                    }
                    None => (false, None, Density::Full, false),
                }
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            if changed || tray_changed {
                // Re-arm the debounced hover timer for the new target. An
                // icon-only square shows a title tip; the labelled tiers keep
                // the (opt-in) thumbnail preview; a strip cell shows the icon's
                // own tooltip.
                let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
                crate::hover_preview::hide();
                crate::bar_tip::hide();
                let want = tray_hover.is_some()
                    || (hover.is_some()
                        && (vertical()
                            || density.is_icon_only()
                            || crate::hover_preview::enabled()));
                if want {
                    let _ = unsafe { SetTimer(Some(hwnd), HOVER_TIMER_ID, HOVER_DELAY_MS, None) };
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == HOVER_TIMER_ID => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            let state = lock_or_recover(bars())
                .iter()
                .find(|b| b.hwnd == key)
                .and_then(|b| b.hover.map(|i| (i, b.density)));
            let tray_hover = lock_or_recover(bars())
                .iter()
                .find(|b| b.hwnd == key)
                .and_then(|b| b.tray_hover);
            // A notification icon's own tooltip wins: its cell sits left of the
            // clock, so it can never overlap a task button.
            if let Some((text, anchor)) = tray_hover.and_then(|i| tray_tooltip(key, i)) {
                crate::bar_tip::show(&text, anchor);
                return LRESULT(0);
            }
            if let Some((i, density)) = state {
                let btn = lock_or_recover(bars())
                    .iter()
                    .find(|b| b.hwnd == key)
                    .and_then(|b| b.rects.get(i).copied());
                if let Some(r) = btn {
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
                    if vertical() || density.is_icon_only() {
                        // A title-less square gets a label bubble (always).
                        if let Some(label) =
                            lock_or_recover(tasks()).get(i).map(|t| t.label.clone())
                        {
                            crate::bar_tip::show(&label, anchor);
                        }
                    } else if crate::hover_preview::enabled()
                        && let Some(target) = lock_or_recover(tasks()).get(i).map(|t| t.hwnd)
                    {
                        crate::hover_preview::show(target, anchor);
                    }
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == CLOCK_TIMER_ID => {
            // Layout is cached; a full-bar repaint is cheap and only redraws.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            crate::bar_tip::hide();
            let changed = {
                let mut bars = lock_or_recover(bars());
                match bars.iter_mut().find(|b| b.hwnd == key) {
                    Some(b) => {
                        // Take all of them so a reset clears every hover
                        // indicator (the strip cell needs no repaint, only the
                        // timer state cleared).
                        let task = b.hover.take().is_some();
                        let pin = b.pin_hover.take().is_some();
                        b.tray_hover = None;
                        task || pin
                    }
                    None => false,
                }
            };
            if changed {
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(bar) = lock_or_recover(bars()).iter_mut().find(|b| b.hwnd == key) {
                bar.tray_pressed = None;
            }
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            crate::bar_tip::hide();
            let x = get_x(lparam);
            let y = get_y(lparam);
            let client = bar_client(key);
            let contains = |r: RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
            if notification_arrow(client).is_some_and(contains) {
                show_notifications(hwnd, client, true);
                return LRESULT(0);
            }
            if (vertical() || clock_hit(client.right, x)) && contains(clock_rect(client)) {
                show_notifications(hwnd, client, false);
                return LRESULT(0);
            }
            // Notification strip: hand the click to the app that registered the
            // icon, exactly as the stock tray would (0.1.3 plan §2.2). Inert in
            // the mirrored shape — `forward` logs and does nothing.
            if let Some(item) = tray_item_at(bar_client(key), x, y) {
                if let Some(bar) = lock_or_recover(bars()).iter_mut().find(|b| b.hwnd == key) {
                    bar.tray_pressed = Some(item.clone());
                }
                crate::tray_bar::forward(&item, crate::tray_bar::MouseEvent::LeftDown);
                return LRESULT(0);
            }
            // Settings button (leftmost) opens the settings window.
            if contains(settings_rect(client)) {
                crate::settings_ui::toggle_from(hwnd);
                return LRESULT(0);
            }
            // Start-menu entry slot. Not offered in the UI any more (see
            // [`grip_w`]), so this only fires for a config-file opt-in — and the
            // slot has zero width unless that opt-in is set.
            if crate::config::current().start_menu
                && contains(RECT {
                    left: settings_rect(client).right,
                    top: BTN_TOP,
                    right: settings_rect(client).right + grip_w(),
                    bottom: BAR_H - BTN_TOP,
                })
            {
                crate::start_menu::toggle();
                return LRESULT(0);
            }
            // Snapshot this bar's interactive geometry.
            let (pin_rects, overflow, hidden_from, hover) = {
                let bars = lock_or_recover(bars());
                match bars.iter().find(|b| b.hwnd == key) {
                    Some(b) => (
                        b.pin_rects.clone(),
                        b.overflow,
                        b.hidden_from,
                        b.rects.iter().position(|r| contains(*r)),
                    ),
                    None => (Vec::new(), None, 0, None),
                }
            };
            // Overflow button: popup over the tasks that did not fit.
            if overflow.is_some_and(|r| crate::win_util::rect_contains(r, x, y)) {
                show_overflow(hwnd, hidden_from);
                return LRESULT(0);
            }
            // Quick-launch pins.
            if let Some(i) = crate::quick_launch::hit(&pin_rects, x, y) {
                let path = lock_or_recover(pins()).get(i).map(|p| p.path.clone());
                if let Some(path) = path {
                    crate::quick_launch::launch_path(&path);
                }
                return LRESULT(0);
            }
            let target = hover.and_then(|i| {
                lock_or_recover(tasks())
                    .get(i)
                    .map(|t| (t.hwnd, t.members.clone()))
            });
            tracing::debug!(?hover, task_hit = target.is_some(), "task button down");
            if let Some((target, members)) = target {
                let fg = unsafe { GetForegroundWindow() }.0 as isize;
                // A grouped button whose group already owns the foreground opens
                // the member list (a second click picks one); otherwise it just
                // switches back to the group. Locks are released before the
                // cross-process title queries below.
                if members.len() > 1 && members.contains(&fg) {
                    let entries: Vec<crate::task_list::Entry> = members
                        .iter()
                        .map(|&h| crate::task_list::Entry {
                            label: window_title(hwnd_from(h)),
                            hwnd: h,
                        })
                        .collect();
                    if let Some(sel) = crate::task_list::popup(hwnd, &entries) {
                        activate(hwnd_from(sel));
                    }
                } else if target != 0 {
                    toggle_or_activate(hwnd_from(target));
                }
            }
            LRESULT(0)
        }
        // Double-clicking the clock opens Windows' own date/time settings — the
        // same destination as a double-click on the stock tray clock. The bar
        // class carries `CS_DBLCLKS`, so the second press arrives here rather
        // than as another `WM_LBUTTONDOWN`; nothing else reacts to double-clicks.
        WM_LBUTTONDBLCLK => {
            let mut client = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            let x = get_x(lparam);
            let y = get_y(lparam);
            if notification_arrow(client).is_some_and(|r| crate::win_util::rect_contains(r, x, y)) {
                show_notifications(hwnd, client, true);
                return LRESULT(0);
            }
            if crate::win_util::rect_contains(clock_rect(client), x, y) {
                crate::tray_popup::hide();
                crate::tray_popup::open_date_time_settings();
                return LRESULT(0);
            }
            // Notification strip first: a double-click on a tray icon is the
            // "default action" gesture (open the app's window).
            if let Some(item) = tray_item_at(client, x, get_y(lparam)) {
                if let Some(bar) = lock_or_recover(bars()).iter_mut().find(|b| b.hwnd == key) {
                    bar.tray_pressed = Some(item.clone());
                }
                crate::tray_bar::forward(&item, crate::tray_bar::MouseEvent::LeftDoubleClick);
                return LRESULT(0);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let pressed = lock_or_recover(bars())
                .iter_mut()
                .find(|b| b.hwnd == key)
                .and_then(|b| b.tray_pressed.take());
            let current = tray_item_at(bar_client(key), get_x(lparam), get_y(lparam));
            if let Some(item) =
                pressed.filter(|old| current.as_ref().is_some_and(|item| item.owner == old.owner))
            {
                crate::tray_bar::forward(&item, crate::tray_bar::MouseEvent::LeftUp);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Half of the right-click pair a hosted icon may need (some apps
            // finalise on release); the mirrored shape ignores it.
            let client = bar_client(key);
            let pressed = lock_or_recover(bars())
                .iter_mut()
                .find(|b| b.hwnd == key)
                .and_then(|b| b.tray_right_pressed.take());
            let current = tray_item_at(client, get_x(lparam), get_y(lparam));
            if let Some(item) =
                pressed.filter(|old| current.as_ref().is_some_and(|item| item.owner == old.owner))
            {
                crate::tray_bar::forward(&item, crate::tray_bar::MouseEvent::RightUp);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if let Some(bar) = lock_or_recover(bars()).iter_mut().find(|b| b.hwnd == key) {
                bar.tray_right_pressed = None;
            }
            let _ = unsafe { KillTimer(Some(hwnd), HOVER_TIMER_ID) };
            crate::hover_preview::hide();
            crate::bar_tip::hide();
            let x = get_x(lparam);
            let y = get_y(lparam);
            // Notification strip: most tray icons open their own context menu on
            // right-button-down, so the event goes to the owner rather than
            // opening one of ours.
            if let Some(item) = tray_item_at(bar_client(key), x, y) {
                if let Some(bar) = lock_or_recover(bars()).iter_mut().find(|b| b.hwnd == key) {
                    bar.tray_right_pressed = Some(item.clone());
                }
                crate::tray_bar::forward(&item, crate::tray_bar::MouseEvent::RightDown);
                return LRESULT(0);
            }
            // Hit-test against the live geometry rather than the (possibly
            // stale) hover index, so a right-click lands on what is under it.
            let (pin_rects, rects) = {
                let bars = lock_or_recover(bars());
                match bars.iter().find(|b| b.hwnd == key) {
                    Some(b) => (b.pin_rects.clone(), b.rects.clone()),
                    None => (Vec::new(), Vec::new()),
                }
            };
            if let Some(i) = crate::quick_launch::hit(&pin_rects, x, y) {
                pin_menu(hwnd, i);
                refresh();
                return LRESULT(0);
            }
            let task_hit = rects
                .iter()
                .position(|r| crate::win_util::rect_contains(*r, x, y));
            // Clone the hit item's fields out of the lock before any cross-process
            // work: `window_menu`/`group_menu` must never run holding a lock.
            let target = task_hit.and_then(|i| {
                lock_or_recover(tasks())
                    .get(i)
                    .map(|t| (t.hwnd, t.label.clone(), t.members.clone()))
            });
            if let Some((target, label, members)) = target {
                if members.len() > 1 {
                    group_menu(hwnd, hwnd_from(target), &label, &members);
                } else {
                    window_menu(hwnd, hwnd_from(target), &label);
                }
                refresh();
            }
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            // Summon: focus the primary bar and start the keyboard cursor at 0.
            // `force_foreground` (not a bare `SetForegroundWindow`) because a
            // top-most tool window is routinely refused the foreground.
            let previous = unsafe { GetForegroundWindow() }.0 as isize;
            if previous != key
                && let Some(bar) = lock_or_recover(bars())
                    .iter_mut()
                    .find(|bar| bar.hwnd == key)
            {
                bar.keyboard_return = previous;
            }
            let _ = unsafe { crate::win_util::force_foreground(hwnd) };
            let _ = unsafe { SetFocus(Some(hwnd)) };
            let changed = {
                let has_tasks = !lock_or_recover(tasks()).is_empty();
                let mut bars = lock_or_recover(bars());
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
        windows::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE
        | windows::Win32::UI::WindowsAndMessaging::WM_THEMECHANGED => {
            crate::appearance::refresh();
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u16 == VK_ESCAPE.0 {
                let previous = {
                    let mut all = lock_or_recover(bars());
                    all.iter_mut()
                        .find(|bar| bar.hwnd == key)
                        .map(|bar| {
                            bar.selected = None;
                            std::mem::take(&mut bar.keyboard_return)
                        })
                        .unwrap_or(0)
                };
                if previous != 0
                    && previous != key
                    && unsafe { IsWindow(Some(hwnd_from(previous))).as_bool() }
                {
                    activate(hwnd_from(previous));
                }
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                return LRESULT(0);
            }
            let vk = wparam.0 as u16;
            let n = lock_or_recover(tasks()).len();
            let mut changed = false;
            if n > 0 {
                let mut bars = lock_or_recover(bars());
                if let Some(bar) = bars.iter_mut().find(|b| b.hwnd == key) {
                    // Once navigating, `1..9` jump straight to the n-th visible
                    // button (no activation — `Enter` still does that, matching
                    // the Left/Right cursor). Ignored outside navigation.
                    if bar.selected.is_some()
                        && let Some(i) = digit_index(vk, bar.rects.len())
                    {
                        bar.selected = Some(i);
                        changed = true;
                        tracing::debug!(index = i, "keyboard digit jump");
                    } else {
                        match vk {
                            k if k == VK_LEFT.0
                                || k == VK_RIGHT.0
                                || k == VK_UP.0
                                || k == VK_DOWN.0 =>
                            {
                                bar.selected = step_index(
                                    bar.selected,
                                    bar.rects.len() + usize::from(bar.overflow.is_some()),
                                    k == VK_RIGHT.0 || k == VK_DOWN.0,
                                );
                                changed = true;
                            }
                            k if k == VK_HOME.0 || k == VK_END.0 => {
                                let count = bar.rects.len() + usize::from(bar.overflow.is_some());
                                bar.selected = (count > 0).then_some(if k == VK_HOME.0 {
                                    0
                                } else {
                                    count.saturating_sub(1)
                                });
                                changed = true;
                            }
                            k if k == VK_RETURN.0 || k == VK_SPACE.0 => {
                                if bar.overflow.is_some() && bar.selected == Some(bar.hidden_from) {
                                    let from = bar.hidden_from;
                                    bar.selected = None;
                                    drop(bars);
                                    show_overflow(hwnd, from);
                                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                                    return LRESULT(0);
                                }

                                let target = bar
                                    .selected
                                    .and_then(|i| lock_or_recover(tasks()).get(i).map(|t| t.hwnd));
                                bar.selected = None;
                                if let Some(target) = target {
                                    drop(bars);
                                    activate(hwnd_from(target));
                                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                                    return LRESULT(0);
                                }
                            }
                            _ => {}
                        }
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
                let has_tasks = !lock_or_recover(tasks()).is_empty();
                let mut bars = lock_or_recover(bars());
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
            crate::bar_tip::hide();
            let changed = {
                let mut bars = lock_or_recover(bars());
                bars.iter_mut().find(|b| b.hwnd == key).is_some_and(|b| {
                    b.keyboard_return = 0;
                    b.selected.take().is_some()
                })
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
                let area = lock_or_recover(bars())
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
            let mut bars = lock_or_recover(bars());
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
        FillRect(hdc, &client, hbrush(hdc, bg));

        // Settings button (left of the menu dot): a three-slider glyph.
        let set_rect = settings_rect(client);
        FillRect(hdc, &set_rect, hbrush(hdc, br.surface));
        let scx = if vertical() {
            PAD + SETTINGS_W / 2
        } else {
            (set_rect.left + set_rect.right) / 2
        };
        for (k, off) in [4, -3, 1].into_iter().enumerate() {
            let y = BAR_H / 2 - 5 + k as i32 * 5;
            let line = RECT {
                left: scx - 6,
                top: y,
                right: scx + 6,
                bottom: y + 1,
            };
            FillRect(hdc, &line, hbrush(hdc, br.accent));
            let knob = RECT {
                left: scx + off - 1,
                top: y - 2,
                right: scx + off + 2,
                bottom: y + 3,
            };
            FillRect(hdc, &knob, hbrush(hdc, br.accent));
        }

        // Start-menu entry: a small accent dot in the slot right of settings.
        // The entry is no longer offered in the UI (see `grip_w`) — the
        // window-list button already covers "find any window" — so this paints
        // only for a config-file opt-in (`taskbar.menu`). The slot itself has
        // zero width otherwise, so nothing is reserved and no dead gap remains.
        if crate::config::current().start_menu {
            let dot = RECT {
                left: settings_rect(client).right + 3,
                top: BAR_H / 2 - 3,
                right: settings_rect(client).right + 9,
                bottom: BAR_H / 2 + 3,
            };
            FillRect(hdc, &dot, hbrush(hdc, br.accent));
        }

        let old_font = SelectObject(hdc, HGDIOBJ(font() as *mut core::ffi::c_void));
        SetBkMode(hdc, TRANSPARENT);

        if vertical() {
            SetTextColor(hdc, COLORREF(theme().text));
            let mut title: Vec<u16> = crate::i18n::text("设置").encode_utf16().collect();
            let mut r = set_rect;
            r.left += SETTINGS_W + PAD;
            DrawTextW(
                hdc,
                &mut title,
                &mut r,
                DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        let mut fg = GetForegroundWindow().0 as isize;
        if fg == hwnd.0 as isize
            && let Some(bar) = lock_or_recover(bars()).iter().find(|bar| bar.hwnd == fg)
        {
            fg = bar.keyboard_return;
        }
        let tasks = lock_or_recover(tasks());
        let (rects, overflow, pin_rects, hover, pin_hover, selected, density) = {
            let bars = lock_or_recover(bars());
            match bars.iter().find(|b| b.hwnd == hwnd.0 as isize) {
                Some(b) => (
                    b.rects.clone(),
                    b.overflow,
                    b.pin_rects.clone(),
                    b.hover,
                    b.pin_hover,
                    b.selected,
                    b.density,
                ),
                None => (
                    Vec::new(),
                    None,
                    Vec::new(),
                    None,
                    None,
                    None,
                    Density::Full,
                ),
            }
        };
        let (icon_size, icon_pad, icon_gap) = density.icon_metrics();

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
            FillRect(hdc, &r, hbrush(hdc, brush));

            if active {
                let marker = RECT {
                    left: r.left,
                    top: r.top,
                    right: r.left + 3,
                    bottom: r.bottom,
                };
                FillRect(hdc, &marker, hbrush(hdc, br.accent));
            }

            // Keyboard cursor: a 1px accent outline around the selected button.
            if is_selected {
                FrameRect(hdc, &r, hbrush(hdc, br.accent));
            }

            // Identity slot: the application icon. Drawn at its natural colors —
            // it is the one element allowed to be loud, because it answers
            // "which app" before the title is read.
            let has_icon = item.icon != 0;

            if density.is_icon_only() {
                // No title: the icon grows to fill the square, dead-centre.
                let cx = (r.left + r.right - icon_size) / 2;
                let cy = (r.top + r.bottom - icon_size) / 2;
                if has_icon {
                    // SAFETY: the handle is owned by the target window or its
                    // class and outlives the call; GDI only reads it.
                    let _ = DrawIconEx(
                        hdc,
                        cx,
                        cy,
                        HICON(item.icon as *mut core::ffi::c_void),
                        icon_size,
                        icon_size,
                        0,
                        None,
                        DI_NORMAL,
                    );
                } else {
                    // No icon: fall back to the title's initial, centred.
                    SetTextColor(hdc, COLORREF(theme().text));
                    let head: String = item.label.chars().take(1).collect();
                    let mut text: Vec<u16> = head.encode_utf16().collect();
                    let mut tr = r;
                    DrawTextW(
                        hdc,
                        &mut text,
                        &mut tr,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    );
                }
                // Groups carry a `(+N)` count in the square's top-right corner
                // (the only place the count is legitimate in icon-only form).
                if item.members.len() > 1 {
                    let count = format!("+{}", item.members.len() - 1);
                    let mut text: Vec<u16> = count.encode_utf16().collect();
                    let mut tr = RECT {
                        left: r.left + icon_size / 2,
                        top: r.top - 1,
                        right: r.right - 2,
                        bottom: r.top + 11,
                    };
                    let old_small =
                        SelectObject(hdc, HGDIOBJ(small_font() as *mut core::ffi::c_void));
                    SetTextColor(hdc, COLORREF(theme().accent));
                    DrawTextW(
                        hdc,
                        &mut text,
                        &mut tr,
                        DT_RIGHT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX,
                    );
                    SelectObject(hdc, old_small);
                }
                continue;
            }

            if has_icon {
                // SAFETY: the handle belongs to the target window or its class
                // and stays valid for the duration of the call; the icon cache
                // never destroys what it hands out.
                let _ = DrawIconEx(
                    hdc,
                    r.left + icon_pad,
                    (r.top + r.bottom - ICON_W) / 2,
                    HICON(item.icon as *mut core::ffi::c_void),
                    ICON_W,
                    ICON_W,
                    0,
                    None,
                    DI_NORMAL,
                );
            }

            SetTextColor(
                hdc,
                if active || is_selected || hover == Some(i) {
                    COLORREF(theme().text)
                } else {
                    COLORREF(theme().muted)
                },
            );
            let mut text: Vec<u16> = item.label.encode_utf16().collect();
            let mut tr = r;
            tr.left += if has_icon {
                icon_pad + ICON_W + icon_gap
            } else {
                9
            };
            tr.right -= 8;
            DrawTextW(
                hdc,
                &mut text,
                &mut tr,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }

        // Quick-launch pins: surface squares carrying the pinned app's icon, or
        // the label's initial when no icon could be extracted. The strip only
        // claims space when it is non-empty (see `refresh`).
        let pin_items: Vec<(String, crate::window_icon::Icon)> = lock_or_recover(pins())
            .iter()
            .map(|p| (p.label.clone(), p.icon))
            .collect();
        for (i, r) in pin_rects.iter().enumerate() {
            let fill = if pin_hover == Some(i) {
                br.hover
            } else {
                br.surface
            };
            FillRect(hdc, r, hbrush(hdc, fill));
            let Some((label, icon)) = pin_items.get(i) else {
                continue;
            };
            let cx = (r.left + r.right - ICON_W) / 2;
            let cy = (r.top + r.bottom - ICON_W) / 2;
            if *icon != 0 {
                // SAFETY: the handle was extracted by `window_icon::for_path`
                // and is valid until the next refresh; GDI only reads it.
                let _ = DrawIconEx(
                    hdc,
                    cx,
                    cy,
                    HICON(*icon as *mut core::ffi::c_void),
                    ICON_W,
                    ICON_W,
                    0,
                    None,
                    DI_NORMAL,
                );
            } else {
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

        // Overflow button: shown when some task buttons do not fit.
        if let Some(r) = overflow {
            FillRect(hdc, &r, hbrush(hdc, br.surface));
            if selected == Some(rects.len()) {
                FrameRect(hdc, &r, hbrush(hdc, br.accent));
            }
            SetTextColor(hdc, COLORREF(theme().muted));
            let mut text: Vec<u16> = "»".encode_utf16().collect();
            let mut tr = r;
            DrawTextW(
                hdc,
                &mut text,
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }

        // Notification-icon strip, sitting just left of the clock.
        let (icons, _) = notification_parts(client);
        if !icons.is_empty() {
            let layout = tray_layout(client);
            crate::tray_bar::paint(hdc, &layout, &icons);
        }
        SetTextColor(hdc, COLORREF(theme().muted));
        if let Some(mut rect) = notification_arrow(client) {
            let mut arrow = [b'^' as u16];
            DrawTextW(
                hdc,
                &mut arrow,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }

        // Right zone: clock and notification-panel entry, right-aligned and muted.
        SetTextColor(hdc, COLORREF(theme().muted));
        let now = GetLocalTime();
        let clock = format!(
            "{:02}-{:02}-{:02} {:02}:{:02}",
            now.wYear % 100,
            now.wMonth,
            now.wDay,
            now.wHour,
            now.wMinute
        );
        if vertical() {
            let mut text: Vec<u16> = clock.encode_utf16().collect();
            let mut r = clock_rect(client);
            DrawTextW(
                hdc,
                &mut text,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        } else {
            draw_right(
                hdc,
                &clock,
                client.right - PAD - CLOCK_W,
                client.right - PAD,
            );
        }

        SelectObject(hdc, old_font);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// The clock's horizontal band inside a bar whose client area is `client_w`
/// pixels wide. Kept next to the paint code (`WM_PAINT` uses these same two
/// edges) so the double-click hit test can never drift from what is drawn.
fn clock_span(client_w: i32) -> (i32, i32) {
    (client_w - PAD - CLOCK_W, client_w - PAD)
}

/// Whether client-space `x` falls inside the clock band.
fn clock_hit(client_w: i32, x: i32) -> bool {
    let (left, right) = clock_span(client_w);
    x >= left && x < right
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
const MENU_PIN: usize = 5;

/// Quick-launch pin right-click menu item ids.
const MENU_OPEN_PIN: usize = 6;
const MENU_UNPIN: usize = 7;

/// Group right-click ("batch") menu item ids, kept clear of the single-window
/// ids above so the two menus can never be confused.
const MENU_GROUP_OPEN_ALL: usize = 8;
const MENU_GROUP_RESTORE_ALL: usize = 9;
const MENU_GROUP_MINIMIZE_ALL: usize = 10;
const MENU_GROUP_CLOSE_ALL: usize = 11;
const MENU_GROUP_PIN_ALL: usize = 12;

/// A batch operation applied to every window in a grouped button.
///
/// The `*All` names are part of the frozen menu contract (and read best next to
/// the single-window actions), so the shared postfix is deliberate.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupAction {
    /// Restore and foreground every member (one `activate`).
    OpenAll,
    /// `SW_RESTORE` every member.
    RestoreAll,
    /// `SW_MINIMIZE` every member.
    MinimizeAll,
    /// `WM_CLOSE` every member.
    CloseAll,
    /// Pin the anchor application to quick launch.
    PinAll,
}

/// Maps a menu command id to a [`GroupAction`]. Pure, so the id↔action table is
/// unit-tested and cannot silently drift from what `group_menu` builds.
fn group_menu_action(id: usize) -> Option<GroupAction> {
    Some(match id {
        MENU_GROUP_OPEN_ALL => GroupAction::OpenAll,
        MENU_GROUP_RESTORE_ALL => GroupAction::RestoreAll,
        MENU_GROUP_MINIMIZE_ALL => GroupAction::MinimizeAll,
        MENU_GROUP_CLOSE_ALL => GroupAction::CloseAll,
        MENU_GROUP_PIN_ALL => GroupAction::PinAll,
        _ => return None,
    })
}

/// Longest label stored for a pin, so a stray title cannot bloat the file.
const PIN_LABEL_MAX: usize = 60;

/// Label stored with a pin: the window title if it has one, else the
/// executable's file stem.
fn pin_label(title: &str, path: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.chars().take(PIN_LABEL_MAX).collect();
    }
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .to_owned()
}

/// Shows the right-click window menu for `target` (labelled `title`) and
/// applies the choice. The last entry pins the owning application to the
/// quick-launch strip, or unpins it when already present.
fn window_menu(owner: HWND, target: HWND, title: &str) {
    // Resolve the owning image path before building the menu: this is a
    // cross-process query and wants no locks held.
    // SAFETY: `target` is a live task window from our list.
    let exe = unsafe { crate::win_util::window_exe_path(target) };
    let pinned = exe.as_deref().is_some_and(crate::quick_launch::is_pinned);
    // Nothing to pin without a path, and a full strip cannot take more.
    let pin_disabled = exe.is_none() || (!pinned && crate::quick_launch::is_full());
    tracing::debug!(exe = ?exe, pinned, pin_disabled, "window menu: pin entry state");

    // SAFETY: `owner` and `target` are live windows owned by this process; the
    // menu's whole lifetime lives inside `popup_menu`.
    let picked = unsafe {
        let (x, y) = crate::win_util::cursor_pos_or(None);
        crate::win_util::popup_menu(
            owner,
            &[
                crate::win_util::MenuRow::command(MENU_RESTORE, crate::i18n::text("还原")),
                crate::win_util::MenuRow::command(MENU_MINIMIZE, crate::i18n::text("最小化")),
                crate::win_util::MenuRow::command(MENU_MAXIMIZE, crate::i18n::text("最大化")),
                crate::win_util::MenuRow::command(MENU_CLOSE, crate::i18n::text("关闭")),
                crate::win_util::MenuRow::Separator,
                if pin_disabled {
                    crate::win_util::MenuRow::disabled(
                        MENU_PIN,
                        if pinned {
                            crate::i18n::text("从快速启动移除")
                        } else {
                            crate::i18n::text("添加到快速启动")
                        },
                    )
                } else {
                    crate::win_util::MenuRow::command(
                        MENU_PIN,
                        if pinned {
                            crate::i18n::text("从快速启动移除")
                        } else {
                            crate::i18n::text("添加到快速启动")
                        },
                    )
                },
            ],
            x,
            y,
        )
    };

    // SAFETY: `target` is a live window owned by this process.
    unsafe {
        match picked {
            Some(MENU_RESTORE) => {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            Some(MENU_MINIMIZE) => {
                let _ = ShowWindow(target, SW_MINIMIZE);
            }
            Some(MENU_MAXIMIZE) => {
                let _ = ShowWindow(target, SW_MAXIMIZE);
            }
            Some(MENU_CLOSE) => {
                let _ = PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            Some(MENU_PIN) => {
                if let Some(path) = exe {
                    let changed = if pinned {
                        crate::quick_launch::remove(&path)
                    } else {
                        crate::quick_launch::add(&pin_label(title, &path), &path)
                    };
                    if changed {
                        // Rebuild the strip (its width feeds the task layout).
                        refresh();
                    }
                }
            }
            _ => {}
        }
    }
}

/// Shows the right-click "batch" menu for a grouped button and applies the
/// choice to every member. `members` is a clone taken before any bar/task lock
/// was released; `anchor` names the group for labelling and pinning.
fn group_menu(owner: HWND, anchor: HWND, label: &str, members: &[isize]) {
    // Resolve the anchor's image path before building the menu: a cross-process
    // query wants no locks held.
    // SAFETY: `anchor` is a live task window from our list.
    let exe = unsafe { crate::win_util::window_exe_path(anchor) };
    let pinned = exe.as_deref().is_some_and(crate::quick_launch::is_pinned);
    let pin_disabled = exe.is_none() || (!pinned && crate::quick_launch::is_full());

    // SAFETY: `owner` is a live bar window owned by this process; the menu's
    // whole lifetime lives inside `popup_menu`.
    let picked = unsafe {
        let (x, y) = crate::win_util::cursor_pos_or(None);
        crate::win_util::popup_menu(
            owner,
            &[
                crate::win_util::MenuRow::command(
                    MENU_GROUP_OPEN_ALL,
                    crate::i18n::text("打开组内全部窗口"),
                ),
                crate::win_util::MenuRow::command(
                    MENU_GROUP_RESTORE_ALL,
                    crate::i18n::text("全部还原"),
                ),
                crate::win_util::MenuRow::command(
                    MENU_GROUP_MINIMIZE_ALL,
                    crate::i18n::text("全部最小化"),
                ),
                crate::win_util::MenuRow::command(
                    MENU_GROUP_CLOSE_ALL,
                    crate::i18n::text("全部关闭"),
                ),
                crate::win_util::MenuRow::Separator,
                if pin_disabled {
                    crate::win_util::MenuRow::disabled(
                        MENU_GROUP_PIN_ALL,
                        crate::i18n::text("添加到快速启动"),
                    )
                } else {
                    crate::win_util::MenuRow::command(
                        MENU_GROUP_PIN_ALL,
                        crate::i18n::text("添加到快速启动"),
                    )
                },
            ],
            x,
            y,
        )
    };

    // SAFETY: every member handle was validated with `IsWindow` before use.
    unsafe {
        let Some(action) = picked.and_then(group_menu_action) else {
            return;
        };
        tracing::info!(?action, members = members.len(), "group menu action");
        match action {
            GroupAction::OpenAll => {
                // Restore every member, then foreground the last one so the
                // whole group comes forward without flashing a window per step.
                let mut last = 0;
                for &m in members {
                    let h = hwnd_from(m);
                    if IsWindow(Some(h)).as_bool() {
                        let _ = ShowWindow(h, SW_RESTORE);
                        last = m;
                    }
                }
                if last != 0 {
                    activate(hwnd_from(last));
                }
            }
            GroupAction::RestoreAll => {
                for &m in members {
                    let h = hwnd_from(m);
                    if IsWindow(Some(h)).as_bool() {
                        let _ = ShowWindow(h, SW_RESTORE);
                    }
                }
            }
            GroupAction::MinimizeAll => {
                for &m in members {
                    let h = hwnd_from(m);
                    if IsWindow(Some(h)).as_bool() {
                        let _ = ShowWindow(h, SW_MINIMIZE);
                    }
                }
            }
            GroupAction::CloseAll => {
                for &m in members {
                    let h = hwnd_from(m);
                    if IsWindow(Some(h)).as_bool() {
                        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            GroupAction::PinAll => match exe {
                // Silently fails (and logs) when the strip is full or already
                // holds this application.
                Some(path) if !crate::quick_launch::add(label, &path) => {
                    tracing::debug!(path, "group menu: pin skipped (duplicate or full)")
                }
                Some(_) => refresh(),
                None => tracing::debug!("group menu: pin skipped (no exe path)"),
            },
        }
    }
}

/// Shows the right-click menu for the quick-launch pin at `index`: open it, or
/// drop it from the strip.
fn pin_menu(owner: HWND, index: usize) {
    let pin = lock_or_recover(pins())
        .get(index)
        .map(|p| (p.label.clone(), p.path.clone()));
    let Some((label, path)) = pin else { return };

    // SAFETY: `owner` is a live bar window owned by this process; the menu's
    // whole lifetime lives inside `popup_menu`.
    let picked = unsafe {
        let (x, y) = crate::win_util::cursor_pos_or(None);
        crate::win_util::popup_menu(
            owner,
            &[
                crate::win_util::MenuRow::command(
                    MENU_OPEN_PIN,
                    crate::ui_format!("打开 {label}", "Open {label}"),
                ),
                crate::win_util::MenuRow::Separator,
                crate::win_util::MenuRow::command(MENU_UNPIN, crate::i18n::text("从快速启动移除")),
            ],
            x,
            y,
        )
    };

    match picked {
        Some(MENU_OPEN_PIN) => crate::quick_launch::launch_path(&path),
        Some(MENU_UNPIN) if crate::quick_launch::remove(&path) => refresh(),
        _ => {}
    }
}

/// Taskbar button left-click semantics, matching the stock shell's toggle:
/// a window that already owns the foreground is minimized; anything else is
/// brought to the foreground (restoring it first if it was minimized). The
/// click itself never activates our bar (`WS_EX_NOACTIVATE`), so
/// `GetForegroundWindow` still reports the window the user was looking at —
/// exactly the state we need to decide the direction of the toggle.
fn toggle_or_activate(target: HWND) {
    // SAFETY: `target` is a live top-level window from our task list. Guard
    // against a stale handle (window closed and the value reused) so we never
    // minimize or foreground the wrong window.
    unsafe {
        if !IsWindow(Some(target)).as_bool() {
            tracing::debug!(?target, "click: target window no longer exists");
            return;
        }
        // A minimized window is never the foreground window, so the `IsIconic`
        // guard is belt-and-braces: it keeps us from minimizing an already
        // minimized window if the two ever disagree.
        let fg = GetForegroundWindow();
        tracing::debug!(
            ?target,
            ?fg,
            is_fg = fg == target,
            iconic = IsIconic(target).as_bool(),
            "task button click"
        );
        if fg == target && !IsIconic(target).as_bool() {
            tracing::debug!(?target, "click: minimizing active window (toggle)");
            if !windows::Win32::UI::WindowsAndMessaging::ShowWindowAsync(target, SW_MINIMIZE)
                .as_bool()
            {
                let _ = PostMessageW(
                    Some(target),
                    windows::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND,
                    WPARAM(windows::Win32::UI::WindowsAndMessaging::SC_MINIMIZE as usize),
                    LPARAM(0),
                );
            }
        } else {
            crate::win_util::force_foreground(target);
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

/// Current title of a live top-level window (empty when unavailable). Used to
/// build the member list of a grouped button.
fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` outlives the call; `hwnd` comes from our task list.
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
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

/// `VK_1..VK_9`, in order, so `position` yields the intended button index
/// (digit `1` → index `0`).
const DIGIT_VKS: [VIRTUAL_KEY; 9] = [VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9];

/// Keyboard `1..9` jump: maps a virtual key to the first nine buttons. `None`
/// for any other key, and for a digit with no visible button behind it.
fn digit_index(vk: u16, visible: usize) -> Option<usize> {
    let i = DIGIT_VKS.iter().position(|k| k.0 == vk)?;
    (i < visible).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::{
        Density, GroupAction, after_settings_for, budget, choose_anchor, density_for, digit_index,
        fold_needed, group_label, group_menu_action, hide_pins, layout, overflows, stable_order,
        step_index,
    };

    /// The settings button must never touch whatever follows it: with the
    /// start-menu slot off it still keeps the standard inter-button gap, and
    /// with the slot on that width is used verbatim.
    #[test]
    fn vertical_tasks_keep_titles_and_never_overlap_footer_even_when_full() {
        for height in [0, 35, 36, 71, 72, 350, 1080] {
            for count in [0, 1, 2, 25, 10000] {
                let start = 44;
                let end = (height - 72).max(start);
                let result = super::vertical_layout(180, start, end, count);
                assert_eq!(result.density, Density::Full);
                assert_eq!(result.hidden_from, result.rects.len());
                let mut previous = start;
                for r in result.rects.iter().chain(result.overflow.iter()) {
                    assert!(r.top >= previous && r.bottom <= end && r.right <= 180 && r.left >= 0);
                    previous = r.bottom;
                }
            }
        }
    }
    #[test]
    fn after_settings_always_keeps_a_gap() {
        assert_eq!(
            after_settings_for(0),
            super::PAD + super::SETTINGS_W + super::BTN_GAP,
            "with no start-menu slot the next button keeps a full gap"
        );
        assert_eq!(
            after_settings_for(super::GRIP),
            super::PAD + super::SETTINGS_W + super::GRIP,
            "an enabled start-menu slot is used as-is"
        );
    }

    /// Minimal task item carrying just the fields `stable_order` looks at.
    fn item(key: &str) -> super::TaskItem {
        super::TaskItem {
            key: key.to_string(),
            hwnd: 0,
            members: Vec::new(),
            label: key.to_string(),
            icon: 0,
        }
    }

    fn keys(items: &[super::TaskItem]) -> Vec<String> {
        items.iter().map(|t| t.key.clone()).collect()
    }

    #[test]
    fn stable_order_keeps_positions_when_z_order_flips() {
        // `c` jumps to the front in Z-order (it was activated); the bar must
        // ignore that and keep a, b, c where they were.
        let prev = vec![item("a"), item("b"), item("c")];
        let current = vec![item("c"), item("a"), item("b")];
        assert_eq!(keys(&stable_order(&prev, current)), ["a", "b", "c"]);
    }

    #[test]
    fn stable_order_appends_new_windows_at_the_end() {
        let prev = vec![item("a"), item("c")];
        let current = vec![item("x"), item("c"), item("y"), item("a")];
        // a, c keep their slots; x, y are appended in enumeration order.
        assert_eq!(keys(&stable_order(&prev, current)), ["a", "c", "x", "y"]);
    }

    #[test]
    fn stable_order_drops_closed_windows_without_moving_the_rest() {
        let prev = vec![item("a"), item("b"), item("c")];
        let current = vec![item("c"), item("a")];
        assert_eq!(keys(&stable_order(&prev, current)), ["a", "c"]);
    }

    #[test]
    fn first_refresh_keeps_enumeration_order() {
        let out = stable_order(&[], vec![item("a"), item("b")]);
        assert_eq!(keys(&out), ["a", "b"]);
    }

    #[test]
    fn anchor_sticks_to_the_first_window_seen() {
        // No previous anchor: the first member wins.
        assert_eq!(choose_anchor(None, &[10, 20, 30]), 10);
        // The stored anchor is kept while it is still a member — so activating
        // another window in the group cannot rename the button.
        assert_eq!(choose_anchor(Some(20), &[10, 20, 30]), 20);
    }

    #[test]
    fn anchor_advances_when_the_anchor_window_closes() {
        // The old anchor (20) is gone; the next surviving member takes over.
        assert_eq!(choose_anchor(Some(20), &[10, 30]), 10);
        // A group with no members falls back to 0 (never drawn).
        assert_eq!(choose_anchor(Some(20), &[]), 0);
    }

    #[test]
    fn group_label_carries_the_extra_count_only_when_merged() {
        assert_eq!(group_label("Editor", 1), "Editor");
        assert_eq!(group_label("Editor", 3), "Editor (+2)");
        // Defensive: an empty group never gains a suffix.
        assert_eq!(group_label("Editor", 0), "Editor");
    }

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
    fn tray_strip_reduces_visible_buttons() {
        // Narrow bar + 4 tasks: reserving tray space must reduce how many
        // buttons stay visible (the rest go behind the overflow button).
        let without = layout(200, 560, 4, 0, Density::Full);
        let with = layout(200, 560, 4, 80, Density::Full);
        assert!(with.overflow.is_some());
        assert!(
            with.rects.len() < without.rects.len(),
            "reserving tray space must reduce visible buttons"
        );
    }

    #[test]
    fn icon_only_tier_shows_squares_when_everything_fits() {
        // A budget in [40, 80) lands in icon-only: the title is dropped but all
        // buttons still fit, drawn as 40–48px squares.
        let bl = layout(200, 560, 4, 0, Density::Full);
        assert_eq!(bl.density, Density::IconOnly);
        assert!(bl.overflow.is_none(), "all four should fit as squares");
        assert_eq!(bl.rects.len(), 4);
        for r in &bl.rects {
            assert!(
                (45..=48).contains(&(r.right - r.left)),
                "icon-only squares clamp to [40,48]"
            );
        }
    }

    #[test]
    fn many_windows_fall_back_to_overflow_and_keep_the_right_zone() {
        // A narrow bar with many tasks: the densest tier still cannot hold them,
        // so an overflow button appears and nothing reaches the right (clock)
        // zone.
        let n = 20;
        let bl = layout(200, 900, n, 0, Density::Full);
        assert!(bl.overflow.is_some(), "expected an overflow button");
        assert!(bl.rects.len() < n, "some tasks must be hidden");
        assert_eq!(bl.hidden_from, bl.rects.len());
        assert!(
            !bl.rects.is_empty(),
            "at least one button must stay visible"
        );
        // The overflow button's right edge must stay left of the clock zone.
        let right_limit = 900 - super::PAD - super::RIGHT_W;
        let last_visible = bl.rects.last().map(|r| r.right).unwrap_or(0);
        let ov = bl.overflow.unwrap();
        assert!(last_visible <= right_limit, "buttons must not cross PAD");
        assert!(ov.right <= right_limit, "overflow must not cover the clock");
    }

    #[test]
    fn few_windows_fit_without_overflow() {
        let bl = layout(200, 1600, 3, 0, Density::Full);
        assert!(bl.overflow.is_none());
        assert_eq!(bl.rects.len(), 3);
        assert_eq!(bl.hidden_from, 3);
        assert_eq!(bl.density, Density::Full);
    }

    #[test]
    fn budget_divides_after_deducting_gaps() {
        // 4 buttons, 3 gaps of 4px: 300 - 12 = 288, /4 = 72.
        assert_eq!(budget(300, 4, 4), 72);
        // Empty list is unbounded.
        assert_eq!(budget(300, 0, 4), i32::MAX);
    }

    #[test]
    fn density_hysteresis_has_a_dead_zone() {
        // Leaving Full needs budget < 112; returning to Full needs budget >= 120.
        assert_eq!(density_for(112, Density::Full), Density::Full);
        assert_eq!(density_for(111, Density::Full), Density::Compact);
        assert_eq!(density_for(119, Density::Compact), Density::Compact);
        assert_eq!(density_for(120, Density::Compact), Density::Full);
        // A single window added/removed (small budget swing) cannot flip tiers.
        assert_eq!(density_for(115, Density::Full), Density::Full);
        assert_eq!(density_for(115, Density::Compact), Density::Compact);

        // Icon-only boundaries: enter below 80, leave at 88 (an 8px dead zone).
        assert_eq!(density_for(80, Density::Full), Density::Compact);
        assert_eq!(density_for(79, Density::Full), Density::IconOnly);
        assert_eq!(density_for(80, Density::Compact), Density::Compact);
        assert_eq!(density_for(79, Density::Compact), Density::IconOnly);
        assert_eq!(density_for(87, Density::IconOnly), Density::IconOnly);
        assert_eq!(density_for(88, Density::IconOnly), Density::Compact);
        assert_eq!(density_for(119, Density::IconOnly), Density::Compact);
        assert_eq!(density_for(120, Density::IconOnly), Density::Full);
    }

    #[test]
    fn density_for_is_idempotent() {
        for b in [0, 40, 79, 80, 87, 88, 111, 112, 119, 120, 200, 400] {
            for prev in [Density::Full, Density::Compact, Density::IconOnly] {
                let once = density_for(b, prev);
                assert_eq!(density_for(b, once), once, "not idempotent at {b}");
            }
        }
    }

    #[test]
    fn density_never_thins_as_budget_grows() {
        // More room must never produce a denser (narrower) tier than the tier at
        // the previous (smaller) budget.
        let order = |d: Density| match d {
            Density::Full => 0,
            Density::Compact => 1,
            Density::IconOnly => 2,
        };
        let mut prev = density_for(0, Density::Full);
        for b in (7..400).step_by(7) {
            let d = density_for(b, prev);
            assert!(
                order(d) <= order(prev),
                "tier got denser as budget grew at {b}"
            );
            prev = d;
        }
    }

    #[test]
    fn compact_tier_fits_more_than_the_old_fixed_minimum() {
        // 6 tasks with the default start: 6*120 would not fit, but the compact
        // tier (>=80px) does — so all six stay visible instead of spilling.
        let bl = layout(200, 1000, 6, 0, Density::Full);
        let avail = 1000 - 200 - super::PAD - super::RIGHT_W;
        assert!(6 * super::D0_ENTRY > avail, "test needs a tight bar");
        assert_eq!(bl.rects.len(), 6);
        assert!(bl.overflow.is_none());
        assert_eq!(bl.density, Density::Compact);
    }

    #[test]
    fn clock_band_is_flush_to_the_right_pad() {
        let client_w = 1680;
        let (left, right) = super::clock_span(client_w);
        assert_eq!(right, client_w - super::PAD);
        assert_eq!(left, client_w - super::PAD - super::CLOCK_W);
        // Inside: the first and last pixels the clock is drawn over.
        assert!(super::clock_hit(client_w, left));
        assert!(super::clock_hit(client_w, right - 1));
        // Outside on both sides.
        assert!(!super::clock_hit(client_w, left - 1));
        assert!(!super::clock_hit(client_w, right));
    }

    #[test]
    fn clock_band_does_not_swallow_the_notification_strip() {
        // The tray strip ends at `client.right - PAD - CLOCK_W - PAD`; its last
        // pixel must stay outside the clock band so the two never overlap.
        let client_w = 1600;
        let tray_last = client_w - super::PAD - super::CLOCK_W - super::PAD;
        assert!(!super::clock_hit(client_w, tray_last));
    }

    #[test]
    fn tray_layout_is_derived_from_the_client_rect_only() {
        // The painter and every mouse handler call `tray_layout` with the same
        // client rect, so its geometry — and therefore the hit test — must be a
        // pure function of that rect. This pins the numbers both sides use.
        let client = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 1600,
            bottom: super::BAR_H,
        };
        let layout = super::tray_layout(client);
        assert_eq!(
            layout.right_x,
            client.right - super::PAD - super::CLOCK_W - super::PAD
        );
        assert_eq!(
            layout.strip_width(3),
            crate::tray_bar::strip_width(3, crate::tray_bar::CELL, crate::tray_bar::GAP)
        );
        // A cell that exists is inside the bar's vertical band.
        let cell = layout.cell_rect(0, 3).expect("one cell");
        assert!(cell.top >= 0 && cell.bottom <= client.bottom);
    }

    #[test]
    fn an_empty_strip_reserves_no_space() {
        // `refresh` derives the reserved width from the icon count; nothing must
        // be reserved (and no cell hit) when the strip is empty.
        let layout = super::tray_layout(windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 1600,
            bottom: super::BAR_H,
        });
        assert_eq!(
            crate::tray_bar::strip_width(0, crate::tray_bar::CELL, crate::tray_bar::GAP),
            0
        );
        assert_eq!(layout.hit_test(0, layout.right_x - 1, 18), None);
    }

    // --- density / fold pure helpers ---------------------------------------

    #[test]
    fn overflows_only_below_the_icon_square_floor() {
        // n=4, gaps 3*4=12: avail 200 → budget 47 (fits); avail 160 → 37 (spills).
        assert!(!overflows(200, 4, 4));
        assert!(overflows(160, 4, 4));
        // An empty list never overflows.
        assert!(!overflows(0, 0, 4));
    }

    #[test]
    fn fold_needed_matches_the_compact_floor() {
        // budget >= 80 → compact or full, no fold.
        assert!(!fold_needed(80 * 4 + 12, 4, 4));
        // budget == 79 → icon-only tier, fold.
        assert!(fold_needed(79 * 4 + 12, 4, 4));
        // Empty list: nothing to fold.
        assert!(!fold_needed(0, 0, 4));
    }

    #[test]
    fn hide_pins_only_when_they_are_the_sole_cause_of_overflow() {
        // With pins it spills but without them it fits → fold the pins.
        assert!(hide_pins(160, 200, 4, 4));
        // Both fit → nothing to fold.
        assert!(!hide_pins(200, 220, 4, 4));
        // Still spills without pins → folding buys nothing.
        assert!(!hide_pins(120, 160, 4, 4));
    }

    #[test]
    fn icon_only_overflow_uses_40px_squares() {
        // 20 windows on a 900px bar: the densest icons still spill, so the
        // visible squares must be exactly 40px (never narrower) plus one `»`.
        let bl = layout(200, 900, 20, 0, Density::Full);
        assert!(bl.overflow.is_some());
        assert_eq!(bl.density, Density::IconOnly);
        assert!(!bl.rects.is_empty());
        for r in &bl.rects {
            assert_eq!(r.right - r.left, 40);
        }
    }

    // --- keyboard digit jump -----------------------------------------------

    #[test]
    fn digit_index_maps_1_to_9_in_order() {
        assert_eq!(digit_index(super::VK_1.0, 5), Some(0));
        assert_eq!(digit_index(super::VK_5.0, 9), Some(4));
        assert_eq!(digit_index(super::VK_9.0, 9), Some(8));
    }

    #[test]
    fn digit_index_ignores_out_of_range_and_other_keys() {
        // Past the visible count is ignored.
        assert_eq!(digit_index(super::VK_9.0, 3), None);
        assert_eq!(digit_index(super::VK_1.0, 0), None);
        // A non-digit key is never a jump.
        assert_eq!(digit_index(super::VK_LEFT.0, 9), None);
    }

    // --- group menu --------------------------------------------------------

    #[test]
    fn group_menu_action_maps_every_id_and_rejects_others() {
        assert_eq!(
            group_menu_action(super::MENU_GROUP_OPEN_ALL),
            Some(GroupAction::OpenAll)
        );
        assert_eq!(
            group_menu_action(super::MENU_GROUP_RESTORE_ALL),
            Some(GroupAction::RestoreAll)
        );
        assert_eq!(
            group_menu_action(super::MENU_GROUP_MINIMIZE_ALL),
            Some(GroupAction::MinimizeAll)
        );
        assert_eq!(
            group_menu_action(super::MENU_GROUP_CLOSE_ALL),
            Some(GroupAction::CloseAll)
        );
        assert_eq!(
            group_menu_action(super::MENU_GROUP_PIN_ALL),
            Some(GroupAction::PinAll)
        );
        // Single-window ids and 0 (menu dismissed) must not alias a group action.
        assert_eq!(group_menu_action(super::MENU_RESTORE), None);
        assert_eq!(group_menu_action(0), None);
        assert_eq!(group_menu_action(999), None);
    }
}
