//! Native task manager host. Collection and actions never run on the UI thread.
mod view;
use crate::{
    palette,
    task_service::{self as service, Identity, Snapshot},
    win_util::{guard_abi, wide},
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{DataExchange::*, LibraryLoader::GetModuleHandleW, Memory::*, Threading::*},
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::PCWSTR,
};

const SEARCH: i32 = 1101;
const REFRESH: i32 = 1102;
const NETWORK: i32 = 1103;
const TREE: i32 = 1104;
const LANGUAGE: i32 = 1105;
const THEME: i32 = 1106;
const LIST: i32 = 1107;
const DETAIL: i32 = 1108;
const LOCATION: i32 = 1109;
const COPY_PATH: i32 = 1110;
const COPY_COMMAND: i32 = 1111;
const SWITCH: i32 = 1112;
const CLOSE: i32 = 1113;
const END: i32 = 1114;
const STATUS: i32 = 1115;
const SUMMARY: i32 = 1116;
const ADMIN: i32 = 1117;
const ACCOUNT: i32 = 1118;
const CONTROLS: [i32; 18] = [
    SEARCH,
    REFRESH,
    NETWORK,
    TREE,
    LANGUAGE,
    THEME,
    ADMIN,
    LIST,
    DETAIL,
    LOCATION,
    COPY_PATH,
    COPY_COMMAND,
    SWITCH,
    CLOSE,
    END,
    STATUS,
    SUMMARY,
    ACCOUNT,
];
type Slot<T> = Arc<Mutex<Option<T>>>;
fn slot<T>() -> Slot<T> {
    Arc::new(Mutex::new(None))
}
fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.try_lock().ok()?.take()
}
fn put<T>(slot: &Slot<T>, value: T) {
    if let Ok(mut result) = slot.lock() {
        *result = Some(value);
    }
}

#[derive(Clone, Default)]
pub struct Options {
    pub chinese: bool,
    pub light: bool,
    pub focus: Option<Identity>,
    pub search: String,
    pub network: bool,
    pub access: Option<crate::task_access::Access>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct ViewRequest {
    focus: Option<Identity>,
    search: String,
    network: bool,
}

enum Action {
    Administrator(isize, crate::task_launch::Request),
    Location(Identity),
    Switch(Identity, String),
    Close(Identity, String),
    End(Identity),
}
struct Workers {
    stop: Arc<AtomicBool>,
    sample: SyncSender<()>,
    sampled: Slot<Result<Snapshot, String>>,
    detail: Slot<Identity>,
    detail_wake: SyncSender<()>,
    detailed: Slot<(Identity, Result<String, String>)>,
    action: SyncSender<Action>,
    acted: Slot<Result<&'static str, String>>,
}
impl Workers {
    fn new(access: crate::task_access::Access) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (sample, rx) = mpsc::sync_channel(1);
        let sampled = slot();
        let target = sampled.clone();
        let closing = stop.clone();
        let sampler_access = access.clone();
        std::thread::spawn(move || {
            let mut collector = service::Collector::with_access(sampler_access);
            while rx.recv().is_ok() && !closing.load(Ordering::Relaxed) {
                let result = collector.sample();
                if !closing.load(Ordering::Relaxed) {
                    put(&target, result);
                }
            }
        });
        let (detail_wake, rx) = mpsc::sync_channel(1);
        let detail = slot();
        let pending = detail.clone();
        let detail_access = access.clone();
        let detailed = slot();
        let target = detailed.clone();
        let closing = stop.clone();
        std::thread::spawn(move || {
            while rx.recv().is_ok() && !closing.load(Ordering::Relaxed) {
                if let Some(identity) = take(&pending) {
                    let result = detail_access.command_line(identity);
                    if !closing.load(Ordering::Relaxed) {
                        put(&target, (identity, result));
                    }
                }
            }
        });
        let (action, rx) = mpsc::sync_channel(1);
        let acted = slot();
        let target = acted.clone();
        let closing = stop.clone();
        std::thread::spawn(move || {
            while let Ok(action) = rx.recv() {
                if closing.load(Ordering::Relaxed) {
                    break;
                }
                let result = match action {
                    Action::Administrator(owner, request) => std::env::current_exe()
                        .map_err(|e| e.to_string())
                        .and_then(|path| crate::task_launch::administrator(&path, owner, request))
                        .map(|outcome| match outcome {
                            crate::task_launch::Outcome::Opened => "admin",
                            crate::task_launch::Outcome::Cancelled => "admin-cancel",
                        }),
                    Action::Location(key) => access.open_location(key).map(|_| "location"),
                    Action::Switch(key, window) => access.activate(key, &window).map(|_| "switch"),
                    Action::Close(key, window) => {
                        access.close_window(key, &window).map(|_| "close")
                    }
                    Action::End(key) => access.terminate(key).map(|_| "end"),
                };
                put(&target, result);
            }
        });
        Self {
            stop,
            sample,
            sampled,
            detail,
            detail_wake,
            detailed,
            action,
            acted,
        }
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
#[derive(Clone)]
enum Notice {
    Outcome(Result<&'static str, String>),
    Sampling(String),
    Partial(String),
    Pending(bool),
}
struct State {
    options: Options,
    access: crate::task_access::Access,
    elevated: bool,
    snapshot: Option<Rc<Snapshot>>,
    rows: Rc<Vec<view::Row>>,
    selected: Option<Identity>,
    limited: Option<u32>,
    focus: Option<Identity>,
    filter: String,
    network: bool,
    tree: bool,
    expanded: HashSet<Identity>,
    sort: (usize, bool),
    rebuilding: bool,
    commands: HashMap<Identity, Result<String, String>>,
    requested: Option<Identity>,
    notice: Option<Notice>,
    workers: Workers,
    next: Instant,
    busy: bool,
    font: HFONT,
}
thread_local! {static STATE:RefCell<Option<State>>=const{RefCell::new(None)};}
fn read<R>(f: impl FnOnce(&State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow().ok()?.as_ref().map(f))
}
fn edit<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}
fn text(en: &str, zh: &str) -> String {
    if read(|s| s.options.chinese).unwrap_or(false) {
        zh
    } else {
        en
    }
    .into()
}
fn control(hwnd: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(Some(hwnd), id).unwrap_or_default() }
}
fn set(hwnd: HWND, value: &str) {
    let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(wide(value).as_ptr())) };
}
fn message(hwnd: HWND, id: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, id, Some(WPARAM(w)), Some(LPARAM(l))) }
}
fn get_text(hwnd: HWND) -> String {
    unsafe {
        let mut value = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let n = GetWindowTextW(hwnd, &mut value);
        String::from_utf16_lossy(&value[..n.max(0) as usize])
    }
}
fn colors() -> palette::Theme {
    palette::base(read(|s| s.options.light).unwrap_or(false))
}

