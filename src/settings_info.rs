//! Read-only, copyable configuration details, separate from everyday settings.
use crate::win_util::{guard_abi, wide};
use std::cell::Cell;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{Controls::EM_SETSEL, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::PCWSTR,
};
const CLASS: &str = "minicon_winmgr_settings_info";
thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static FONT: Cell<HFONT> = const { Cell::new(HFONT(std::ptr::null_mut())) };
}
fn resize(hwnd: HWND) {
    unsafe {
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let unit = |n| n * GetDpiForWindow(hwnd).max(96) as i32 / 96;
        let margin = unit(20).min(rect.right.max(1) / 8);
        let _ = MoveWindow(
            GetDlgItem(Some(hwnd), 1).unwrap_or_default(),
            margin,
            margin,
            (rect.right - margin * 2).max(1),
            (rect.bottom - margin * 2 - unit(48)).max(1),
            true,
        );
        let _ = MoveWindow(
            GetDlgItem(Some(hwnd), 2).unwrap_or_default(),
            (rect.right - margin - unit(90)).max(0),
            (rect.bottom - margin - unit(32)).max(0),
            unit(90),
            unit(32),
            true,
        );
    }
}
fn update_font(hwnd: HWND) {
    unsafe {
        let mut value = LOGFONTW {
            lfHeight: -(14 * GetDpiForWindow(hwnd).max(96) as i32 / 96),
            ..Default::default()
        };
        let face = wide("Microsoft YaHei UI");
        value.lfFaceName[..face.len()].copy_from_slice(&face);
        let font = CreateFontIndirectW(&value);
        for id in [1, 2] {
            let _ = SendMessageW(
                GetDlgItem(Some(hwnd), id).unwrap_or_default(),
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        let old = FONT.replace(font);
        if !old.is_invalid() {
            let _ = DeleteObject(old.into());
        }
    }
}
pub(crate) fn show(owner: HWND, value: &str) -> Result<(), windows::core::Error> {
    unsafe {
        let mut hwnd = WINDOW.get();
        if !IsWindow(Some(hwnd)).as_bool() {
            let instance = HINSTANCE(GetWindowLongPtrW(owner, GWLP_HINSTANCE) as *mut _);
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: PCWSTR(wide(CLASS).as_ptr()),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(windows::core::Error::from_thread());
            }
            let mut work = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(
                MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
                &mut work,
            )
            .as_bool()
            {
                return Err(windows::core::Error::from_thread());
            }
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let width = (540 * dpi / 96).min(work.rcWork.right - work.rcWork.left);
            let height = (580 * dpi / 96).min(work.rcWork.bottom - work.rcWork.top);
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let created = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                PCWSTR(wide(CLASS).as_ptr()),
                PCWSTR(wide(crate::i18n::text("配置与关于 — minicon-winmgr")).as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                work.rcWork.left + (work.rcWork.right - work.rcWork.left - width) / 2,
                work.rcWork.top + (work.rcWork.bottom - work.rcWork.top - height) / 2,
                width,
                height,
                Some(owner),
                None,
                Some(instance),
                None,
            );
            SetThreadDpiAwarenessContext(previous);
            hwnd = created?;
            let controls = (|| -> Result<(), windows::core::Error> {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(wide("EDIT").as_ptr()),
                    PCWSTR::null(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                    0,
                    0,
                    1,
                    1,
                    Some(hwnd),
                    Some(HMENU(std::ptr::without_provenance_mut(1))),
                    Some(instance),
                    None,
                )?;
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(wide("BUTTON").as_ptr()),
                    PCWSTR(wide(crate::i18n::text("关闭")).as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                    0,
                    0,
                    1,
                    1,
                    Some(hwnd),
                    Some(HMENU(2usize as *mut _)),
                    Some(instance),
                    None,
                )?;
                Ok(())
            })();
            if let Err(error) = controls {
                let _ = DestroyWindow(hwnd);
                return Err(error);
            }
            WINDOW.set(hwnd);
            update_font(hwnd);
        }
        let edit = GetDlgItem(Some(hwnd), 1)?;
        SetWindowTextW(edit, PCWSTR(wide(&value.replace('\n', "\r\n")).as_ptr()))?;
        resize(hwnd);
        crate::appearance::install(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        crate::win_util::force_foreground(hwnd);
        let _ = SetFocus(Some(edit));
        let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    Ok(())
}
pub(crate) fn filter(msg: &MSG) -> bool {
    let hwnd = WINDOW.get();
    if hwnd.is_invalid() || (msg.hwnd != hwnd && !unsafe { IsChild(hwnd, msg.hwnd).as_bool() }) {
        return false;
    }
    if msg.message == WM_KEYDOWN {
        if msg.wParam.0 == VK_ESCAPE.0 as usize
            || (msg.wParam.0 == VK_RETURN.0 as usize && unsafe { GetDlgCtrlID(msg.hwnd) } == 2)
        {
            unsafe {
                let _ = SendMessageW(hwnd, WM_CLOSE, None, None);
            }
            return true;
        }
        if msg.wParam.0 == b'A' as usize && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 {
            unsafe {
                let _ = SendMessageW(msg.hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            return true;
        }
    }
    unsafe { IsDialogMessageW(hwnd, msg).as_bool() }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "settings information",
        || match msg {
            WM_CLOSE => unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                crate::settings_panel::restore_info_focus();
                LRESULT(0)
            },
            WM_COMMAND if wp.0 & 0xffff == 2 => unsafe { SendMessageW(hwnd, WM_CLOSE, None, None) },
            WM_SIZE => {
                resize(hwnd);
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => unsafe {
                let dc = HDC(wp.0 as *mut _);
                SetTextColor(dc, COLORREF(GetSysColor(COLOR_WINDOWTEXT)));
                SetBkColor(dc, COLORREF(GetSysColor(COLOR_WINDOW)));
                LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
            },
            WM_DPICHANGED => unsafe {
                let rect = *(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                update_font(hwnd);
                resize(hwnd);
                LRESULT(0)
            },
            WM_DESTROY => {
                WINDOW.set(HWND::default());
                let font = FONT.replace(HFONT::default());
                if !font.is_invalid() {
                    unsafe {
                        let _ = DeleteObject(font.into());
                    }
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        },
        || unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    )
}
