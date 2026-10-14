//! Launcher UI: a hotkey-summoned popup with a search field and result list.
//!
//! Self-contained: this module owns its window class, controls, and hotkey
//! (Alt+Space). It reuses `crate::launcher` for discovery/search/launch.
//!
//! Layout: a borderless top-most popup holding an `EDIT` (query) over a
//! `LISTBOX` (ranked results). The `EDIT` is subclassed so `Enter` / `Esc` /
//! `Up` / `Down` are handled without a dialog manager.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, RegisterHotKey, SetFocus, UnregisterHotKey, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SPACE,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, EN_CHANGE, ES_AUTOHSCROLL, GWLP_WNDPROC,
    GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, HMENU, IsChild, LB_ADDSTRING,
    LB_GETCOUNT, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, LBN_DBLCLK, LBS_NOTIFY,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SendMessageW, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE,
    WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN, WM_KILLFOCUS, WNDCLASS_STYLES, WNDCLASSW,
    WNDPROC, WS_BORDER, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

// --- geometry / ids -------------------------------------------------------
const POPUP_W: i32 = 560;
const POPUP_H: i32 = 340;
const MARGIN: i32 = 8;
const EDIT_H: i32 = 28;
const LIST_GAP: i32 = 8;
const ID_EDIT: u16 = 1001;
const ID_LIST: u16 = 1002;
/// Distinct from the taskbar's hotkey id (1).
const HOTKEY_ID: i32 = 2;
/// Maximum ranked results shown / matched.
const MAX_RESULTS: usize = 20;

const CLASS_NAME: &str = "minicon_winmgr_launcher";
const WINDOW_TITLE: &str = "minicon launcher";
/// Light background so the popup is not a black rectangle before controls fill.
const BG_COLOR: COLORREF = COLORREF(0x00FF_FFFF);

// --- process-lifetime UI state --------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static POPUP: AtomicIsize = AtomicIsize::new(0);
static EDIT: AtomicIsize = AtomicIsize::new(0);
static LIST: AtomicIsize = AtomicIsize::new(0);
static ORIG_EDIT_PROC: AtomicIsize = AtomicIsize::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn popup() -> Option<HWND> {
    existing(&POPUP)
}

fn edit() -> Option<HWND> {
    existing(&EDIT)
}

fn list() -> Option<HWND> {
    existing(&LIST)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

/// Lazily discovered Start Menu shortcuts, cached on first search.
fn items() -> &'static Vec<crate::model::Shortcut> {
    static ITEMS: OnceLock<Vec<crate::model::Shortcut>> = OnceLock::new();
    ITEMS.get_or_init(|| {
        let found = crate::launcher::discover_shortcuts();
        tracing::info!(count = found.len(), "launcher indexed Start Menu shortcuts");
        found
    })
}

/// Registers the launcher hotkey and prepares its popup window class.
pub fn init(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if popup().is_some() {
        return Ok(()); // already initialised
    }
    // SAFETY: standard Win32 window setup; every buffer outlives its call and
    // both wndprocs are valid `extern "system"` functions.
    unsafe {
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);
        // A class brush keeps the popup from flashing uninitialised pixels.
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
            // ERROR_CLASS_ALREADY_EXISTS (1410) is fine on a repeated init.
            if code != 1410 {
                return Err(format!("RegisterClassW(launcher) failed (Win32 error {code})").into());
            }
        }

        let (x, y) = crate::win_util::centered_upper_quarter(
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
            POPUP_W,
        );
        // Hidden on creation: no `WS_VISIBLE`.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            x,
            y,
            POPUP_W,
            POPUP_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        POPUP.store(hwnd.0 as isize, Ordering::SeqCst);

        let edit_class = wide("EDIT");
        let list_class = wide("LISTBOX");
        let no_text = wide("");

        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(edit_class.as_ptr()),
            PCWSTR(no_text.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32),
            MARGIN,
            MARGIN,
            POPUP_W - 2 * MARGIN,
            EDIT_H,
            Some(hwnd),
            Some(HMENU(ID_EDIT as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )?;
        EDIT.store(edit.0 as isize, Ordering::SeqCst);

        let list_top = MARGIN + EDIT_H + LIST_GAP;
        let list = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(list_class.as_ptr()),
            PCWSTR(no_text.as_ptr()),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_VSCROLL.0 | LBS_NOTIFY as u32,
            ),
            MARGIN,
            list_top,
            POPUP_W - 2 * MARGIN,
            POPUP_H - list_top - MARGIN,
            Some(hwnd),
            Some(HMENU(ID_LIST as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )?;
        LIST.store(list.0 as isize, Ordering::SeqCst);

        // Subclass the search field so Enter/Esc/Up/Down reach our logic even
        // though the field owns the keyboard focus.
        let edit_proc_addr =
            edit_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        let previous = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc_addr as usize as isize);
        ORIG_EDIT_PROC.store(previous, Ordering::SeqCst);

        match RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_ALT, VK_SPACE.0 as u32) {
            Ok(()) => tracing::info!("launcher hotkey registered (Alt+Space)"),
            Err(err) => {
                tracing::warn!(%err, "launcher hotkey registration failed (Alt+Space taken?)");
            }
        }
        tracing::info!(hwnd = ?hwnd, "launcher popup created (hidden)");
    }
    Ok(())
}

