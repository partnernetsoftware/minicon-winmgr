//! Run dialog (Win+R style): a small popup with a single command field.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Trigger: `Win+R`. A borderless top-most popup holding one `EDIT` and OK /
//! Cancel buttons. `Enter` or OK runs the typed command via `ShellExecuteW`;
//! `Esc` or Cancel dismisses. Mirrors the structure of `crate::launcher_ui`
//! (own window class, own hotkey, no dialog manager).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_WIN, RegisterHotKey, SetFocus, UnregisterHotKey, VK_ESCAPE, VK_R,
    VK_RETURN,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CallWindowProcW, CreateWindowExW, DefWindowProcW,
    ES_AUTOHSCROLL, GWLP_WNDPROC, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, HMENU,
    IsChild, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SW_SHOWNORMAL,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow, WA_INACTIVE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN,
    WM_KILLFOCUS, WNDCLASS_STYLES, WNDCLASSW, WNDPROC, WS_BORDER, WS_CHILD, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

// --- geometry / ids -------------------------------------------------------
const POPUP_W: i32 = 420;
const POPUP_H: i32 = 150;
const MARGIN: i32 = 12;
const EDIT_H: i32 = 26;
const BTN_W: i32 = 84;
const BTN_H: i32 = 26;
const GAP: i32 = 8;
const ID_EDIT: u16 = 2001;
const ID_OK: u16 = 2002;
const ID_CANCEL: u16 = 2003;
/// Distinct from the taskbar (1) and launcher (2) hotkey ids; only needs to be
/// unique within this window.
const HOTKEY_ID: i32 = 3;

const CLASS_NAME: &str = "minicon_winmgr_run_dialog";
const WINDOW_TITLE: &str = "minicon run";
/// Light background so the popup is not a black rectangle before controls fill.
const BG_COLOR: COLORREF = COLORREF(0x00FF_FFFF);

// --- process-lifetime UI state --------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static POPUP: AtomicIsize = AtomicIsize::new(0);
static EDIT: AtomicIsize = AtomicIsize::new(0);
static ORIG_EDIT_PROC: AtomicIsize = AtomicIsize::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

fn popup() -> Option<HWND> {
    existing(&POPUP)
}

fn edit() -> Option<HWND> {
    existing(&EDIT)
}

/// Registers the run-dialog hotkey and prepares its (hidden) popup window.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
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
                return Err(
                    format!("RegisterClassW(run_dialog) failed (Win32 error {code})").into(),
                );
            }
        }

        let (x, y) = centered_origin();
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
        let button_class = wide("BUTTON");
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

        // Buttons sit on a right-aligned row along the bottom edge.
        let btn_y = POPUP_H - MARGIN - BTN_H;
        let cancel_x = POPUP_W - MARGIN - BTN_W;
        let ok_x = cancel_x - GAP - BTN_W;

        let ok_text = wide("OK");
        let _ok = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(button_class.as_ptr()),
            PCWSTR(ok_text.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_DEFPUSHBUTTON as u32),
            ok_x,
            btn_y,
            BTN_W,
            BTN_H,
            Some(hwnd),
            Some(HMENU(ID_OK as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )?;

        let cancel_text = wide("Cancel");
        let _cancel = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(button_class.as_ptr()),
            PCWSTR(cancel_text.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_PUSHBUTTON as u32),
            cancel_x,
            btn_y,
            BTN_W,
            BTN_H,
            Some(hwnd),
            Some(HMENU(ID_CANCEL as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )?;

        // Subclass the command field so Enter/Esc reach our logic even though
        // the field owns the keyboard focus.
        let edit_proc_addr =
            edit_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        let previous = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc_addr as usize as isize);
        ORIG_EDIT_PROC.store(previous, Ordering::SeqCst);

        // Try the canonical Win+R first, then fall back to combos that the
        // default shell does not grab, so the dialog stays reachable when
        // another shell owns Win+R (companion mode).
        let candidates = [
            (MOD_WIN, VK_R.0 as u32, "Win+R"),
            (MOD_CONTROL | MOD_ALT, VK_R.0 as u32, "Ctrl+Alt+R"),
        ];
        let mut registered = false;
        for (mods, vk, label) in candidates {
            match RegisterHotKey(Some(hwnd), HOTKEY_ID, mods, vk) {
                Ok(()) => {
                    tracing::info!(hotkey = label, "run dialog hotkey registered");
                    registered = true;
                    break;
                }
                Err(err) => {
                    tracing::warn!(hotkey = label, %err, "run dialog hotkey candidate taken");
                }
            }
        }
        if !registered {
            tracing::error!("run dialog hotkey: all candidates taken; dialog unreachable by key");
        }
        tracing::info!(hwnd = ?hwnd, "run dialog popup created (hidden)");
    }
    Ok(())
}

