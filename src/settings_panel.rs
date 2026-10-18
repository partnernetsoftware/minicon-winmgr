//! Native settings controls, scrolling, focus and DPI. All window ownership is
//! confined to the shell message thread; the persistence worker never sees HWNDs.
use crate::config::{self, GroupBy, MergeStrategy, Position, ThemeMode, keys};
use crate::win_util::{guard_abi, wide};
use std::cell::RefCell;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            Controls::*,
            HiDpi::*,
            Input::KeyboardAndMouse::*,
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};

const CLASS: &str = "minicon_winmgr_settings";
const VIEW: &str = "minicon_winmgr_settings_view";
const UPDATE_STATUS: u16 = 6400;
const CHECK_UPDATE: u16 = 6401;
const DOWNLOAD_UPDATE: u16 = 6402;
const RESTART_UPDATE: u16 = 6403;
const SHOW: u16 = 6101;
const TRAY: u16 = 6102;
const NEVER: u16 = 6103;
const FULL: u16 = 6104;
const ALWAYS: u16 = 6105;
const EXE: u16 = 6106;
const PID: u16 = 6107;
const PREVIEW: u16 = 6108;
const REPLACE: u16 = 6110;
const ROLLBACK: u16 = 6111;
const STATUS: u16 = 6112;
const RETRY: u16 = 6113;
const CLOSE: u16 = 6114;
const DETAIL: u16 = 6115;
const CONFIG_INFO: u16 = 6120;
const ADVANCED: u16 = 6205;
const ENGLISH: u16 = 6150;
const CHINESE: u16 = 6151;
const LIGHT: u16 = 6130;
const DARK: u16 = 6131;
const AUTO: u16 = 6132;
const TOP: u16 = 6140;
const BOTTOM: u16 = 6141;
const LEFT: u16 = 6142;
const RIGHT: u16 = 6143;
const ENSURE_FOCUS: u32 = WM_APP + 140;
const REFLOW: u32 = WM_APP + 143;
const POLL: usize = 90;
static INSTANCE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
#[derive(Clone)]
struct Control {
    id: u16,
    caption: String,
    hwnd: HWND,
    height: i32,
    top: i32,
    laid_height: i32,
    footer: bool,
}
struct Panel {
    hwnd: HWND,
    view: HWND,
    instance: HINSTANCE,
    controls: Vec<Control>,
    font: HFONT,
    heading_font: HFONT,
    title_font: HFONT,
    dpi: u32,
    scroll: i32,
    content: i32,
    viewport: i32,
    last_focus: HWND,
    layout: bool,
    error: Option<String>,
    anchor: Option<HWND>,
    advanced: bool,
    cards: Vec<RECT>,
}
thread_local! { static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) }; }
fn with<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|p| p.borrow_mut().as_mut().map(f))
}
fn handle() -> HWND {
    with(|p| p.hwnd).unwrap_or_default()
}
pub(crate) fn owner() -> Option<HWND> {
    let hwnd = handle();
    (!hwnd.is_invalid()).then_some(hwnd)
}
pub(crate) fn refresh_shell_status() {
    if owner().is_some() {
        let status = crate::settings_ui::shell_status_text();
        with(|p| {
            if let Some(c) = p.controls.iter_mut().find(|c| c.id == 6307) {
                c.caption = status.clone();
            }
        });
        label(control(6307), &status);
        let enabled = !crate::settings_ui::shell_busy();
        unsafe {
            let _ = EnableWindow(control(REPLACE), enabled);
            let _ = EnableWindow(control(ROLLBACK), enabled);
        }
    }
}
pub(crate) fn restore_info_focus() {
    if let Some(hwnd) = owner() {
        unsafe {
            crate::win_util::force_foreground(hwnd);
            let _ = SetFocus(Some(control(CONFIG_INFO)));
        }
        ensure_focus();
    }
}
fn control(id: u16) -> HWND {
    with(|p| {
        p.controls
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.hwnd)
            .unwrap_or_default()
    })
    .unwrap_or_default()
}
fn scale_at(n: i32, dpi: u32) -> i32 {
    n * dpi as i32 / 96
}
fn is_heading(id: u16) -> bool {
    matches!(id, 6200 | 6201 | 6210)
}
fn high_contrast() -> bool {
    let mut value = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            value.cbSize,
            Some((&mut value as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    value.dwFlags.contains(HCF_HIGHCONTRASTON)
}
fn in_card(id: u16) -> bool {
    matches!(
        id,
        UPDATE_STATUS..=RESTART_UPDATE | SHOW | TRAY
            | NEVER
            | FULL
            | ALWAYS
            | EXE
            | PID
            | PREVIEW
            | 6213
            | ENGLISH
            | CHINESE
            | 6312
            | 6202
            | 6220
            | 6301
            ..=6305 | LIGHT | DARK | AUTO | TOP | BOTTOM | LEFT | RIGHT | 6211 | 6212 | 6310 | 6311
    )
}
fn control_color(wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let dc = HDC(wp.0 as *mut _);
        let id = GetDlgCtrlID(HWND(lp.0 as *mut _));
        let color = if id == STATUS as i32 && crate::settings_store::status().failed > 0 {
            COLOR_HOTLIGHT
        } else if id == 6191 || (6301..=6312).contains(&id) || id == STATUS as i32 {
            COLOR_GRAYTEXT
        } else {
            COLOR_WINDOWTEXT
        };
        let color = if color == COLOR_GRAYTEXT && !high_contrast() {
            let foreground = crate::appearance::color(COLOR_WINDOWTEXT);
            let background = crate::appearance::color(COLOR_WINDOW);
            let blend = |shift: u32| {
                (((foreground >> shift) & 255u32) * 3 + ((background >> shift) & 255u32) * 2) / 5
            };
            blend(0) | (blend(8) << 8) | (blend(16) << 16)
        } else {
            crate::appearance::color(color)
        };
        SetTextColor(dc, COLORREF(color));
        let surface = if in_card(id as u16) || id == STATUS as i32 || id == CLOSE as i32 {
            COLOR_WINDOW
        } else {
            COLOR_BTNFACE
        };
        SetBkColor(dc, COLORREF(crate::appearance::color(surface)));
        LRESULT(crate::appearance::brush(dc, surface).0 as isize)
    }
}
fn paint_surface(hwnd: HWND, viewport: bool) -> LRESULT {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        FillRect(dc, &rect, crate::appearance::brush(dc, COLOR_BTNFACE));
        if viewport {
            let cards = with(|p| p.cards.clone()).unwrap_or_default();
            let pen = CreatePen(
                PS_SOLID,
                1,
                COLORREF(crate::appearance::color(if high_contrast() {
                    COLOR_WINDOWTEXT
                } else {
                    COLOR_3DSHADOW
                })),
            );
            let old_pen = SelectObject(dc, pen.into());
            let old_brush = SelectObject(dc, crate::appearance::brush(dc, COLOR_WINDOW).into());
            for card in cards {
                let _ = RoundRect(
                    dc,
                    card.left,
                    card.top,
                    card.right,
                    card.bottom,
                    scale(2),
                    scale(2),
                );
            }
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            let _ = DeleteObject(pen.into());
        } else {
            rect.top = with(|p| p.viewport).unwrap_or(rect.bottom);
            FillRect(dc, &rect, crate::appearance::brush(dc, COLOR_WINDOW));
        }
        let _ = EndPaint(hwnd, &paint);
    }
    LRESULT(0)
}
// Keep native button behavior, keyboard handling and UIA provider; draw only
// its surface so classic Windows themes do not add heavy bevels.
fn choice(id: u16) -> bool {
    matches!(
        id,
        ENGLISH
            | CHINESE
            | SHOW
            | TRAY
            | PREVIEW
            | NEVER
            | FULL
            | ALWAYS
            | EXE
            | PID
            | LIGHT
            | DARK
            | AUTO
            | TOP
            | BOTTOM
            | LEFT
            | RIGHT
    )
}
fn paint_choice(hwnd: HWND, dc: HDC) {
    unsafe {
        let id = GetDlgCtrlID(hwnd) as u16;
        let radio = !matches!(id, SHOW | TRAY | PREVIEW);
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        FillRect(dc, &rect, crate::appearance::brush(dc, COLOR_WINDOW));
        let enabled = IsWindowEnabled(hwnd).as_bool();
        let color = crate::appearance::color(if enabled {
            COLOR_WINDOWTEXT
        } else {
            COLOR_GRAYTEXT
        });
        let edge = scale(13);
        let y = (rect.bottom - edge) / 2;
        let pen = CreatePen(PS_SOLID, scale(1).max(1), COLORREF(color));
        let previous_pen = SelectObject(dc, pen.into());
        let previous_brush = SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
        if radio {
            let _ = Ellipse(dc, 0, y, edge, y + edge);
        } else {
            let _ = Rectangle(dc, 0, y, edge, y + edge);
        }
        if send(hwnd, BM_GETCHECK, 0, 0).0 != 0 {
            if radio {
                SetDCBrushColor(dc, COLORREF(crate::appearance::color(COLOR_HOTLIGHT)));
                SelectObject(dc, GetStockObject(DC_BRUSH));
                let inset = scale(4);
                let _ = Ellipse(dc, inset, y + inset, edge - inset, y + edge - inset);
            } else {
                let _ = MoveToEx(dc, scale(3), y + scale(6), None);
                let _ = LineTo(dc, scale(6), y + scale(9));
                let _ = LineTo(dc, scale(11), y + scale(3));
            }
        }
        SelectObject(dc, previous_brush);
        SelectObject(dc, previous_pen);
        let _ = DeleteObject(pen.into());
        let old_font = SelectObject(dc, with(|p| p.font).unwrap_or_default().into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(color));
        rect.left = edge + scale(6);
        let mut value: Vec<u16> = text(hwnd).encode_utf16().collect();
        DrawTextW(
            dc,
            &mut value,
            &mut rect,
            DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if GetFocus() == hwnd {
            let _ = DrawFocusRect(dc, &rect);
        }
        SelectObject(dc, old_font);
    }
}
fn paint_button(hwnd: HWND, dc: HDC) {
    unsafe {
        let id = GetDlgCtrlID(hwnd) as u16;
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let link = matches!(id, CONFIG_INFO | ADVANCED);
        let background = if link { COLOR_BTNFACE } else { COLOR_WINDOW };
        FillRect(dc, &rect, crate::appearance::brush(dc, background));
        if !link {
            let old_brush = SelectObject(
                dc,
                crate::appearance::brush(
                    dc,
                    if send(hwnd, BM_GETSTATE, 0, 0).0 & BST_PUSHED as isize != 0 {
                        COLOR_BTNFACE
                    } else {
                        COLOR_WINDOW
                    },
                )
                .into(),
            );
            let pen = CreatePen(
                PS_SOLID,
                1,
                COLORREF(crate::appearance::color(COLOR_3DSHADOW)),
            );
            let old_pen = SelectObject(dc, pen.into());
            let _ = RoundRect(dc, 0, 0, rect.right, rect.bottom, scale(2), scale(2));
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            let _ = DeleteObject(pen.into());
        }
        let old_font = SelectObject(dc, with(|p| p.font).unwrap_or_default().into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(
            dc,
            COLORREF(crate::appearance::color(
                if !IsWindowEnabled(hwnd).as_bool() {
                    COLOR_GRAYTEXT
                } else if link {
                    COLOR_HOTLIGHT
                } else {
                    COLOR_WINDOWTEXT
                },
            )),
        );
        let mut value: Vec<_> = text(hwnd).encode_utf16().collect();
        let _ = DrawTextW(
            dc,
            &mut value,
            &mut rect,
            DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | if link { DT_LEFT } else { DT_CENTER },
        );
        if GetFocus() == hwnd {
            rect.left += scale(3);
            rect.top += scale(3);
            rect.right -= scale(3);
            rect.bottom -= scale(3);
            let _ = DrawFocusRect(dc, &rect);
        }
        SelectObject(dc, old_font);
    }
}
fn scale(n: i32) -> i32 {
    n * with(|p| p.dpi).unwrap_or(96) as i32 / 96
}
fn text(hwnd: HWND) -> String {
    unsafe {
        let mut b = vec![0; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let n = GetWindowTextW(hwnd, &mut b);
        String::from_utf16_lossy(&b[..n.max(0) as usize])
    }
}
fn label(hwnd: HWND, value: &str) {
    let value = crate::i18n::text(value);
    if text(hwnd) != value {
        let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(wide(value).as_ptr())) };
    }
}
fn send(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(lp))) }
}

