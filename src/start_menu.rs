//! Start menu / ORB — a categorized application menu popup.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Self-contained: owns its window class and wndproc, reuses `crate::launcher`
//! for discovery/launch. `toggle()` is called by the taskbar's brand-dot click.
//!
//! Layout: a borderless top-most popup holding a single `LISTBOX` of discovered
//! Start Menu shortcuts. The list box is subclassed so `Enter` / `Esc` are
//! handled without a dialog manager (the list owns keyboard focus).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, GWLP_WNDPROC, GetSystemMetrics, HMENU,
    IsChild, LB_ADDSTRING, LB_GETCOUNT, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, LBN_DBLCLK,
    LBS_NOTIFY, RegisterClassW, SM_CYSCREEN, SW_HIDE, SW_SHOW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE,
    WM_COMMAND, WM_DESTROY, WM_KEYDOWN, WM_KILLFOCUS, WNDCLASS_STYLES, WNDCLASSW, WNDPROC,
    WS_BORDER, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::PCWSTR;

use crate::launcher::Shortcut;
use crate::win_util::wide;

// --- geometry / ids -------------------------------------------------------
const ORB_W: i32 = 320;
const ORB_H: i32 = 440;
const MARGIN: i32 = 6;
const ID_LIST: u16 = 2001;

const CLASS_NAME: &str = "minicon_winmgr_start_menu";
const WINDOW_TITLE: &str = "minicon start menu";
/// Light background so the popup is not a black rectangle before controls fill.
const BG_COLOR: COLORREF = COLORREF(0x00FF_FFFF);

// --- process-lifetime UI state --------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static ORB: AtomicIsize = AtomicIsize::new(0);
static LIST: AtomicIsize = AtomicIsize::new(0);
static ORIG_LIST_PROC: AtomicIsize = AtomicIsize::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the shortcut list has been discovered and pushed into the list box.
static LOADED: AtomicBool = AtomicBool::new(false);

/// Discovered Start Menu shortcuts, in list-box row order.
static ITEMS: Mutex<Vec<Shortcut>> = Mutex::new(Vec::new());

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn orb() -> Option<HWND> {
    existing(&ORB)
}

fn list() -> Option<HWND> {
    existing(&LIST)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

/// Locks the items cache, recovering from a poisoned mutex (the data is a plain
/// `Vec` with no invariants a panic could break).
fn items_guard() -> std::sync::MutexGuard<'static, Vec<Shortcut>> {
    ITEMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Path for the shortcut at list-box row `index`, if any. Pure so it can be
/// unit-tested without touching Win32.
fn path_at(items: &[Shortcut], index: i32) -> Option<String> {
    if index < 0 {
        return None;
    }
    items.get(index as usize).map(|s| s.path.clone())
}

/// Registers the start-menu window class and prepares (hidden) UI.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if orb().is_some() {
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
            hbrBackground: background,
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            let code = GetLastError().0;
            // ERROR_CLASS_ALREADY_EXISTS (1410) is fine on a repeated init.
            if code != 1410 {
                return Err(
                    format!("RegisterClassW(start_menu) failed (Win32 error {code})").into(),
                );
            }
        }

        let (x, y) = anchor_origin();
        // Hidden on creation: no `WS_VISIBLE`.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            x,
            y,
            ORB_W,
            ORB_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        ORB.store(hwnd.0 as isize, Ordering::SeqCst);

        let list_class = wide("LISTBOX");
        let no_text = wide("");

        let list = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(list_class.as_ptr()),
            PCWSTR(no_text.as_ptr()),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_VSCROLL.0 | LBS_NOTIFY as u32,
            ),
            MARGIN,
            MARGIN,
            ORB_W - 2 * MARGIN,
            ORB_H - 2 * MARGIN,
            Some(hwnd),
            Some(HMENU(ID_LIST as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )?;
        LIST.store(list.0 as isize, Ordering::SeqCst);

        // Subclass the list box so Enter/Esc reach our logic even though the
        // control owns the keyboard focus.
        let list_proc_addr =
            list_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        let previous = SetWindowLongPtrW(list, GWLP_WNDPROC, list_proc_addr as usize as isize);
        ORIG_LIST_PROC.store(previous, Ordering::SeqCst);

        tracing::info!(hwnd = ?hwnd, "start menu popup created (hidden)");
    }
    Ok(())
}

/// Screen-space origin: left-aligned, nudged up from the bottom edge (where
/// the ORB / brand dot sits). Pure so it can be unit-tested.
fn anchor_origin_for(screen_h: i32, win_h: i32) -> (i32, i32) {
    (0, (screen_h - win_h - 48).max(0))
}

fn anchor_origin() -> (i32, i32) {
    // SAFETY: pure metric queries.
    unsafe { anchor_origin_for(GetSystemMetrics(SM_CYSCREEN), ORB_H) }
}

/// Shows the menu, lazily populating the list, and focuses row 0.
pub fn toggle() {
    if visible() {
        hide();
    } else {
        show();
    }
}

/// Whether the popup is currently on screen.
fn visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

