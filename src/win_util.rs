//! Small shared helpers for Win32 interop.
//!
//! This module is the crate's single home for the things that used to be
//! re-derived in every window module: string marshalling, foreground forcing,
//! rectangle hit tests, popup menus, popup placement, GDI brush lifetime, and
//! the two panic guards. If a second copy of any of these appears somewhere
//! else, that is a bug in waiting — see `docs/plan_0_1_3.md` §P1-1.

use std::sync::{Mutex, MutexGuard};

use windows::Win32::Foundation::{COLORREF, CloseHandle, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH, HGDIOBJ};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetForegroundWindow,
    GetWindowThreadProcessId, HWND_TOP, IsIconic, MF_GRAYED, MF_MENUBARBREAK, MF_SEPARATOR,
    MF_STRING, PostMessageW, SC_RESTORE, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow, ShowWindowAsync,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TRACK_POPUP_MENU_FLAGS, TrackPopupMenu,
    WM_SYSCOMMAND,
};
use windows::core::{PCWSTR, PWSTR};

/// `ERROR_CLASS_ALREADY_EXISTS`: `RegisterClassW` failing with this is the
/// normal "another instance already registered the class" case, not a real
/// error. Every window module used to define its own copy of the literal.
pub(crate) const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

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
pub(crate) fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
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
pub(crate) fn guard_abi<R>(
    what: &str,
    body: impl FnOnce() -> R,
    fallback: impl FnOnce() -> R,
) -> R {
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
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Half-open containment test: `right`/`bottom` are exclusive edges.
///
/// Six modules hand-rolled this predicate; keeping one copy means a hit test can
/// never drift from the rectangle it is testing.
pub(crate) const fn rect_contains(r: RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Horizontal centre of a `win`-wide window on a `screen`-wide monitor,
/// clamped to 0 so an oversized window cannot be placed off the left edge.
const fn centered_x(screen: i32, win: i32) -> i32 {
    let x = (screen - win) / 2;
    if x < 0 { 0 } else { x }
}

/// Screen-space origin that centres a `win_w`×`win_h` window on the monitor.
///
/// Shared by the settings window and any future centred dialog: hand-rolling
/// this per module is how two popups end up at slightly different places.
pub(crate) const fn centered_origin(
    screen_w: i32,
    screen_h: i32,
    win_w: i32,
    win_h: i32,
) -> (i32, i32) {
    (centered_x(screen_w, win_w), centered_x(screen_h, win_h))
}

/// Screen-space origin that centres a `win_w`-wide popup horizontally and puts
/// it in the **upper quarter** of the monitor.
///
/// The launcher and the Run dialog use this deliberately: sitting high leaves
/// the result list room to grow downwards.
pub(crate) const fn centered_upper_quarter(screen_w: i32, screen_h: i32, win_w: i32) -> (i32, i32) {
    let y = screen_h / 4;
    (centered_x(screen_w, win_w), if y < 0 { 0 } else { y })
}

/// Places a `w`×`h` card near `anchor` (a screen rect) inside `work`.
///
/// Centred on the anchor horizontally, preferring to sit `gap` px **above** it
/// and flipping **below** when there is no room above, then clamped inside the
/// work area (right/bottom edges first, so the card keeps its size whenever the
/// work area is bigger than it). Pure, so both callers are unit-testable —
/// this replaced two byte-identical copies (hover preview + bar tip).
pub(crate) fn anchored_card(anchor: RECT, work: RECT, w: i32, h: i32, gap: i32) -> RECT {
    let center_x = (anchor.left + anchor.right) / 2;
    let mut left = center_x - w / 2;

    let above = anchor.top - h - gap;
    let mut top = if above >= work.top {
        above
    } else {
        anchor.bottom + gap
    };

    if anchor.right <= work.left {
        left = anchor.right + gap;
        top = anchor.top;
    } else if anchor.left >= work.right {
        left = anchor.left - w - gap;
        top = anchor.top;
    }
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

/// One row of a popup menu built by [`popup_menu`].
///
/// Labels are `String` rather than `&str` because several call sites format
/// them (`format!("打开 {label}")`); the helper widens them just before use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuRow {
    /// A horizontal separator line.
    Separator,
    /// A selectable command carrying `id`.
    Command {
        id: usize,
        label: String,
        /// `false` greys the row out: drawn, but never selectable.
        enabled: bool,
        /// Start a new menu column before this row (long lists).
        starts_column: bool,
    },
}

impl MenuRow {
    /// A selectable command.
    pub(crate) fn command(id: usize, label: impl Into<String>) -> Self {
        MenuRow::Command {
            id,
            label: label.into(),
            enabled: true,
            starts_column: false,
        }
    }

    /// A greyed-out command (shown, not selectable).
    pub(crate) fn disabled(id: usize, label: impl Into<String>) -> Self {
        MenuRow::Command {
            id,
            label: label.into(),
            enabled: false,
            starts_column: false,
        }
    }

    /// A selectable command that begins a new column.
    pub(crate) fn first_in_column(id: usize, label: impl Into<String>) -> Self {
        MenuRow::Command {
            id,
            label: label.into(),
            enabled: true,
            starts_column: true,
        }
    }
}

/// Flags every popup in this crate uses: return the command id instead of
/// dispatching, right-button-selectable, left-aligned (`TPM_LEFTALIGN` is 0,
/// i.e. the default — spelled out so the intent is visible).
const POPUP_FLAGS: u32 = TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0 | TPM_LEFTALIGN.0;

/// Builds a popup menu from `rows`, shows it at `(x, y)`, and returns the
/// chosen command id (`None` when dismissed).
///
/// The whole lifecycle lives here — `CreatePopupMenu` → `AppendMenuW` →
/// `SetForegroundWindow` → `TrackPopupMenu` → `DestroyMenu` — because five
/// hand-rolled copies had five chances to leak the menu or forget the
/// foreground call that makes a popup dismiss when the user clicks away.
///
/// # Safety
/// `owner` must be a live window owned by this process.
pub(crate) unsafe fn popup_menu(owner: HWND, rows: &[MenuRow], x: i32, y: i32) -> Option<usize> {
    // SAFETY: the menu is created and destroyed entirely within this call; each
    // `wide` buffer outlives the `AppendMenuW` that copies it; `owner` is the
    // caller's live window.
    unsafe {
        let menu = match CreatePopupMenu() {
            Ok(menu) => menu,
            Err(err) => {
                tracing::warn!(%err, "popup_menu: CreatePopupMenu failed");
                return None;
            }
        };

        for row in rows {
            match row {
                MenuRow::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                MenuRow::Command {
                    id,
                    label,
                    enabled,
                    starts_column,
                } => {
                    let mut flags = MF_STRING;
                    if !enabled {
                        flags |= MF_GRAYED;
                    }
                    if *starts_column {
                        flags |= MF_MENUBARBREAK;
                    }
                    let text = wide(label);
                    let _ = AppendMenuW(menu, flags, *id, PCWSTR(text.as_ptr()));
                }
            }
        }

        // Required so the popup dismisses correctly when clicking away.
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(
            menu,
            TRACK_POPUP_MENU_FLAGS(POPUP_FLAGS),
            x,
            y,
            Some(0),
            owner,
            None,
        );
        let _ = DestroyMenu(menu);

        // With `TPM_RETURNCMD` the return value is the chosen id, or 0 when the
        // menu was dismissed without a selection.
        let id = (cmd.0 & 0xFFFF) as usize;
        (id != 0).then_some(id)
    }
}

/// Cursor position in screen coordinates, or `fallback`'s bottom-left corner
/// when the cursor cannot be read (and `(0, 0)` when there is no fallback).
///
/// # Safety
/// Pure `GetCursorPos` query.
pub(crate) unsafe fn cursor_pos_or(fallback: Option<RECT>) -> (i32, i32) {
    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid out-pointer for the duration of the call.
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        return (pt.x, pt.y);
    }
    match fallback {
        Some(r) => (r.left, r.bottom),
        None => (0, 0),
    }
}

/// A GDI brush that deletes itself.
///
/// Seven paint paths used to pair `CreateSolidBrush` with a later
/// `DeleteObject` by hand, so any early return leaked a GDI handle — and GDI
/// handles are a per-process, hard-capped resource. Holding the brush in this
/// type makes the pair automatic.
///
/// **Not** for long-lived brushes: the taskbar's per-theme brushes and the
/// settings window class brush are created once and must outlive the call, so
/// they stay raw handles on purpose.
pub(crate) struct Brush(HBRUSH);

impl Brush {
    /// Creates a solid brush, or `None` if GDI refuses.
    ///
    /// # Safety
    /// The returned brush is owned by the caller and freed on drop.
    pub(crate) unsafe fn solid(color: COLORREF) -> Option<Self> {
        // SAFETY: `CreateSolidBrush` takes a plain colour value.
        let brush = unsafe { CreateSolidBrush(color) };
        (!brush.0.is_null()).then_some(Brush(brush))
    }

    /// The raw handle, for `FillRect` / `FrameRect` / DC selection.
    pub(crate) const fn handle(&self) -> HBRUSH {
        self.0
    }
}

impl Drop for Brush {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateSolidBrush`, is never shared, and is
        // not selected into any DC at this point (callers only pass it to
        // `FillRect`/`FrameRect`, which do not keep it).
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
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
/// Returns whether foreground activation was accepted. Restoring a foreign
/// window is asynchronous; callers verifying visibility must also check IsIconic.
///
/// # Safety
/// `hwnd` must be a valid window handle; call on the message thread.
pub(crate) unsafe fn force_foreground(hwnd: HWND) -> bool {
    // SAFETY: pure window/thread queries plus the documented foreground
    // workarounds; all handles come from Win32.
    unsafe {
        if IsIconic(hwnd).as_bool() && !ShowWindowAsync(hwnd, SW_RESTORE).as_bool() {
            // A medium-integrity caller cannot directly change Task Manager's
            // show state. Let the target process its standard system command.
            // SetForegroundWindow alone can report success while it stays iconic.
            if let Err(error) = PostMessageW(
                Some(hwnd),
                WM_SYSCOMMAND,
                WPARAM(SC_RESTORE as usize),
                LPARAM(0),
            ) {
                tracing::warn!(%error, ?hwnd, "window restore request was refused");
            }
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
pub(crate) unsafe fn window_exe_path(hwnd: HWND) -> Option<String> {
    // SAFETY: all handles are released before returning; the buffer outlives
    // the calls that write into it.
    unsafe {
        let pid = window_pid(hwnd);
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

/// Process id owning `hwnd`, or 0 when it cannot be resolved.
///
/// # Safety
/// `hwnd` must be a valid window handle.
pub(crate) unsafe fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer for the duration of the call.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
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

    // --- geometry --------------------------------------------------------

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = rect(10, 20, 30, 40);
        assert!(rect_contains(r, 10, 20), "top-left corner is inside");
        assert!(rect_contains(r, 29, 39), "just inside the far corner");
        assert!(!rect_contains(r, 30, 39), "right edge is exclusive");
        assert!(!rect_contains(r, 29, 40), "bottom edge is exclusive");
        assert!(!rect_contains(r, 9, 20));
        assert!(!rect_contains(r, 10, 19));
    }

    #[test]
    fn centered_origin_centres_on_both_axes() {
        assert_eq!(centered_origin(1920, 1080, 560, 400), (680, 340));
    }

    #[test]
    fn centered_origin_clamps_an_oversized_window_to_zero() {
        assert_eq!(centered_origin(400, 300, 560, 400).0, 0);
        assert_eq!(centered_origin(400, 300, 560, 400).1, 0);
    }

    #[test]
    fn centered_upper_quarter_sits_in_the_upper_quarter() {
        assert_eq!(centered_upper_quarter(1920, 1080, 560), (680, 270));
    }

    #[test]
    fn centered_upper_quarter_never_goes_negative() {
        assert_eq!(centered_upper_quarter(1920, 0, 560).1, 0);
    }

    #[test]
    fn anchored_cards_open_into_work_area_from_both_vertical_edges() {
        let work = RECT {
            left: 180,
            top: 0,
            right: 1500,
            bottom: 1000,
        };
        let left = anchored_card(
            RECT {
                left: 10,
                top: 200,
                right: 170,
                bottom: 230,
            },
            work,
            240,
            160,
            6,
        );
        assert!(left.left >= work.left && left.top == 200);
        let right = anchored_card(
            RECT {
                left: 1510,
                top: 200,
                right: 1670,
                bottom: 230,
            },
            work,
            240,
            160,
            6,
        );
        assert!(right.right <= work.right && right.top == 200);
    }
    #[test]
    fn anchored_card_prefers_above_the_anchor() {
        let anchor = rect(100, 300, 140, 320);
        let work = rect(0, 0, 1920, 1080);
        let card = anchored_card(anchor, work, 200, 100, 6);
        // Centred on the anchor: centre 120, width 200 -> left 20.
        assert_eq!(card.left, 20);
        // Above: 300 - 100 - 6 = 194.
        assert_eq!(card.top, 194);
        assert_eq!((card.right, card.bottom), (220, 294));
    }

    #[test]
    fn anchored_card_flips_below_when_there_is_no_room_above() {
        let anchor = rect(100, 4, 140, 24);
        let work = rect(0, 0, 1920, 1080);
        let card = anchored_card(anchor, work, 200, 100, 6);
        assert_eq!(card.top, 30, "flipped below the anchor");
    }

    #[test]
    fn anchored_card_clamps_inside_the_work_area() {
        let anchor = rect(0, 500, 20, 520);
        let work = rect(0, 0, 100, 100);
        let card = anchored_card(anchor, work, 60, 40, 6);
        assert!(card.left >= work.left && card.right <= work.right);
        assert!(card.top >= work.top && card.bottom <= work.bottom);
    }

    #[test]
    fn anchored_card_keeps_its_size_when_the_work_area_is_larger() {
        let anchor = rect(1000, 500, 1040, 520);
        let work = rect(0, 0, 1920, 1080);
        let card = anchored_card(anchor, work, 200, 100, 6);
        assert_eq!(card.right - card.left, 200);
        assert_eq!(card.bottom - card.top, 100);
    }

    // --- menu rows -------------------------------------------------------

    #[test]
    fn an_empty_rect_contains_nothing() {
        // A degenerate rect must never swallow a click; the bar has several
        // "not laid out yet" rects that would otherwise match (0, 0).
        let empty = rect(0, 0, 0, 0);
        assert!(!rect_contains(empty, 0, 0));
        assert!(!rect_contains(empty, -1, -1));
    }

    #[test]
    fn anchored_card_with_a_zero_gap_sits_flush_above() {
        let anchor = rect(100, 300, 140, 320);
        let work = rect(0, 0, 1920, 1080);
        let card = anchored_card(anchor, work, 200, 100, 0);
        assert_eq!(card.bottom, anchor.top);
    }

    #[test]
    fn menu_row_constructors_set_the_expected_flags() {
        assert_eq!(
            MenuRow::command(3, "open"),
            MenuRow::Command {
                id: 3,
                label: String::from("open"),
                enabled: true,
                starts_column: false,
            }
        );
        assert_eq!(
            MenuRow::disabled(4, "pin"),
            MenuRow::Command {
                id: 4,
                label: String::from("pin"),
                enabled: false,
                starts_column: false,
            }
        );
        assert_eq!(
            MenuRow::first_in_column(5, "x"),
            MenuRow::Command {
                id: 5,
                label: String::from("x"),
                enabled: true,
                starts_column: true,
            }
        );
    }

    #[test]
    fn popup_flags_always_ask_for_the_command_id() {
        // Without `TPM_RETURNCMD` the helper would get "did the user pick
        // anything" instead of the chosen id, and every call site would break.
        assert_ne!(POPUP_FLAGS & TPM_RETURNCMD.0, 0);
        assert_ne!(POPUP_FLAGS & TPM_RIGHTBUTTON.0, 0);
    }

    #[test]
    fn error_class_already_exists_matches_the_win32_value() {
        assert_eq!(ERROR_CLASS_ALREADY_EXISTS, 1410);
    }
}