pub(crate) fn install(instance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    INSTANCE.store(instance.0 as isize, std::sync::atomic::Ordering::Relaxed);
    if !handle().is_invalid() && unsafe { IsWindow(Some(handle())).as_bool() } {
        return Ok(());
    }
    shutdown();
    unsafe {
        for (name, proc) in [
            (
                CLASS,
                Some(wndproc as unsafe extern "system" fn(_, _, _, _) -> _),
            ),
            (
                VIEW,
                Some(viewproc as unsafe extern "system" fn(_, _, _, _) -> _),
            ),
        ] {
            let class_name = wide(name);
            let wc = WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: instance,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(windows::core::Error::from_thread().into());
            }
        }
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let result = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            PCWSTR(wide(CLASS).as_ptr()),
            PCWSTR(
                wide(&crate::ui_format!(
                    "设置 — minicon-winmgr {}",
                    "Settings — minicon-winmgr {}",
                    env!("CARGO_PKG_VERSION")
                ))
                .as_ptr(),
            ),
            WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            540,
            700,
            None,
            None,
            Some(instance),
            None,
        );
        SetThreadDpiAwarenessContext(previous);
        let hwnd = result?;
        let view = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            PCWSTR(wide(VIEW).as_ptr()),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
            0,
            0,
            1,
            1,
            Some(hwnd),
            None,
            Some(instance),
            None,
        )?;
        PANEL.with(|cell| {
            *cell.borrow_mut() = Some(Panel {
                hwnd,
                view,
                instance,
                controls: Vec::new(),
                font: HFONT::default(),
                heading_font: HFONT::default(),
                title_font: HFONT::default(),
                dpi: 0,
                scroll: 0,
                content: 0,
                viewport: 0,
                last_focus: HWND::default(),
                layout: false,
                error: None,
                anchor: None,
                advanced: false,
                cards: Vec::new(),
            })
        });
        build()?;
        refresh();
        layout();
        SetTimer(Some(hwnd), POLL, 100, None);
    }
    Ok(())
}