/// Normalizes raw user input into the string handed to the shell.
///
/// Trims surrounding whitespace and strips a single layer of surrounding
/// double quotes so pasted `"C:\path with spaces"` still launches. Pure, so it
/// can be unit-tested without any Win32 state.
pub fn normalize_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Screen-space origin that centres a `win_w`-wide popup and puts it in the
/// upper quarter of the work area. Pure so it can be unit-tested.
fn centered_origin_for(screen_w: i32, screen_h: i32, win_w: i32) -> (i32, i32) {
    (((screen_w - win_w) / 2).max(0), (screen_h / 4).max(0))
}

fn centered_origin() -> (i32, i32) {
    // SAFETY: pure metric queries.
    unsafe {
        centered_origin_for(
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
            POPUP_W,
        )
    }
}

/// Shows the popup, clears the command field, and focuses it.
fn show() {
    let Some(hwnd) = popup() else { return };
    if let Some(edit) = edit() {
        let empty = wide("");
        // SAFETY: `empty` outlives the call; `edit` is our live control.
        unsafe {
            let _ = SetWindowTextW(edit, PCWSTR(empty.as_ptr()));
        }
    }
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Some(edit) = edit() {
            let _ = SetFocus(Some(edit));
        }
    }
    VISIBLE.store(true, Ordering::SeqCst);
    tracing::info!("run dialog shown");
}

/// Hides the popup without destroying it (the hotkey must stay registered).
fn hide() {
    let Some(hwnd) = popup() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    VISIBLE.store(false, Ordering::SeqCst);
    tracing::debug!("run dialog hidden");
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

/// Reads the current command-field text (empty when unavailable).
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

/// Normalizes the typed command, launches it via the shell, and hides.
fn run_command() {
    let cmd = normalize_input(&read_edit_text());
    if cmd.is_empty() {
        tracing::warn!("run dialog: empty command ignored");
        return;
    }
    let operation = wide("open");
    let file = wide(&cmd);
    // SAFETY: both wide buffers outlive the call; null `parameters`/`directory`
    // are valid for the shell `open` verb.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value <= 32 to signal failure.
    let code = result.0 as isize;
    if code <= 32 {
        tracing::warn!(%cmd, code, "run dialog: ShellExecuteW failed to launch command");
    } else {
        tracing::info!(%cmd, "run dialog launched command");
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
    if code != BN_CLICKED {
        return;
    }
    match id {
        ID_OK => run_command(),
        ID_CANCEL => hide(),
        _ => {}
    }
}

/// Window procedure for the run-dialog popup.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
            ORIG_EDIT_PROC.store(0, Ordering::SeqCst);
            VISIBLE.store(false, Ordering::SeqCst);
            tracing::info!("run dialog window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Subclass procedure for the command field: maps Enter/Esc to run/dismiss and
/// delegates everything else to the original `EDIT` procedure.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_RETURN.0 {
                run_command();
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

    let previous = ORIG_EDIT_PROC.load(Ordering::SeqCst);
    if previous == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `previous` is the `WNDPROC` returned by `SetWindowLongPtrW`.
    let original: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::{centered_origin_for, normalize_input};

    #[test]
    fn trims_whitespace() {
        assert_eq!(normalize_input("  notepad  "), "notepad");
    }

    #[test]
    fn strips_surrounding_quotes() {
        assert_eq!(
            normalize_input("\"C:\\Program Files\\app.exe\""),
            "C:\\Program Files\\app.exe"
        );
    }

    #[test]
    fn keeps_inner_quotes() {
        assert_eq!(normalize_input("cmd /c \"echo hi\""), "cmd /c \"echo hi\"");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(normalize_input("   "), "");
    }

    #[test]
    fn centers_horizontally_and_sits_in_upper_quarter() {
        assert_eq!(centered_origin_for(1920, 1080, 420), (750, 270));
    }

    #[test]
    fn clamps_when_window_wider_than_screen() {
        assert_eq!(centered_origin_for(400, 800, 420).0, 0);
    }
}