pub fn run(mut options: Options) -> Result<(), String> {
    let access = match options.access.take() {
        Some(access) => access,
        None => crate::task_access::Access::current()?,
    };
    unsafe {
        let elevated = service::elevated()?;
        let instance_key = access.instance_key()?;
        let class = format!(
            "minicon_tskmgr_{}",
            env!("CARGO_PKG_VERSION").replace('.', "_")
        );
        let class = format!(
            "{}_{}_{}",
            class.replace('.', "_"),
            u8::from(elevated),
            instance_key
        );
        let name = wide(&format!(
            "Local\\minicon-tskmgr.{}.{}.{}",
            env!("CARGO_PKG_VERSION"),
            u8::from(elevated),
            instance_key
        ));
        let mutex = CreateMutexW(None, false, PCWSTR(name.as_ptr())).map_err(|e| e.to_string())?;
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        struct Guard(HANDLE);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
        let _guard = Guard(mutex);
        if already {
            let existing = FindWindowW(PCWSTR(wide(&class).as_ptr()), None)
                .map_err(|_| "Task manager is starting. Try again shortly.")?;
            let mut pid = 0;
            GetWindowThreadProcessId(existing, Some(&mut pid));
            service::identify(pid)?;
            let image = crate::win_util::window_exe_path(existing)
                .ok_or("Cannot verify the existing task manager")?;
            let normalize =
                |value: &str| value.strip_prefix(r"\\?\").unwrap_or(value).to_lowercase();
            let current = std::env::current_exe().map_err(|e| e.to_string())?;
            if normalize(&image) != normalize(&current.to_string_lossy()) {
                return Err("Another build of this task manager is running. Close it before opening this build.".into());
            }
            if options.focus.is_some() || !options.search.is_empty() || options.network {
                let mut payload = serde_json::to_vec(&ViewRequest {
                    focus: options.focus,
                    search: options.search.clone(),
                    network: options.network,
                })
                .map_err(|e| e.to_string())?;
                if payload.len() > 4096 || options.search.encode_utf16().count() > 512 {
                    return Err("Search request is too long".into());
                }
                let data = COPYDATASTRUCT {
                    dwData: 0x54534b32,
                    cbData: payload.len() as u32,
                    lpData: payload.as_mut_ptr().cast(),
                };
                let mut result = 0usize;
                let delivered = SendMessageTimeoutW(
                    existing,
                    WM_COPYDATA,
                    WPARAM(0),
                    LPARAM((&data as *const COPYDATASTRUCT) as isize),
                    SMTO_ABORTIFHUNG,
                    1000,
                    Some(&mut result),
                );
                if delivered.0 == 0 || result != 1 {
                    return Err("The running task manager did not accept the view request.".into());
                }
            }
            let _ = crate::win_util::force_foreground(existing);
            return Ok(());
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).ok();
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES,
        })
        .ok()
        .map_err(|e| e.to_string())?;
        let instance = HINSTANCE(GetModuleHandleW(None).map_err(|e| e.to_string())?.0);
        let name = wide(&class);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: PCWSTR(name.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread().to_string());
        }
        let focus = options.focus;
        let filter = options.search.clone();
        let network = options.network;
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                options,
                access: access.clone(),
                elevated,
                snapshot: None,
                rows: Rc::new(vec![]),
                selected: focus,
                limited: None,
                focus,
                filter,
                network,
                tree: false,
                expanded: HashSet::new(),
                sort: (0, false),
                rebuilding: false,
                commands: HashMap::new(),
                requested: None,
                notice: None,
                workers: Workers::new(access),
                next: Instant::now(),
                busy: false,
                font: HFONT::default(),
            })
        });
        let created = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            PCWSTR(name.as_ptr()),
            PCWSTR(wide(&format!("minicon-tskmgr {}", env!("CARGO_PKG_VERSION"))).as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1120,
            740,
            None,
            None,
            Some(instance),
            None,
        );
        let hwnd = match created {
            Ok(h) => h,
            Err(e) => {
                STATE.with(|s| s.borrow_mut().take());
                return Err(e.to_string());
            }
        };
        if let Err(error) = install(hwnd, instance) {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }
        let initial_search = read(|s| s.filter.clone()).unwrap_or_default();
        message(control(hwnd, SEARCH), EM_SETLIMITTEXT, 512, 0);
        set(control(hwnd, SEARCH), &initial_search);
        message(control(hwnd, NETWORK), BM_SETCHECK, usize::from(network), 0);
        labels(hwnd);
        theme(hwnd);
        font(hwnd);
        resize(hwnd);
        columns(hwnd);
        selected_detail(hwnd);
        SetTimer(Some(hwnd), 1, 200, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(control(hwnd, SEARCH)));
        let mut msg = MSG::default();
        loop {
            let status = GetMessageW(&mut msg, None, 0, 0).0;
            if status == 0 {
                break;
            }
            if status < 0 {
                return Err(windows::core::Error::from_thread().to_string());
            }
            if msg.message == WM_KEYDOWN {
                if msg.wParam.0 == VK_F5.0 as usize {
                    command(hwnd, REFRESH);
                    continue;
                }
                if msg.wParam.0 == VK_F.0 as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                    let _ = SetFocus(Some(control(hwnd, SEARCH)));
                    continue;
                }
                if msg.wParam.0 == VK_ESCAPE.0 as usize {
                    if msg.hwnd == control(hwnd, SEARCH) && !get_text(msg.hwnd).is_empty() {
                        set(msg.hwnd, "");
                    } else {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    continue;
                }
                if msg.wParam.0 == VK_A.0 as usize
                    && GetKeyState(VK_CONTROL.0 as i32) < 0
                    && [control(hwnd, SEARCH), control(hwnd, DETAIL)].contains(&msg.hwnd)
                {
                    message(msg.hwnd, EM_SETSEL, 0, -1);
                    continue;
                }
                if msg.hwnd == control(hwnd, LIST)
                    && [
                        VK_LEFT.0 as usize,
                        VK_RIGHT.0 as usize,
                        VK_RETURN.0 as usize,
                    ]
                    .contains(&msg.wParam.0)
                {
                    if tree_key(hwnd, msg.wParam.0) {
                        continue;
                    }
                    if msg.wParam.0 == VK_RETURN.0 as usize {
                        command(hwnd, SWITCH);
                        continue;
                    }
                }
                if msg.wParam.0 == VK_DELETE.0 as usize && msg.hwnd == control(hwnd, LIST) {
                    command(hwnd, END);
                    continue;
                }
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}

unsafe fn install(hwnd: HWND, instance: HINSTANCE) -> Result<(), String> {
    unsafe {
        for id in CONTROLS {
            let (class, style) = match id {
                SEARCH => ("EDIT", WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32)),
                DETAIL => (
                    "EDIT",
                    WS_TABSTOP
                        | WS_VSCROLL
                        | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                ),
                LIST => (
                    "SysListView32",
                    WS_TABSTOP
                        | WS_VSCROLL
                        | WS_HSCROLL
                        | WINDOW_STYLE(
                            LVS_REPORT | LVS_OWNERDATA | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
                        ),
                ),
                NETWORK | TREE => ("BUTTON", WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32)),
                STATUS | SUMMARY | ACCOUNT => ("STATIC", WINDOW_STYLE(0)),
                _ => ("BUTTON", WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32)),
            };
            let child = CreateWindowExW(
                if id == SEARCH || id == DETAIL {
                    WS_EX_CLIENTEDGE
                } else {
                    WINDOW_EX_STYLE(0)
                },
                PCWSTR(wide(class).as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                Some(hwnd),
                Some(HMENU(id as usize as *mut _)),
                Some(instance),
                None,
            )
            .map_err(|e| e.to_string())?;
            let _ = SetWindowTheme(child, PCWSTR(wide("").as_ptr()), PCWSTR(wide("").as_ptr()));
        }
        message(
            control(hwnd, LIST),
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
        );
        message(
            control(hwnd, SEARCH),
            EM_SETCUEBANNER,
            0,
            wide(&text(
                "Search name, PID, path or window title",
                "搜索名称、PID、路径或窗口标题",
            ))
            .as_ptr() as isize,
        );
        Ok(())
    }
}
fn labels(hwnd: HWND) {
    for (id, en, zh) in [
        (REFRESH, "Refresh", "刷新"),
        (
            ADMIN,
            "All users / system (admin)…",
            "所有用户 / 系统（管理员）…",
        ),
        (NETWORK, "Network", "网络"),
        (TREE, "Process tree", "进程树"),
        (LANGUAGE, "中文", "English"),
        (THEME, "Light / Dark", "白天 / 黑夜"),
        (LOCATION, "Open location", "打开位置"),
        (COPY_PATH, "Copy path", "复制路径"),
        (COPY_COMMAND, "Copy command", "复制命令行"),
        (SWITCH, "Switch to", "切换窗口"),
        (CLOSE, "Close window", "关闭窗口"),
        (END, "End process…", "结束进程…"),
    ] {
        set(control(hwnd, id), &text(en, zh));
    }
    if let Some(accounts) = read(|s| {
        if s.access.scope() == service::Scope::OwnTasks {
            return format!(
                "{}: {} · {} {}",
                text("Account", "账户"),
                s.access.origin().owner,
                text("Session", "会话"),
                s.access.origin().session
            );
        }
        format!(
            "{}: {} · {} {}\r\n{}: {} · {} {}",
            text("Origin", "原用户"),
            s.access.origin().owner,
            text("Session", "会话"),
            s.access.origin().session,
            text("Execution", "执行账户"),
            s.access.execution().owner,
            text("Session", "会话"),
            s.access.execution().session
        )
    }) {
        set(control(hwnd, ACCOUNT), &accounts);
    }
    set(
        hwnd,
        &format!(
            "minicon-tskmgr {} — {}{}",
            env!("CARGO_PKG_VERSION"),
            if read(|s| s.access.scope() == service::Scope::AllTasks).unwrap_or(false) {
                text("All users and system tasks", "所有用户与系统任务")
            } else {
                text("My tasks", "我的任务")
            },
            if read(|s| s.elevated).unwrap_or(false) {
                text(" · Administrator", " · 管理员权限")
            } else {
                String::new()
            }
        ),
    );
    message(
        control(hwnd, SEARCH),
        EM_SETCUEBANNER,
        0,
        wide(&text(
            "Search processes, users or endpoints",
            "搜索进程、用户或网络端点",
        ))
        .as_ptr() as isize,
    );
}
fn font(hwnd: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let mut value = LOGFONTW {
            lfHeight: -(14 * dpi / 96),
            ..Default::default()
        };
        let face = wide("Segoe UI");
        value.lfFaceName[..face.len()].copy_from_slice(&face);
        let new = CreateFontIndirectW(&value);
        if new.is_invalid() {
            return;
        }
        let old = edit(|s| {
            let old = s.font;
            s.font = new;
            old
        })
        .unwrap_or_default();
        for id in CONTROLS {
            message(control(hwnd, id), WM_SETFONT, new.0 as usize, 1);
        }
        if !old.is_invalid() {
            let _ = DeleteObject(old.into());
        }
    }
}
fn theme(hwnd: HWND) {
    unsafe {
        let t = colors();
        let dark = i32::from(!read(|s| s.options.light).unwrap_or(false));
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast(),
            4,
        );
        let list = control(hwnd, LIST);
        message(list, LVM_SETBKCOLOR, 0, t.surface as isize);
        message(list, LVM_SETTEXTBKCOLOR, 0, t.surface as isize);
        message(list, LVM_SETTEXTCOLOR, 0, t.text as isize);
        let _ = RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE,
        );
    }
}
fn resize(hwnd: HWND) {
    unsafe {
        let mut r = RECT::default();
        if GetClientRect(hwnd, &mut r).is_err() {
            return;
        }
        let scale = |n| n * GetDpiForWindow(hwnd).max(96) as i32 / 96;
        let m = scale(14);
        let gap = scale(8);
        let h = scale(30);
        let width = r.right - 2 * m;
        let place = |id, x, y, w: i32, h: i32| {
            let _ = MoveWindow(control(hwnd, id), x, y, w.max(1), h.max(1), true);
        };
        let fixed = scale(480);
        place(SEARCH, m, m, (width - fixed).max(scale(100)), h);
        let mut x = (m + width - fixed).max(m + scale(100));
        for (id, w) in [
            (REFRESH, 78),
            (NETWORK, 82),
            (TREE, 110),
            (LANGUAGE, 85),
            (THEME, 100),
        ] {
            let w = scale(w);
            place(id, x, m, w, h);
            x += w + gap;
        }
        let button_w = (width - scale(40)) / 6;
        let bottom = (r.bottom - m - h).max(m + h);
        for (i, id) in [LOCATION, COPY_PATH, COPY_COMMAND, SWITCH, CLOSE, END]
            .iter()
            .enumerate()
        {
            place(*id, m + i as i32 * (button_w + gap), bottom, button_w, h);
        }
        place(STATUS, m, bottom - scale(34), width, scale(28));
        let detail_h = scale(104);
        let detail_y = (bottom - scale(42) - detail_h).max(m + scale(110));
        place(DETAIL, m, detail_y, width, detail_h);
        let own = read(|s| s.access.scope() == service::Scope::OwnTasks).unwrap_or(true);
        let _ = ShowWindow(control(hwnd, ADMIN), if own { SW_SHOW } else { SW_HIDE });
        place(
            ADMIN,
            m + width - scale(255),
            m + h + scale(5),
            scale(255),
            scale(25),
        );
        place(
            SUMMARY,
            m,
            m + h + scale(5),
            if own { width - scale(265) } else { width },
            scale(25),
        );
        place(ACCOUNT, m, m + h + scale(34), width, scale(36));
        let list_y = m + h + scale(78);
        place(
            LIST,
            m,
            list_y,
            width,
            (detail_y - gap - list_y).max(scale(60)),
        );
    }
}
fn columns(hwnd: HWND) {
    let list = control(hwnd, LIST);
    while message(list, LVM_DELETECOLUMN, 0, 0).0 != 0 {}
    let network = read(|s| s.network).unwrap_or(false);
    let columns = if network {
        vec![
            ("Process", "进程", 190),
            ("PID", "PID", 70),
            ("Protocol", "协议", 75),
            ("Local endpoint", "本地端点", 245),
            ("Remote endpoint", "远端端点", 245),
            ("State", "状态", 140),
            ("Owner", "所属用户", 230),
            ("Session", "会话", 75),
        ]
    } else {
        vec![
            ("Process", "进程", 230),
            ("PID", "PID", 75),
            ("Session", "会话", 75),
            ("Owner", "所属用户", 290),
            ("CPU %", "CPU %", 75),
            ("Working MiB", "工作集 MiB", 115),
            ("Private MiB", "私有提交 MiB", 115),
            ("Window", "窗口", 300),
        ]
    };
    let dpi = unsafe { GetDpiForWindow(hwnd).max(96) } as i32;
    for (index, (en, zh, width)) in columns.iter().enumerate() {
        let mut label = wide(&text(en, zh));
        let column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
            cx: width * dpi / 96,
            pszText: windows::core::PWSTR(label.as_mut_ptr()),
            iSubItem: index as i32,
            ..Default::default()
        };
        message(
            list,
            LVM_INSERTCOLUMNW,
            index,
            (&column as *const LVCOLUMNW) as isize,
        );
    }
}
fn rebuild(hwnd: HWND) {
    let Some((snapshot, filter, network, tree, expanded, sort, selected, focus)) = read(|s| {
        (
            s.snapshot.clone(),
            s.filter.clone(),
            s.network,
            s.tree,
            s.expanded.clone(),
            s.sort,
            s.selected,
            s.focus,
        )
    }) else {
        return;
    };
    let Some(snapshot) = snapshot else {
        return;
    };
    let rows = Rc::new(view::rows(
        &snapshot, &filter, network, tree, &expanded, sort,
    ));
    let desired = focus.or(selected);
    let limited = read(|s| s.limited).flatten();
    let index = desired
        .and_then(|key| rows.iter().position(|row| row.identity == Some(key)))
        .or_else(|| {
            limited.and_then(|pid| {
                rows.iter()
                    .position(|row| row.identity.is_none() && row.pid == pid)
            })
        });
    edit(|s| {
        s.rows = rows.clone();
        s.rebuilding = true;
        if index.is_none() {
            s.limited = None;
        }
        if focus.is_some() {
            s.focus = None;
        }
    });
    let list = control(hwnd, LIST);
    message(list, WM_SETREDRAW, 0, 0);
    let clear = LVITEMW {
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        state: LIST_VIEW_ITEM_STATE_FLAGS(0),
        ..Default::default()
    };
    message(
        list,
        LVM_SETITEMSTATE,
        usize::MAX,
        (&clear as *const LVITEMW) as isize,
    );
    message(list, LVM_SETITEMCOUNT, rows.len(), LVSICF_NOSCROLL as isize);
    if let Some(index) = index {
        let selected = LVITEMW {
            stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
            state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
            ..Default::default()
        };
        message(
            list,
            LVM_SETITEMSTATE,
            index,
            (&selected as *const LVITEMW) as isize,
        );
        if focus.is_some() {
            message(list, LVM_ENSUREVISIBLE, index, 0);
        }
    }
    edit(|s| s.rebuilding = false);
    message(list, WM_SETREDRAW, 1, 0);
    unsafe {
        let _ = InvalidateRect(Some(list), None, true);
    }
    let summary = format!(
        "{} · {} {} · {} {} · {}",
        if snapshot.scope == service::Scope::AllTasks {
            text("All users", "所有用户")
        } else {
            text("My account", "当前用户")
        },
        snapshot.processes.len() + snapshot.unavailable.len(),
        text("processes", "个进程"),
        snapshot.endpoints.len(),
        text("endpoints", "个端点"),
        text(
            "CPU: all cores · MiB · 2s",
            "CPU 整机占比 · 内存 MiB · 每 2 秒刷新"
        )
    );
    let extra = if snapshot.ownership_unavailable > 0 {
        format!(
            " {} {}",
            snapshot.ownership_unavailable,
            text("processes have limited metadata", "个进程信息受限")
        )
    } else {
        String::new()
    };
    set(control(hwnd, SUMMARY), &(summary + &extra));
    if !snapshot.limitations.is_empty() {
        edit(|s| {
            if matches!(
                s.notice,
                None | Some(Notice::Sampling(_) | Notice::Partial(_))
            ) {
                s.notice = Some(Notice::Partial(snapshot.limitations.join("; ")));
            }
        });
        render_notice(hwnd);
    } else {
        edit(|s| {
            if matches!(s.notice, Some(Notice::Sampling(_) | Notice::Partial(_))) {
                s.notice = None;
            }
        });
        render_notice(hwnd);
    }
    selected_detail(hwnd);
}
fn selected_detail(hwnd: HWND) {
    let Some((snapshot, key, visible, command, busy, requested)) = read(|s| {
        let visible = s
            .selected
            .is_some_and(|key| s.rows.iter().any(|r| r.identity == Some(key)));
        (
            s.snapshot.clone(),
            s.selected,
            visible,
            s.selected.and_then(|k| s.commands.get(&k).cloned()),
            s.busy,
            s.requested,
        )
    }) else {
        return;
    };
    let process = key.and_then(|key| {
        snapshot
            .as_ref()?
            .processes
            .iter()
            .find(|p| p.identity == key)
    });
    let mut allowed = [false; 6];
    if let Some(process) = process.filter(|_| visible) {
        let command_text = match &command {
            Some(Ok(value)) => value.clone(),
            Some(Err(error)) => format!("{}: {error}", text("Unavailable", "不可用")),
            None => text("Reading command line…", "正在读取命令行…"),
        };
        set(
            control(hwnd, DETAIL),
            &format!(
                "{} · PID {} · {} {} · {} {}\r\n{}: {}\r\n{}: {}\r\n{}: {}",
                process.name,
                process.identity.pid,
                text("Parent", "父进程"),
                process.parent_pid,
                text("Session", "会话"),
                process
                    .session
                    .map_or_else(|| "—".into(), |n| n.to_string()),
                text("Owner", "所属用户"),
                process.owner.as_deref().unwrap_or("—"),
                text("Executable", "程序路径"),
                process.path.as_deref().unwrap_or("—"),
                text("Command line", "命令行"),
                command_text
            ),
        );
        allowed = [
            process.path.is_some(),
            process.path.is_some(),
            matches!(command, Some(Ok(_))),
            !process.windows.is_empty(),
            !process.windows.is_empty(),
            process.identity.pid != unsafe { GetCurrentProcessId() },
        ];
        if command.is_none() && requested != Some(process.identity) {
            let key = process.identity;
            edit(|s| {
                s.requested = Some(key);
                put(&s.workers.detail, key);
                let _ = s.workers.detail_wake.try_send(());
            });
        }
    } else {
        set(
            control(hwnd, DETAIL),
            &if key.is_some() {
                text(
                    "Selected process is no longer visible. Clear the search or select another task.",
                    "所选进程已不可见，请清除搜索或重新选择任务。",
                )
            } else {
                read(|s| {
                    s.limited.and_then(|pid| {
                        s.snapshot
                            .as_ref()?
                            .unavailable
                            .iter()
                            .find(|p| p.pid == pid)
                            .map(|p| {
                                format!(
                                    "{} · PID {} · {}: {}\r\n{}: {}\r\n{}: {}\r\n{}",
                                    p.name,
                                    p.pid,
                                    text("Session", "会话"),
                                    p.session.map_or_else(|| "—".into(), |n| n.to_string()),
                                    text("Owner", "所属用户"),
                                    p.owner.as_deref().unwrap_or("—"),
                                    text("Unavailable", "不可用"),
                                    p.reason,
                                    text(
                                        "Identity unavailable; management actions are disabled.",
                                        "无法核实进程身份，管理操作已禁用。"
                                    )
                                )
                            })
                    })
                })
                .flatten()
                .unwrap_or_else(|| {
                    text(
                        "Select a process or endpoint to view details.",
                        "选择进程或端点查看详情。",
                    )
                })
            },
        );
    }
    for (index, id) in [LOCATION, COPY_PATH, COPY_COMMAND, SWITCH, CLOSE, END]
        .iter()
        .enumerate()
    {
        unsafe {
            let _ = EnableWindow(control(hwnd, *id), allowed[index] && !busy);
        }
    }
    unsafe {
        let _ = EnableWindow(control(hwnd, ADMIN), !busy);
    }
}
fn selection(hwnd: HWND) {
    if read(|s| s.rebuilding).unwrap_or(true) {
        return;
    }
    let index = message(
        control(hwnd, LIST),
        LVM_GETNEXTITEM,
        usize::MAX,
        LVNI_SELECTED as isize,
    )
    .0;
    let key = read(|s| {
        if index >= 0 {
            s.rows.get(index as usize).and_then(|r| r.identity)
        } else {
            None
        }
    })
    .flatten();
    edit(|s| {
        s.selected = key;
        s.limited = if index >= 0 {
            s.rows
                .get(index as usize)
                .filter(|r| r.identity.is_none())
                .map(|r| r.pid)
        } else {
            None
        };
    });
    selected_detail(hwnd);
}
fn tick(hwnd: HWND) {
    let events = read(|s| {
        (
            take(&s.workers.sampled),
            take(&s.workers.detailed),
            take(&s.workers.acted),
        )
    });
    if let Some((sample, detail, action)) = events {
        if let Some(result) = sample {
            match result {
                Ok(snapshot) => {
                    edit(|s| {
                        s.commands
                            .retain(|key, _| snapshot.processes.iter().any(|p| p.identity == *key));
                        s.snapshot = Some(Rc::new(snapshot));
                    });
                    rebuild(hwnd);
                }
                Err(error) => notify(hwnd, Notice::Sampling(error)),
            }
        }
        if let Some((key, result)) = detail {
            edit(|s| {
                if s.commands.len() >= 128 && !s.commands.contains_key(&key) {
                    s.commands.retain(|k, _| Some(*k) == s.selected);
                }
                s.commands.insert(key, result);
                if s.requested == Some(key) {
                    s.requested = None;
                }
            });
            selected_detail(hwnd);
        }
        if let Some(result) = action {
            edit(|s| {
                s.busy = false;
                s.next = Instant::now();
            });
            notify(hwnd, Notice::Outcome(result));
            selected_detail(hwnd);
        }
    }
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() };
    if visible {
        edit(|s| {
            if Instant::now() >= s.next {
                let _ = s.workers.sample.try_send(());
                s.next = Instant::now() + Duration::from_secs(2);
            }
        });
    }
}
fn command(hwnd: HWND, id: i32) {
    match id {
        ADMIN => {
            let Some(request) = read(|s| {
                if s.busy || s.access.scope() != service::Scope::OwnTasks {
                    return None;
                }
                Some(crate::task_launch::Request {
                    focus: s.selected,
                    search: s.filter.clone(),
                    network: s.network,
                    chinese: s.options.chinese,
                    light: s.options.light,
                })
            })
            .flatten() else {
                return;
            };
            let queued = edit(|s| {
                s.workers
                    .action
                    .try_send(Action::Administrator(hwnd.0 as isize, request))
                    .is_ok()
            })
            .unwrap_or(false);
            if queued {
                edit(|s| s.busy = true);
            }
            notify(hwnd, Notice::Pending(queued));
            selected_detail(hwnd);
        }
        REFRESH => {
            edit(|s| {
                s.next = Instant::now();
                if let Some(key) = s.selected {
                    s.commands.remove(&key);
                    s.requested = None;
                }
            });
            selected_detail(hwnd);
        }
        SEARCH => {
            let value = get_text(control(hwnd, SEARCH));
            edit(|s| s.filter = value);
            rebuild(hwnd);
        }
        NETWORK | TREE => {
            let checked = message(control(hwnd, id), BM_GETCHECK, 0, 0).0 == BST_CHECKED.0 as isize;
            edit(|s| {
                if id == NETWORK {
                    s.network = checked;
                    s.sort = (0, false);
                } else {
                    s.tree = checked;
                }
            });
            unsafe {
                let _ = EnableWindow(control(hwnd, TREE), !read(|s| s.network).unwrap_or(false));
            }
            if id == NETWORK {
                columns(hwnd);
            }
            rebuild(hwnd);
        }
        LANGUAGE => {
            edit(|s| s.options.chinese = !s.options.chinese);
            labels(hwnd);
            render_notice(hwnd);
            columns(hwnd);
            rebuild(hwnd);
        }
        THEME => {
            edit(|s| s.options.light = !s.options.light);
            theme(hwnd);
        }
        _ => {
            let Some((key, process, command, busy)) = read(|s| {
                let key = s.selected?;
                if !s.rows.iter().any(|r| r.identity == Some(key)) {
                    return None;
                }
                Some((
                    key,
                    s.snapshot
                        .as_ref()?
                        .processes
                        .iter()
                        .find(|p| p.identity == key)?
                        .clone(),
                    s.commands.get(&key).cloned(),
                    s.busy,
                ))
            })
            .flatten() else {
                return;
            };
            if busy {
                return;
            }
            if id == COPY_PATH || id == COPY_COMMAND {
                let value = if id == COPY_PATH {
                    process.path
                } else {
                    command.and_then(Result::ok)
                };
                if let Some(value) = value {
                    let result = clipboard(hwnd, &value);
                    notify(hwnd, Notice::Outcome(result.map(|_| "copy")));
                }
                return;
            }
            let action = match id {
                LOCATION => Action::Location(key),
                SWITCH => {
                    let Some(window) = choose_window(hwnd, &process) else {
                        return;
                    };
                    Action::Switch(key, window.id)
                }
                CLOSE => {
                    let Some(w) = choose_window(hwnd, &process) else {
                        return;
                    };
                    Action::Close(key, w.id.clone())
                }
                END => {
                    let question = format!(
                        "{}\n\n{} · PID {} · {} {}\n{}",
                        text("End this process?", "结束这个进程？"),
                        process.name,
                        key.pid,
                        text("Session", "会话"),
                        process
                            .session
                            .map_or_else(|| "—".into(), |n| n.to_string()),
                        text(
                            "Unsaved data may be lost. This ends only the selected process.",
                            "未保存的数据可能丢失。此操作仅结束所选进程。"
                        )
                    );
                    let answer = unsafe {
                        MessageBoxW(
                            Some(hwnd),
                            PCWSTR(wide(&question).as_ptr()),
                            PCWSTR(wide(&text("End process", "结束进程")).as_ptr()),
                            MB_OKCANCEL | MB_DEFBUTTON2 | MB_ICONWARNING,
                        )
                    };
                    if answer != IDOK {
                        notify(hwnd, Notice::Outcome(Ok("cancel")));
                        return;
                    }
                    Action::End(key)
                }
                _ => return,
            };
            let queued = edit(|s| {
                if s.workers.action.try_send(action).is_ok() {
                    s.busy = true;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            notify(hwnd, Notice::Pending(queued));
            selected_detail(hwnd);
        }
    }
}
fn notify(hwnd: HWND, notice: Notice) {
    edit(|s| s.notice = Some(notice));
    render_notice(hwnd);
}
fn render_notice(hwnd: HWND) {
    let notice = read(|s| s.notice.clone()).flatten();
    let value = match notice {
        None => String::new(),
        Some(Notice::Outcome(Ok("close"))) => text(
            "Close request sent. The application may ask you to save.",
            "已发送关闭请求，应用可能询问是否保存。",
        ),
        Some(Notice::Outcome(Ok("end"))) => text("Process ended.", "进程已结束。"),
        Some(Notice::Outcome(Ok("copy"))) => text("Copied.", "已复制。"),
        Some(Notice::Outcome(Ok("switch"))) => text("Window activated.", "窗口已激活。"),
        Some(Notice::Outcome(Ok("cancel"))) => {
            text("Cancelled; task unchanged.", "已取消，任务未改变。")
        }
        Some(Notice::Outcome(Ok("admin"))) => {
            text("Administrator tasks opened.", "已打开管理员任务窗口。")
        }
        Some(Notice::Outcome(Ok("admin-cancel"))) => text(
            "Authorization cancelled; this view is unchanged.",
            "已取消授权，当前视图未改变。",
        ),
        Some(Notice::Outcome(Ok(_))) => text("Opened file location.", "已打开文件位置。"),
        Some(Notice::Outcome(Err(e))) => format!("{}: {e}", text("Operation failed", "操作失败")),
        Some(Notice::Sampling(e)) => format!(
            "{}: {e}",
            text(
                "Refresh failed; previous data retained",
                "刷新失败，保留上次数据"
            )
        ),
        Some(Notice::Partial(e)) => {
            format!("{}: {e}", text("Some data unavailable", "部分数据不可用"))
        }
        Some(Notice::Pending(true)) => text("Working…", "正在处理…"),
        Some(Notice::Pending(false)) => text("Another operation is pending.", "已有操作正在等待。"),
    };
    set(control(hwnd, STATUS), &value);
}
fn tree_key(hwnd: HWND, key: usize) -> bool {
    let Some((identity, children, expanded, snapshot)) = read(|s| {
        if !s.tree || s.network {
            return None;
        }
        let identity = s.selected?;
        let row = s.rows.iter().find(|r| r.identity == Some(identity))?;
        Some((
            identity,
            row.children,
            s.expanded.contains(&identity),
            s.snapshot.clone(),
        ))
    })
    .flatten() else {
        return false;
    };
    if key == VK_RIGHT.0 as usize || key == VK_RETURN.0 as usize {
        if !children {
            return false;
        }
        edit(|s| {
            if key == VK_RETURN.0 as usize && expanded {
                s.expanded.remove(&identity);
            } else {
                s.expanded.insert(identity);
            }
        });
    } else if expanded {
        edit(|s| {
            s.expanded.remove(&identity);
        });
    } else {
        let Some(snapshot) = snapshot else {
            return false;
        };
        let Some(child) = snapshot.processes.iter().find(|p| p.identity == identity) else {
            return false;
        };
        let Some(parent) = snapshot.processes.iter().find(|p| {
            p.identity.pid == child.parent_pid
                && p.identity.created_100ns < child.identity.created_100ns
                && p.session == child.session
        }) else {
            return false;
        };
        edit(|s| s.selected = Some(parent.identity));
    }
    rebuild(hwnd);
    true
}
fn choose_window(hwnd: HWND, process: &service::Process) -> Option<service::Window> {
    match process.windows.len() {
        0 => None,
        1 => process.windows.first().cloned(),
        _ => unsafe {
            let rows: Vec<_> = process
                .windows
                .iter()
                .enumerate()
                .map(|(i, w)| crate::win_util::MenuRow::command(i + 1, w.title.replace('&', "&&")))
                .collect();
            let (x, y) = crate::win_util::cursor_pos_or(None);
            let index = crate::win_util::popup_menu(hwnd, &rows, x, y)?;
            process.windows.get(index.checked_sub(1)?).cloned()
        },
    }
}
fn clipboard(hwnd: HWND, text: &str) -> Result<(), String> {
    unsafe {
        OpenClipboard(Some(hwnd)).map_err(|e| e.to_string())?;
        struct Close;
        impl Drop for Close {
            fn drop(&mut self) {
                let _ = unsafe { CloseClipboard() };
            }
        }
        let _close = Close;
        let text = wide(text);
        let memory = GlobalAlloc(GMEM_MOVEABLE, text.len() * 2).map_err(|e| e.to_string())?;
        let pointer = GlobalLock(memory);
        if pointer.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("Clipboard allocation failed".into());
        }
        std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast(), text.len());
        let _ = GlobalUnlock(memory);
        let result =
            EmptyClipboard().and_then(|_| SetClipboardData(13, Some(HANDLE(memory.0))).map(|_| ()));
        if result.is_err() {
            let _ = GlobalFree(Some(memory));
        }
        result.map_err(|e| e.to_string())
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    guard_abi(
        "task manager window",
        || unsafe {
            if l.0 == 0
                && matches!(
                    msg,
                    WM_NOTIFY | WM_COPYDATA | WM_DRAWITEM | WM_DPICHANGED | WM_GETMINMAXINFO
                )
            {
                return LRESULT(0);
            }
            match msg {
                WM_TIMER => {
                    tick(hwnd);
                    LRESULT(0)
                }
                WM_COMMAND => {
                    let id = (w.0 & 65535) as i32;
                    let notification = (w.0 >> 16) as u32;
                    if (id == SEARCH && notification == EN_CHANGE)
                        || (id != SEARCH && notification == BN_CLICKED)
                    {
                        command(hwnd, id);
                    }
                    LRESULT(0)
                }
                WM_NOTIFY => {
                    let header = &*(l.0 as *const NMHDR);
                    if header.idFrom == LIST as usize {
                        match header.code {
                            LVN_GETDISPINFOW => {
                                let info = &mut *(l.0 as *mut NMLVDISPINFOW);
                                if info.item.mask & LVIF_TEXT != LIST_VIEW_ITEM_FLAGS(0)
                                    && info.item.iItem >= 0
                                    && info.item.iSubItem >= 0
                                    && !info.item.pszText.is_null()
                                    && info.item.cchTextMax > 0
                                    && let Some(rows) = read(|s| s.rows.clone())
                                    && let Some(value) = rows
                                        .get(info.item.iItem as usize)
                                        .and_then(|r| r.cells.get(info.item.iSubItem as usize))
                                {
                                    let data = wide(value);
                                    let count =
                                        (data.len() - 1).min(info.item.cchTextMax as usize - 1);
                                    std::ptr::copy_nonoverlapping(
                                        data.as_ptr(),
                                        info.item.pszText.0,
                                        count,
                                    );
                                    *info.item.pszText.0.add(count) = 0;
                                }
                                LRESULT(0)
                            }
                            LVN_ITEMCHANGED | LVN_ODSTATECHANGED => {
                                selection(hwnd);
                                LRESULT(0)
                            }
                            LVN_COLUMNCLICK => {
                                let info = &*(l.0 as *const NMLISTVIEW);
                                edit(|s| {
                                    let column = info.iSubItem.max(0) as usize;
                                    s.sort = if s.sort.0 == column {
                                        (column, !s.sort.1)
                                    } else {
                                        (column, (4..=6).contains(&column) && !s.network)
                                    };
                                });
                                rebuild(hwnd);
                                LRESULT(0)
                            }
                            NM_DBLCLK => {
                                let key = read(|s| {
                                    let key = s.selected?;
                                    let row = s.rows.iter().find(|r| r.identity == Some(key))?;
                                    Some((key, row.children, s.tree && !s.network))
                                })
                                .flatten();
                                if let Some((key, true, true)) = key {
                                    edit(|s| {
                                        if !s.expanded.remove(&key) {
                                            s.expanded.insert(key);
                                        }
                                    });
                                    rebuild(hwnd);
                                } else {
                                    command(hwnd, SWITCH);
                                }
                                LRESULT(0)
                            }
                            _ => DefWindowProcW(hwnd, msg, w, l),
                        }
                    } else {
                        DefWindowProcW(hwnd, msg, w, l)
                    }
                }
                WM_COPYDATA => {
                    let data = &*(l.0 as *const COPYDATASTRUCT);
                    if data.dwData != 0x54534b32
                        || data.cbData == 0
                        || data.cbData > 4096
                        || data.lpData.is_null()
                    {
                        return LRESULT(0);
                    }
                    let bytes =
                        std::slice::from_raw_parts(data.lpData.cast::<u8>(), data.cbData as usize);
                    let Ok(request) = serde_json::from_slice::<ViewRequest>(bytes) else {
                        return LRESULT(0);
                    };
                    if request.search.encode_utf16().count() > 512 || request.search.contains('\0')
                    {
                        return LRESULT(0);
                    }
                    edit(|s| {
                        s.focus = request.focus;
                        s.selected = request.focus.or(s.selected);
                        s.limited = None;
                        s.next = Instant::now();
                        s.filter = request.search.clone();
                        s.network = request.network;
                        s.tree = false;
                    });
                    message(control(hwnd, TREE), BM_SETCHECK, 0, 0);
                    message(
                        control(hwnd, NETWORK),
                        BM_SETCHECK,
                        usize::from(request.network),
                        0,
                    );
                    set(control(hwnd, SEARCH), &request.search);
                    columns(hwnd);
                    rebuild(hwnd);
                    LRESULT(1)
                }
                WM_SIZE => {
                    resize(hwnd);
                    LRESULT(0)
                }
                WM_GETMINMAXINFO => {
                    let info = &mut *(l.0 as *mut MINMAXINFO);
                    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                    info.ptMinTrackSize = POINT {
                        x: 800 * dpi / 96,
                        y: 480 * dpi / 96,
                    };
                    LRESULT(0)
                }
                WM_DPICHANGED => {
                    let r = &*(l.0 as *const RECT);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    font(hwnd);
                    resize(hwnd);
                    columns(hwnd);
                    LRESULT(0)
                }
                WM_ERASEBKGND => {
                    let dc = HDC(w.0 as *mut _);
                    let mut r = RECT::default();
                    let _ = GetClientRect(hwnd, &mut r);
                    SetDCBrushColor(dc, COLORREF(colors().bg));
                    FillRect(dc, &r, HBRUSH(GetStockObject(DC_BRUSH).0));
                    LRESULT(1)
                }
                WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
                    let dc = HDC(w.0 as *mut _);
                    let color = colors();
                    let id = GetDlgCtrlID(HWND(l.0 as *mut _));
                    let bg = if id == SEARCH || id == DETAIL {
                        color.surface
                    } else {
                        color.bg
                    };
                    SetTextColor(dc, COLORREF(color.text));
                    SetBkColor(dc, COLORREF(bg));
                    SetDCBrushColor(dc, COLORREF(bg));
                    LRESULT(GetStockObject(DC_BRUSH).0 as isize)
                }
                WM_DRAWITEM => {
                    let draw = &*(l.0 as *const DRAWITEMSTRUCT);
                    let color = colors();
                    let font = read(|s| s.font).unwrap_or_default();
                    let old_font = SelectObject(draw.hDC, font.into());
                    SetDCBrushColor(
                        draw.hDC,
                        COLORREF(if draw.itemState.0 & ODS_SELECTED.0 != 0 {
                            color.hover
                        } else {
                            color.surface
                        }),
                    );
                    FillRect(draw.hDC, &draw.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
                    SetDCBrushColor(draw.hDC, COLORREF(color.muted));
                    FrameRect(draw.hDC, &draw.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
                    SetTextColor(
                        draw.hDC,
                        COLORREF(if draw.itemState.0 & ODS_DISABLED.0 != 0 {
                            color.muted
                        } else {
                            color.text
                        }),
                    );
                    SetBkMode(draw.hDC, TRANSPARENT);
                    let mut label = wide(&get_text(draw.hwndItem));
                    let mut r = draw.rcItem;
                    DrawTextW(
                        draw.hDC,
                        &mut label,
                        &mut r,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                    );
                    if draw.itemState.0 & ODS_FOCUS.0 != 0 {
                        let _ = DrawFocusRect(draw.hDC, &draw.rcItem);
                    }
                    SelectObject(draw.hDC, old_font);
                    LRESULT(1)
                }
                WM_CLOSE => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    let _ = KillTimer(Some(hwnd), 1);
                    let old = STATE.with(|s| s.borrow_mut().take());
                    if let Some(old) = old
                        && !old.font.is_invalid()
                    {
                        let _ = DeleteObject(old.font.into());
                    }
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, w, l),
            }
        },
        || LRESULT(0),
    )
}