fn add(
    id: u16,
    title: &str,
    height: i32,
    style: WINDOW_STYLE,
    footer: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let style = if matches!(id, 6211 | 6212 | 6213 | 6220 | 6202) {
        style | WINDOW_STYLE(0x0200 /* SS_CENTERIMAGE */)
    } else {
        style
    };
    let (parent, instance) = with(|p| (if footer { p.hwnd } else { p.view }, p.instance)).unwrap();
    let button = style.0 & WS_TABSTOP.0 != 0
        || matches!(
            id,
            FULL | ALWAYS | PID | DARK | AUTO | BOTTOM | LEFT | RIGHT | CHINESE
        );
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(wide(if button { "BUTTON" } else { "STATIC" }).as_ptr()),
            PCWSTR(wide(title).as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            1,
            1,
            Some(parent),
            Some(HMENU(id as usize as *mut _)),
            Some(instance),
            None,
        )
    }?;
    if button {
        unsafe { SetWindowSubclass(hwnd, Some(childproc), 1, 0) }.ok()?;
    }
    with(|p| {
        p.controls.push(Control {
            id,
            caption: title.into(),
            hwnd,
            height,
            top: 0,
            laid_height: height,
            footer,
        })
    });
    Ok(())
}
fn build() -> Result<(), Box<dyn std::error::Error>> {
    let shell_status = crate::settings_ui::shell_status_text();
    let check = WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32 | BS_MULTILINE as u32);
    let radio = WINDOW_STYLE(BS_AUTORADIOBUTTON as u32 | BS_MULTILINE as u32);
    let push = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32 | BS_MULTILINE as u32);
    let note = WINDOW_STYLE(0x0080 /* SS_NOPREFIX; SS_LEFT = 0 */);
    for (id, title, height, style) in [
        (6190, crate::i18n::text("设置"), 34, note),
        (6191, "", 22, note),
        (6210, crate::i18n::text("外观与位置"), 26, note),
        (6213, crate::i18n::text("语言"), 24, note),
        (ENGLISH, "English", 30, radio | WS_GROUP | WS_TABSTOP),
        (CHINESE, "中文", 30, radio),
        (6312, "", 22, note),
        (6211, crate::i18n::text("颜色主题"), 24, note),
        (
            LIGHT,
            crate::i18n::text("白天"),
            30,
            radio | WS_GROUP | WS_TABSTOP,
        ),
        (DARK, crate::i18n::text("黑夜"), 30, radio),
        (AUTO, crate::i18n::text("自动"), 30, radio),
        (6310, "", 22, note),
        (6212, crate::i18n::text("任务栏位置"), 24, note),
        (
            TOP,
            crate::i18n::text("上方"),
            30,
            radio | WS_GROUP | WS_TABSTOP,
        ),
        (BOTTOM, crate::i18n::text("下方"), 30, radio),
        (LEFT, crate::i18n::text("左侧"), 30, radio),
        (RIGHT, crate::i18n::text("右侧"), 30, radio),
        (6311, "", 22, note),
        (6200, crate::i18n::text("任务与窗口"), 26, note),
        (SHOW, crate::i18n::text("显示任务栏"), 30, check | WS_GROUP),
        (
            TRAY,
            crate::i18n::text("显示通知区域"),
            30,
            check | WS_GROUP,
        ),
        (6301, "", 24, note),
        (6302, "", 24, note),
        (6201, "窗口行为", 28, note),
        (6220, crate::i18n::text("合并窗口"), 24, note),
        (
            NEVER,
            MergeStrategy::Never.label(),
            30,
            radio | WS_GROUP | WS_TABSTOP,
        ),
        (FULL, crate::i18n::text("拥挤时合并"), 30, radio),
        (ALWAYS, MergeStrategy::Always.label(), 30, radio),
        (6303, "", 22, note),
        (6202, crate::i18n::text("合并依据"), 26, note),
        (
            EXE,
            crate::i18n::text("按应用"),
            30,
            radio | WS_GROUP | WS_TABSTOP,
        ),
        (PID, crate::i18n::text("按进程"), 30, radio),
        (6304, "", 22, note),
        (
            PREVIEW,
            crate::i18n::text("悬停显示窗口缩略图"),
            30,
            check | WS_GROUP,
        ),
        (6305, "", 22, note),
        (UPDATE_STATUS, "", 26, note),
        (
            CHECK_UPDATE,
            crate::i18n::text("检查更新"),
            30,
            push | WS_GROUP,
        ),
        (DOWNLOAD_UPDATE, crate::i18n::text("更新"), 30, push),
        (RESTART_UPDATE, crate::i18n::text("重启"), 30, push),
        (
            CONFIG_INFO,
            crate::i18n::text("配置与关于…"),
            32,
            push | WS_GROUP,
        ),
        (
            ADVANCED,
            crate::i18n::text("展开桌面管理…"),
            32,
            push | WS_GROUP,
        ),
        (6307, &shell_status, 84, note),
        (REPLACE, crate::i18n::text("接管桌面…"), 34, push | WS_GROUP),
        (ROLLBACK, crate::i18n::text("恢复接管前设置…"), 34, push),
    ] {
        add(id, title, height, style, false)?;
    }
    add(STATUS, crate::i18n::text("更改自动保存"), 52, note, true)?;
    add(RETRY, crate::i18n::text("重试保存"), 30, push, true)?;
    add(DETAIL, crate::i18n::text("查看详情"), 30, push, true)?;
    add(CLOSE, crate::i18n::text("关闭"), 30, push, true)?;
    Ok(())
}

