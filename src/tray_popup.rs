//! Reusable native notification panel: real accessible buttons, bounded viewport.
use crate::tray::TrayItem;
use crate::tray_bar::{self, MouseEvent};
use crate::win_util::{guard_abi, wide};
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetMonitorInfoW, HFONT, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromRect,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
    SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

const CLASS: &str = "minicon_winmgr_notification_panel";
const HOTKEY: i32 = 515;
const FIRST: usize = 5100;
const WM_MOUSELEAVE: u32 = 0x02a3;
struct Panel {
    hwnd: HWND,
    instance: HINSTANCE,
    anchor: RECT,
    bar: HWND,
    items: Vec<TrayItem>,
    buttons: Vec<HWND>,
    page: usize,
    page_size: usize,
    columns: usize,
    font: HFONT,
    font_dpi: u32,
    selected: usize,
    pressed: Option<(usize, TrayItem)>,
    right_pressed: Option<(usize, TrayItem)>,
    mouse_releasing: bool,
    pending: Option<(Vec<TrayItem>, Vec<TrayItem>)>,
    all: bool,
    launch: Option<Receiver<Result<(), String>>>,
}
thread_local! { static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) }; }
fn with<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|s| s.borrow_mut().as_mut().map(f))
}
fn handle() -> HWND {
    with(|s| s.hwnd).unwrap_or_default()
}
pub(crate) fn install(instance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let name = wide(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: PCWSTR(name.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
                (windows::Win32::Graphics::Gdi::COLOR_BTNFACE.0 + 1) as *mut _,
            ),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let context = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let result = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(name.as_ptr()),
            PCWSTR(wide("通知区域").as_ptr()),
            WS_POPUP | WS_BORDER,
            0,
            0,
            220,
            150,
            None,
            None,
            Some(instance),
            None,
        );
        if !context.is_invalid() {
            SetThreadDpiAwarenessContext(context);
        }
        let hwnd = result?;
        PANEL.with(|s| {
            *s.borrow_mut() = Some(Panel {
                hwnd,
                instance,
                anchor: RECT::default(),
                bar: HWND::default(),
                items: vec![],
                buttons: vec![],
                page: 0,
                page_size: 16,
                columns: 4,
                font: HFONT::default(),
                font_dpi: 0,
                selected: 0,
                pressed: None,
                right_pressed: None,
                mouse_releasing: false,
                pending: None,
                all: false,
                launch: None,
            })
        });
        if RegisterHotKey(Some(hwnd), HOTKEY, MOD_WIN | MOD_NOREPEAT, VK_B.0 as u32).is_err() {
            RegisterHotKey(
                Some(hwnd),
                HOTKEY,
                MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                VK_B.0 as u32,
            )?;
            tracing::info!("notification panel hotkey: Ctrl+Alt+B");
        } else {
            tracing::info!("notification panel hotkey: Win+B");
        }
    }
    Ok(())
}
pub(crate) fn hide() {
    let hwnd = handle();
    if !hwnd.is_invalid() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        with(|s| {
            s.pressed = None;
            s.right_pressed = None;
            s.pending = None;
        });
        unsafe {
            let _ = KillTimer(Some(hwnd), 3);
        }
        // Hidden controls no longer need an icon snapshot. Destroy controls
        // before dropping their image owners, so closed panels do not retain
        // icons belonging to applications that have since exited.
        let buttons = with(|s| std::mem::take(&mut s.buttons)).unwrap_or_default();
        for button in buttons {
            unsafe {
                let _ = DestroyWindow(button);
            }
        }
        with(|s| s.items.clear());
        crate::bar_tip::hide();
    }
}
pub(crate) fn shutdown() {
    let hwnd = handle();
    if !hwnd.is_invalid() {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), HOTKEY);
            let _ = DestroyWindow(hwnd);
            if let Some(font) = with(|s| s.font)
                && !font.is_invalid()
            {
                let _ = DeleteObject(font.into());
            }
        }
        PANEL.with(|s| *s.borrow_mut() = None);
    }
}
pub(crate) fn toggle(bar: HWND, anchor: RECT, items: Vec<TrayItem>, all: bool) {
    if unsafe { IsWindowVisible(handle()).as_bool() } {
        hide();
        return;
    }
    show(bar, anchor, items, all);
}
fn show(bar: HWND, anchor: RECT, items: Vec<TrayItem>, all: bool) {
    with(|s| {
        s.bar = bar;
        s.anchor = anchor;
        s.items = items;
        s.page = 0;
        s.selected = 0;
        s.all = all;
    });
    rebuild();
    unsafe {
        let _ = ShowWindow(handle(), SW_SHOW);
        let _ = crate::win_util::force_foreground(handle());
    }
    focus(0);
}
pub(crate) fn update(items: &[TrayItem], hidden: &[TrayItem]) {
    if !unsafe { IsWindowVisible(handle()).as_bool() } {
        return;
    }
    // Keep the pressed button and its identity intact until the native button
    // releases capture. The next refresh applies any pending registration change.
    let capture = unsafe { GetCapture() };
    if unsafe { GetKeyState(VK_LBUTTON.0 as i32) } < 0
        || unsafe { GetKeyState(VK_RBUTTON.0 as i32) } < 0
        || unsafe { IsChild(handle(), capture) }.as_bool()
    {
        with(|s| s.pending = Some((items.to_vec(), hidden.to_vec())));
        unsafe {
            SetTimer(Some(handle()), 3, 100, None);
        }
        return;
    }
    with(|s| s.pending = None);
    unsafe {
        let _ = KillTimer(Some(handle()), 3);
    }
    let all = with(|s| s.all).unwrap_or(false);
    let next: Vec<_> = if all { items.to_vec() } else { hidden.to_vec() };
    let changed = with(|s| {
        if s.items == next {
            return false;
        }
        let identity = s.items.get(s.selected).and_then(|x| x.owner);
        s.items = next;
        s.selected = identity
            .and_then(|owner| s.items.iter().position(|x| x.owner == Some(owner)))
            .unwrap_or(s.selected.min(s.items.len().saturating_sub(1)));
        s.pressed = None;
        true
    })
    .unwrap_or(false);
    if changed {
        rebuild();
    }
}
#[derive(Debug)]
struct Geometry {
    width: i32,
    height: i32,
    columns: usize,
    capacity: usize,
    page: usize,
    visible_rows: usize,
}
impl Geometry {
    fn new(work: RECT, dpi: u32, count: usize, page: usize) -> Self {
        let scale = |n: i32| n * dpi.max(96) as i32 / 96;
        let width = scale(216).min((work.right - work.left).max(1));
        let columns = ((width - scale(16) - 2 + scale(4)) / scale(36)).clamp(1, 4) as usize;
        let rows = ((work.bottom - work.top - scale(102)) / scale(36)).clamp(1, 8) as usize;
        let capacity = rows * columns;
        let page = page.min(count.saturating_sub(1) / capacity);
        let visible_rows = count
            .saturating_sub(page * capacity)
            .min(capacity)
            .div_ceil(columns)
            .max(1);
        let footer = if count > capacity { 78 } else { 48 };
        let height = scale(visible_rows as i32 * 36 + footer).min((work.bottom - work.top).max(1));
        Self {
            width,
            height,
            columns,
            capacity,
            page,
            visible_rows,
        }
    }
}

