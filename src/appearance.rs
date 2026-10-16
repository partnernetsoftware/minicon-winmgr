//! Shared industrial palette for native windows. All subclassing runs on the UI thread.
use crate::{config, win_util::guard_abi};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::Threading::GetCurrentProcessId,
        UI::{
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::BOOL,
};

pub(crate) fn color(index: SYS_COLOR_INDEX) -> u32 {
    let t = config::theme();
    match index {
        COLOR_WINDOW => t.surface,
        COLOR_BTNFACE => t.bg,
        COLOR_WINDOWTEXT | COLOR_BTNTEXT => t.text,
        COLOR_GRAYTEXT | COLOR_3DSHADOW => t.muted,
        COLOR_HOTLIGHT | COLOR_HIGHLIGHT => t.accent,
        COLOR_HIGHLIGHTTEXT => t.surface,
        _ => unsafe { GetSysColor(index) },
    }
}
pub(crate) fn brush(dc: HDC, index: SYS_COLOR_INDEX) -> HBRUSH {
    unsafe {
        SetDCBrushColor(dc, COLORREF(color(index)));
        HBRUSH(GetStockObject(DC_BRUSH).0)
    }
}
fn class(hwnd: HWND) -> String {
    let mut name = [0; 128];
    let length = unsafe { GetClassNameW(hwnd, &mut name) };
    String::from_utf16_lossy(&name[..length.max(0) as usize])
}
pub(crate) fn install(hwnd: HWND) {
    let name = class(hwnd);
    crate::i18n::localize_window(hwnd);
    if !name.starts_with("minicon_winmgr_") {
        return;
    }
    // These windows already paint their complete surface from the palette.
    if !matches!(
        name.as_str(),
        "minicon_winmgr_taskbar"
            | "minicon_winmgr_bar_tip"
            | "minicon_winmgr_hover_preview"
            | "minicon_winmgr_settings"
    ) {
        let _ = unsafe { SetWindowSubclass(hwnd, Some(native), 0xAC16, 0) };
    }
    let t = config::theme();
    let dark: i32 = i32::from((t.bg & 255) + ((t.bg >> 8) & 255) + ((t.bg >> 16) & 255) < 384);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast(),
            size_of::<i32>() as u32,
        );
        if name != "minicon_winmgr_settings" {
            let _ = EnumChildWindows(Some(hwnd), Some(child), LPARAM(0));
        }
    }
}
unsafe extern "system" fn child(hwnd: HWND, _: LPARAM) -> BOOL {
    guard_abi(
        "theme child",
        || {
            crate::i18n::localize_window(hwnd);
            if class(hwnd).eq_ignore_ascii_case("button") {
                let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
                let kind = style & 0xf;
                if style & (BS_ICON | BS_BITMAP) as u32 == 0
                    && (kind == BS_PUSHBUTTON as u32 || kind == BS_DEFPUSHBUTTON as u32)
                {
                    let _ = unsafe { SetWindowSubclass(hwnd, Some(button), 0xAC17, 0) };
                }
            }
            BOOL(1)
        },
        || BOOL(1),
    )
}
fn paint_button(hwnd: HWND, dc: HDC) {
    unsafe {
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        FillRect(dc, &rect, brush(dc, COLOR_WINDOW));
        FrameRect(dc, &rect, brush(dc, COLOR_3DSHADOW));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(
            dc,
            COLORREF(color(
                if windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(hwnd).as_bool() {
                    COLOR_WINDOWTEXT
                } else {
                    COLOR_GRAYTEXT
                },
            )),
        );
        let font = SendMessageW(hwnd, WM_GETFONT, None, None).0;
        let previous = SelectObject(
            dc,
            if font == 0 {
                GetStockObject(DEFAULT_GUI_FONT)
            } else {
                HGDIOBJ(font as *mut _)
            },
        );
        let mut title = vec![0; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let length = GetWindowTextW(hwnd, &mut title);
        DrawTextW(
            dc,
            &mut title[..length.max(0) as usize],
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == hwnd {
            rect.left += 3;
            rect.top += 3;
            rect.right -= 3;
            rect.bottom -= 3;
            let _ = DrawFocusRect(dc, &rect);
        }
        SelectObject(dc, previous);
    }
}
unsafe extern "system" fn button(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    guard_abi(
        "theme button",
        || {
            if msg == WM_PAINT {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    paint_button(hwnd, dc);
                    let _ = EndPaint(hwnd, &ps);
                }
                return LRESULT(0);
            }
            if msg == WM_PRINTCLIENT {
                paint_button(hwnd, HDC(wp.0 as *mut _));
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
        },
        || unsafe { DefSubclassProc(hwnd, msg, wp, lp) },
    )
}
unsafe extern "system" fn visit(hwnd: HWND, _: LPARAM) -> BOOL {
    guard_abi(
        "appearance enumeration",
        || {
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if pid == unsafe { GetCurrentProcessId() } && class(hwnd).starts_with("minicon_winmgr_")
            {
                install(hwnd);
                unsafe {
                    let _ = RedrawWindow(
                        Some(hwnd),
                        None,
                        None,
                        RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
                    );
                }
            }
            BOOL(1)
        },
        || BOOL(1),
    )
}
pub(crate) fn refresh() {
    config::refresh_theme();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(0));
    }
    crate::diagnostic_ui::refresh();
}
unsafe extern "system" fn native(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    guard_abi(
        "native appearance",
        || {
            if matches!(
                msg,
                WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX
            ) {
                let dc = HDC(wp.0 as *mut _);
                unsafe {
                    SetTextColor(dc, COLORREF(config::theme().text));
                    SetBkColor(dc, COLORREF(config::theme().surface));
                }
                return LRESULT(brush(dc, COLOR_WINDOW).0 as isize);
            }
            if msg == WM_ERASEBKGND {
                let dc = HDC(wp.0 as *mut _);
                let mut rect = RECT::default();
                unsafe {
                    let _ = GetClientRect(hwnd, &mut rect);
                    FillRect(dc, &rect, brush(dc, COLOR_BTNFACE));
                }
                return LRESULT(1);
            }
            if msg == WM_SETTINGCHANGE || msg == WM_THEMECHANGED {
                config::refresh_theme();
            }
            unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
        },
        || unsafe { DefSubclassProc(hwnd, msg, wp, lp) },
    )
}