/// Shows the popup, resets the query, and focuses the search field.
fn show() {
    let Some(hwnd) = popup() else { return };
    if let Some(edit) = edit() {
        let empty = wide("");
        // SAFETY: `empty` outlives the call; `edit` is our live control.
        unsafe {
            let _ = SetWindowTextW(edit, PCWSTR(empty.as_ptr()));
        }
    }
    refresh_results();
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = crate::win_util::force_foreground(hwnd);
        if let Some(edit) = edit() {
            let _ = SetFocus(Some(edit));
        }
    }
    VISIBLE.store(true, Ordering::SeqCst);
    tracing::info!("launcher shown");
}

/// Hides the popup without destroying it (the hotkey must stay registered).
fn hide() {
    let Some(hwnd) = popup() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    VISIBLE.store(false, Ordering::SeqCst);
    tracing::debug!("launcher hidden");
}

/// Whether the popup is currently on screen.
fn visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

fn toggle() {
    if visible() {
        hide();
    } else {
        show();
    }
}

/// Re-runs the fuzzy search for the current query and repopulates the list.
fn refresh_results() {
    let Some(list) = list() else { return };
    let query = read_edit_text();
    let results = crate::launcher::search_ranked(items(), &query, MAX_RESULTS);
    // SAFETY: `list` is our live list box; the added strings are copied by the
    // control before each call returns.
    unsafe {
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for shortcut in &results {
            let text = wide(&shortcut.name);
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
        if !results.is_empty() {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
        }
    }
    tracing::debug!(count = results.len(), %query, "launcher results refreshed");
}

/// Reads the current search-field text (empty when unavailable).
fn read_edit_text() -> String {
    let Some(edit) = edit() else {
        return String::new();
    };
    // SAFETY: `edit` is our live control; `buf` is sized from the control.
    unsafe {
        let len = GetWindowTextLengthW(edit);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(edit, &mut buf);
        if copied <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..copied as usize])
    }
}

/// Moves the list selection by `delta`, clamping at both ends.
fn move_selection(delta: i32) {
    let Some(list) = list() else { return };
    // SAFETY: `list` is our live list box.
    unsafe {
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0 as i32;
        if count <= 0 {
            return;
        }
        let current = SendMessageW(list, LB_GETCURSEL, None, None).0 as i32;
        let next = if current < 0 {
            0
        } else {
            (current + delta).clamp(0, count - 1)
        };
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(next as usize)), None);
    }
}

/// Path of the selected result, resolved by replaying the search so the list
/// row maps back to its shortcut.
fn selected_path() -> Option<String> {
    let list = list()?;
    // SAFETY: `list` is our live list box.
    let index = unsafe { SendMessageW(list, LB_GETCURSEL, None, None).0 } as i32;
    if index < 0 {
        return None;
    }
    let query = read_edit_text();
    let results = crate::launcher::search_ranked(items(), &query, MAX_RESULTS);
    results.get(index as usize).map(|s| s.path.clone())
}