fn rebuild() {
    let focused_id = unsafe { GetDlgCtrlID(GetFocus()) };
    let Some((hwnd, instance, anchor, old, items, page, selected)) = with(|s| {
        (
            s.hwnd,
            s.instance,
            s.anchor,
            std::mem::take(&mut s.buttons),
            s.items.clone(),
            s.page,
            s.selected,
        )
    }) else {
        return;
    };
    unsafe {
        for button in old {
            let _ = DestroyWindow(button);
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = |n: i32| n * dpi as i32 / 96;
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST), &mut mi);
        let work = mi.rcWork;
        let Geometry {
            width,
            height,
            columns,
            capacity,
            page,
            visible_rows,
        } = Geometry::new(work, dpi, items.len(), page);
        let (old_font, font_dpi) = with(|s| (s.font, s.font_dpi)).unwrap_or_default();
        let font = if font_dpi == dpi && !old_font.is_invalid() {
            old_font
        } else {
            let mut metrics = NONCLIENTMETRICSW {
                cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
                ..Default::default()
            };
            let font = if SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some(&mut metrics as *mut _ as *mut _),
                0,
                dpi,
            )
            .is_ok()
            {
                CreateFontIndirectW(&metrics.lfMessageFont)
            } else {
                HFONT::default()
            };
            if !old_font.is_invalid() {
                let _ = DeleteObject(old_font.into());
            }
            with(|s| {
                s.font = font;
                s.font_dpi = dpi;
            });
            font
        };
        let x = (anchor.right - width).clamp(work.left, (work.right - width).max(work.left));
        let y =
            (anchor.top - height - scale(6)).clamp(work.top, (work.bottom - height).max(work.top));
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        with(|s| {
            s.page = page;
            s.page_size = capacity;
            s.columns = columns;
        });
        let mut buttons = vec![];
        if items.is_empty() {
            let caption = if crate::tray_host::is_installed() {
                "暂无应用通知图标"
            } else {
                "应用图标由 Windows 通知区域管理"
            };
            let notice = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(wide("STATIC").as_ptr()),
                PCWSTR(wide(caption).as_ptr()),
                WS_CHILD | WS_VISIBLE,
                scale(8),
                scale(8),
                (width - scale(16) - 2).max(1),
                scale(32),
                Some(hwnd),
                Some(HMENU(5 as *mut _)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            buttons.push(notice);
        }
        for (offset, item) in items
            .iter()
            .enumerate()
            .skip(page * capacity)
            .take(capacity)
        {
            let local = offset - page * capacity;
            let title = if item.tooltip.is_empty() {
                "应用通知图标"
            } else {
                &item.tooltip
            };
            let button = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(wide("BUTTON").as_ptr()),
                PCWSTR(wide(title).as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE((BS_ICON | BS_NOTIFY) as u32),
                scale(8 + (local % columns) as i32 * 36),
                scale(8 + (local / columns) as i32 * 36),
                scale(32),
                scale(32),
                Some(hwnd),
                Some(HMENU((FIRST + offset) as *mut _)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            let image = item.image.as_ref().map_or(item.hicon, |i| i.0);
            SendMessageW(
                button,
                BM_SETIMAGE,
                Some(WPARAM(IMAGE_ICON.0 as usize)),
                Some(LPARAM(image)),
            );
            let _ = SetWindowSubclass(button, Some(child_proc), 1, offset);
            buttons.push(button);
        }
        let base = scale(12 + visible_rows as i32 * 36);
        let bw = ((width - scale(24) - 2) / 2).max(1);
        for (id, title, bx, by) in [
            (1, "音量设置", scale(8), base),
            (2, "网络设置", scale(16) + bw, base),
            (3, "上一页", scale(8), base + scale(30)),
            (4, "下一页", scale(16) + bw, base + scale(30)),
        ] {
            if id >= 3 && items.len() <= capacity {
                continue;
            }
            let button = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(wide("BUTTON").as_ptr()),
                PCWSTR(wide(title).as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                bx,
                by,
                bw,
                scale(26),
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            let _ = SetWindowSubclass(button, Some(child_proc), 1, usize::MAX);
            if id == 3 {
                let _ = EnableWindow(button, page > 0);
            }
            if id == 4 {
                let _ = EnableWindow(button, (page + 1) * capacity < items.len());
            }
            buttons.push(button);
        }
        for button in &buttons {
            SendMessageW(
                *button,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        with(|s| s.buttons = buttons);
        if IsWindowVisible(hwnd).as_bool() {
            if (1..=4).contains(&focused_id) {
                let target = GetDlgItem(Some(hwnd), focused_id).unwrap_or_default();
                if IsWindowEnabled(target).as_bool() {
                    let _ = SetFocus(Some(target));
                } else {
                    focus(selected);
                }
            } else {
                focus(selected);
            }
        }
    }
}
fn focus(index: usize) {
    let Some((button, need)) = with(|s| {
        s.selected = index.min(s.items.len().saturating_sub(1));
        let need = !s.items.is_empty() && s.selected / s.page_size != s.page;
        if need {
            s.page = s.selected / s.page_size;
        }
        (
            if s.items.is_empty() {
                s.buttons
                    .iter()
                    .find(|button| unsafe {
                        GetWindowLongW(**button, GWL_STYLE) as u32 & WS_TABSTOP.0 != 0
                    })
                    .copied()
            } else {
                s.buttons
                    .get(s.selected.saturating_sub(s.page * s.page_size))
                    .copied()
            },
            need,
        )
    }) else {
        return;
    };
    if need {
        rebuild();
        return;
    }
    if let Some(button) = button {
        unsafe {
            let _ = SetFocus(Some(button));
        }
    }
}
fn activate(index: usize, event: MouseEvent) {
    if let Some(Some(item)) = with(|s| s.items.get(index).cloned()) {
        if matches!(event, MouseEvent::KeyboardSelect | MouseEvent::KeyboardMenu) {
            let button = with(|s| {
                s.buttons
                    .get(index.saturating_sub(s.page * s.page_size))
                    .copied()
            })
            .flatten();
            let mut rect = RECT::default();
            if let Some(button) = button {
                let _ = unsafe { GetWindowRect(button, &mut rect) };
            }
            tray_bar::forward_at(
                &item,
                event,
                POINT {
                    x: rect.left,
                    y: rect.top,
                },
            );
        } else {
            tray_bar::forward(&item, event);
        }
    }
}

fn open_system(id: usize) {
    if with(|s| s.launch.is_some()).unwrap_or(true) {
        return;
    }
    let (sender, receiver) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("notification-settings".into())
        .spawn(move || {
            let com =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            let request = crate::shell_launch::LaunchRequest {
                file: if id == 1 {
                    "ms-settings:sound"
                } else {
                    "ms-settings:network"
                }
                .into(),
                params: String::new(),
                directory: None,
            };
            let result = crate::shell_launch::execute(&request)
                .or_else(|_| {
                    crate::shell_launch::execute(&crate::shell_launch::LaunchRequest {
                        file: "control.exe".into(),
                        params: if id == 1 { "mmsys.cpl" } else { "ncpa.cpl" }.into(),
                        directory: None,
                    })
                })
                .map_err(|error| error.message);
            if com.is_ok() {
                unsafe {
                    CoUninitialize();
                }
            }
            let _ = sender.send(result);
        });
    match started {
        Ok(_) => {
            with(|s| s.launch = Some(receiver));
            unsafe {
                SetTimer(Some(handle()), 2, 100, None);
            }
        }
        Err(error) => {
            tracing::error!(%error, "notification settings worker failed");
        }
    }
}
unsafe extern "system" fn child_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    index: usize,
) -> LRESULT {
    guard_abi(
        "tray_popup::child",
        || unsafe {
            match msg {
                WM_SETFOCUS => {
                    if index != usize::MAX {
                        with(|s| s.selected = index);
                    }
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    let vk = wp.0 as u16;
                    if vk == VK_ESCAPE.0 {
                        hide();
                        return LRESULT(0);
                    }
                    if vk == VK_TAB.0 {
                        let buttons: Vec<_> = with(|s| s.buttons.clone())
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|b| {
                                IsWindowEnabled(*b).as_bool()
                                    && GetWindowLongW(*b, GWL_STYLE) as u32 & WS_TABSTOP.0 != 0
                            })
                            .collect();
                        if let Some(i) = buttons.iter().position(|b| *b == hwnd) {
                            let step = if GetKeyState(VK_SHIFT.0 as i32) < 0 {
                                buttons.len() - 1
                            } else {
                                1
                            };
                            let _ = SetFocus(Some(buttons[(i + step) % buttons.len()]));
                        }
                        return LRESULT(0);
                    }
                    if vk == VK_RETURN.0 && index == usize::MAX {
                        SendMessageW(
                            handle(),
                            WM_COMMAND,
                            Some(WPARAM(GetDlgCtrlID(hwnd) as usize)),
                            Some(LPARAM(hwnd.0 as isize)),
                        );
                        return LRESULT(0);
                    }
                    if index != usize::MAX {
                        let (count, columns) =
                            with(|s| (s.items.len(), s.columns)).unwrap_or((0, 4));
                        let next = match vk {
                            v if v == VK_LEFT.0 => Some(index.saturating_sub(1)),
                            v if v == VK_RIGHT.0 => Some((index + 1).min(count.saturating_sub(1))),
                            v if v == VK_UP.0 => Some(index.saturating_sub(columns)),
                            v if v == VK_DOWN.0 => {
                                Some((index + columns).min(count.saturating_sub(1)))
                            }
                            v if v == VK_HOME.0 => Some(0),
                            v if v == VK_END.0 => Some(count.saturating_sub(1)),
                            _ => None,
                        };
                        if let Some(next) = next {
                            focus(next);
                            return LRESULT(0);
                        }
                        if vk == VK_RETURN.0 {
                            activate(index, MouseEvent::KeyboardSelect);
                            return LRESULT(0);
                        }
                        if vk == VK_APPS.0 || (vk == VK_F10.0 && GetKeyState(VK_SHIFT.0 as i32) < 0)
                        {
                            activate(index, MouseEvent::KeyboardMenu);
                            return LRESULT(0);
                        }
                    }
                }
                WM_LBUTTONDOWN if index != usize::MAX => {
                    with(|s| s.pressed = s.items.get(index).cloned().map(|i| (index, i)));
                    activate(index, MouseEvent::LeftDown);
                }
                WM_LBUTTONDBLCLK if index != usize::MAX => {
                    with(|s| s.pressed = s.items.get(index).cloned().map(|i| (index, i)));
                    activate(index, MouseEvent::LeftDoubleClick);
                }
                WM_LBUTTONUP if index != usize::MAX => {
                    let pressed = with(|s| s.pressed.take()).flatten();
                    let current = with(|s| s.items.get(index).and_then(|i| i.owner)).flatten();
                    let mut rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rect);
                    let x = lp.0 as u16 as i16 as i32;
                    let y = (lp.0 >> 16) as u16 as i16 as i32;
                    if let Some((_, item)) =
                        pressed.filter(|(old, item)| *old == index && item.owner == current)
                        && crate::win_util::rect_contains(rect, x, y)
                    {
                        tray_bar::forward(&item, MouseEvent::LeftUp);
                    }
                    // Native buttons do not send BN_CLICKED after every double
                    // click. Forward mouse release ourselves and keep their
                    // subsequent BN_CLICKED from becoming keyboard activation.
                    with(|s| s.mouse_releasing = true);
                    let result = DefSubclassProc(hwnd, msg, wp, lp);
                    with(|s| s.mouse_releasing = false);
                    return result;
                }
                WM_RBUTTONDOWN if index != usize::MAX => {
                    with(|s| s.right_pressed = s.items.get(index).cloned().map(|i| (index, i)));
                    activate(index, MouseEvent::RightDown);
                    return LRESULT(0);
                }
                WM_RBUTTONUP if index != usize::MAX => {
                    let pressed = with(|s| s.right_pressed.take()).flatten();
                    let current = with(|s| s.items.get(index).and_then(|i| i.owner)).flatten();
                    if let Some((_, item)) =
                        pressed.filter(|(old, item)| *old == index && item.owner == current)
                    {
                        tray_bar::forward(&item, MouseEvent::RightUp);
                    }
                    return LRESULT(0);
                }
                WM_MOUSEMOVE if index != usize::MAX => {
                    let mut tracking = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = TrackMouseEvent(&mut tracking);
                    if let Some(Some(item)) = with(|s| s.items.get(index).cloned()) {
                        let mut r = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut r);
                        crate::bar_tip::show(
                            if item.tooltip.is_empty() {
                                "应用通知图标"
                            } else {
                                &item.tooltip
                            },
                            r,
                        );
                    }
                }
                WM_MOUSELEAVE => {
                    crate::bar_tip::hide();
                }
                _ => {}
            }
            DefSubclassProc(hwnd, msg, wp, lp)
        },
        || unsafe { DefSubclassProc(hwnd, msg, wp, lp) },
    )
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "tray_popup::proc",
        || unsafe {
            match msg {
                WM_HOTKEY => {
                    let bar = FindWindowW(
                        PCWSTR(wide("minicon_winmgr_taskbar").as_ptr()),
                        PCWSTR::null(),
                    )
                    .unwrap_or_default();
                    let mut anchor = RECT::default();
                    let _ = GetWindowRect(bar, &mut anchor);
                    show(
                        bar,
                        anchor,
                        crate::tray::items()
                            .into_iter()
                            .filter(|i| i.owner.is_some())
                            .collect(),
                        true,
                    );
                    return LRESULT(0);
                }
                WM_COMMAND => {
                    // BS_NOTIFY also sends focus notifications. Only a genuine
                    // BN_CLICKED activates an icon; moving focus must be inert.
                    if (wp.0 >> 16) & 0xffff != BN_CLICKED as usize {
                        return LRESULT(0);
                    }
                    if with(|s| s.mouse_releasing).unwrap_or(false) {
                        return LRESULT(0);
                    }
                    let id = wp.0 & 0xffff;
                    match id {
                        1 | 2 => {
                            open_system(id);
                        }
                        3 | 4 => {
                            with(|s| {
                                if id == 3 {
                                    s.page = s.page.saturating_sub(1);
                                } else {
                                    s.page += 1;
                                }
                                s.selected = s.page * s.page_size;
                            });
                            rebuild();
                        }
                        n if n >= FIRST => {
                            let index = n - FIRST;
                            let pressed = with(|s| s.pressed.take()).flatten();
                            if let Some((old, item)) = pressed {
                                let current =
                                    with(|s| s.items.get(index).and_then(|i| i.owner)).flatten();
                                if old == index && current == item.owner {
                                    tray_bar::forward(&item, MouseEvent::LeftUp);
                                }
                            } else {
                                activate(index, MouseEvent::KeyboardSelect);
                            }
                        }
                        _ => {}
                    }
                    return LRESULT(0);
                }
                WM_ACTIVATE if wp.0 & 0xffff == 0 => {
                    let bar = with(|s| s.bar).unwrap_or_default();
                    if HWND(lp.0 as *mut _) != bar {
                        hide();
                    }
                    return LRESULT(0);
                }
                WM_CLOSE => {
                    hide();
                    return LRESULT(0);
                }
                WM_TIMER if wp.0 == 2 => {
                    let result = with(|s| {
                        s.launch.as_ref().and_then(|rx| match rx.try_recv() {
                            Ok(result) => Some(result),
                            Err(mpsc::TryRecvError::Disconnected) => {
                                Some(Err("无法打开系统设置，请重试。".into()))
                            }
                            Err(mpsc::TryRecvError::Empty) => None,
                        })
                    })
                    .flatten();
                    if let Some(result) = result {
                        with(|s| s.launch = None);
                        let _ = KillTimer(Some(hwnd), 2);
                        match result {
                            Ok(()) => hide(),
                            Err(error) => {
                                let _ = MessageBoxW(
                                    Some(hwnd),
                                    PCWSTR(wide(&error).as_ptr()),
                                    PCWSTR(wide("无法打开系统设置").as_ptr()),
                                    MB_OK | MB_ICONERROR,
                                );
                            }
                        }
                    }
                    return LRESULT(0);
                }
                WM_TIMER if wp.0 == 3 => {
                    let _ = KillTimer(Some(hwnd), 3);
                    if let Some((items, hidden)) = with(|s| s.pending.take()).flatten() {
                        update(&items, &hidden);
                    }
                    return LRESULT(0);
                }
                WM_DPICHANGED => {
                    // Rebuild after SetWindowPos/DPI notifications unwind; moving
                    // to another monitor must not recursively destroy controls.
                    let _ = PostMessageW(Some(hwnd), WM_APP + 51, WPARAM(0), LPARAM(0));
                    return LRESULT(0);
                }
                m if m == WM_APP + 51 => {
                    rebuild();
                    return LRESULT(0);
                }
                _ => {}
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        },
        || unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_grid_and_footer_stay_inside_small_work_areas() {
        for dpi in [96, 144, 192] {
            let scale = |n: i32| n * dpi as i32 / 96;
            for (width, height) in [(1680, 1014), (640, 480), (320, 240), (240, 240)] {
                let work = RECT {
                    left: -width,
                    top: -height,
                    right: 0,
                    bottom: 0,
                };
                for count in [0, 1, 70, 256] {
                    for requested_page in [0, 1, usize::MAX] {
                        let g = Geometry::new(work, dpi, count, requested_page);
                        assert!(g.width <= width && g.height <= height, "{g:?}");
                        assert!((1..=4).contains(&g.columns));
                        assert!(g.page * g.capacity <= count.saturating_sub(1));
                        let displayed = count.saturating_sub(g.page * g.capacity).min(g.capacity);
                        for index in 0..displayed {
                            let right = scale(8 + (index % g.columns) as i32 * 36 + 32);
                            let bottom = scale(8 + (index / g.columns) as i32 * 36 + 32);
                            assert!(right < g.width && bottom < g.height, "{g:?}");
                        }
                        let bw = (g.width - scale(24) - 2) / 2;
                        assert!(scale(16) + 2 * bw < g.width);
                        let footer_bottom = scale(
                            12 + g.visible_rows as i32 * 36
                                + 26
                                + if count > g.capacity { 30 } else { 0 },
                        );
                        assert!(footer_bottom < g.height, "{g:?}, dpi={dpi}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_icon_is_reachable_after_page_size_changes() {
        for dpi in [96, 144, 192] {
            for width in [240, 320, 640] {
                let work = RECT {
                    right: width,
                    bottom: 240,
                    ..Default::default()
                };
                let count = 257;
                let capacity = Geometry::new(work, dpi, count, 0).capacity;
                let reached: Vec<_> = (0..count.div_ceil(capacity))
                    .flat_map(|page| {
                        let g = Geometry::new(work, dpi, count, page);
                        g.page * g.capacity..((g.page + 1) * g.capacity).min(count)
                    })
                    .collect();
                assert_eq!(reached, (0..count).collect::<Vec<_>>());
            }
        }
    }
}
