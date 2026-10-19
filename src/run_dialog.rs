//! Native, reusable run dialog. All disk and shell work belongs to bounded workers.
use crate::{
    run_history::{Candidate, CandidateSource, History, merge_candidates},
    win_util::wide,
};
use std::{
    cell::{Cell, RefCell},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
use windows::Win32::{
    System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    },
    UI::Controls::{
        BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, EM_GETSEL, EM_SETLIMITTEXT, EM_SETSEL,
        ODS_SELECTED,
    },
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::PCWSTR,
};

const EDIT: u16 = 2001;
const RUN: u16 = 2002;
const CANCEL: u16 = 2003;
const CLEAR: u16 = 2004;
const DROP: u16 = 2005;
const LIST: u16 = 2006;
const HELP: u16 = 2007;
const STATUS: u16 = 2008;
const LABEL: u16 = 2009;
const ADMIN: u16 = 2010;
#[derive(Debug)]
struct LaunchJob {
    id: u64,
    input: String,
    mode: crate::shell_launch::LaunchMode,
    owner: isize,
    selection: (u32, u32),
}
pub(crate) const HOTKEY_ID: usize = 3;
thread_local! { static INSTANCE: Cell<HINSTANCE> = const { Cell::new(HINSTANCE(core::ptr::null_mut())) }; }
enum Job {
    Query(u64, String),
    Record(String),
    Clear,
}
enum Reply {
    Query(u64, Vec<Candidate>),
    History(Vec<String>, Option<String>, bool, bool),
    Launch(LaunchJob, Result<(), crate::shell_launch::LaunchError>),
}
struct State {
    window: HWND,
    controls: Vec<(u16, HWND, isize)>,
    font: HFONT,
    font_dpi: u32,
    jobs: SyncSender<Job>,
    queries: SyncSender<(u64, String, Vec<String>)>,
    entries: Vec<String>,
    launches: SyncSender<LaunchJob>,
    replies: Receiver<Reply>,
    pending: Option<Job>,
    actions: std::collections::VecDeque<Job>,
    changed: Instant,
    query: u64,
    waiting: Option<Instant>,
    candidates: Vec<Candidate>,
    preview_draft: Option<(String, u32, u32)>,
    setting_preview: bool,
    expanded: bool,
    history: bool,
    clear_retry: bool,
    visible: bool,
    composing: bool,
    busy: bool,
    authorizing: bool,
    generation: u64,
    launch_id: u64,
    dpi: u32,
    work: RECT,
    query_busy: bool,
    notice: Option<String>,
    layout_active: bool,
    layout_dirty: bool,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.with(|cell| cell.borrow_mut().as_mut().map(f))
}
fn window() -> HWND {
    with(|s| s.window).unwrap_or_default()
}
fn control(id: u16) -> HWND {
    with(|s| {
        s.controls
            .iter()
            .find(|c| c.0 == id)
            .map(|c| c.1)
            .unwrap_or_default()
    })
    .unwrap_or_default()
}
fn text(hwnd: HWND) -> String {
    unsafe {
        let mut b = vec![0; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let n = GetWindowTextW(hwnd, &mut b);
        String::from_utf16_lossy(&b[..n.max(0) as usize])
    }
}
fn set_text(hwnd: HWND, value: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide(value).as_ptr()));
    }
}
fn status(value: &str) {
    if text(control(STATUS)) == value {
        return;
    }
    set_text(control(STATUS), value);
    layout();
}
fn send(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l))) }
}
fn enabled() {
    let (busy, history) =
        with(|s| (s.busy || s.actions.len() >= 32, s.history || s.clear_retry)).unwrap_or_default();
    let choosing = with(|s| s.expanded && !s.candidates.is_empty()).unwrap_or(false)
        && send(control(LIST), LB_GETCURSEL, 0, 0).0 >= 0;
    set_text(
        control(RUN),
        crate::i18n::text(if choosing { "采用" } else { "运行" }),
    );
    set_text(
        control(HELP),
        crate::i18n::text(if choosing {
            "↑↓ 选择 · Enter 采用 · Esc 收起候选"
        } else {
            "输入程序、文件夹、文档或网址"
        }),
    );
    unsafe {
        let _ = EnableWindow(
            control(RUN),
            !busy && (choosing || !text(control(EDIT)).trim().is_empty()),
        );
        let _ = EnableWindow(control(ADMIN), !busy);
        let _ = EnableWindow(control(CLEAR), history);
        let _ = ShowWindow(control(CLEAR), if history { SW_SHOW } else { SW_HIDE });
    }
}