/// Launches the selected shortcut and hides the popup.
fn launch_selected() {
    let Some(path) = selected_path() else {
        tracing::debug!("launcher: nothing selected to launch");
        return;
    };
    match crate::launcher::launch(&path) {
        Ok(()) => tracing::info!(%path, "launcher launched shortcut"),
        Err(err) => tracing::warn!(%path, %err, "launcher launch failed"),
    }
    hide();
}

/// Whether `new_focus` (the `WM_KILLFOCUS` wParam) lies outside the popup.
fn focus_left_popup(new_focus: isize) -> bool {
    if new_focus == 0 {
        return true;
    }
    let Some(hwnd) = popup() else { return true };
    // SAFETY: `hwnd` is our live popup; `IsChild` is a pure query.
    unsafe { !IsChild(hwnd, hwnd_from(new_focus)).as_bool() }
}

/// Handles `WM_COMMAND` notifications from the child controls.
fn handle_command(wparam: WPARAM) {
    let id = (wparam.0 & 0xFFFF) as u16;
    let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
    if id == ID_EDIT && code == EN_CHANGE {
        refresh_results();
    } else if id == ID_LIST && code == LBN_DBLCLK {
        launch_selected();
    }
}

/// Window procedure for the launcher popup.
///
/// The body runs under [`crate::win_util::guard_abi`] so a panic cannot unwind
/// across the FFI boundary; on panic it degrades to `DefWindowProcW`.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the guard only runs the body and degrades to the default handler
    // on a panic; all handles come straight from Win32.
    crate::win_util::guard_abi(
        "launcher_ui::wndproc",
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
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
            toggle();
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_command(wparam);
            LRESULT(0)
        }
        // Losing activation (clicking another app) dismisses the popup.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if visible() {
                hide();
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            if visible() && focus_left_popup(wparam.0 as isize) {
                hide();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: `hwnd` is our live popup; teardown of its own hotkey.
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
            }
            POPUP.store(0, Ordering::SeqCst);
            EDIT.store(0, Ordering::SeqCst);
            LIST.store(0, Ordering::SeqCst);
            ORIG_EDIT_PROC.store(0, Ordering::SeqCst);
            VISIBLE.store(false, Ordering::SeqCst);
            tracing::info!("launcher window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Subclass procedure for the search field: maps keys to launcher actions and
/// delegates everything else to the original `EDIT` procedure.
///
/// The body runs under [`crate::win_util::guard_abi`]; on panic it degrades to
/// the same default the body uses — the original `EDIT` procedure, or
/// `DefWindowProcW` when there is none.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the guard only runs the body and, on a panic, takes the same
    // default path the body takes for unhandled messages (`CallWindowProcW`
    // down the original `EDIT` proc, or `DefWindowProcW` if there is none);
    // all handles come straight from Win32.
    crate::win_util::guard_abi(
        "launcher_ui::edit_proc",
        || unsafe { edit_proc_inner(hwnd, msg, wparam, lparam) },
        || unsafe {
            let previous = ORIG_EDIT_PROC.load(Ordering::SeqCst);
            if previous == 0 {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let original: WNDPROC = std::mem::transmute::<isize, WNDPROC>(previous);
            CallWindowProcW(original, hwnd, msg, wparam, lparam)
        },
    )
}

/// Inner body of [`edit_proc`].
unsafe extern "system" fn edit_proc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_RETURN.0 {
                launch_selected();
                return LRESULT(0);
            }
            if key == VK_ESCAPE.0 {
                hide();
                return LRESULT(0);
            }
            if key == VK_DOWN.0 {
                move_selection(1);
                return LRESULT(0);
            }
            if key == VK_UP.0 {
                move_selection(-1);
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS if visible() && focus_left_popup(wparam.0 as isize) => hide(),
        _ => {}
    }

    let previous = ORIG_EDIT_PROC.load(Ordering::SeqCst);
    if previous == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `previous` is the `WNDPROC` returned by `SetWindowLongPtrW`.
    let original: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
}
