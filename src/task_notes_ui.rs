//! Native analysis editor; storage runs on finite workers, never the message loop.
use crate::{
    palette,
    task_notes::*,
    task_service::Identity,
    win_util::{guard_abi, wide},
};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex, OnceLock},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{LibraryLoader::GetModuleHandleW, Time::*},
        UI::{
            Controls::*, HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::*, WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};
const HEADER: i32 = 2100;
const HISTORY: i32 = 2101;
const INPUT: i32 = 2102;
const EVIDENCE: i32 = 2103;
const FLAG: i32 = 2104;
const SAVE: i32 = 2105;
const RELOAD: i32 = 2106;
const CLOSE: i32 = 2107;
const STATUS: i32 = 2108;
const HINT: i32 = 2109;
const SCOPE: i32 = 2110;
type ResultSlot = Arc<Mutex<Option<Result<Thread, Error>>>>;
struct Editor {
    hwnd: HWND,
    store: Store,
    subject: Subject,
    application: Option<Subject>,
    run: Identity,
    author: String,
    chinese: bool,
    light: bool,
    font: HFONT,
    revision: u64,
    pending: Option<ResultSlot>,
    saving: bool,
    dirty: bool,
    loading: bool,
}
thread_local! {static EDITOR:RefCell<Option<Editor>>=const {RefCell::new(None)};}
fn read<R>(f: impl FnOnce(&Editor) -> R) -> Option<R> {
    EDITOR.with(|s| s.try_borrow().ok()?.as_ref().map(f))
}
fn edit<R>(f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
    EDITOR.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}