/// Register only the shortcut. No window, history IO, workers or timer until first use.
pub fn install(instance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    INSTANCE.set(instance);
    let owner = crate::hub::shell_hwnd().ok_or("Run hotkey owner is unavailable")?;
    for (modifiers, label) in [(MOD_WIN, "Win+R"), (MOD_CONTROL | MOD_ALT, "Ctrl+Alt+R")] {
        if unsafe {
            RegisterHotKey(
                Some(owner),
                HOTKEY_ID as i32,
                modifiers | MOD_NOREPEAT,
                VK_R.0 as u32,
            )
        }
        .is_ok()
        {
            tracing::info!(
                hotkey = label,
                "Run hotkey registered; UI deferred until first use"
            );
            return Ok(());
        }
    }
    Err("Run hotkey registration failed".into())
}
fn prepare(instance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if !window().is_invalid() {
        return Ok(());
    }
    let (jobs_tx, jobs_rx) = mpsc::sync_channel(1);
    let (launch_tx, launch_rx) = mpsc::sync_channel::<LaunchJob>(1);
    let (reply_tx, reply_rx) = mpsc::channel();
    let (query_tx, query_rx) = mpsc::sync_channel::<(u64, String, Vec<String>)>(1);
    let query_reply = reply_tx.clone();
    std::thread::Builder::new()
        .name("run-path".into())
        .spawn(move || {
            let mut applications = None;
            while let Ok((id, input, entries)) = query_rx.recv() {
                let mut items = merge_candidates(&entries, &input);
                if crate::run_history::application_query(&input) {
                    let apps =
                        applications.get_or_insert_with(crate::run_history::discover_applications);
                    items = crate::run_history::with_applications(items, &input, apps);
                }
                let _ = query_reply.send(Reply::Query(id, items));
            }
        })?;
    let history_reply = reply_tx.clone();
    std::thread::Builder::new()
        .name("run-history".into())
        .spawn(move || {
            let mut history = History::load_default();
            let _ = history_reply.send(Reply::History(
                history.entries().to_vec(),
                None,
                true,
                false,
            ));
            while let Ok(job) = jobs_rx.recv() {
                match job {
                    Job::Query(..) => {}
                    Job::Record(input) => {
                        let error = history.record_success(&input).err().map(|error| {
                            tracing::warn!(%error,"Run history persistence failed");
                            crate::i18n::text("历史保存失败，本次仍可使用，重启后可能丢失。").into()
                        });
                        let _ = history_reply.send(Reply::History(
                            history.entries().to_vec(),
                            error,
                            false,
                            false,
                        ));
                    }
                    Job::Clear => {
                        let error = history.clear().err().map(|_| {
                            crate::i18n::text("本次历史已清空，磁盘历史清除失败，重启后可能恢复。请再次清空重试。")
                                .into()
                        });
                        let retry = error.is_some();
                        let _ = history_reply.send(Reply::History(Vec::new(), error, false, retry));
                    }
                }
            }
        })?;
    std::thread::Builder::new()
        .name("run-launch".into())
        .spawn(move || {
            let com =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            while let Ok(job) = launch_rx.recv() {
                let result = if com.is_err() {
                    Err(crate::shell_launch::LaunchError {
                        code: None,
                        message: crate::i18n::text("启动服务初始化失败，请重启后重试。").into(),
                    })
                } else {
                    crate::shell_launch::parse_command(&job.input).and_then(|r| {
                        crate::shell_launch::execute_with_mode(
                            &r,
                            job.mode,
                            Some(HWND(job.owner as *mut _)),
                        )
                    })
                };
                let _ = reply_tx.send(Reply::Launch(job, result));
            }
            if com.is_ok() {
                unsafe {
                    CoUninitialize();
                }
            }
        })?;
    unsafe {
        let previous_dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        struct RestoreDpi(DPI_AWARENESS_CONTEXT);
        impl Drop for RestoreDpi {
            fn drop(&mut self) {
                unsafe {
                    if !self.0.is_invalid() {
                        let _ = SetThreadDpiAwarenessContext(self.0);
                    }
                }
            }
        }
        let _restore_dpi = RestoreDpi(previous_dpi);
        let class = wide("minicon_winmgr_run_dialog");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 && GetLastError().0 != 1410 {
            return Err(windows::core::Error::from_thread().into());
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class.as_ptr()),
            PCWSTR(wide(crate::i18n::text("运行")).as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            0,
            0,
            480,
            210,
            None,
            None,
            Some(instance),
            None,
        )?;
        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                window: hwnd,
                controls: Vec::new(),
                font: HFONT::default(),
                font_dpi: 0,
                jobs: jobs_tx,
                queries: query_tx,
                entries: Vec::new(),
                launches: launch_tx,
                replies: reply_rx,
                pending: None,
                actions: Default::default(),
                changed: Instant::now(),
                query: 0,
                waiting: None,
                candidates: Vec::new(),
                preview_draft: None,
                setting_preview: false,
                expanded: false,
                history: false,
                clear_retry: false,
                visible: false,
                composing: false,
                busy: false,
                authorizing: false,
                generation: 0,
                launch_id: 0,
                dpi: 96,
                work: RECT {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                query_busy: false,
                notice: None,
                layout_active: false,
                layout_dirty: false,
            })
        });
        for (id, class, label, style) in [
            (
                HELP,
                "STATIC",
                crate::i18n::text("输入程序、文件夹、文档或网址"),
                0,
            ),
            (LABEL, "STATIC", crate::i18n::text("打开(&O)："), 0),
            (
                EDIT,
                "EDIT",
                "",
                ES_AUTOHSCROLL as u32 | WS_BORDER.0 | WS_TABSTOP.0,
            ),
            (DROP, "BUTTON", "▾", BS_PUSHBUTTON as u32),
            (
                LIST,
                "LISTBOX",
                "",
                LBS_NOTIFY as u32
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_HASSTRINGS as u32
                    | LBS_NOINTEGRALHEIGHT as u32
                    | WS_BORDER.0
                    | WS_VSCROLL.0,
            ),
            (
                STATUS,
                "EDIT",
                "",
                ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL.0,
            ),
            (
                ADMIN,
                "BUTTON",
                crate::i18n::text("使用管理员权限运行"),
                BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0,
            ),
            (
                CLEAR,
                "BUTTON",
                crate::i18n::text("清空历史"),
                BS_PUSHBUTTON as u32 | WS_TABSTOP.0,
            ),
            (
                RUN,
                "BUTTON",
                crate::i18n::text("运行"),
                BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0,
            ),
            (
                CANCEL,
                "BUTTON",
                crate::i18n::text("取消"),
                BS_PUSHBUTTON as u32 | WS_TABSTOP.0,
            ),
        ] {
            let child = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(wide(class).as_ptr()),
                PCWSTR(wide(label).as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
                0,
                0,
                0,
                0,
                Some(hwnd),
                Some(HMENU(id as usize as *mut _)),
                Some(instance),
                None,
            )?;
            let previous = if matches!(
                id,
                EDIT | RUN | CANCEL | CLEAR | DROP | LIST | STATUS | ADMIN
            ) {
                SetWindowLongPtrW(child, GWLP_WNDPROC, child_proc as *const () as isize)
            } else {
                0
            };
            with(|s| s.controls.push((id, child, previous)));
        }
        // Expose a useful accessible edit name without putting placeholder text in the value.
        let _ = SetWindowTextW(control(EDIT), PCWSTR(wide("").as_ptr()));
        send(control(EDIT), EM_SETLIMITTEXT, 32767, 0);
        let _ = ShowWindow(control(LIST), SW_HIDE);
        let _ = SetTimer(Some(hwnd), 1, 30, None);
    }
    enabled();
    Ok(())
}