pub(crate) fn toggle_from(anchor: HWND) {
    with(|p| p.anchor = Some(anchor));
    let hwnd = handle();
    if unsafe { IsWindowVisible(hwnd).as_bool() } && unsafe { GetForegroundWindow() } == hwnd {
        hide();
    } else {
        show();
    }
}
pub(crate) fn show() {
    refresh_shell_status();
    if !unsafe { IsWindow(Some(handle())).as_bool() } {
        let raw = INSTANCE.load(std::sync::atomic::Ordering::Relaxed);
        if raw == 0 {
            return;
        }
        if let Err(error) = install(HINSTANCE(raw as *mut _)) {
            info_box(&crate::ui_format!(
                "无法创建设置窗口：{error}",
                "Unable to create the settings window: {error}"
            ));
            return;
        }
    }
    let hwnd = handle();
    if hwnd.is_invalid() {
        return;
    }
    crate::hub::cancel_hover();
    fit(true);
    refresh();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    unsafe {
        crate::win_util::force_foreground(hwnd);
    }
    let last = with(|p| p.last_focus).unwrap_or_default();
    let focus = if usable(last) { last } else { first_focus() };
    unsafe {
        let _ = SetFocus(Some(focus));
    }
    ensure_focus();
}
fn hide() {
    crate::settings_info::close();
    let focus = unsafe { GetFocus() };
    if unsafe { IsChild(handle(), focus).as_bool() } {
        with(|p| p.last_focus = focus);
    }
    crate::hub::cancel_hover();
    unsafe {
        let _ = ShowWindow(handle(), SW_HIDE);
    }
}
fn usable(hwnd: HWND) -> bool {
    !hwnd.is_invalid()
        && unsafe { IsChild(handle(), hwnd).as_bool() }
        && unsafe { IsWindowEnabled(hwnd).as_bool() }
        && unsafe { IsWindowVisible(hwnd).as_bool() }
}
fn first_focus() -> HWND {
    with(|p|p.controls.iter().find(|c|unsafe{IsWindowEnabled(c.hwnd).as_bool()} && unsafe{GetWindowLongW(c.hwnd,GWL_STYLE)} as u32 & WS_TABSTOP.0 !=0).map(|c|c.hwnd).unwrap_or_default()).unwrap_or_default()
}
fn enabled(id: u16, on: bool) {
    let hwnd = control(id);
    if unsafe { IsWindowEnabled(hwnd).as_bool() } != on {
        unsafe {
            let _ = EnableWindow(hwnd, on);
        }
    }
}
fn source_note(key: &'static str, reason: Option<&str>) -> String {
    match (config::env_source(key), reason) {
        (Some(_), Some(reason)) => {
            format!("{} · {reason}", crate::i18n::text("此项由启动配置固定"))
        }
        (Some(_), None) => crate::i18n::text("此项由启动配置固定").to_string(),
        (None, Some(reason)) => reason.to_string(),
        (None, None) => String::new(),
    }
}
fn refresh() {
    crate::settings_ui::complete_shell_operation();
    crate::update::poll();
    label(
        handle(),
        &crate::ui_format!(
            "设置 — minicon-winmgr {}",
            "Settings — minicon-winmgr {}",
            env!("CARGO_PKG_VERSION")
        ),
    );
    let cfg = config::current();
    let controls = with(|p| p.controls.clone()).unwrap_or_default();
    for c in controls {
        if !c.caption.is_empty() && c.id != 6307 {
            label(c.hwnd, &c.caption);
        }
    }
    label(
        control(ADVANCED),
        if with(|p| p.advanced).unwrap_or(false) {
            crate::i18n::text("收起桌面管理")
        } else {
            crate::i18n::text("展开桌面管理…")
        },
    );
    let actual = crate::hub::taskbar_running();
    label(control(6191), crate::i18n::text("外观与日常行为"));
    let previous_focus = unsafe { GetFocus() };
    for (id, value) in [
        (ENGLISH, cfg.language == crate::i18n::Language::En),
        (CHINESE, cfg.language == crate::i18n::Language::Zh),
        (LIGHT, cfg.theme_mode == ThemeMode::Light),
        (DARK, cfg.theme_mode == ThemeMode::Dark),
        (AUTO, cfg.theme_mode == ThemeMode::Auto),
        (TOP, cfg.position == Position::Top),
        (BOTTOM, cfg.position == Position::Bottom),
        (LEFT, cfg.position == Position::Left),
        (RIGHT, cfg.position == Position::Right),
        (SHOW, config::preferred_taskbar()),
        (TRAY, cfg.show_tray),
        (NEVER, cfg.merge == MergeStrategy::Never),
        (FULL, cfg.merge == MergeStrategy::WhenFull),
        (ALWAYS, cfg.merge == MergeStrategy::Always),
        (EXE, cfg.group_by == GroupBy::Exe),
        (PID, cfg.group_by == GroupBy::Pid),
        (PREVIEW, cfg.hover_preview),
    ] {
        send(control(id), BM_SETCHECK, usize::from(value), 0);
    }
    for (ids, key, dependency) in [
        (&[ENGLISH, CHINESE][..], keys::LANGUAGE, true),
        (&[LIGHT, DARK, AUTO][..], keys::THEME_MODE, true),
        (
            &[TOP, BOTTOM, LEFT, RIGHT][..],
            keys::TASKBAR_POSITION,
            true,
        ),
        (&[SHOW][..], keys::TASKBAR_SHOW, true),
        (&[TRAY][..], keys::TASKBAR_TRAY, actual),
        (&[NEVER, FULL, ALWAYS][..], keys::TASKBAR_GROUP, actual),
        (
            &[EXE, PID][..],
            keys::TASKBAR_GROUP_BY,
            actual && cfg.merge != MergeStrategy::Never,
        ),
        (&[PREVIEW][..], keys::HOVER_PREVIEW, actual),
    ] {
        for &id in ids {
            enabled(id, dependency && config::env_source(key).is_none());
        }
    }
    // Exactly one tab stop per radio group, including environment-disabled groups.
    for (id, on) in [
        (ENGLISH, cfg.language == crate::i18n::Language::En),
        (CHINESE, cfg.language == crate::i18n::Language::Zh),
        (LIGHT, cfg.theme_mode == ThemeMode::Light),
        (DARK, cfg.theme_mode == ThemeMode::Dark),
        (AUTO, cfg.theme_mode == ThemeMode::Auto),
        (TOP, cfg.position == Position::Top),
        (BOTTOM, cfg.position == Position::Bottom),
        (LEFT, cfg.position == Position::Left),
        (RIGHT, cfg.position == Position::Right),
        (NEVER, cfg.merge == MergeStrategy::Never),
        (FULL, cfg.merge == MergeStrategy::WhenFull),
        (ALWAYS, cfg.merge == MergeStrategy::Always),
        (EXE, cfg.group_by == GroupBy::Exe),
        (PID, cfg.group_by == GroupBy::Pid),
    ] {
        unsafe {
            let style = GetWindowLongW(control(id), GWL_STYLE) as u32;
            SetWindowLongW(
                control(id),
                GWL_STYLE,
                ((style & !WS_TABSTOP.0) | if on { WS_TABSTOP.0 } else { 0 }) as i32,
            );
        }
    }
    label(control(6312), &source_note(keys::LANGUAGE, None));
    label(control(6310), &source_note(keys::THEME_MODE, None));
    label(control(6311), &source_note(keys::TASKBAR_POSITION, None));
    let pending = config::preferred_taskbar() != actual;
    label(
        control(6301),
        &source_note(
            keys::TASKBAR_SHOW,
            pending.then_some(if actual {
                crate::i18n::text("重启后隐藏；本次仍显示任务栏")
            } else {
                crate::i18n::text("重启后显示；本次未显示任务栏")
            }),
        ),
    );
    label(
        control(6302),
        &source_note(
            keys::TASKBAR_TRAY,
            (!actual).then_some(crate::i18n::text("本次无任务栏")),
        ),
    );
    label(
        control(6303),
        &source_note(
            keys::TASKBAR_GROUP,
            (!actual).then_some(crate::i18n::text("本次无任务栏")),
        ),
    );
    label(
        control(6304),
        &source_note(
            keys::TASKBAR_GROUP_BY,
            if !actual {
                Some(crate::i18n::text("本次无任务栏"))
            } else {
                None
            },
        ),
    );
    label(
        control(6305),
        &source_note(
            keys::HOVER_PREVIEW,
            (!actual).then_some(crate::i18n::text("本次无任务栏")),
        ),
    );
    let update = crate::update::view();
    use crate::update::Phase;
    let version = update
        .version
        .as_deref()
        .unwrap_or(env!("CARGO_PKG_VERSION"));
    let update_text = match update.phase {
        Phase::Recovered => crate::i18n::text("已恢复旧版本，可重新检查更新。").into(),
        Phase::Idle => crate::ui_format!("版本 {}", "Version {}", env!("CARGO_PKG_VERSION")),
        Phase::Checking => crate::i18n::text("正在检查更新…").into(),
        Phase::Current => crate::ui_format!(
            "{} · 已是最新版本",
            "{} · Up to date",
            env!("CARGO_PKG_VERSION")
        ),
        Phase::Available => format!("{} → {version}", env!("CARGO_PKG_VERSION")),
        Phase::Downloading => crate::ui_format!(
            "正在下载… {:.1} MiB",
            "Downloading… {:.1} MiB",
            update.bytes as f64 / 1048576.0
        ),
        Phase::Ready if update.error.is_some() => crate::i18n::text("重启未完成，请重试。").into(),
        Phase::Ready => crate::ui_format!("{} 已就绪，重启后更新", "{} ready to restart", version),
        Phase::Restarting => crate::i18n::text("正在重启…").into(),
        Phase::Failed
            if update
                .error
                .as_deref()
                .is_some_and(|v| v.starts_with("installation directory is not writable")) =>
        {
            crate::i18n::text("此处无法更新，请下载发布文件后手动替换。").into()
        }
        Phase::Failed => crate::i18n::text("更新未完成，请重试。").into(),
    };
    label(control(UPDATE_STATUS), &update_text);
    enabled(
        CHECK_UPDATE,
        !matches!(
            update.phase,
            Phase::Checking | Phase::Downloading | Phase::Ready | Phase::Restarting
        ),
    );
    enabled(DOWNLOAD_UPDATE, crate::update::can_download());
    enabled(
        RESTART_UPDATE,
        update.phase == Phase::Ready && !crate::settings_store::status().dirty(),
    );
    let status = crate::settings_store::status();
    let message = if let Some(error) = with(|p| p.error.clone()).flatten() {
        error
    } else if status.failed > 0 {
        crate::ui_format!(
            "本次修改已保留，{} 项保存失败；下次启动尚未保存。请重试。",
            "Applied to this session. {} setting(s) could not be saved for next start. Please retry.",
            status.failed
        )
    } else if status.pending > 0 {
        crate::i18n::text("本次修改已保留，正在保存…").into()
    } else if status.saved && pending {
        crate::i18n::text("已保存，任务栏显示设置重启后生效。").into()
    } else if status.saved {
        crate::i18n::text("已保存").into()
    } else {
        crate::i18n::text("更改自动保存").into()
    };
    label(control(STATUS), &message);
    enabled(RETRY, status.failed > 0);
    enabled(DETAIL, status.failed > 0);
    let visibility_changed =
        unsafe { IsWindowVisible(control(RETRY)).as_bool() } != (status.failed > 0);
    for id in [RETRY, DETAIL] {
        unsafe {
            let _ = ShowWindow(
                control(id),
                if status.failed > 0 { SW_SHOW } else { SW_HIDE },
            );
        }
    }
    if visibility_changed {
        layout();
    }
    if !previous_focus.is_invalid()
        && unsafe { IsChild(handle(), previous_focus).as_bool() }
        && !unsafe { IsWindowEnabled(previous_focus).as_bool() }
    {
        let next =
            unsafe { GetNextDlgTabItem(handle(), Some(previous_focus), false) }.unwrap_or_default();
        unsafe {
            let _ = SetFocus(Some(if next.is_invalid() {
                control(CLOSE)
            } else {
                next
            }));
        }
    }
}