/// Discovers shortcuts on first use and pushes their names into the list box.
fn ensure_items() {
    if LOADED.load(Ordering::SeqCst) {
        return;
    }
    let all = crate::launcher::discover_shortcuts();
    // Show the most-used apps first (empty query ranks by launch frequency).
    let found: Vec<crate::launcher::Shortcut> = crate::launcher::search_ranked(&all, "", all.len())
        .into_iter()
        .map(|s| crate::launcher::Shortcut {
            name: s.name.clone(),
            path: s.path.clone(),
        })
        .collect();
    tracing::info!(
        count = found.len(),
        "start menu indexed Start Menu shortcuts"
    );
    if let Some(list) = list() {
        // SAFETY: `list` is our live list box; the added strings are copied by
        // the control before each call returns.
        unsafe {
            SendMessageW(list, LB_RESETCONTENT, None, None);
            for shortcut in &found {
                let text = wide(&shortcut.name);
                SendMessageW(
                    list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(text.as_ptr() as isize)),
                );
            }
        }
    }
    *items_guard() = found;
    LOADED.store(true, Ordering::SeqCst);
}

/// Shows the popup, selects the first entry, and focuses the list.
fn show() {
    let Some(hwnd) = orb() else { return };
    ensure_items();
    // SAFETY: `hwnd`/`list` are our live windows.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Some(list) = list() {
            let count = SendMessageW(list, LB_GETCOUNT, None, None).0 as i32;
            if count > 0 {
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            }
            let _ = SetFocus(Some(list));
        }
    }
    VISIBLE.store(true, Ordering::SeqCst);
    tracing::info!("start menu shown");
}

/// Hides the popup without destroying it.
fn hide() {
    let Some(hwnd) = orb() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    VISIBLE.store(false, Ordering::SeqCst);
    tracing::debug!("start menu hidden");
}

/// Path of the currently selected row, resolved via the module cache.
fn selected_path() -> Option<String> {
    let list = list()?;
    // SAFETY: `list` is our live list box.
    let index = unsafe { SendMessageW(list, LB_GETCURSEL, None, None).0 } as i32;
    path_at(&items_guard(), index)
}

/// Launches the selected shortcut and hides the menu.
fn launch_selected() {
    let Some(path) = selected_path() else {
        tracing::debug!("start menu: nothing selected to launch");
        return;
    };
    match crate::launcher::launch(&path) {
        Ok(()) => tracing::info!(%path, "start menu launched shortcut"),
        Err(err) => tracing::warn!(%path, %err, "start menu launch failed"),
    }
    hide();
}

/// Whether `new_focus` (the `WM_KILLFOCUS` wParam) lies outside the popup.
fn focus_left_popup(new_focus: isize) -> bool {
    if new_focus == 0 {
        return true;
    }
    let Some(hwnd) = orb() else { return true };
    // SAFETY: `hwnd` is our live popup; `IsChild` is a pure query.
    unsafe { !IsChild(hwnd, hwnd_from(new_focus)).as_bool() }
}

/// Window procedure for the start-menu popup.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if id == ID_LIST && code == LBN_DBLCLK {
                launch_selected();
            }
            LRESULT(0)
        }
        // Losing activation (clicking another app) dismisses the menu.
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
            ORB.store(0, Ordering::SeqCst);
            LIST.store(0, Ordering::SeqCst);
            ORIG_LIST_PROC.store(0, Ordering::SeqCst);
            VISIBLE.store(false, Ordering::SeqCst);
            LOADED.store(false, Ordering::SeqCst);
            tracing::info!("start menu window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Subclass procedure for the list box: maps Enter/Esc to menu actions and
/// delegates everything else to the original `LISTBOX` procedure.
unsafe extern "system" fn list_proc(
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
        }
        WM_KILLFOCUS if visible() && focus_left_popup(wparam.0 as isize) => hide(),
        _ => {}
    }

    let previous = ORIG_LIST_PROC.load(Ordering::SeqCst);
    if previous == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `previous` is the `WNDPROC` returned by `SetWindowLongPtrW`.
    let original: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, path: &str) -> Shortcut {
        Shortcut {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    }

    #[test]
    fn path_at_resolves_rows_to_paths() {
        let items = vec![item("Alpha", "a.lnk"), item("Beta", "b.lnk")];
        assert_eq!(path_at(&items, 0).as_deref(), Some("a.lnk"));
        assert_eq!(path_at(&items, 1).as_deref(), Some("b.lnk"));
    }

    #[test]
    fn path_at_rejects_out_of_range_and_negative() {
        let items = vec![item("Alpha", "a.lnk")];
        assert_eq!(path_at(&items, -1), None);
        assert_eq!(path_at(&items, 1), None);
        assert_eq!(path_at(&[], 0), None);
    }

    #[test]
    fn anchor_origin_sits_above_bottom_edge() {
        assert_eq!(anchor_origin_for(1080, 440), (0, 592));
    }

    #[test]
    fn anchor_origin_never_negative_on_short_screens() {
        assert_eq!(anchor_origin_for(200, 440).1, 0);
    }
}