fn position() {
    unsafe {
        let foreground = GetForegroundWindow();
        let mut monitor = MonitorFromWindow(foreground, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() {
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            monitor = MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY);
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let (mut x, mut y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        with(|s| {
            s.work = info.rcWork;
            s.dpi = x.max(96);
        });
    }
    layout();
}
#[derive(Debug)]
struct Geometry {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    list_height: i32,
    status_height: i32,
    button_y: i32,
    input_y: i32,
    admin_y: i32,
    list_y: i32,
}
fn geometry(work: RECT, dpi: u32, count: usize, status_height: i32, caption: i32) -> Geometry {
    let scale = |n: i32| n * dpi.max(1) as i32 / 96;
    let width = scale(480).min((work.right - work.left).max(1));
    let available = (work.bottom - work.top - caption).max(1);
    let compact = available < scale(182);
    let input_y = if compact { scale(20) } else { scale(42) };
    let fixed = input_y + scale(if compact { 60 } else { 118 });
    // Reserve input and buttons first; candidates consume only the remaining space.
    let status_height = if status_height > 0 {
        status_height.max(scale(22)).min((available - fixed).max(0))
    } else {
        0
    };
    let list_budget = (available - fixed - status_height).max(0);
    let overhead = scale(4); // native list border; full values remain accessible
    let rows = ((list_budget - overhead).max(0) / scale(44).max(1)).min(count.min(6) as i32);
    let list_height = if rows > 0 {
        rows * scale(44) + overhead
    } else {
        0
    };
    let list_y = input_y + scale(if compact { 30 } else { 36 });
    let button_y = list_y + scale(if compact { 0 } else { 36 }) + list_height + status_height;
    let admin_y = if compact { 0 } else { button_y - scale(24) };
    let height = (button_y + scale(if compact { 30 } else { 46 }) + caption)
        .min((work.bottom - work.top).max(1));
    Geometry {
        x: work.left + (work.right - work.left - width) / 2,
        y: (work.top + (work.bottom - work.top) / 4)
            .min(work.bottom - height)
            .max(work.top),
        width,
        height,
        list_height,
        status_height,
        button_y,
        input_y,
        admin_y,
        list_y,
    }
}
fn result_is_current(visible: bool, generation: u64, request: u64) -> bool {
    visible && generation == request
}
/// Native DrawText may dereference its pointer even when cchText is zero.
/// Keep empty text out of the API and retain a readable trailing NUL otherwise.
fn measure_status(dc: HDC, value: &str, width: i32) -> i32 {
    if value.is_empty() {
        return 0;
    }
    let mut measured = RECT {
        right: width.max(1),
        ..Default::default()
    };
    let mut content = wide(value);
    let length = content.len() - 1;
    unsafe {
        DrawTextW(
            dc,
            &mut content[..length],
            &mut measured,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
    }
    measured.bottom.max(0)
}
fn layout() {
    let entered = with(|s| {
        s.layout_dirty = true;
        if s.layout_active {
            false
        } else {
            s.layout_active = true;
            s.layout_dirty = false;
            true
        }
    })
    .unwrap_or(false);
    if !entered {
        return;
    }
    struct LayoutGuard;
    impl Drop for LayoutGuard {
        fn drop(&mut self) {
            with(|s| s.layout_active = false);
        }
    }
    let _guard = LayoutGuard;
    let Some((hwnd, dpi, work, expanded, controls, oldfont, font_dpi)) = with(|s| {
        (
            s.window,
            s.dpi,
            s.work,
            s.expanded,
            s.controls.clone(),
            s.font,
            s.font_dpi,
        )
    }) else {
        return;
    };
    let scale = |n: i32| n * dpi as i32 / 96;
    let width = scale(480).min(work.right - work.left).max(1);
    unsafe {
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let font = if font_dpi == dpi && !oldfont.is_invalid() {
            oldfont
        } else if SystemParametersInfoForDpi(
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
        let dc = GetDC(Some(hwnd));
        let original = SelectObject(dc, font.into());
        let status_text = text(control(STATUS));
        let measured = measure_status(
            dc,
            &status_text,
            width - scale(40) - GetSystemMetricsForDpi(SM_CXVSCROLL, dpi),
        );
        let _ = SelectObject(dc, original);
        let _ = ReleaseDC(Some(hwnd), dc);
        let mut frame = RECT::default();
        let _ = AdjustWindowRectExForDpi(
            &mut frame,
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            dpi,
        );
        let count = if expanded {
            with(|s| s.candidates.len()).unwrap_or(0)
        } else {
            0
        };
        let g = geometry(
            work,
            dpi,
            count,
            if measured == 0 {
                0
            } else {
                measured + scale(4)
            },
            frame.bottom - frame.top,
        );
        let list_height = g.list_height;
        // SetWindowPos can synchronously deliver WM_DPICHANGED and re-enter layout.
        // Publish the reusable font first so that nested layout does not create another one.
        with(|s| {
            s.font = font;
            s.font_dpi = dpi;
        });
        let _ = SetWindowPos(
            hwnd,
            None,
            g.x,
            g.y,
            g.width,
            g.height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let cw = client.right;
        let m = scale(16).min((cw / 24).max(1));
        let w = (cw - 2 * m).max(1);
        let label_width = scale(68).min(w / 4);
        let drop_width = scale(32).min(w / 8).max(1);
        let button_width = scale(80).min((w - scale(16)) / 3).max(1);
        for (id, child, _) in controls {
            let (x, y, w, h) = match id {
                HELP => (m, scale(12), w, scale(24)),
                LABEL => (m, g.input_y + scale(6), label_width, scale(24)),
                EDIT => (
                    m + label_width,
                    g.input_y,
                    (w - label_width - drop_width).max(1),
                    scale(30),
                ),
                DROP => (cw - m - drop_width, g.input_y, drop_width, scale(30)),
                LIST => (m, g.list_y, w, list_height),
                STATUS => (m, g.list_y + scale(4) + list_height, w, g.status_height),
                ADMIN => (
                    m,
                    g.admin_y,
                    w,
                    scale(if g.input_y == scale(20) { 18 } else { 24 }),
                ),
                CLEAR => (m, g.button_y, button_width, scale(30)),
                RUN => (
                    cw - m - 2 * button_width - scale(8),
                    g.button_y,
                    button_width,
                    scale(30),
                ),
                _ => (cw - m - button_width, g.button_y, button_width, scale(30)),
            };
            let _ = MoveWindow(child, x, y, w, h, true);
            send(child, WM_SETFONT, font.0 as usize, 1);
            if id == STATUS {
                let _ = ShowWindow(
                    child,
                    if g.status_height > 0 {
                        SW_SHOW
                    } else {
                        SW_HIDE
                    },
                );
            }
            if id == HELP {
                let _ = ShowWindow(
                    child,
                    if g.input_y == scale(42) {
                        SW_SHOW
                    } else {
                        SW_HIDE
                    },
                );
            }
            if id == LIST {
                send(child, LB_SETITEMHEIGHT, 0, scale(44) as isize);
            }
        }
        with(|s| {
            s.font = font;
            s.font_dpi = dpi;
        });
        if !oldfont.is_invalid() && oldfont != font {
            let _ = DeleteObject(oldfont.into());
        }
        let _ = ShowWindow(
            control(LIST),
            if expanded && list_height > 0 {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
    }
}
fn expand(value: bool) {
    let changed = with(|s| {
        let changed = s.expanded != value;
        s.expanded = value;
        changed
    })
    .unwrap_or(false);
    if !value {
        send(control(LIST), LB_SETCURSEL, usize::MAX, 0);
    }
    if changed || value {
        layout();
    }
    enabled();
}
fn begin_session() -> bool {
    if with(|s| s.visible).unwrap_or(false) {
        return false;
    }
    send(control(ADMIN), BM_SETCHECK, BST_UNCHECKED.0 as usize, 0);
    with(|s| {
        s.visible = true;
        s.generation = s.generation.wrapping_add(1);
    });
    true
}

pub(crate) fn show() {
    if window().is_invalid()
        && let Err(error) = prepare(INSTANCE.get())
    {
        tracing::error!(%error, "Run first-use initialization failed");
        shutdown();
        let _ = install(INSTANCE.get());
        unsafe {
            MessageBoxW(
                crate::hub::shell_hwnd(),
                PCWSTR(
                    wide(crate::i18n::text(
                        "运行窗口初始化失败，请重试。详情见日志。",
                    ))
                    .as_ptr(),
                ),
                PCWSTR(wide(crate::i18n::text("运行")).as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
        return;
    }
    if begin_session() {
        position();
        if !with(|s| s.busy).unwrap_or(false) {
            let notice = with(|s| s.notice.clone()).flatten();
            status(notice.as_deref().unwrap_or(""));
        }
    }
    crate::i18n::localize_window(window());
    if let Some(controls) = with(|s| s.controls.iter().map(|c| c.1).collect::<Vec<_>>()) {
        for child in controls {
            crate::i18n::localize_window(child);
        }
    }
    unsafe {
        crate::appearance::install(window());
        let _ = ShowWindow(window(), SW_SHOW);
        let _ = crate::win_util::force_foreground(window());
        let _ = SetFocus(Some(control(EDIT)));
    }
    send(control(EDIT), EM_SETSEL, 0, -1);
    enabled();
}
fn hide() {
    cancel_preview();
    with(|s| {
        s.visible = false;
        s.composing = false;
        s.generation = s.generation.wrapping_add(1);
        s.query = s.query.wrapping_add(1);
        s.pending = None;
        s.waiting = None;
    });
    expand(false);
    unsafe {
        let _ = ShowWindow(window(), SW_HIDE);
    }
}
fn internal_activation(next: HWND) -> bool {
    if next.is_invalid() {
        return false;
    }
    let root = window();
    unsafe {
        if next == root
            || IsChild(root, next).as_bool()
            || GetWindowThreadProcessId(next, None) == GetWindowThreadProcessId(root, None)
        {
            return true;
        }
        let mut owner = next;
        for _ in 0..8 {
            let Ok(parent) = GetWindow(owner, GW_OWNER) else {
                break;
            };
            if parent == root {
                return true;
            }
            if parent.is_invalid() {
                break;
            }
            owner = parent;
        }
    }
    false
}
fn query() {
    let input = text(control(EDIT));
    with(|s| {
        s.preview_draft = None;
        s.query = s.query.wrapping_add(1);
        s.pending = Some(Job::Query(s.query, input));
        s.changed = Instant::now();
        s.waiting = None;
        s.candidates.clear();
    });
    send(control(LIST), LB_RESETCONTENT, 0, 0);
    expand(false);
    enabled();
}
fn selected_choice() -> Option<String> {
    let selected = send(control(LIST), LB_GETCURSEL, 0, 0).0;
    with(|s| {
        if s.expanded && selected >= 0 {
            s.candidates.get(selected as usize).map(|c| c.text.clone())
        } else {
            None
        }
    })
    .flatten()
}

// SetWindowText synchronously sends EN_CHANGE. Preview updates must not start
// a new query, clear the list, or invalidate an in-flight launch as an edit.
fn preview_text(value: &str, start: u32, end: u32) {
    with(|s| s.setting_preview = true);
    set_text(control(EDIT), value);
    with(|s| s.setting_preview = false);
    send(control(EDIT), EM_SETSEL, start as usize, end as isize);
}

fn preview_selection() {
    let Some(choice) = selected_choice() else {
        return;
    };
    if with(|s| s.preview_draft.is_none()).unwrap_or(false) {
        let input = text(control(EDIT));
        let (mut start, mut end) = (0u32, 0u32);
        send(
            control(EDIT),
            EM_GETSEL,
            &mut start as *mut u32 as usize,
            &mut end as *mut u32 as isize,
        );
        with(|s| s.preview_draft = Some((input, start, end)));
    }
    let end = choice.encode_utf16().count() as u32;
    preview_text(&choice, end, end);
    status("");
    enabled();
}

fn cancel_preview() {
    if let Some((draft, start, end)) = with(|s| s.preview_draft.take()).flatten() {
        preview_text(&draft, start, end);
    }
}

fn adopt() -> bool {
    let choice = selected_choice();
    if let Some(choice) = choice {
        let end = choice.encode_utf16().count() as u32;
        preview_text(&choice, end, end);
        with(|s| {
            s.preview_draft = None;
            s.pending = None;
            s.query = s.query.wrapping_add(1);
        });
        expand(false);
        status("");
        unsafe {
            let _ = SetFocus(Some(control(EDIT)));
        }
        send(
            control(EDIT),
            EM_SETSEL,
            choice.encode_utf16().count(),
            choice.encode_utf16().count() as isize,
        );
        true
    } else {
        false
    }
}
fn run() {
    if adopt() {
        return;
    }
    let input = text(control(EDIT));
    if input.trim().is_empty() {
        return;
    }
    let mut selection = (0u32, 0u32);
    send(
        control(EDIT),
        EM_GETSEL,
        &mut selection.0 as *mut u32 as usize,
        &mut selection.1 as *mut u32 as isize,
    );
    let mode = if send(control(ADMIN), BM_GETCHECK, 0, 0).0 == BST_CHECKED.0 as isize {
        crate::shell_launch::LaunchMode::Administrator
    } else {
        crate::shell_launch::LaunchMode::Standard
    };
    let queued = with(|s| {
        if s.busy || s.actions.len() >= 32 {
            return false;
        }
        let id = s.generation;
        if s.launches
            .try_send(LaunchJob {
                id,
                input,
                mode,
                owner: s.window.0 as isize,
                selection,
            })
            .is_err()
        {
            return false;
        }
        s.busy = true;
        s.launch_id = id;
        s.authorizing = mode == crate::shell_launch::LaunchMode::Administrator;
        s.query = s.query.wrapping_add(1);
        s.pending = None;
        s.waiting = None;
        true
    })
    .unwrap_or(false);
    if queued {
        expand(false);
        status(crate::i18n::text("正在打开…"));
        enabled();
    }
}
fn poll() {
    if with(|s| s.layout_dirty && !s.layout_active).unwrap_or(false) {
        layout();
    }
    let replies = with(|s| s.replies.try_iter().collect::<Vec<_>>()).unwrap_or_default();
    for reply in replies {
        match reply {
            Reply::Query(id, items) => {
                with(|s| s.query_busy = false);
                if !with(|s| id == s.query && s.visible && !s.composing).unwrap_or(false) {
                    continue;
                }
                with(|s| {
                    s.waiting = None;
                    s.candidates = items.clone();
                });
                send(control(LIST), LB_RESETCONTENT, 0, 0);
                let dc = unsafe { GetDC(Some(control(LIST))) };
                let font = with(|s| s.font).unwrap_or_default();
                let old = unsafe { SelectObject(dc, font.into()) };
                let mut extent = 0;
                for item in &items {
                    let label = format!("[{}] {}", source_label(item.source), item.text);
                    let mut size = SIZE::default();
                    unsafe {
                        let _ = GetTextExtentPoint32W(
                            dc,
                            &label.encode_utf16().collect::<Vec<_>>(),
                            &mut size,
                        );
                    }
                    extent = extent.max(size.cx);
                    send(
                        control(LIST),
                        LB_ADDSTRING,
                        0,
                        wide(&label).as_ptr() as isize,
                    );
                }
                unsafe {
                    let _ = SelectObject(dc, old);
                    let _ = ReleaseDC(Some(control(LIST)), dc);
                }
                send(
                    control(LIST),
                    LB_SETHORIZONTALEXTENT,
                    (extent + 16).max(0) as usize,
                    0,
                );
                if !items.is_empty() {
                    expand(true);
                } else {
                    expand(false);
                }
                enabled();
            }
            Reply::History(entries, error, initial, retry) => {
                let cleared = !initial && entries.is_empty() && error.is_none();
                let recent = entries.first().cloned();
                with(|s| {
                    s.history = !entries.is_empty();
                    s.entries = entries;
                    s.clear_retry = retry;
                });
                if initial
                    && let Some(recent) = recent
                    && text(control(EDIT)).is_empty()
                {
                    set_text(control(EDIT), &recent);
                    with(|s| s.pending = None);
                }
                if let Some(error) = error {
                    with(|s| s.notice = Some(error.clone()));
                    status(&error);
                } else if cleared {
                    with(|s| s.notice = None);
                    status(crate::i18n::text("历史已清空"));
                }
                enabled();
            }
            Reply::Launch(job, result) => {
                let LaunchJob {
                    id,
                    input,
                    selection,
                    ..
                } = job;
                let current = with(|s| {
                    s.busy = false;
                    s.authorizing = false;
                    result_is_current(s.visible, s.generation, id)
                })
                .unwrap_or(false);
                if result.is_ok() {
                    with(|s| s.actions.push_back(Job::Record(input.clone())));
                    let restore =
                        with(|s| !s.visible && s.generation == id.wrapping_add(1)).unwrap_or(false);
                    if restore {
                        set_text(control(EDIT), &input);
                        with(|s| s.pending = None);
                    }
                }
                if current {
                    match result {
                        Ok(()) => {
                            set_text(control(EDIT), &input);
                            hide();
                        }
                        Err(error) => {
                            status(&error.message);
                            unsafe {
                                if IsWindowVisible(window()).as_bool() {
                                    let _ = SetFocus(Some(control(EDIT)));
                                }
                            }
                            send(
                                control(EDIT),
                                EM_SETSEL,
                                selection.0 as usize,
                                selection.1 as isize,
                            );
                        }
                    }
                }
                enabled();
            }
        }
    }
    let timedout = with(|s| {
        s.waiting
            .is_some_and(|t| t.elapsed() > Duration::from_millis(500))
    })
    .unwrap_or(false);
    if timedout {
        with(|s| {
            s.waiting = None;
            s.query = s.query.wrapping_add(1);
        });
        status(crate::i18n::text("路径建议暂不可用"));
    }
    with(|s| {
        if let Some(job) = s.actions.pop_front() {
            match s.jobs.try_send(job) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(job)) => s.actions.push_front(job),
                Err(_) => {}
            }
            return;
        }
        let ready = match s.pending {
            Some(Job::Query(..)) => {
                !s.composing && s.changed.elapsed() >= Duration::from_millis(150)
            }
            Some(_) => true,
            None => false,
        };
        if ready
            && !s.query_busy
            && let Some(Job::Query(id, input)) = s.pending.take()
        {
            match s.queries.try_send((id, input, s.entries.clone())) {
                Ok(()) => {
                    s.waiting = Some(Instant::now());
                    s.query_busy = true;
                }
                Err(mpsc::TrySendError::Full((id, input, _))) => {
                    s.pending = Some(Job::Query(id, input))
                }
                Err(_) => {}
            }
        }
    });
}
fn focus_next(back: bool) {
    let mut ids = vec![EDIT];
    if unsafe { IsWindowEnabled(control(ADMIN)).as_bool() } {
        ids.push(ADMIN);
    }
    if with(|s| s.history || s.clear_retry).unwrap_or(false) {
        ids.push(CLEAR);
    }
    if unsafe { IsWindowEnabled(control(RUN)).as_bool() } {
        ids.push(RUN);
    }
    ids.push(CANCEL);
    let focus = unsafe { GetFocus() };
    let index = ids.iter().position(|id| control(*id) == focus).unwrap_or(0);
    let next = if back {
        (index + ids.len() - 1) % ids.len()
    } else {
        (index + 1) % ids.len()
    };
    unsafe {
        let _ = SetFocus(Some(control(ids[next])));
    }
}
fn key(hwnd: HWND, msg: u32, w: WPARAM) -> bool {
    let composing = with(|s| s.composing).unwrap_or(false);
    if composing {
        return false;
    }
    let code = w.0 as u16;
    let alt = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
    if msg == WM_SYSKEYDOWN && code == VK_O.0 {
        unsafe {
            let _ = SetFocus(Some(control(EDIT)));
        }
        send(control(EDIT), EM_SETSEL, 0, -1);
        return true;
    }
    if code == VK_TAB.0 {
        focus_next(unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0);
        return true;
    }
    if code == VK_ESCAPE.0 {
        if with(|s| s.expanded).unwrap_or(false) {
            cancel_preview();
            expand(false);
        } else {
            hide();
        }
        return true;
    }
    if hwnd == control(EDIT) {
        if code == VK_A.0 && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 {
            send(hwnd, EM_SETSEL, 0, -1);
            return true;
        }
        if code == VK_RETURN.0 {
            run();
            return true;
        }
        if code == VK_DOWN.0 || code == VK_UP.0 {
            if alt {
                let expanded = with(|s| s.expanded).unwrap_or(false);
                if expanded {
                    cancel_preview();
                }
                expand(!expanded);
                if !expanded {
                    query();
                }
                return true;
            }
            if !with(|s| s.expanded).unwrap_or(false) {
                query();
                expand(true);
            } else {
                let count = with(|s| s.candidates.len()).unwrap_or(0);
                if count > 0 {
                    let selected = send(control(LIST), LB_GETCURSEL, 0, 0).0;
                    let next = if code == VK_DOWN.0 {
                        (selected + 1).min(count as isize - 1)
                    } else {
                        if selected < 0 {
                            count as isize - 1
                        } else {
                            (selected - 1).max(0)
                        }
                    };
                    send(control(LIST), LB_SETCURSEL, next as usize, 0);
                    preview_selection();
                }
            }
            return true;
        }
    } else if code == VK_RETURN.0 {
        let id = unsafe { GetDlgCtrlID(hwnd) } as u16;
        match id {
            RUN => run(),
            CANCEL => hide(),
            CLEAR => clear(),
            DROP => dropdown(),
            ADMIN => {
                send(control(ADMIN), BM_CLICK, 0, 0);
            }
            LIST => {
                adopt();
            }
            _ => {}
        }
        return true;
    }
    false
}
fn dropdown() {
    let expanded = with(|s| s.expanded).unwrap_or(false);
    if expanded {
        cancel_preview();
    }
    expand(!expanded);
    if !expanded {
        query();
    }
    unsafe {
        let _ = SetFocus(Some(control(EDIT)));
    }
}
fn clear() {
    cancel_preview();
    with(|s| {
        if !s.actions.iter().any(|job| matches!(job, Job::Clear)) {
            s.actions.push_back(Job::Clear);
        }
        s.pending = None;
        s.query = s.query.wrapping_add(1);
        s.candidates.clear();
        s.entries.clear();
        s.history = false;
        s.clear_retry = true;
    });
    expand(false);
    status(crate::i18n::text("正在清空历史…"));
}

fn source_label(source: CandidateSource) -> &'static str {
    crate::i18n::text(match source {
        CandidateSource::History => "最近",
        CandidateSource::Path => "路径",
        CandidateSource::Application => "应用",
    })
}
fn paint_candidate(item: &DRAWITEMSTRUCT) {
    let Some(candidate) = with(|s| s.candidates.get(item.itemID as usize).cloned()).flatten()
    else {
        return;
    };
    let dpi = with(|s| s.dpi).unwrap_or(96);
    let scale = |n: i32| n * dpi as i32 / 96;
    let theme = crate::config::theme();
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let primary = candidate.label.as_deref().unwrap_or(&candidate.text);
    let detail = if candidate.source == CandidateSource::History {
        source_label(candidate.source).to_string()
    } else {
        format!("{} · {}", source_label(candidate.source), candidate.text)
    };
    unsafe {
        let dc = item.hDC;
        let stock = HBRUSH(GetStockObject(DC_BRUSH).0);
        SetDCBrushColor(
            dc,
            COLORREF(if selected { theme.hover } else { theme.surface }),
        );
        FillRect(dc, &item.rcItem, stock);
        if selected {
            SetDCBrushColor(dc, COLORREF(theme.accent));
            FrameRect(dc, &item.rcItem, stock);
        }
        SetBkMode(dc, TRANSPARENT);
        let font = with(|s| s.font).unwrap_or_default();
        let old = SelectObject(dc, font.into());
        let mut top = item.rcItem;
        top.left += scale(10);
        top.right -= scale(10);
        top.top += scale(4);
        top.bottom = top.top + scale(20);
        SetTextColor(dc, COLORREF(theme.text));
        let mut title: Vec<u16> = primary.encode_utf16().collect();
        if !title.is_empty() {
            DrawTextW(
                dc,
                &mut title,
                &mut top,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
        let mut bottom = top;
        bottom.top = item.rcItem.top + scale(24);
        bottom.bottom = item.rcItem.bottom - scale(3);
        SetTextColor(dc, COLORREF(theme.muted));
        let mut subtitle: Vec<u16> = detail.encode_utf16().collect();
        DrawTextW(
            dc,
            &mut subtitle,
            &mut bottom,
            DT_SINGLELINE | DT_VCENTER | DT_PATH_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(dc, old);
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    crate::win_util::guard_abi(
        "run_dialog::wndproc",
        || unsafe {
            match msg {
                WM_DRAWITEM if l.0 != 0 => {
                    let item = &*(l.0 as *const DRAWITEMSTRUCT);
                    if item.CtlID == LIST as u32 {
                        paint_candidate(item);
                        return LRESULT(1);
                    }
                    return DefWindowProcW(hwnd, msg, w, l);
                }
                WM_HOTKEY if w.0 == 3 => show(),
                WM_CLOSE => hide(),
                WM_TIMER => poll(),
                WM_ACTIVATE if w.0 & 0xffff == WA_INACTIVE as usize => {
                    if with(|s| s.visible && !(s.busy && s.authorizing)).unwrap_or(false)
                        && !internal_activation(HWND(l.0 as *mut _))
                    {
                        hide();
                    }
                }
                WM_DPICHANGED => {
                    if l.0 != 0 {
                        let proposed = *(l.0 as *const RECT);
                        let monitor = MonitorFromRect(&proposed, MONITOR_DEFAULTTONEAREST);
                        let mut info = MONITORINFO {
                            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                            ..Default::default()
                        };
                        if GetMonitorInfoW(monitor, &mut info).as_bool() {
                            with(|s| s.work = info.rcWork);
                        }
                    }
                    with(|s| s.dpi = (w.0 & 0xffff) as u32);
                    layout();
                }
                WM_DISPLAYCHANGE => position(),
                WM_COMMAND => {
                    let id = (w.0 & 0xffff) as u16;
                    let code = (w.0 >> 16) as u32;
                    if id == EDIT && code == EN_CHANGE {
                        if with(|s| s.setting_preview).unwrap_or(false) {
                            return LRESULT(0);
                        }
                        with(|s| {
                            if s.busy {
                                s.generation = s.generation.wrapping_add(1);
                            }
                        });
                        if !with(|s| s.composing).unwrap_or(false) {
                            status("");
                            query();
                        }
                    } else if code == BN_CLICKED {
                        match id {
                            RUN => run(),
                            CANCEL => hide(),
                            CLEAR => clear(),
                            DROP => dropdown(),
                            ADMIN => {
                                status("");
                            }
                            _ => {}
                        }
                    } else if id == LIST && code == LBN_SELCHANGE {
                        adopt();
                    }
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if !key(hwnd, msg, w) {
                        return DefWindowProcW(hwnd, msg, w, l);
                    }
                }
                WM_DESTROY => {
                    let _ = KillTimer(Some(hwnd), 1);
                }
                _ => return DefWindowProcW(hwnd, msg, w, l),
            }
            LRESULT(0)
        },
        || unsafe { DefWindowProcW(hwnd, msg, w, l) },
    )
}
unsafe extern "system" fn child_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let previous = with(|s| {
        s.controls
            .iter()
            .find(|c| c.1 == hwnd)
            .map(|c| c.2)
            .unwrap_or(0)
    })
    .unwrap_or(0);
    let fallback = || unsafe {
        if previous == 0 {
            DefWindowProcW(hwnd, msg, w, l)
        } else {
            CallWindowProcW(
                std::mem::transmute::<isize, WNDPROC>(previous),
                hwnd,
                msg,
                w,
                l,
            )
        }
    };
    crate::win_util::guard_abi(
        "run_dialog::child_proc",
        || {
            if msg == WM_IME_STARTCOMPOSITION {
                with(|s| {
                    s.preview_draft = None;
                    s.composing = true;
                    s.query = s.query.wrapping_add(1);
                    s.pending = None;
                    s.waiting = None;
                });
                expand(false);
            }
            if msg == WM_IME_ENDCOMPOSITION {
                let result = fallback();
                with(|s| s.composing = false);
                query();
                return result;
            }
            if (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN) && key(hwnd, msg, w) {
                return LRESULT(0);
            }
            // Suppress translated control characters consumed above.
            if msg == WM_CHAR
                && !with(|s| s.composing).unwrap_or(false)
                && matches!(w.0, 9 | 13 | 27 | 1)
            {
                return LRESULT(0);
            }
            fallback()
        },
        fallback,
    )
}
pub fn shutdown() {
    if let Some(owner) = crate::hub::shell_hwnd() {
        let _ = unsafe { UnregisterHotKey(Some(owner), HOTKEY_ID as i32) };
    }
    let hwnd = window();
    if !hwnd.is_invalid() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    let old = STATE.with(|cell| cell.borrow_mut().take());
    if let Some(state) = old {
        unsafe {
            if !state.font.is_invalid() {
                let _ = DeleteObject(state.font.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_candidate_navigation_previews_without_requery_and_restores_draft() {
        let instance = HINSTANCE(unsafe {
            windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap()
                .0
        });
        prepare(instance).unwrap();
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                shutdown();
            }
        }
        let _cleanup = Cleanup;
        // Hidden native controls: no hotkey registration, external input,
        // command execution, history writes or replacement of the live shell.
        let choices = [
            "mini first --name 张三",
            "\"C:\\Program Files\\Mini App.exe\" --test",
        ];
        let fill = || {
            with(|s| {
                s.pending = None;
                s.candidates = choices
                    .iter()
                    .map(|value| Candidate {
                        text: (*value).into(),
                        label: None,
                        source: CandidateSource::History,
                    })
                    .collect();
            });
            send(control(LIST), LB_RESETCONTENT, 0, 0);
            for value in choices {
                send(
                    control(LIST),
                    LB_ADDSTRING,
                    0,
                    wide(value).as_ptr() as isize,
                );
            }
            expand(true);
        };
        set_text(control(EDIT), "mini");
        send(control(EDIT), EM_SETSEL, 1, 3);
        fill();
        let query_id = with(|s| s.query).unwrap();
        let press = |code: VIRTUAL_KEY| {
            send(control(EDIT), WM_KEYDOWN, code.0 as usize, 0);
        };
        press(VK_DOWN);
        assert_eq!(text(control(EDIT)), choices[0]);
        press(VK_DOWN);
        assert_eq!(text(control(EDIT)), choices[1]);
        press(VK_UP);
        assert_eq!(text(control(EDIT)), choices[0]);
        assert!(with(|s| s.expanded && s.query == query_id && s.pending.is_none()).unwrap());
        assert_eq!(send(control(LIST), LB_GETCOUNT, 0, 0).0, 2);
        press(VK_ESCAPE);
        assert_eq!(text(control(EDIT)), "mini");
        let (mut start, mut end) = (0u32, 0u32);
        send(
            control(EDIT),
            EM_GETSEL,
            &mut start as *mut u32 as usize,
            &mut end as *mut u32 as isize,
        );
        assert_eq!((start, end), (1, 3));
        assert!(!with(|s| s.expanded).unwrap());

        fill();
        press(VK_DOWN);
        press(VK_RETURN);
        assert_eq!(text(control(EDIT)), choices[0]);
        assert!(with(|s| !s.expanded && s.preview_draft.is_none() && !s.busy).unwrap());

        set_text(control(EDIT), "mini");
        fill();
        press(VK_DOWN);
        send(control(EDIT), WM_CHAR, 'x' as usize, 0);
        assert_eq!(text(control(EDIT)), format!("{}x", choices[0]));
        assert!(with(|s| s.preview_draft.is_none() && s.pending.is_some() && !s.expanded).unwrap());
        fill();
        press(VK_DOWN);
        hide();
        assert_eq!(text(control(EDIT)), format!("{}x", choices[0]));

        fill();
        with(|s| s.composing = true);
        let before = text(control(EDIT));
        assert!(!key(control(EDIT), WM_KEYDOWN, WPARAM(VK_DOWN.0 as usize)));
        assert_eq!(text(control(EDIT)), before);
    }

    #[test]
    fn native_admin_selection_snapshots_and_cancelled_or_stale_replies_preserve_drafts() {
        let instance = HINSTANCE(unsafe {
            windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap()
                .0
        });
        prepare(instance).unwrap();
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                shutdown();
            }
        }
        let _cleanup = Cleanup;
        // Replace only this hidden fixture's channels: no external launch or UAC.
        let (launches, received) = mpsc::sync_channel(1);
        let (replies, incoming) = mpsc::channel();
        with(|s| {
            s.launches = launches;
            s.replies = incoming;
        });
        assert!(begin_session());
        assert_eq!(send(control(ADMIN), BM_GETCHECK, 0, 0).0, 0);
        send(control(ADMIN), BM_SETCHECK, BST_CHECKED.0 as usize, 0);
        assert_eq!(
            send(control(ADMIN), BM_GETCHECK, 0, 0).0,
            BST_CHECKED.0 as isize
        );
        set_text(control(EDIT), "fixture --name 张三");
        send(control(EDIT), EM_SETSEL, 2, 7);
        assert!(
            !begin_session(),
            "repeated summon must retain the current selection"
        );
        assert_eq!(send(control(ADMIN), BM_GETCHECK, 0, 0).0, 1);
        run();
        run();
        let job = received.try_recv().unwrap();
        assert!(received.try_recv().is_err(), "duplicate submission");
        assert_eq!(job.mode, crate::shell_launch::LaunchMode::Administrator);
        assert_eq!(job.input, "fixture --name 张三");
        assert_eq!(job.selection, (2, 7));
        assert!(!unsafe { IsWindowEnabled(control(ADMIN)).as_bool() });
        send(window(), WM_ACTIVATE, WA_INACTIVE as usize, 0);
        assert!(
            with(|s| s.visible).unwrap(),
            "UAC deactivation must not discard the pending result"
        );
        replies
            .send(Reply::Launch(
                job,
                Err(crate::shell_launch::error_from_code(1223)),
            ))
            .unwrap();
        poll();
        assert_eq!(text(control(EDIT)), "fixture --name 张三");
        assert!(with(|s| !s.busy && !s.authorizing && s.actions.is_empty()).unwrap());
        let (mut start, mut end) = (0u32, 0u32);
        send(
            control(EDIT),
            EM_GETSEL,
            &mut start as *mut u32 as usize,
            &mut end as *mut u32 as isize,
        );
        assert_eq!((start, end), (2, 7));
        assert!(!text(control(STATUS)).is_empty());
        hide();
        assert!(begin_session());
        assert_eq!(send(control(ADMIN), BM_GETCHECK, 0, 0).0, 0);
        run();
        let job = received.try_recv().unwrap();
        assert_eq!(job.mode, crate::shell_launch::LaunchMode::Standard);
        hide();
        assert!(begin_session());
        set_text(control(EDIT), "new draft");
        status("new status");
        replies
            .send(Reply::Launch(
                job,
                Err(crate::shell_launch::error_from_code(5)),
            ))
            .unwrap();
        poll();
        assert_eq!(text(control(EDIT)), "new draft");
        assert_eq!(text(control(STATUS)), "new status");
        assert!(with(|s| s.actions.is_empty() && !s.busy).unwrap());
        let (records, recorded) = mpsc::sync_channel(1);
        with(|s| s.jobs = records);
        set_text(control(EDIT), "accepted old command");
        run();
        let job = received.try_recv().unwrap();
        set_text(control(EDIT), "edited pending command");
        status("keep this status");
        replies.send(Reply::Launch(job, Ok(()))).unwrap();
        poll();
        assert_eq!(text(control(EDIT)), "edited pending command");
        assert_eq!(text(control(STATUS)), "keep this status");
        assert!(
            matches!(recorded.try_recv(), Ok(Job::Record(value)) if value == "accepted old command")
        );
    }

    #[test]
    fn layouts_stay_inside_each_monitor_and_keep_actions_visible() {
        for dpi in [96, 144, 192] {
            for work in [
                RECT {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1040,
                },
                RECT {
                    left: -1600,
                    top: -200,
                    right: 0,
                    bottom: 700,
                },
                RECT {
                    left: 40,
                    top: 60,
                    right: 360,
                    bottom: 300,
                },
            ] {
                let caption = 24 * dpi as i32 / 96;
                let g = geometry(work, dpi, 20, 44 * dpi as i32 / 96, caption);
                assert!(g.x >= work.left && g.y >= work.top, "{g:?}");
                assert!(
                    g.x + g.width <= work.right && g.y + g.height <= work.bottom,
                    "{g:?}"
                );
                assert!(
                    g.button_y + 30 * dpi as i32 / 96 <= g.height - caption,
                    "actions clipped: {g:?}"
                );
                assert!(g.list_y >= g.input_y + 30 * dpi as i32 / 96);
                assert!(
                    g.status_height >= 16 * dpi as i32 / 96,
                    "error text disappeared: {g:?}"
                );
            }
        }
    }
    #[test]
    fn candidates_use_only_needed_rows_and_yield_space_to_error_text() {
        let work = RECT {
            right: 1000,
            bottom: 500,
            ..Default::default()
        };
        let quiet = geometry(work, 96, 0, 0, 24);
        let failure = geometry(work, 96, 0, 44, 24);
        assert_eq!(quiet.status_height, 0);
        assert!(failure.height > quiet.height);
        let one = geometry(work, 96, 1, 22, 24);
        let many = geometry(work, 96, 20, 22, 24);
        assert_eq!(one.list_height, 48);
        assert_eq!(many.list_height, 268);
        let long_error = geometry(work, 96, 20, 220, 24);
        assert_eq!(long_error.status_height, 220);
        assert!(long_error.list_height < many.list_height);
        assert!(long_error.button_y + 30 <= long_error.height - 24);
    }
    #[test]
    fn hidden_reopened_or_edited_requests_cannot_change_current_ui() {
        let submitted = 7;
        assert!(result_is_current(true, submitted, submitted));
        assert!(!result_is_current(false, submitted, submitted));
        assert!(!result_is_current(true, submitted + 1, submitted)); // edit while pending
        assert!(!result_is_current(true, submitted + 2, submitted)); // hide, reopen
        assert!(!result_is_current(true, 0, u64::MAX)); // generation wraps on hide
    }
    #[test]
    fn native_status_measurement_handles_empty_and_wrapped_chinese() {
        // Exercise the real Win32 boundary that previously read empty Vec's 0x2 pointer.
        assert_eq!(measure_status(HDC::default(), "", 120), 0);
        unsafe {
            let dc = CreateCompatibleDC(None);
            assert!(!dc.is_invalid());
            let single = measure_status(dc, "找不到目标。", 500);
            let wrapped = measure_status(
                dc,
                "找不到目标。请检查名称或路径后重试。没有默认打开方式，请为此文件设置关联应用。",
                100,
            );
            assert!(single > 0);
            assert!(wrapped > single);
            for _ in 0..100 {
                assert_eq!(measure_status(dc, "", 100), 0);
            }
            assert!(DeleteDC(dc).as_bool());
        }
    }
}
