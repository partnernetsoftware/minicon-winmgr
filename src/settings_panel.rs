//! Native settings controls, scrolling, focus and DPI. All window ownership is
//! confined to the shell message thread; the persistence worker never sees HWNDs.
use crate::config::{self, GroupBy, MergeStrategy, keys};
use crate::win_util::{guard_abi, wide};
use std::cell::RefCell;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{
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
const ENSURE_FOCUS: u32 = WM_APP + 140;
const POLL: usize = 90;
static INSTANCE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
#[derive(Clone)]
struct Control {
    id: u16,
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
    matches!(id, 6200 | 6201 | 6203 | 6204 | 6205)
}
fn control_color(wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let dc = HDC(wp.0 as *mut _);
        let id = GetDlgCtrlID(HWND(lp.0 as *mut _));
        let color = if id == STATUS as i32 && crate::settings_store::status().failed > 0 {
            COLOR_HOTLIGHT
        } else if id == 6191 || (6301..=6307).contains(&id) || id == STATUS as i32 {
            COLOR_GRAYTEXT
        } else {
            COLOR_WINDOWTEXT
        };
        let color = if color == COLOR_GRAYTEXT {
            let foreground = GetSysColor(COLOR_WINDOWTEXT);
            let background = GetSysColor(COLOR_WINDOW);
            let blend = |shift: u32| {
                (((foreground >> shift) & 255u32) * 3 + ((background >> shift) & 255u32) * 2) / 5
            };
            blend(0) | (blend(8) << 8) | (blend(16) << 16)
        } else {
            GetSysColor(color)
        };
        SetTextColor(dc, COLORREF(color));
        SetBkColor(dc, COLORREF(GetSysColor(COLOR_WINDOW)));
        LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
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
            let wc = WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: instance,
                lpszClassName: PCWSTR(wide(name).as_ptr()),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
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
            PCWSTR(wide("设置 — minicon-winmgr").as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VSCROLL,
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
    let (parent, instance) = with(|p| (if footer { p.hwnd } else { p.view }, p.instance)).unwrap();
    let button = style.0 & WS_TABSTOP.0 != 0 || matches!(id, FULL | ALWAYS | PID);
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
    let check = WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32 | BS_MULTILINE as u32);
    let radio = WINDOW_STYLE(BS_AUTORADIOBUTTON as u32 | BS_MULTILINE as u32);
    let push = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32 | BS_MULTILINE as u32);
    let note = WINDOW_STYLE(0x0080 /* SS_NOPREFIX; SS_LEFT = 0 */);
    for (id, title, height, style) in [
        (6190, "设置", 38, note),
        (6191, "调整任务栏、窗口与预览", 26, note),
        (6200, "任务栏", 28, note),
        (SHOW, "显示任务栏", 30, check | WS_GROUP),
        (6301, "", 24, note),
        (TRAY, "显示通知区域", 30, check | WS_GROUP),
        (6302, "", 24, note),
        (6201, "窗口合并", 28, note),
        (
            NEVER,
            MergeStrategy::Never.label(),
            30,
            radio | WS_GROUP | WS_TABSTOP,
        ),
        (FULL, "放不下时合并", 30, radio),
        (ALWAYS, MergeStrategy::Always.label(), 30, radio),
        (6303, "", 22, note),
        (6202, "合并依据", 26, note),
        (EXE, GroupBy::Exe.label(), 30, radio | WS_GROUP | WS_TABSTOP),
        (PID, GroupBy::Pid.label(), 30, radio),
        (6304, "", 22, note),
        (6203, "预览", 28, note),
        (PREVIEW, "悬停显示窗口缩略图", 30, check | WS_GROUP),
        (6305, "", 22, note),
        (6205, "桌面管理", 28, note),
        (6307, "更改 Windows 桌面外壳，操作前会再次确认。", 26, note),
        (REPLACE, "接管桌面…", 34, push | WS_GROUP),
        (ROLLBACK, "还原系统桌面…", 34, push),
        (6204, "关于", 28, note),
        (6306, "", 56, note),
    ] {
        add(id, title, height, style, false)?;
    }
    add(STATUS, "更改自动保存", 52, note, true)?;
    add(RETRY, "重试保存", 30, push, true)?;
    add(DETAIL, "查看详情", 30, push, true)?;
    add(CLOSE, "关闭", 30, push, true)?;
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
    if !unsafe { IsWindow(Some(handle())).as_bool() } {
        let raw = INSTANCE.load(std::sync::atomic::Ordering::Relaxed);
        if raw == 0 {
            return;
        }
        if let Err(error) = install(HINSTANCE(raw as *mut _)) {
            info_box(&format!("无法创建设置窗口：{error}"));
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
    match reason {
        Some(r) => format!("{}；{r}", config::source(key)),
        None => config::source(key),
    }
}
fn refresh() {
    let cfg = config::current();
    let actual = crate::hub::taskbar_running();
    let previous_focus = unsafe { GetFocus() };
    for (id, value) in [
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
    label(
        control(6301),
        &format!(
            "重启后生效 · 当前{} / 下次{} · {}",
            if actual {
                "显示"
            } else {
                "未显示 / 不可用"
            },
            if config::preferred_taskbar() {
                "显示"
            } else {
                "隐藏"
            },
            config::source(keys::TASKBAR_SHOW)
        ),
    );
    let mode = if crate::shell_swap::self_is_shell() {
        "替换模式：应用通知图标可交互"
    } else {
        "伴随模式：应用通知图标由 Windows 管理"
    };
    label(
        control(6302),
        &format!(
            "{mode}\n{}",
            source_note(keys::TASKBAR_TRAY, (!actual).then_some("本次无任务栏"))
        ),
    );
    label(
        control(6303),
        &source_note(keys::TASKBAR_GROUP, (!actual).then_some("本次无任务栏")),
    );
    label(
        control(6304),
        &source_note(
            keys::TASKBAR_GROUP_BY,
            if !actual {
                Some("本次无任务栏")
            } else if cfg.merge == MergeStrategy::Never {
                Some("需先启用窗口合并")
            } else {
                None
            },
        ),
    );
    label(
        control(6305),
        &source_note(keys::HOVER_PREVIEW, (!actual).then_some("本次无任务栏")),
    );
    label(
        control(6306),
        &format!(
            "主题：配置配色（修改后重启生效）\n任务栏唤出配置：{}（注册结果见日志）\n版本：{}",
            cfg.hotkey,
            env!("CARGO_PKG_VERSION")
        ),
    );
    let status = crate::settings_store::status();
    let message = if let Some(error) = with(|p| p.error.clone()).flatten() {
        error
    } else if status.failed > 0 {
        format!(
            "本次修改已保留，{} 项保存失败；下次启动尚未保存。请重试。",
            status.failed
        )
    } else if status.pending > 0 {
        "本次修改已保留，正在保存…".into()
    } else if status.saved && config::preferred_taskbar() != cfg.show_taskbar {
        "已保存，任务栏显示设置重启后生效。".into()
    } else if status.saved {
        "已生效并保存。".into()
    } else {
        "更改会自动保存".into()
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
                    with(|p| p.error = Some(format!("无法开启预览：{e}")));
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
            PCWSTR(wide("设置").as_ptr()),
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
    let width = (660 * dpi as i32 / 96).min(work.right - work.left).max(1);
    let height = (900 * dpi as i32 / 96).min(work.bottom - work.top).max(1);
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
    let margin = scale(28).min(width.max(1) / 8);
    let footer = scale(76).min((height - 1).max(0));
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
            metrics.lfMessageFont.lfHeight = -scale_at(16, dpi);
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
        let mut top = scale(18);
        let mut positions = Vec::new();
        let mut row_top = top;
        let horizontal = g.width >= scale(420);
        for c in controls.iter().filter(|c| !c.footer) {
            let row = match c.id {
                NEVER | FULL | ALWAYS if horizontal => Some((
                    NEVER,
                    [NEVER, FULL, ALWAYS]
                        .iter()
                        .position(|id| *id == c.id)
                        .unwrap(),
                    3,
                )),
                EXE | PID if horizontal => Some((EXE, usize::from(c.id == PID), 2)),
                REPLACE | ROLLBACK if horizontal => {
                    Some((REPLACE, usize::from(c.id == ROLLBACK), 2))
                }
                _ => None,
            };
            if is_heading(c.id) {
                top += scale(if c.id == 6200 { 10 } else { 18 });
            }
            let (x, width) = if let Some((first, index, count)) = row {
                if c.id == first {
                    row_top = top;
                } else {
                    top = row_top;
                }
                let width = (g.width - scale(12) * (count - 1)) / count;
                (index as i32 * (width + scale(12)), width)
            } else {
                (0, g.width)
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
            let height = scale(c.height).max(measured.bottom + scale(4));
            positions.push((c.id, x, top, width, height));
            top += height;
        }
        top += scale(20);
        SelectObject(dc, old);
        let _ = ReleaseDC(Some(hwnd), dc);
        let scroll = with(|p| {
            p.content = top;
            p.viewport = g.viewport;
            p.scroll = p.scroll.clamp(0, (top - g.viewport).max(0));
            p.scroll
        })
        .unwrap();
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
    }
    with(|p| p.layout = false);
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
            if [REPLACE, ROLLBACK, RETRY, DETAIL, CLOSE].contains(&id) {
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
                wide("仍有设置尚未保存。\n选择“是”放弃未保存修改并退出；选择“否”返回设置重试。")
                    .as_ptr(),
            ),
            PCWSTR(wide("设置尚未保存").as_ptr()),
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
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                fit(false);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == POLL => {
                refresh();
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
            let _ = MoveWindow(handle(), 40, 40, 660, 900, true);
        }
        layout();
        let original = with(|p| p.content).unwrap();
        for _ in 0..5 {
            unsafe {
                let _ = MoveWindow(handle(), 40, 40, 180, 400, true);
            }
            layout();
            unsafe {
                let _ = MoveWindow(handle(), 40, 40, 660, 900, true);
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
