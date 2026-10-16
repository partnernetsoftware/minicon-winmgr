//! Optional native diagnostic window. No sampling until the user presses Start.
use crate::{
    diagnostics::{Recording, Scenario, Snapshot, Stop, Timestamp},
    win_util::{guard_abi, wide},
};
use std::{cell::RefCell, path::PathBuf, time::Instant};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{Controls::Dialogs::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::PCWSTR,
};
const CLASS: &str = "minicon_winmgr_diagnostics";
const PREVIEW: i32 = 1;
const START: i32 = 2;
const STOP: i32 = 3;
const EXPORT: i32 = 4;
const CLOSE: i32 = 5;
const SCENARIO: i32 = 6;
const HEADING: i32 = 7;
const SCOPE: i32 = 8;
const STATUS: i32 = 9;
const LABEL: i32 = 10;
const SAVED: i32 = 11;
const ACTIVE_SCENARIO: i32 = 12;
const SAMPLE_TIMER: usize = 1;
const EXPORT_TIMER: usize = 2;
#[derive(Default)]
struct Panel {
    hwnd: HWND,
    font: HFONT,
    heading: HFONT,
    clock: Option<Instant>,
    record: Option<Recording>,
    error: Option<&'static str>,
}
thread_local! { static PANEL:RefCell<Panel>=RefCell::new(Panel::default()); }
#[cfg(test)]
thread_local! { static TIMER_MESSAGES:std::cell::Cell<u32>=const{std::cell::Cell::new(0)}; }
fn window() -> HWND {
    PANEL.with(|p| p.borrow().hwnd)
}
fn control(id: i32) -> HWND {
    unsafe { GetDlgItem(Some(window()), id).unwrap_or_default() }
}
fn text(id: i32, value: &str) {
    unsafe {
        let _ = SetWindowTextW(control(id), PCWSTR(wide(value).as_ptr()));
    }
}
fn active() -> bool {
    PANEL.with(|p| p.borrow().record.as_ref().is_some_and(Recording::active))
}
fn snapshot() -> Option<Snapshot> {
    PANEL.with(|p| p.borrow().record.as_ref().map(Recording::snapshot))
}
fn update(body: bool) {
    let hwnd = window();
    if hwnd.is_invalid() {
        return;
    }
    let (active, count, reason, error) = PANEL.with(|p| {
        let p = p.borrow();
        (
            p.record.as_ref().is_some_and(Recording::active),
            p.record.as_ref().map_or(0, Recording::count),
            p.record.as_ref().and_then(Recording::reason),
            p.error,
        )
    });
    let export = crate::diagnostic_export::status();
    text(
        START,
        crate::i18n::text(if count == 0 {
            "开始记录"
        } else {
            "新记录"
        }),
    );
    text(STOP, crate::i18n::text("停止"));
    text(EXPORT, crate::i18n::text("导出摘要…"));
    text(CLOSE, crate::i18n::text("关闭"));
    text(HEADING, crate::i18n::text("资源观察"));
    text(LABEL, crate::i18n::text("场景"));
    let status = if let Some(error) = error {
        crate::i18n::text(error).to_string()
    } else if active {
        crate::ui_format!("正在记录 · {count} 个样本", "Recording · {count} samples")
    } else if reason == Some(Stop::Limit) {
        crate::ui_format!(
            "已达到记录上限 · {count} 个样本",
            "Recording limit reached · {count} samples"
        )
    } else if count > 0 {
        crate::ui_format!("已停止 · {count} 个样本", "Stopped · {count} samples")
    } else {
        crate::i18n::text("准备就绪，尚未记录").to_string()
    };
    text(STATUS, &status);
    text(
        SCOPE,
        crate::i18n::text("导出仅含版本、时间、场景和资源数值。请先查看上方摘要；保存时可取消。"),
    );
    text(
        SAVED,
        crate::i18n::text(if export.busy {
            "正在保存摘要…"
        } else {
            match export.result {
                None => "",
                Some(true) => "上次摘要已保存",
                Some(false) => "保存失败，请选择可写位置后重试。",
            }
        }),
    );
    unsafe {
        let _ = EnableWindow(control(START), !active);
        let _ = EnableWindow(control(STOP), active);
        let _ = EnableWindow(control(EXPORT), count > 0 && !export.busy);
        let _ = ShowWindow(control(SCENARIO), if active { SW_HIDE } else { SW_SHOW });
        let _ = ShowWindow(
            control(ACTIVE_SCENARIO),
            if active { SW_SHOW } else { SW_HIDE },
        );
        if active {
            let label = PANEL.with(|p| p.borrow().record.as_ref().map(|r| r.scenario().label()));
            if let Some(label) = label {
                text(ACTIVE_SCENARIO, crate::i18n::text(label));
            }
        }
        if !export.busy {
            let _ = KillTimer(Some(hwnd), EXPORT_TIMER);
        }
    }
    if body {
        let value=snapshot().map_or_else(||crate::i18n::text("点击“开始记录”后，每 5 秒采样一次，最长 8 小时。\n关闭此窗口会停止记录，已有数据保留至新记录或程序退出。\n\n记录包含诊断自身开销；资源变化不等于泄漏，也不自动代表稳定性通过。\n缺失指标显示为暂不可用，不填成零。").to_string(),|s|s.display());
        unsafe {
            let edit = control(PREVIEW);
            let scroll = SendMessageW(edit, 0x00CE, None, None).0;
            let selection = SendMessageW(edit, 0x00B0, None, None).0 as u32;
            let _ = SetWindowTextW(edit, PCWSTR(wide(&value.replace('\n', "\r\n")).as_ptr()));
            let _ = SendMessageW(
                edit,
                0x00B1,
                Some(WPARAM((selection & 0xffff) as usize)),
                Some(LPARAM((selection >> 16) as isize)),
            );
            let _ = SendMessageW(edit, 0x00B6, Some(WPARAM(0)), Some(LPARAM(scroll)));
        }
    }
}
fn sample() {
    let elapsed = PANEL.with(|p| {
        let p = p.borrow();
        if p.record.as_ref().is_some_and(Recording::active) {
            p.clock.map(|c| c.elapsed())
        } else {
            None
        }
    });
    if let Some(elapsed) = elapsed {
        let observation = crate::metrics::observe();
        let time = Timestamp::now();
        PANEL.with(|p| {
            if let Some(record) = &mut p.borrow_mut().record {
                record.append(elapsed, time, observation);
            }
        });
        if !active() {
            unsafe {
                let _ = KillTimer(Some(window()), SAMPLE_TIMER);
            }
        }
    }
}
fn stop(reason: Stop) {
    sample();
    PANEL.with(|p| {
        if let Some(record) = &mut p.borrow_mut().record {
            record.stop(reason);
        }
    });
    unsafe {
        let _ = KillTimer(Some(window()), SAMPLE_TIMER);
    }
    update(true);
}
fn start() {
    if active() {
        return;
    }
    if snapshot().is_some() {
        let answer = unsafe {
            MessageBoxW(
                Some(window()),
                PCWSTR(wide(crate::i18n::text("开始新记录会替换当前记录。继续？")).as_ptr()),
                PCWSTR(wide(crate::i18n::text("资源观察")).as_ptr()),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
            )
        };
        if answer != IDYES {
            return;
        }
    }
    let choice = unsafe { SendMessageW(control(SCENARIO), CB_GETCURSEL, None, None).0 };
    let scenario = match choice {
        1 => Scenario::Idle,
        2 => Scenario::Windows,
        3 => Scenario::Update,
        _ => Scenario::Daily,
    };
    PANEL.with(|p| {
        let mut p = p.borrow_mut();
        p.record = None;
        p.error = None;
        p.clock = Some(Instant::now());
        p.record = Some(Recording::new(
            scenario,
            Timestamp::now(),
            crate::metrics::observe,
        ));
    });
    if unsafe {
        SetTimer(
            Some(window()),
            SAMPLE_TIMER,
            crate::diagnostics::INTERVAL_MS,
            None,
        )
    } == 0
    {
        PANEL.with(|p| {
            let mut p = p.borrow_mut();
            p.record = None;
            p.clock = None;
            p.error = Some("无法开始记录，请重试。");
        });
    }
    update(true);
    if active() {
        unsafe {
            let _ = SetFocus(Some(control(STOP)));
        }
    }
}
fn export() {
    if crate::diagnostic_export::status().busy {
        return;
    }
    use std::os::windows::ffi::OsStringExt;
    let mut filename = vec![0u16; 32768];
    let name = wide("winmgr-diagnostics.md");
    filename[..name.len()].copy_from_slice(&name);
    let filter = wide("Markdown (*.md)\0*.md\0\0");
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: window(),
        lpstrFile: windows::core::PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrDefExt: windows::core::w!("md"),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
        ..Default::default()
    };
    if !unsafe { GetSaveFileNameW(&mut dialog) }.as_bool() {
        if unsafe { CommDlgExtendedError() }.0 != 0 {
            PANEL.with(|p| p.borrow_mut().error = Some("无法打开保存窗口，请重试。"));
            update(false);
        }
        return;
    }
    let length = filename
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(filename.len());
    let path = PathBuf::from(std::ffi::OsString::from_wide(&filename[..length]));
    if let Some(snapshot) = snapshot() {
        if crate::diagnostic_export::submit(path, snapshot) {
            unsafe {
                SetTimer(Some(window()), EXPORT_TIMER, 250, None);
            }
        } else {
            PANEL.with(|p| p.borrow_mut().error = Some("无法保存摘要，请稍后重试。"));
        }
    }
    update(false);
}
fn layout(hwnd: HWND) {
    unsafe {
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let u = |n: i32| n * dpi / 96;
        let margin = u(16).min(rect.right.max(1) / 8);
        let width = (rect.right - 2 * margin).max(1);
        let gap = u(8);
        let move_control = |id: i32, x: i32, y: i32, w: i32, h: i32| {
            let _ = MoveWindow(
                GetDlgItem(Some(hwnd), id).unwrap_or_default(),
                x,
                y,
                w.max(1),
                h.max(1),
                true,
            );
        };
        move_control(HEADING, margin, u(14), width, u(28));
        move_control(STATUS, margin, u(45), width, u(22));
        move_control(LABEL, margin, u(77), u(76), u(24));
        move_control(
            SCENARIO,
            margin + u(80),
            u(72),
            (width - u(80)).max(1),
            u(160),
        );
        move_control(
            ACTIVE_SCENARIO,
            margin + u(80),
            u(77),
            (width - u(80)).max(1),
            u(24),
        );
        let buttons = (rect.bottom - margin - u(32)).max(u(160));
        let saved = buttons - u(27);
        let scope = saved - u(48);
        let bottom = scope - u(8);
        move_control(PREVIEW, margin, u(112), width, (bottom - u(112)).max(1));
        move_control(SCOPE, margin, scope, width, u(44));
        move_control(SAVED, margin, saved, width, u(22));
        let cell = ((width - 3 * gap) / 4).max(1);
        for (index, id) in [START, STOP, EXPORT, CLOSE].iter().enumerate() {
            move_control(
                *id,
                margin + index as i32 * (cell + gap),
                buttons,
                cell,
                u(32),
            );
        }
    }
}
fn fonts(hwnd: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let create = |size: i32, weight: i32| {
            let mut value = LOGFONTW {
                lfHeight: -(size * dpi / 96),
                lfWeight: weight,
                lfCharSet: DEFAULT_CHARSET,
                lfQuality: CLEARTYPE_QUALITY,
                lfPitchAndFamily: VARIABLE_PITCH.0 | FF_SWISS.0,
                ..Default::default()
            };
            let face = wide("Segoe UI");
            value.lfFaceName[..face.len()].copy_from_slice(&face);
            CreateFontIndirectW(&value)
        };
        let body = create(14, 400);
        let heading = create(18, 600);
        for id in 1..=12 {
            let _ = SendMessageW(
                GetDlgItem(Some(hwnd), id).unwrap_or_default(),
                WM_SETFONT,
                Some(WPARAM(if id == HEADING {
                    heading.0 as usize
                } else {
                    body.0 as usize
                })),
                Some(LPARAM(1)),
            );
        }
        PANEL.with(|p| {
            let mut p = p.borrow_mut();
            for old in [p.font, p.heading] {
                if !old.is_invalid() {
                    let _ = DeleteObject(old.into());
                }
            }
            p.font = body;
            p.heading = heading;
        });
    }
}
pub(crate) fn show(owner: HWND) -> windows::core::Result<()> {
    unsafe {
        let mut hwnd = window();
        if !IsWindow(Some(hwnd)).as_bool() {
            let instance = HINSTANCE(GetWindowLongPtrW(owner, GWLP_HINSTANCE) as *mut _);
            let class = wide(CLASS);
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: PCWSTR(class.as_ptr()),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(windows::core::Error::from_thread());
            }
            let mut monitor = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(
                MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
            .as_bool()
            {
                return Err(windows::core::Error::from_thread());
            }
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let w = (600 * dpi / 96).min(monitor.rcWork.right - monitor.rcWork.left);
            let h = (590 * dpi / 96).min(monitor.rcWork.bottom - monitor.rcWork.top);
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let result = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                monitor.rcWork.left + (monitor.rcWork.right - monitor.rcWork.left - w) / 2,
                monitor.rcWork.top + (monitor.rcWork.bottom - monitor.rcWork.top - h) / 2,
                w,
                h,
                Some(owner),
                None,
                Some(instance),
                None,
            );
            SetThreadDpiAwarenessContext(previous);
            hwnd = result?;
            PANEL.with(|p| p.borrow_mut().hwnd = hwnd);
            let result = (|| -> windows::core::Result<()> {
                for (id, class, style) in [
                    (
                        PREVIEW,
                        "EDIT",
                        WS_CHILD
                            | WS_VISIBLE
                            | WS_TABSTOP
                            | WS_VSCROLL
                            | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                    ),
                    (
                        SCENARIO,
                        "COMBOBOX",
                        WS_CHILD
                            | WS_VISIBLE
                            | WS_TABSTOP
                            | WS_VSCROLL
                            | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                    ),
                    (START, "BUTTON", WS_CHILD | WS_VISIBLE | WS_TABSTOP),
                    (STOP, "BUTTON", WS_CHILD | WS_VISIBLE | WS_TABSTOP),
                    (EXPORT, "BUTTON", WS_CHILD | WS_VISIBLE | WS_TABSTOP),
                    (CLOSE, "BUTTON", WS_CHILD | WS_VISIBLE | WS_TABSTOP),
                    (HEADING, "STATIC", WS_CHILD | WS_VISIBLE),
                    (STATUS, "STATIC", WS_CHILD | WS_VISIBLE),
                    (LABEL, "STATIC", WS_CHILD | WS_VISIBLE),
                    (SCOPE, "STATIC", WS_CHILD | WS_VISIBLE),
                    (SAVED, "STATIC", WS_CHILD | WS_VISIBLE),
                    (ACTIVE_SCENARIO, "STATIC", WS_CHILD),
                ] {
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        PCWSTR(wide(class).as_ptr()),
                        PCWSTR::null(),
                        style,
                        0,
                        0,
                        1,
                        1,
                        Some(hwnd),
                        Some(HMENU(id as usize as *mut _)),
                        Some(instance),
                        None,
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = DestroyWindow(hwnd);
                return Err(error);
            }
            fonts(hwnd);
        }
        refresh();
        layout(hwnd);
        crate::appearance::install(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        crate::win_util::force_foreground(hwnd);
        let _ = SetFocus(Some(control(if active() { STOP } else { START })));
        if crate::diagnostic_export::status().busy {
            SetTimer(Some(hwnd), EXPORT_TIMER, 250, None);
        }
        Ok(())
    }
}
pub(crate) fn refresh() {
    if window().is_invalid() {
        return;
    }
    unsafe {
        let title = crate::ui_format!(
            "诊断 — minicon-winmgr {}",
            "Diagnostics — minicon-winmgr {}",
            env!("CARGO_PKG_VERSION")
        );
        let _ = SetWindowTextW(window(), PCWSTR(wide(&title).as_ptr()));
        let current = SendMessageW(control(SCENARIO), CB_GETCURSEL, None, None).0;
        let selected = if current >= 0 {
            current
        } else {
            PANEL.with(|p| {
                p.borrow()
                    .record
                    .as_ref()
                    .map_or(0, |r| r.scenario().index() as isize)
            })
        };
        let _ = SendMessageW(control(SCENARIO), CB_RESETCONTENT, None, None);
        for label in ["日常使用", "空闲", "窗口操作", "版本更新"] {
            let _ = SendMessageW(
                control(SCENARIO),
                CB_ADDSTRING,
                None,
                Some(LPARAM(wide(crate::i18n::text(label)).as_ptr() as isize)),
            );
        }
        let _ = SendMessageW(
            control(SCENARIO),
            CB_SETCURSEL,
            Some(WPARAM(selected as usize)),
            None,
        );
    }
    update(true);
}
pub(crate) fn close() {
    let hwnd = window();
    if !hwnd.is_invalid() {
        unsafe {
            let _ = SendMessageW(hwnd, WM_CLOSE, None, None);
        }
    }
}
pub(crate) fn shutdown() {
    close();
    PANEL.with(|p| {
        let mut p = p.borrow_mut();
        p.record = None;
        p.clock = None;
    });
}
pub(crate) fn filter(msg: &MSG) -> bool {
    let hwnd = window();
    if hwnd.is_invalid() || (msg.hwnd != hwnd && !unsafe { IsChild(hwnd, msg.hwnd).as_bool() }) {
        return false;
    }
    if msg.message == WM_KEYDOWN {
        if msg.wParam.0 == VK_ESCAPE.0 as usize {
            close();
            return true;
        }
        let id = unsafe { GetDlgCtrlID(msg.hwnd) };
        if msg.wParam.0 == VK_RETURN.0 as usize && [START, STOP, EXPORT, CLOSE].contains(&id) {
            unsafe {
                let _ = SendMessageW(msg.hwnd, BM_CLICK, None, None);
            }
            return true;
        }
        if id == PREVIEW
            && msg.wParam.0 == b'A' as usize
            && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0
        {
            unsafe {
                let _ = SendMessageW(msg.hwnd, 0x00B1, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
            return true;
        }
    }
    unsafe { IsDialogMessageW(hwnd, msg).as_bool() }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "diagnostic window",
        || match msg {
            WM_COMMAND => {
                match (wp.0 & 0xffff) as i32 {
                    START => start(),
                    STOP => stop(Stop::User),
                    EXPORT => export(),
                    CLOSE => close(),
                    _ => (),
                }
                LRESULT(0)
            }
            WM_TIMER => {
                #[cfg(test)]
                TIMER_MESSAGES.with(|count| count.set(count.get() + 1));
                if wp.0 == SAMPLE_TIMER {
                    sample();
                    update(true);
                } else if wp.0 == EXPORT_TIMER {
                    update(false);
                }
                LRESULT(0)
            }
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => unsafe {
                let info = &mut *(lp.0 as *mut MINMAXINFO);
                let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                info.ptMinTrackSize.x = 420 * dpi / 96;
                info.ptMinTrackSize.y = 420 * dpi / 96;
                LRESULT(0)
            },
            WM_DPICHANGED => unsafe {
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
                fonts(hwnd);
                layout(hwnd);
                LRESULT(0)
            },
            WM_CLOSE => {
                stop(Stop::Closed);
                unsafe {
                    let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
                    let _ = DestroyWindow(hwnd);
                    if IsWindow(Some(owner)).as_bool() {
                        let button = GetDlgItem(Some(owner), 3).unwrap_or_default();
                        let _ = SetFocus(Some(button));
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe {
                    let _ = KillTimer(Some(hwnd), SAMPLE_TIMER);
                    let _ = KillTimer(Some(hwnd), EXPORT_TIMER);
                }
                PANEL.with(|p| {
                    let mut p = p.borrow_mut();
                    if let Some(record) = &mut p.record {
                        record.stop(Stop::Closed);
                    }
                    for font in [p.font, p.heading] {
                        if !font.is_invalid() {
                            unsafe {
                                let _ = DeleteObject(font.into());
                            }
                        }
                    }
                    p.hwnd = HWND::default();
                    p.font = HFONT::default();
                    p.heading = HFONT::default();
                });
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
    const SHOW: u32 = 0x8000 + 201;
    const STATE: u32 = 0x8000 + 202;
    unsafe extern "system" fn harness(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            SHOW => {
                unsafe {
                    let _ = SendMessageW(
                        GetDlgItem(Some(crate::settings_info::test_handle()), 3).unwrap(),
                        BM_CLICK,
                        None,
                        None,
                    );
                }
                LRESULT(0)
            }
            STATE => {
                let (exists, active, count) = PANEL.with(|p| {
                    let p = p.borrow();
                    (
                        p.record.is_some(),
                        p.record.as_ref().is_some_and(Recording::active),
                        p.record.as_ref().map_or(0, Recording::count),
                    )
                });
                let flags = usize::from(exists)
                    | (usize::from(active) << 1)
                    | (usize::from(crate::diagnostic_export::initialized()) << 2)
                    | (usize::from(crate::diagnostic_export::status().busy) << 3);
                LRESULT((flags | (count << 8) | ((TIMER_MESSAGES.get() as usize) << 24)) as isize)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }
    #[test]
    #[ignore = "isolated diagnostic UI fixture; run diagnostics layer"]
    fn native_window_fixture() {
        let _ = crate::config::current();
        unsafe {
            let name = windows::core::w!("winmgr_diagnostic_test_owner");
            let class = WNDCLASSW {
                lpszClassName: name,
                lpfnWndProc: Some(harness),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                name,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            crate::settings_info::show(owner, "Diagnostic entry fixture").unwrap();
            let info = crate::settings_info::test_handle();
            let _ = SendMessageW(GetDlgItem(Some(info), 3).unwrap(), BM_CLICK, None, None);
            for id in [HEADING, PREVIEW] {
                let child = control(id);
                let dc = GetDC(Some(child));
                let font = SendMessageW(child, WM_GETFONT, None, None).0;
                let old = SelectObject(dc, HGDIOBJ(font as *mut _));
                let mut face = [0u16; 128];
                let length = GetTextFaceW(dc, Some(&mut face));
                println!(
                    "DIAGNOSTIC_FONT {id} {}",
                    String::from_utf16_lossy(&face[..length.max(0) as usize])
                        .trim_end_matches('\0')
                );
                SelectObject(dc, old);
                ReleaseDC(Some(child), dc);
            }
            assert!(!active());
            assert!(snapshot().is_none());
            assert!(!crate::diagnostic_export::initialized());
            println!(
                "DIAGNOSTIC_UI_READY {{\"owner\":{},\"window\":{},\"info\":{}}}",
                owner.0 as usize,
                window().0 as usize,
                info.0 as usize
            );
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if !filter(&msg) && !crate::settings_info::filter(&msg) {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            shutdown();
            crate::diagnostic_export::shutdown();
            let _ = UnregisterClassW(name, None);
        }
    }
}