fn apply(id: u16) {
    with(|p| p.error = None);
    let cfg = config::current();
    match id {
        CHECK_UPDATE => crate::update::check(),
        DOWNLOAD_UPDATE => crate::update::download(),
        RESTART_UPDATE => crate::update::restart(),
        ENGLISH | CHINESE => {
            config::set_language(if id == ENGLISH {
                crate::i18n::Language::En
            } else {
                crate::i18n::Language::Zh
            });
            refresh_shell_status();
        }
        LIGHT | DARK | AUTO => {
            config::set_theme_mode(match id {
                LIGHT => ThemeMode::Light,
                DARK => ThemeMode::Dark,
                _ => ThemeMode::Auto,
            });
        }
        TOP | BOTTOM | LEFT | RIGHT => {
            config::set_position(match id {
                TOP => Position::Top,
                BOTTOM => Position::Bottom,
                LEFT => Position::Left,
                _ => Position::Right,
            });
        }
        ADVANCED => {
            refresh_shell_status();
            let expanded = with(|p| {
                p.advanced = !p.advanced;
                p.advanced
            })
            .unwrap_or(false);
            label(
                control(ADVANCED),
                if expanded {
                    crate::i18n::text("收起桌面管理")
                } else {
                    crate::i18n::text("展开桌面管理…")
                },
            );
            layout();
            ensure_focus();
            return;
        }
        SHOW => {
            config::set_show_taskbar(!config::preferred_taskbar());
        }
        TRAY => {
            config::set_show_tray(!cfg.show_tray);
            if !config::current().show_tray {
                crate::tray_popup::hide();
            }
        }
        NEVER => {
            config::set_merge(MergeStrategy::Never);
        }
        FULL => {
            config::set_merge(MergeStrategy::WhenFull);
        }
        ALWAYS => {
            config::set_merge(MergeStrategy::Always);
        }
        EXE => {
            config::set_group_by(GroupBy::Exe);
        }
        PID => {
            config::set_group_by(GroupBy::Pid);
        }
        PREVIEW => {
            if !cfg.hover_preview {
                let instance = with(|p| p.instance).unwrap();
                if let Err(e) = crate::hover_preview::prepare(instance) {
                    with(|p| {
                        p.error = Some(crate::ui_format!(
                            "无法开启预览：{e}",
                            "Unable to enable previews: {e}"
                        ))
                    });
                    refresh();
                    return;
                }
            }
            config::set_hover_preview(!cfg.hover_preview);
            crate::hub::cancel_hover();
        }
        REPLACE | ROLLBACK => {
            if id == REPLACE {
                crate::settings_ui::do_replace();
            } else {
                crate::settings_ui::do_rollback();
            }
            if unsafe { IsWindowVisible(handle()).as_bool() } {
                unsafe {
                    crate::win_util::force_foreground(handle());
                    let _ = SetFocus(Some(control(id)));
                }
            }
        }
        RETRY => crate::settings_store::retry(),
        DETAIL => {
            if let Some(error) = crate::settings_store::status().error {
                info_box(&error);
            }
        }
        CONFIG_INFO => {
            let executable = std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|error| error.to_string());
            let mut message = crate::ui_format!(
                "minicon-winmgr {} · {}\n\n程序路径：{}\n更新保留安装路径和文件名；版本以本页显示为准。\n\n任务栏唤出配置：{}（实际注册结果见日志）\n\n配置来源（启动时）\n",
                "minicon-winmgr {} · {}\n\nExecutable: {}\nUpdates preserve the installation path and filename; the version shown here is authoritative.\n\nTaskbar hotkey: {} (registration results are recorded in the log)\n\nConfiguration sources at startup\n",
                env!("CARGO_PKG_VERSION"),
                crate::privilege::description(),
                executable,
                cfg.hotkey
            );
            for (title, key, value) in [
                (
                    crate::i18n::text("语言"),
                    keys::LANGUAGE,
                    cfg.language.as_key(),
                ),
                (
                    crate::i18n::text("颜色主题"),
                    keys::THEME_MODE,
                    cfg.theme_mode.label(),
                ),
                (
                    crate::i18n::text("任务栏位置"),
                    keys::TASKBAR_POSITION,
                    cfg.position.label(),
                ),
                (
                    crate::i18n::text("任务栏显示（下次启动）"),
                    keys::TASKBAR_SHOW,
                    if config::preferred_taskbar() {
                        crate::i18n::text("开启")
                    } else {
                        crate::i18n::text("关闭")
                    },
                ),
                (
                    crate::i18n::text("通知区域"),
                    keys::TASKBAR_TRAY,
                    if cfg.show_tray {
                        crate::i18n::text("开启")
                    } else {
                        crate::i18n::text("关闭")
                    },
                ),
                (
                    crate::i18n::text("窗口合并"),
                    keys::TASKBAR_GROUP,
                    cfg.merge.label(),
                ),
                (
                    crate::i18n::text("合并依据"),
                    keys::TASKBAR_GROUP_BY,
                    cfg.group_by.label(),
                ),
                (
                    crate::i18n::text("悬停预览"),
                    keys::HOVER_PREVIEW,
                    if cfg.hover_preview {
                        crate::i18n::text("开启")
                    } else {
                        crate::i18n::text("关闭")
                    },
                ),
            ] {
                message.push_str(&crate::ui_format!(
                    "{title}：{value}\n来源：{}\n\n",
                    "{title}: {value}\nSource: {}\n\n",
                    config::source(key)
                ));
            }
            if let Some(error) = crate::update::view().error {
                message.push_str(&crate::ui_format!(
                    "更新详情：{}\n\n",
                    "Update details: {}\n\n",
                    error
                ));
            }
            if let Some(path) = config::settings_path() {
                message.push_str(&crate::ui_format!(
                    "设置保存位置：{}",
                    "Settings file: {}",
                    path.display()
                ));
            }
            if let Err(error) = crate::settings_info::show(handle(), &message) {
                info_box(&crate::ui_format!(
                    "无法打开配置详情：{error}",
                    "Unable to open configuration details: {error}"
                ));
            }
            return;
        }
        CLOSE => {
            hide();
            return;
        }
        _ => return,
    }
    crate::hub::request_taskbar_refresh();
    refresh();
    layout();
}
fn info_box(message: &str) {
    unsafe {
        MessageBoxW(
            Some(handle()),
            PCWSTR(wide(message).as_ptr()),
            PCWSTR(wide(crate::i18n::text("设置")).as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn work_for(hwnd: HWND) -> RECT {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                right: 1024,
                bottom: 768,
                ..Default::default()
            }
        }
    }
}
fn fit(center: bool) {
    let hwnd = handle();
    if hwnd.is_invalid() {
        return;
    }
    let anchor = with(|p| p.anchor.take())
        .flatten()
        .unwrap_or_else(|| unsafe { GetForegroundWindow() });
    let work = work_for(if center && !anchor.is_invalid() {
        anchor
    } else {
        hwnd
    });
    let dpi = unsafe {
        GetDpiForWindow(if center && !anchor.is_invalid() {
            anchor
        } else {
            hwnd
        })
    }
    .max(96);
    let width = (640 * dpi as i32 / 96).min(work.right - work.left).max(1);
    let height = (634 * dpi as i32 / 96).min(work.bottom - work.top).max(1);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            work.left
                + crate::win_util::centered_origin(
                    work.right - work.left,
                    work.bottom - work.top,
                    width,
                    height,
                )
                .0,
            work.top
                + crate::win_util::centered_origin(
                    work.right - work.left,
                    work.bottom - work.top,
                    width,
                    height,
                )
                .1,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    layout();
}

#[derive(Debug)]
struct Geometry {
    margin: i32,
    width: i32,
    footer: i32,
    viewport: i32,
}
fn geometry(width: i32, height: i32, dpi: u32) -> Geometry {
    let scale = |n: i32| n * dpi as i32 / 96;
    let margin = scale(24).min(width.max(1) / 8);
    let footer = scale(64).min((height - 1).max(0));
    Geometry {
        margin,
        width: (width - 2 * margin).max(1),
        footer,
        viewport: (height - footer).max(1),
    }
}
fn layout() {
    if with(|p| {
        if p.layout {
            return true;
        }
        p.layout = true;
        false
    })
    .unwrap_or(true)
    {
        return;
    }
    unsafe {
        let hwnd = handle();
        let dpi = GetDpiForWindow(hwnd).max(96);
        let font = if with(|p| p.dpi != dpi).unwrap_or(false) {
            let mut metrics = NONCLIENTMETRICSW {
                cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
                ..Default::default()
            };
            let _ = SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                metrics.cbSize,
                Some((&mut metrics as *mut NONCLIENTMETRICSW).cast()),
                0,
                dpi,
            );
            metrics.lfMessageFont.lfHeight = -scale_at(14, dpi);
            let face = wide("Microsoft YaHei UI");
            metrics.lfMessageFont.lfFaceName = [0; 32];
            metrics.lfMessageFont.lfFaceName[..face.len()].copy_from_slice(&face);
            let font = CreateFontIndirectW(&metrics.lfMessageFont);
            metrics.lfMessageFont.lfWeight = 600;
            metrics.lfMessageFont.lfHeight = -scale_at(14, dpi);
            let heading = CreateFontIndirectW(&metrics.lfMessageFont);
            metrics.lfMessageFont.lfHeight = -scale_at(25, dpi);
            let title = CreateFontIndirectW(&metrics.lfMessageFont);
            let old = with(|p| {
                let old = [p.font, p.heading_font, p.title_font];
                p.font = font;
                p.heading_font = heading;
                p.title_font = title;
                p.dpi = dpi;
                old
            })
            .unwrap();
            let controls = with(|p| p.controls.clone()).unwrap();
            for c in controls {
                let selected = if c.id == 6190 {
                    title
                } else if is_heading(c.id) {
                    heading
                } else {
                    font
                };
                send(c.hwnd, WM_SETFONT, selected.0 as usize, 1);
            }
            for old in old {
                if !old.is_invalid() {
                    let _ = DeleteObject(old.into());
                }
            }
            font
        } else {
            with(|p| p.font).unwrap()
        };
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let mut g = geometry(client.right, client.bottom, dpi);
        let failed = crate::settings_store::status().failed > 0;
        if failed {
            g.footer = (g.footer + scale(if g.width < scale(420) { 64 } else { 40 }))
                .min((client.bottom - 1).max(0));
            g.viewport = (client.bottom - g.footer).max(1);
        }
        let (controls, view) = with(|p| (p.controls.clone(), p.view)).unwrap();
        let dc = GetDC(Some(hwnd));
        let old = SelectObject(dc, font.into());
        let mut top = scale(16);
        let mut positions = Vec::new();
        let mut row_top = top;
        let horizontal = g.width >= scale(420);
        let compact = g.width >= scale(520);
        for c in controls.iter().filter(|c| !c.footer) {
            if matches!(c.id, 6201 | 6191) {
                let _ = ShowWindow(c.hwnd, SW_HIDE);
                continue;
            }

            if matches!(c.id, REPLACE | ROLLBACK | 6307) {
                let visible = with(|p| p.advanced).unwrap_or(false);
                let _ = ShowWindow(c.hwnd, if visible { SW_SHOW } else { SW_HIDE });
                if !visible {
                    continue;
                }
            }
            if (6301..=6305).contains(&c.id) || matches!(c.id, 6310..=6312) {
                let visible = !text(c.hwnd).is_empty();
                let _ = ShowWindow(c.hwnd, if visible { SW_SHOW } else { SW_HIDE });
                if !visible {
                    continue;
                }
            }
            let row = match c.id {
                ENGLISH | CHINESE if horizontal => Some((ENGLISH, usize::from(c.id == CHINESE), 2)),
                LIGHT | DARK | AUTO if horizontal => Some((
                    LIGHT,
                    [LIGHT, DARK, AUTO]
                        .iter()
                        .position(|id| *id == c.id)
                        .unwrap(),
                    3,
                )),
                TOP | BOTTOM | LEFT | RIGHT if horizontal => Some((
                    TOP,
                    [TOP, BOTTOM, LEFT, RIGHT]
                        .iter()
                        .position(|id| *id == c.id)
                        .unwrap(),
                    4,
                )),
                NEVER | FULL | ALWAYS if horizontal => Some((
                    NEVER,
                    [NEVER, FULL, ALWAYS]
                        .iter()
                        .position(|id| *id == c.id)
                        .unwrap(),
                    3,
                )),
                EXE | PID if horizontal => Some((EXE, usize::from(c.id == PID), 2)),
                SHOW | TRAY if compact => Some((SHOW, usize::from(c.id == TRAY), 2)),
                CHECK_UPDATE | DOWNLOAD_UPDATE | RESTART_UPDATE if horizontal => Some((
                    CHECK_UPDATE,
                    [CHECK_UPDATE, DOWNLOAD_UPDATE, RESTART_UPDATE]
                        .iter()
                        .position(|id| *id == c.id)
                        .unwrap(),
                    3,
                )),
                CONFIG_INFO | ADVANCED if horizontal => {
                    Some((CONFIG_INFO, usize::from(c.id == ADVANCED), 2))
                }
                REPLACE | ROLLBACK if horizontal => {
                    Some((REPLACE, usize::from(c.id == ROLLBACK), 2))
                }
                _ => None,
            };
            if c.id == CONFIG_INFO {
                top += scale(12);
            }
            if c.id == UPDATE_STATUS {
                top += scale(24);
            }
            if matches!(c.id, SHOW | 6213) {
                top += scale(10);
            }
            if matches!(c.id, 6220 | 6202 | PREVIEW | 6212 | 6211) || (c.id == TRAY && !compact) {
                top += scale(6);
            }
            if is_heading(c.id) {
                top += scale(if c.id == 6200 { 18 } else { 8 });
            }
            let inset = if in_card(c.id) { scale(14) } else { 0 };
            let available = (g.width - inset * 2).max(1);
            let inline_label = compact && matches!(c.id, 6211 | 6212 | 6213 | 6220 | 6202);
            let inline_options = compact
                && matches!(
                    c.id,
                    ENGLISH
                        | CHINESE
                        | LIGHT
                        | DARK
                        | AUTO
                        | TOP
                        | BOTTOM
                        | LEFT
                        | RIGHT
                        | NEVER
                        | FULL
                        | ALWAYS
                        | EXE
                        | PID
                );
            if inline_label {
                row_top = top;
            }
            let label_width = scale(94);

            let (x, width) = if let Some((first, index, count)) = row {
                if c.id == first && !inline_options {
                    row_top = top;
                } else {
                    top = row_top;
                }
                let left = if inline_options { label_width } else { 0 };
                let gap = scale(8);
                let width = (available - left - gap * (count - 1)) / count;
                (inset + left + index as i32 * (width + gap), width)
            } else {
                (
                    inset,
                    if inline_label {
                        label_width
                    } else if matches!(c.id, CONFIG_INFO | ADVANCED) {
                        scale(160).min(available)
                    } else {
                        available
                    },
                )
            };
            let selected = if c.id == 6190 {
                with(|p| p.title_font).unwrap()
            } else if is_heading(c.id) {
                with(|p| p.heading_font).unwrap()
            } else {
                font
            };
            SelectObject(dc, selected.into());
            let mut measured = RECT {
                right: (width - scale(24)).max(1),
                ..Default::default()
            };
            let mut value: Vec<u16> = text(c.hwnd).encode_utf16().collect();
            if !value.is_empty() {
                DrawTextW(
                    dc,
                    &mut value,
                    &mut measured,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                );
            }
            // Preferred height is immutable: a narrow intermediate layout must
            // never become the minimum height of all future layouts.
            let height = if inline_label || inline_options {
                scale(30)
            } else {
                scale(c.height).max(measured.bottom + scale(4))
            };
            positions.push((c.id, x, top, width, height));
            top += height;
            if c.id == 6305 {
                top += scale(6);
            }
        }
        top += scale(12);
        SelectObject(dc, old);
        let _ = ReleaseDC(Some(hwnd), dc);
        let scroll = with(|p| {
            p.content = top;
            p.viewport = g.viewport;
            p.scroll = p.scroll.clamp(0, (top - g.viewport).max(0));
            p.scroll
        })
        .unwrap();
        let cards = [
            (6213, RIGHT),
            (SHOW, PREVIEW),
            (UPDATE_STATUS, RESTART_UPDATE),
        ]
        .into_iter()
        .filter_map(|(start, end)| {
            let a = positions.iter().find(|c| c.0 == start)?;
            let b = positions.iter().rev().find(|c| {
                c.0 == end || (end == PREVIEW && c.0 == 6305) || (end == RIGHT && c.0 == 6311)
            })?;
            Some(RECT {
                left: 0,
                top: a.2 - scale(10) - scroll,
                right: g.width,
                bottom: b.2 + b.4 + scale(10) - scroll,
            })
        })
        .collect();
        with(|p| p.cards = cards);
        let _ = MoveWindow(view, g.margin, 0, g.width, g.viewport, true);
        for (id, x, top, width, height) in positions {
            let _ = MoveWindow(control(id), x, top - scroll, width, height, true);
            with(|p| {
                if let Some(c) = p.controls.iter_mut().find(|c| c.id == id) {
                    c.top = top;
                    c.laid_height = height;
                }
            });
        }
        let y = client.bottom - g.footer + scale(12);
        let bw = scale(90).min(g.width / 3);
        let by = (client.bottom - scale(48)).max(y + 1);
        let _ = MoveWindow(
            control(STATUS),
            g.margin,
            by + scale(6),
            (g.width - bw - scale(16)).max(1),
            scale(34).min(client.bottom - by - scale(6)),
            true,
        );
        let _ = MoveWindow(
            control(CLOSE),
            g.margin + g.width - bw,
            by,
            bw,
            scale(32),
            true,
        );
        for (i, id) in [RETRY, DETAIL].iter().enumerate() {
            let _ = MoveWindow(
                control(*id),
                g.margin + i as i32 * (bw + scale(8)),
                by,
                bw,
                scale(32),
                true,
            );
        }
        if failed {
            let _ = MoveWindow(
                control(STATUS),
                g.margin,
                y,
                g.width,
                (by - y - scale(6)).max(1),
                true,
            );
        }
        SetScrollInfo(
            hwnd,
            SB_VERT,
            &SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_ALL,
                nMin: 0,
                nMax: (top - 1).max(0),
                nPage: g.viewport as u32,
                nPos: scroll,
                ..Default::default()
            },
            true,
        );
        let _ = InvalidateRect(Some(view), None, true);
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
    with(|p| p.layout = false);
    // SetScrollInfo can show/hide the native scrollbar and synchronously send
    // WM_SIZE while layout is guarded. Reflow once the client width settles.
    unsafe {
        let hwnd = handle();
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut client = RECT::default();
        let mut view = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let _ = GetClientRect(with(|p| p.view).unwrap(), &mut view);
        if view.right != geometry(client.right, client.bottom, dpi).width {
            let _ = PostMessageW(Some(hwnd), REFLOW, WPARAM(0), LPARAM(0));
        }
    }
}
fn ensure_focus() {
    let focus = unsafe { GetFocus() };
    let changed = with(|p| {
        let Some(c) = p.controls.iter().find(|c| c.hwnd == focus && !c.footer) else {
            return false;
        };
        let height = c.laid_height;
        let next = if c.top < p.scroll {
            c.top
        } else if c.top + height > p.scroll + p.viewport {
            c.top + height - p.viewport
        } else {
            p.scroll
        };
        let changed = next != p.scroll;
        p.scroll = next;
        changed
    })
    .unwrap_or(false);
    if changed {
        layout();
    }
}
pub(crate) fn filter(msg: &MSG) -> bool {
    if crate::diagnostic_ui::filter(msg) {
        return true;
    }
    if crate::settings_info::filter(msg) {
        return true;
    }
    let hwnd = handle();
    if hwnd.is_invalid() || !unsafe { IsWindowVisible(hwnd).as_bool() } {
        return false;
    }
    if msg.hwnd != hwnd && !unsafe { IsChild(hwnd, msg.hwnd).as_bool() } {
        return false;
    }
    if msg.message == WM_KEYDOWN {
        if msg.wParam.0 == VK_ESCAPE.0 as usize {
            hide();
            return true;
        }
        if msg.wParam.0 == VK_RETURN.0 as usize {
            let id = unsafe { GetDlgCtrlID(msg.hwnd) } as u16;
            if [
                CHECK_UPDATE,
                DOWNLOAD_UPDATE,
                RESTART_UPDATE,
                REPLACE,
                ROLLBACK,
                RETRY,
                DETAIL,
                CLOSE,
                CONFIG_INFO,
                ADVANCED,
            ]
            .contains(&id)
            {
                apply(id);
            }
            return true;
        }
    }
    unsafe { IsDialogMessageW(hwnd, msg).as_bool() }
}
pub(crate) fn allow_exit() -> bool {
    if !crate::settings_store::status().dirty() {
        return true;
    }
    let answer = unsafe {
        MessageBoxW(
            Some(handle()),
            PCWSTR(
                wide(crate::i18n::text(
                    "仍有设置尚未保存。\n选择“是”放弃未保存修改并退出；选择“否”返回设置重试。",
                ))
                .as_ptr(),
            ),
            PCWSTR(wide(crate::i18n::text("设置尚未保存")).as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if answer == IDYES {
        true
    } else {
        show();
        false
    }
}
pub(crate) fn shutdown() {
    crate::diagnostic_ui::shutdown();
    if let Some(p) = PANEL.with(|cell| cell.borrow_mut().take()) {
        unsafe {
            let _ = DestroyWindow(p.hwnd);
            for font in [p.font, p.heading_font, p.title_font] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font.into());
                }
            }
        }
    }
}