fn tr(en: &str, zh: &str) -> String {
    if read(|e| e.chinese).unwrap_or(false) {
        zh
    } else {
        en
    }
    .into()
}
fn ctl(hwnd: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(Some(hwnd), id).unwrap_or_default() }
}
fn send(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l))) }
}
fn set(hwnd: HWND, value: &str) {
    let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(wide(value).as_ptr())) };
}
fn get(hwnd: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(hwnd).clamp(0, 32768) as usize;
        let mut text = vec![0u16; n + 1];
        let n = GetWindowTextW(hwnd, &mut text).max(0) as usize;
        String::from_utf16_lossy(&text[..n])
    }
}
fn colors() -> palette::Theme {
    palette::base(read(|e| e.light).unwrap_or(false))
}
fn stamp(ms: u64) -> String {
    unsafe {
        let Some(ticks) = ms
            .checked_add(11644473600000)
            .and_then(|n| n.checked_mul(10000))
        else {
            return ms.to_string();
        };
        let file = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        if FileTimeToSystemTime(&file, &mut utc).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
        {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
            )
        } else {
            ms.to_string()
        }
    }
}
fn history(thread: &Thread) -> String {
    let mut output = format!(
        "{}: {} · revision {}\r\n\r\n",
        tr("Current state", "当前状态"),
        thread.flag.name(),
        thread.revision
    );
    for entry in &thread.entries {
        output.push_str(&format!(
            "{} · {:?}: {}{}\r\n{}\r\n",
            stamp(entry.at_unix_ms),
            entry.author.kind,
            entry.author.name,
            entry
                .flag
                .map_or(String::new(), |flag| format!(" · {}", flag.name())),
            entry.text.replace("\r\n", "\n").replace('\n', "\r\n")
        ));
        if !entry.evidence.is_empty() {
            output.push_str(&format!(
                "{}: {}\r\n",
                tr("Evidence / source", "依据 / 来源"),
                entry.evidence
            ));
        }
        output.push_str("\r\n");
    }
    output
}
fn start(hwnd: HWND, save: bool) {
    let content = get(ctl(hwnd, INPUT));
    let evidence = get(ctl(hwnd, EVIDENCE));
    let flag = match send(ctl(hwnd, FLAG), CB_GETCURSEL, 0, 0).0 {
        1 => Some(Flag::Normal),
        2 => Some(Flag::Watching),
        3 => Some(Flag::Suspicious),
        4 => Some(Flag::Resolved),
        5 => Some(Flag::Unreviewed),
        _ => None,
    };
    let slot: ResultSlot = Arc::new(Mutex::new(None));
    let job = edit(|e| {
        if e.pending.is_some() {
            return None;
        }
        let request = save.then(|| Append {
            subject: e.subject.clone(),
            run: Some(e.run),
            expected_revision: e.revision,
            author: Author {
                kind: AuthorKind::Human,
                name: e.author.clone(),
            },
            text: content,
            evidence,
            flag,
        });
        e.pending = Some(slot.clone());
        e.saving = save;
        Some((e.store.clone(), e.subject.clone(), request))
    })
    .flatten();
    if let Some((store, subject, request)) = job {
        std::thread::spawn(move || {
            let result = if let Some(request) = request {
                store.append(request)
            } else {
                store.thread(&subject)
            };
            if let Ok(mut pending) = slot.lock() {
                *pending = Some(result);
            }
        });
        set(
            ctl(hwnd, STATUS),
            &tr(
                "Working… Your draft stays here if saving fails.",
                "正在处理… 保存失败会保留草稿。",
            ),
        );
        for id in [SAVE, RELOAD, INPUT, EVIDENCE, FLAG, SCOPE] {
            let _ = unsafe { EnableWindow(ctl(hwnd, id), false) };
        }
    }
}
fn poll(hwnd: HWND) {
    let result = read(|e| e.pending.as_ref().and_then(|slot| slot.lock().ok()?.take())).flatten();
    let Some(result) = result else { return };
    let saved = edit(|e| {
        e.pending = None;
        let saving = e.saving;
        e.saving = false;
        saving
    })
    .unwrap_or(false);
    match result {
        Ok(thread) => {
            edit(|e| e.revision = thread.revision);
            set(ctl(hwnd, HISTORY), &history(&thread));
            send(ctl(hwnd, HISTORY), EM_SETSEL, usize::MAX, -1);
            send(ctl(hwnd, HISTORY), EM_SCROLLCARET, 0, 0);
            if saved {
                edit(|e| e.loading = true);
                set(ctl(hwnd, INPUT), "");
                set(ctl(hwnd, EVIDENCE), "");
                send(ctl(hwnd, FLAG), CB_SETCURSEL, 0, 0);
                edit(|e| {
                    e.loading = false;
                    e.dirty = false;
                });
            }
            set(
                ctl(hwnd, STATUS),
                &if saved {
                    tr(
                        "Saved. Previous notes remain in the history.",
                        "已追加保存，之前的分析记录保留。",
                    )
                } else {
                    tr(
                        "History loaded. Review it before appending; drafts are retained.",
                        "历史已载入。请先查看再追加，现有草稿保留。",
                    )
                },
            );
        }
        Err(error) => set(
            ctl(hwnd, STATUS),
            &format!("{}: {}", error.code, error.message),
        ),
    }
    for id in [SAVE, RELOAD, INPUT, EVIDENCE, FLAG, SCOPE] {
        let _ = unsafe { EnableWindow(ctl(hwnd, id), true) };
    }
}
fn resize(hwnd: HWND) {
    unsafe {
        let mut r = RECT::default();
        if GetClientRect(hwnd, &mut r).is_err() {
            return;
        }
        let scale = |n| n * GetDpiForWindow(hwnd).max(96) as i32 / 96;
        let m = scale(12);
        let width = r.right - m * 2;
        let bottom = r.bottom - m - scale(30);
        let history_h = (r.bottom - scale(355)).max(scale(100));
        let place = |id, x, y, w: i32, h: i32| {
            let _ = MoveWindow(ctl(hwnd, id), x, y, w.max(1), h.max(1), true);
        };
        place(HEADER, m, m, width, scale(64));
        place(SCOPE, m, scale(78), width, scale(180));
        place(HISTORY, m, scale(112), width, history_h);
        let y = scale(120) + history_h;
        place(HINT, m, y, width, scale(22));
        place(INPUT, m, y + scale(24), width, scale(80));
        place(EVIDENCE, m, y + scale(110), width, scale(26));
        place(FLAG, m, y + scale(142), scale(240), scale(180));
        place(
            STATUS,
            m + scale(250),
            y + scale(142),
            width - scale(250),
            scale(52),
        );
        for (i, id) in [SAVE, RELOAD, CLOSE].iter().enumerate() {
            place(
                *id,
                m + i as i32 * scale(160),
                bottom,
                scale(150),
                scale(30),
            );
        }
    }
}
fn font(hwnd: HWND) {
    unsafe {
        let mut value = LOGFONTW {
            lfHeight: -(14 * GetDpiForWindow(hwnd).max(96) as i32 / 96),
            ..Default::default()
        };
        let face = wide("Segoe UI");
        value.lfFaceName[..face.len()].copy_from_slice(&face);
        let new = CreateFontIndirectW(&value);
        if new.is_invalid() {
            return;
        }
        let old = edit(|e| {
            let old = e.font;
            e.font = new;
            old
        })
        .unwrap_or_default();
        for id in [
            HEADER, HISTORY, HINT, INPUT, EVIDENCE, FLAG, SCOPE, SAVE, RELOAD, CLOSE, STATUS,
        ] {
            send(ctl(hwnd, id), WM_SETFONT, new.0 as usize, 1);
        }
        if !old.is_invalid() {
            let _ = DeleteObject(old.into());
        }
    }
}
pub fn close_if_open() -> bool {
    let hwnd = read(|e| e.hwnd).unwrap_or_default();
    if hwnd.is_invalid() {
        return true;
    }
    send(hwnd, WM_CLOSE, 0, 0);
    !unsafe { IsWindow(Some(hwnd)).as_bool() }
}
fn header(hwnd: HWND) {
    if let Some(text) = read(|e| {
        format!(
            "{}\r\n{}\r\nOwner: {} · PID {}",
            if e.subject.image.is_some() {
                tr("Application — across runs", "应用 — 跨运行")
            } else {
                tr(
                    "This run — PID + creation time",
                    "本次运行 — PID + 创建时间",
                )
            },
            e.application
                .as_ref()
                .and_then(|s| s.image.as_deref())
                .unwrap_or("(image unavailable)"),
            e.subject.owner,
            e.run.pid
        )
    }) {
        set(ctl(hwnd, HEADER), &text);
    }
}
fn change_scope(hwnd: HWND) {
    let selected = send(ctl(hwnd, SCOPE), CB_GETCURSEL, 0, 0).0;
    let current = read(|e| i32::from(e.subject.image.is_some())).unwrap_or(0);
    if selected == current as isize {
        return;
    }
    let blocked = read(|e| e.pending.is_some()).unwrap_or(true);
    let discard = !read(|e| e.dirty).unwrap_or(false)
        || unsafe {
            MessageBoxW(
                Some(hwnd),
                PCWSTR(
                    wide(&tr(
                        "Discard the unsaved draft and change scope?",
                        "放弃草稿并切换分析范围？",
                    ))
                    .as_ptr(),
                ),
                PCWSTR(wide("Minicon Tskmgr").as_ptr()),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
            ) == IDYES
        };
    if blocked || !discard {
        send(ctl(hwnd, SCOPE), CB_SETCURSEL, current as usize, 0);
        return;
    }
    edit(|e| {
        e.subject = if selected == 1 {
            e.application.clone().unwrap_or_else(|| e.subject.clone())
        } else {
            Subject::for_run(e.subject.owner.clone(), e.run).expect("validated run")
        };
        e.revision = 0;
        e.loading = true;
    });
    set(ctl(hwnd, INPUT), "");
    set(ctl(hwnd, EVIDENCE), "");
    send(ctl(hwnd, FLAG), CB_SETCURSEL, 0, 0);
    edit(|e| {
        e.loading = false;
        e.dirty = false;
    });
    header(hwnd);
    start(hwnd, false);
}
pub fn open(
    parent: HWND,
    store: Store,
    subject: Subject,
    run: Identity,
    author: String,
    chinese: bool,
    light: bool,
) -> Result<(), String> {
    let application = subject.image.is_some().then_some(subject.clone());
    let subject = Subject::for_run(subject.owner, run).map_err(|e| e.message)?;
    unsafe {
        if read(|_| ()).is_some() {
            return Err("An analysis editor is already open".into());
        }
        let instance = HINSTANCE(GetModuleHandleW(None).map_err(|e| e.to_string())?.0);
        let class = wide("minicon_task_analysis");
        static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
        REGISTERED
            .get_or_init(|| {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: PCWSTR(class.as_ptr()),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                if RegisterClassW(&wc) == 0 {
                    Err(windows::core::Error::from_thread().to_string())
                } else {
                    Ok(())
                }
            })
            .clone()?;
        EDITOR.with(|s| {
            *s.borrow_mut() = Some(Editor {
                hwnd: HWND::default(),
                store,
                subject: subject.clone(),
                application,
                run,
                author,
                chinese,
                light,
                font: HFONT::default(),
                revision: 0,
                pending: None,
                saving: false,
                dirty: false,
                loading: false,
            })
        });
        let hwnd = match CreateWindowExW(
            WS_EX_CONTROLPARENT,
            PCWSTR(class.as_ptr()),
            PCWSTR(
                wide(&tr(
                    "Task analysis — notes and flags",
                    "任务分析 — 笔记与标识",
                ))
                .as_ptr(),
            ),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            880,
            700,
            Some(parent),
            None,
            Some(instance),
            None,
        ) {
            Ok(h) => h,
            Err(error) => {
                EDITOR.with(|s| s.borrow_mut().take());
                return Err(error.to_string());
            }
        };
        edit(|e| e.hwnd = hwnd);
        let setup = (|| -> Result<(), String> {
            for id in [
                HEADER, HISTORY, HINT, INPUT, EVIDENCE, FLAG, SAVE, RELOAD, CLOSE, STATUS, SCOPE,
            ] {
                let (class, style) = match id {
                    HISTORY => (
                        "EDIT",
                        WS_TABSTOP
                            | WS_VSCROLL
                            | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                    ),
                    INPUT => (
                        "EDIT",
                        WS_TABSTOP
                            | WS_VSCROLL
                            | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32),
                    ),
                    EVIDENCE => ("EDIT", WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32)),
                    FLAG | SCOPE => (
                        "COMBOBOX",
                        WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                    ),
                    SAVE | RELOAD | CLOSE => {
                        ("BUTTON", WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32))
                    }
                    _ => ("STATIC", WINDOW_STYLE(0)),
                };
                let child = CreateWindowExW(
                    if [HISTORY, INPUT, EVIDENCE].contains(&id) {
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
                send(
                    child,
                    WM_SETFONT,
                    GetStockObject(DEFAULT_GUI_FONT).0 as usize,
                    0,
                );
            }
            Ok(())
        })();
        if let Err(error) = setup {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }
        header(hwnd);
        send(
            ctl(hwnd, SCOPE),
            CB_ADDSTRING,
            0,
            wide(&tr("This process run", "本次进程运行")).as_ptr() as isize,
        );
        if read(|e| e.application.is_some()).unwrap_or(false) {
            send(
                ctl(hwnd, SCOPE),
                CB_ADDSTRING,
                0,
                wide(&tr("Application (across restarts)", "整个应用（跨重启）")).as_ptr() as isize,
            );
        }
        send(ctl(hwnd, SCOPE), CB_SETCURSEL, 0, 0);
        set(
            ctl(hwnd, HINT),
            &tr(
                "Append analysis (facts / hypotheses); evidence or source below",
                "追加分析（事实 / 推测）；下方填写依据或来源",
            ),
        );
        send(ctl(hwnd, INPUT), EM_SETLIMITTEXT, 4096, 0);
        send(ctl(hwnd, EVIDENCE), EM_SETLIMITTEXT, 2048, 0);
        send(
            ctl(hwnd, EVIDENCE),
            EM_SETCUEBANNER,
            0,
            wide(&tr("Evidence / source (optional)", "依据 / 来源（可选）")).as_ptr() as isize,
        );
        for value in [
            tr("Keep current state", "保持当前状态"),
            tr("Normal", "正常"),
            tr("Watching", "关注中"),
            tr("Suspicious", "异常 / 待核实"),
            tr("Resolved", "已处理"),
            tr("Clear state / unreviewed", "清除状态 / 未分析"),
        ] {
            send(
                ctl(hwnd, FLAG),
                CB_ADDSTRING,
                0,
                wide(&value).as_ptr() as isize,
            );
        }
        send(ctl(hwnd, FLAG), CB_SETCURSEL, 0, 0);
        for (id, en, zh) in [
            (SAVE, "Append note / state", "追加笔记 / 状态"),
            (RELOAD, "Reload history", "重载历史"),
            (CLOSE, "Close", "关闭"),
        ] {
            set(ctl(hwnd, id), &tr(en, zh));
        }
        let dark = i32::from(!light);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast(),
            4,
        );
        font(hwnd);
        resize(hwnd);
        let _ = EnableWindow(parent, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(Some(hwnd), 1, 150, None);
        start(hwnd, false);
        let mut msg = MSG::default();
        while IsWindow(Some(hwnd)).as_bool() {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result <= 0 {
                if result == 0 {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_ESCAPE.0 as usize {
                send(hwnd, WM_CLOSE, 0, 0);
                continue;
            }
            if msg.message == WM_KEYDOWN && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                if msg.wParam.0 == b'S' as usize {
                    start(hwnd, true);
                    continue;
                }
                if msg.wParam.0 == b'A' as usize
                    && [INPUT, EVIDENCE, HISTORY]
                        .iter()
                        .any(|id| ctl(hwnd, *id) == msg.hwnd)
                {
                    send(msg.hwnd, EM_SETSEL, 0, -1);
                    continue;
                }
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        let _ = EnableWindow(parent, true);
        let _ = SetForegroundWindow(parent);
        Ok(())
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    guard_abi(
        "task analysis window",
        || unsafe {
            match msg {
                WM_TIMER => {
                    poll(hwnd);
                    LRESULT(0)
                }
                WM_SIZE => {
                    resize(hwnd);
                    LRESULT(0)
                }
                WM_GETMINMAXINFO => {
                    let info = &mut *(l.0 as *mut MINMAXINFO);
                    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                    info.ptMinTrackSize = POINT {
                        x: 720 * dpi / 96,
                        y: 600 * dpi / 96,
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
                    LRESULT(0)
                }
                WM_COMMAND => {
                    let id = (w.0 & 65535) as i32;
                    let code = (w.0 >> 16) as u32;
                    if (id == INPUT || id == EVIDENCE) && code == EN_CHANGE
                        || id == FLAG && code == CBN_SELCHANGE
                    {
                        edit(|e| {
                            if !e.loading {
                                e.dirty = true;
                            }
                        });
                    }
                    if id == SCOPE && code == CBN_SELCHANGE {
                        change_scope(hwnd);
                    }
                    if code == BN_CLICKED {
                        match id {
                            SAVE => start(hwnd, true),
                            RELOAD => start(hwnd, false),
                            CLOSE => {
                                send(hwnd, WM_CLOSE, 0, 0);
                            }
                            _ => (),
                        }
                    }
                    LRESULT(0)
                }
                WM_ERASEBKGND => {
                    let mut r = RECT::default();
                    let _ = GetClientRect(hwnd, &mut r);
                    let dc = HDC(w.0 as *mut _);
                    SetDCBrushColor(dc, COLORREF(colors().bg));
                    FillRect(dc, &r, HBRUSH(GetStockObject(DC_BRUSH).0));
                    LRESULT(1)
                }
                WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                    let dc = HDC(w.0 as *mut _);
                    let id = GetDlgCtrlID(HWND(l.0 as *mut _));
                    let color = colors();
                    let bg = if [HEADER, HINT, STATUS].contains(&id) {
                        color.bg
                    } else {
                        color.surface
                    };
                    SetTextColor(dc, COLORREF(color.text));
                    SetBkColor(dc, COLORREF(bg));
                    SetDCBrushColor(dc, COLORREF(bg));
                    LRESULT(GetStockObject(DC_BRUSH).0 as isize)
                }
                WM_DRAWITEM => {
                    let item = &*(l.0 as *const DRAWITEMSTRUCT);
                    let color = colors();
                    SetDCBrushColor(
                        item.hDC,
                        COLORREF(if item.itemState.0 & ODS_SELECTED.0 != 0 {
                            color.hover
                        } else {
                            color.surface
                        }),
                    );
                    FillRect(item.hDC, &item.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
                    SetDCBrushColor(item.hDC, COLORREF(color.muted));
                    FrameRect(item.hDC, &item.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
                    SetTextColor(
                        item.hDC,
                        COLORREF(if item.itemState.0 & ODS_DISABLED.0 != 0 {
                            color.muted
                        } else {
                            color.text
                        }),
                    );
                    SetBkMode(item.hDC, TRANSPARENT);
                    let mut label = wide(&get(item.hwndItem));
                    let mut r = item.rcItem;
                    DrawTextW(
                        item.hDC,
                        &mut label,
                        &mut r,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                    if item.itemState.0 & ODS_FOCUS.0 != 0 {
                        let _ = DrawFocusRect(item.hDC, &item.rcItem);
                    }
                    LRESULT(1)
                }
                WM_CLOSE => {
                    if read(|e| e.pending.is_some() && e.saving).unwrap_or(false) {
                        return LRESULT(0);
                    }
                    if read(|e| e.dirty).unwrap_or(false)
                        && MessageBoxW(
                            Some(hwnd),
                            PCWSTR(
                                wide(&tr("Discard the unsaved draft?", "放弃尚未保存的草稿？"))
                                    .as_ptr(),
                            ),
                            PCWSTR(wide("Minicon Tskmgr").as_ptr()),
                            MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
                        ) != IDYES
                    {
                        return LRESULT(0);
                    }
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    let _ = KillTimer(Some(hwnd), 1);
                    if let Some(editor) = EDITOR.with(|s| s.borrow_mut().take())
                        && !editor.font.is_invalid()
                    {
                        let _ = DeleteObject(editor.font.into());
                    }
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, w, l),
            }
        },
        || LRESULT(0),
    )
}