unsafe extern "system" fn childproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    guard_abi(
        "settings child",
        || {
            let id = unsafe { GetDlgCtrlID(hwnd) } as u16;
            if matches!(
                id,
                CHECK_UPDATE
                    | DOWNLOAD_UPDATE
                    | RESTART_UPDATE
                    | CONFIG_INFO
                    | ADVANCED
                    | CLOSE
                    | RETRY
                    | DETAIL
                    | REPLACE
                    | ROLLBACK
            ) || choice(id)
            {
                if msg == WM_PAINT {
                    unsafe {
                        let mut paint = PAINTSTRUCT::default();
                        let dc = BeginPaint(hwnd, &mut paint);
                        if choice(id) {
                            paint_choice(hwnd, dc)
                        } else {
                            paint_button(hwnd, dc)
                        }
                        let _ = EndPaint(hwnd, &paint);
                    }
                    return LRESULT(0);
                }
                if msg == WM_PRINTCLIENT {
                    if choice(id) {
                        paint_choice(hwnd, HDC(wp.0 as *mut _))
                    } else {
                        paint_button(hwnd, HDC(wp.0 as *mut _))
                    }
                    return LRESULT(0);
                }
            }
            if msg == WM_SETFOCUS {
                let _ = unsafe { PostMessageW(Some(handle()), ENSURE_FOCUS, WPARAM(0), LPARAM(0)) };
            }
            unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
        },
        || unsafe { DefSubclassProc(hwnd, msg, wp, lp) },
    )
}
unsafe extern "system" fn viewproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "settings viewport",
        || {
            if msg == WM_PAINT {
                return paint_surface(hwnd, true);
            }
            if matches!(msg, WM_CTLCOLORSTATIC | WM_CTLCOLORBTN) {
                return control_color(wp, lp);
            }
            if matches!(msg, WM_COMMAND | WM_MOUSEWHEEL) {
                return send(handle(), msg, wp.0, lp.0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        },
        || unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    )
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "settings",
        || match msg {
            WM_PAINT => paint_surface(hwnd, false),
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => control_color(wp, lp),
            WM_CLOSE => {
                hide();
                LRESULT(0)
            }
            WM_COMMAND => {
                if wp.0 >> 16 == BN_CLICKED as usize {
                    let id = (wp.0 & 0xffff) as u16;
                    if unsafe { IsWindowEnabled(control(id)).as_bool() } {
                        apply(id);
                    }
                }
                LRESULT(0)
            }
            WM_SIZE => {
                layout();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                unsafe {
                    let r = *(lp.0 as *const RECT);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                layout();
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_THEMECHANGED => {
                crate::appearance::refresh();
                fit(false);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == POLL => {
                refresh();
                LRESULT(0)
            }
            REFLOW => {
                layout();
                LRESULT(0)
            }
            ENSURE_FOCUS => {
                ensure_focus();
                LRESULT(0)
            }
            WM_VSCROLL | WM_MOUSEWHEEL => {
                let mut info = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    ..Default::default()
                };
                let _ = unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) };
                let page = with(|p| p.viewport).unwrap_or(100);
                let delta = if msg == WM_MOUSEWHEEL {
                    -((wp.0 >> 16) as u16 as i16 as i32) * scale(72) / 120
                } else {
                    match (wp.0 & 0xffff) as u32 {
                        0 => -scale(32),
                        1 => scale(32),
                        2 => -page,
                        3 => page,
                        5 | 4 => info.nTrackPos - with(|p| p.scroll).unwrap_or(0),
                        6 => -i32::MAX / 2,
                        7 => i32::MAX / 2,
                        _ => 0,
                    }
                };
                with(|p| p.scroll = (p.scroll + delta).clamp(0, (p.content - p.viewport).max(0)));
                layout();
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        },
        || unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_focus_and_escape_remain_responsive_during_two_second_save() {
        let events = crate::settings_store::install_delayed_test_writer();
        let module =
            unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }.unwrap();
        install(HINSTANCE(module.0)).unwrap();
        show();
        let preferred: Vec<_> = with(|p| p.controls.iter().map(|c| c.height).collect()).unwrap();
        unsafe {
            let _ = MoveWindow(handle(), 40, 40, 640, 550, true);
        }
        layout();
        let original = with(|p| p.content).unwrap();
        for _ in 0..5 {
            unsafe {
                let _ = MoveWindow(handle(), 40, 40, 180, 400, true);
            }
            layout();
            unsafe {
                let _ = MoveWindow(handle(), 40, 40, 640, 550, true);
            }
            layout();
            assert_eq!(with(|p| p.content).unwrap(), original);
        }
        assert_eq!(
            with(|p| p.controls.iter().map(|c| c.height).collect::<Vec<_>>()).unwrap(),
            preferred
        );
        assert!(config::set_show_taskbar(!config::preferred_taskbar()));
        assert!(
            events
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
        );
        let focused = unsafe { GetFocus() };
        let start = std::time::Instant::now();
        assert!(filter(&MSG {
            hwnd: focused,
            message: WM_KEYDOWN,
            wParam: WPARAM(VK_TAB.0 as usize),
            ..Default::default()
        }));
        assert_ne!(unsafe { GetFocus() }, focused);
        assert!(filter(&MSG {
            hwnd: unsafe { GetFocus() },
            message: WM_KEYDOWN,
            wParam: WPARAM(VK_ESCAPE.0 as usize),
            ..Default::default()
        }));
        assert!(!unsafe { IsWindowVisible(handle()).as_bool() });
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        assert!(
            !events
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
        );
        shutdown();
    }
    #[test]
    fn small_workareas_keep_content_and_footer_accessible() {
        for dpi in [96, 144, 192] {
            for (w, h) in [(640, 440), (420, 300), (900, 700)] {
                let g = geometry(w, h, dpi);
                assert!(g.viewport > 0 && g.footer > 0);
                assert_eq!(g.viewport + g.footer, h);
                assert!(g.width + 2 * g.margin <= w);
            }
        }
    }
}
