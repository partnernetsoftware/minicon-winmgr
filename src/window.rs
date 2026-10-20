//! Win32 message-only window and the process message pump.
//!
//! A shell must own a thread that pumps Win32 messages. For M1 we create a
//! message-only window (`HWND_MESSAGE` parent) that is invisible and receives
//! no user input, then run the classic `GetMessage` / `DispatchMessage` loop.
//!
//! Shutdown paths:
//! - `WM_CLOSE` / `WM_DESTROY` -> destroy the window, then `PostQuitMessage`.
//! - Console Ctrl+C / break / close -> a console control handler posts
//!   `WM_CLOSE` to the shell window, so shutdown stays on the message thread.
//!
//! Companion mode: this never touches the Winlogon `Shell` registry value.

use crate::hub;
use crate::session;
use crate::win_util::{guard_abi, wide};
use crate::window_snapshot;

use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_QUERYENDSESSION, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASS_STYLES, WNDCLASSW,
};
use windows::core::{BOOL, PCWSTR};

const CLASS_NAME: &str = "minicon_winmgr_message_window";
const WINDOW_TITLE: &str = "minicon-winmgr (message-only)";
const HEARTBEAT_TIMER_ID: usize = 1;
const DEFAULT_HEARTBEAT_SECS: u64 = 30;
const SETTINGS_MESSAGE: u32 = 0x8000 + 141;
pub(crate) const STARTUP_READY_MESSAGE: u32 = 0x8000 + 142;
const COMMIT_STARTUP_MESSAGE: u32 = 0x8000 + 144;
pub(crate) const SHELL_RESULT_MESSAGE: u32 = 0x8000 + 147;
const TRAY_TRANSITION_MESSAGE: u32 = 0x8000 + 148;
const TAKEOVER_MESSAGE: u32 = 0x8000 + 145;
const EXECUTE_TAKEOVER_MESSAGE: u32 = 0x8000 + 146;
static RUN_LAUNCH_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static MENU_LAUNCH_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) fn standalone_launch_ready() -> bool {
    RUN_LAUNCH_READY.load(std::sync::atomic::Ordering::SeqCst)
        || crate::launcher_ui::reachable()
        || (MENU_LAUNCH_READY.load(std::sync::atomic::Ordering::SeqCst)
            && crate::config::current().start_menu
            && crate::taskbar::running())
}
pub(crate) fn ready_for_handover() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};
    let Some(hwnd) = hub::shell_hwnd() else {
        return false;
    };
    let mut result = 0;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            STARTUP_READY_MESSAGE,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            1000,
            Some(&mut result),
        )
    };
    sent.0 != 0 && result == 1
}
static STARTUP_TAKEOVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) fn takeover_on_startup() {
    STARTUP_TAKEOVER.store(true, std::sync::atomic::Ordering::SeqCst);
}
pub(crate) fn request_takeover() -> Result<bool, String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    };
    let hwnd = unsafe {
        FindWindowExW(
            Some(HWND_MESSAGE),
            None,
            PCWSTR(wide(CLASS_NAME).as_ptr()),
            None,
        )
    }
    .unwrap_or_default();
    if hwnd.is_invalid() {
        return Ok(false);
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if !crate::single_instance::same_user_session(pid) {
        return Err("The running instance does not belong to this account/session".into());
    }
    let mut result = 0;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            TAKEOVER_MESSAGE,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            1000,
            Some(&mut result),
        )
    };
    if sent.0 == 0 || result != 1 {
        return Err(
            "The running instance is not ready or did not accept the takeover request".into(),
        );
    }
    // Never start a second process when an existing instance is present.
    Ok(true)
}
thread_local! { static STARTUP_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

pub(crate) fn transition_tray(install: bool) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SMTO_ABORTIFHUNG, SendMessageTimeoutW};
    let hwnd = hub::shell_hwnd().ok_or("The UI thread is unavailable")?;
    let mut result = 0;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            TRAY_TRANSITION_MESSAGE,
            WPARAM(usize::from(install)),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            5000,
            Some(&mut result),
        )
    };
    if sent.0 != 0 && result == 1 {
        Ok(())
    } else {
        Err("The UI thread could not complete the notification-host transition".into())
    }
}

pub(crate) fn request_settings() -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    };
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(5) {
        let hwnd = unsafe {
            FindWindowExW(
                Some(HWND_MESSAGE),
                None,
                PCWSTR(wide(CLASS_NAME).as_ptr()),
                None,
            )
        }
        .unwrap_or_default();
        if !hwnd.is_invalid() {
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if !crate::single_instance::same_user_session(pid) {
                return Err("设置窗口不属于当前用户会话。".into());
            }
            unsafe {
                let _ = AllowSetForegroundWindow(pid);
            }
            let mut result = 0;
            let sent = unsafe {
                SendMessageTimeoutW(
                    hwnd,
                    SETTINGS_MESSAGE,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG,
                    250,
                    Some(&mut result),
                )
            };
            if sent.0 != 0 && result == 1 {
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err("已有实例尚未就绪或没有响应，请稍后重试 --settings。".into())
}

/// Runs the shell message loop until a quit message is received.
pub fn run(
    show_settings: bool,
    on_ready: impl FnOnce(bool) -> Result<(), String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: standard Win32 window setup; all buffers outlive their calls and
    // the wndproc is a valid `extern "system"` function.
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let hinstance = HINSTANCE(hinstance.0);
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };

        if RegisterClassW(&wnd_class) == 0 {
            return Err(format!("RegisterClassW failed (Win32 error {})", GetLastError().0).into());
        }

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )?;

        // Published before anything else can need it: the console handler, the
        // window-event hook and the tray host all reach the message thread
        // through this one handle.
        hub::set_shell_hwnd(hwnd);
        tracing::info!(hwnd = ?hwnd, "message-only window created");

        SetConsoleCtrlHandler(Some(console_ctrl_handler), true)?;
        tracing::info!("console control handler installed");

        if let Err(err) = session::register(hwnd) {
            tracing::error!(%err, "WTS session registration failed; continuing without it");
        }

        let heartbeat_ms = heartbeat_interval().as_millis() as u32;
        if SetTimer(Some(hwnd), HEARTBEAT_TIMER_ID, heartbeat_ms, None) == 0 {
            tracing::warn!("SetTimer failed; heartbeat disabled");
        } else {
            tracing::info!(interval_ms = heartbeat_ms, "heartbeat timer armed");
        }

        let mut taskbar_ready = true;
        if crate::taskbar::enabled() {
            if let Err(err) = crate::taskbar::create(hinstance) {
                taskbar_ready = false;
                tracing::error!(%err, "taskbar creation failed; continuing without it");
            }
        } else {
            tracing::info!("taskbar disabled via env");
        }
        // Assembly layer: let modules that change what the bar shows ask for a
        // rebuild without importing `taskbar` (which imports them back).
        hub::set_taskbar_refresh(refresh_ui);
        hub::set_taskbar_state(crate::taskbar::running, crate::taskbar::cancel_hover);

        // Launcher popup (Alt+Space) — self-contained window + hotkey.
        let launcher_ready = match crate::launcher_ui::init(hinstance) {
            Ok(()) => crate::launcher_ui::reachable(),
            Err(error) => {
                tracing::warn!(%error, "launcher initialization failed");
                false
            }
        };

        // Start menu (ORB) — opened from the taskbar brand dot.
        let menu_ready = match crate::start_menu::install(hinstance) {
            Ok(()) => crate::config::current().start_menu,
            Err(err) => {
                tracing::error!(%err, "start menu init failed; continuing without it");
                false
            }
        };
        MENU_LAUNCH_READY.store(menu_ready, std::sync::atomic::Ordering::SeqCst);

        // Run dialog (Win+R).
        let run_ready = match crate::run_dialog::install(hinstance) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "Run hotkeys unavailable");
                false
            }
        };

        RUN_LAUNCH_READY.store(run_ready, std::sync::atomic::Ordering::SeqCst);

        // Settings window (opened from the taskbar's settings button).
        let settings_ready = match crate::settings_ui::install(hinstance) {
            Ok(()) => true,
            Err(err) => {
                tracing::error!(%err, "settings init failed; continuing without it");
                false
            }
        };

        // Hover window-preview popup (shown by the taskbar on hover).
        if let Err(err) = crate::hover_preview::install(hinstance) {
            tracing::error!(%err, "hover preview init failed; continuing without it");
        }

        // Tray host is inert unless MINICON_TRAY_HOST=1 (replacement mode).
        let tray_ready = match crate::tray_host::install(hinstance) {
            Ok(()) => true,
            Err(err) => {
                tracing::error!(%err, "tray host install failed; continuing without it");
                false
            }
        };
        if let Err(err) = crate::tray_popup::install(hinstance) {
            tracing::error!(%err, "notification panel install failed");
        }

        // Window-management hotkeys (Win+Left/Right/Up).
        if let Err(err) = crate::hotkeys::install(hinstance) {
            tracing::error!(%err, "window hotkeys init failed; continuing without them");
        }

        // React to window create/destroy/foreground → debounced taskbar refresh.
        if let Err(err) = crate::window_events::install() {
            tracing::error!(%err, "window-event hook init failed; continuing without it");
        }

        let mut msg = MSG::default();
        tracing::info!("entering message pump");
        crate::metrics::log_sample();
        for m in crate::monitor::enumerate() {
            tracing::info!(
                x = m.x,
                y = m.y,
                width = m.width,
                height = m.height,
                primary = m.primary,
                "monitor work area"
            );
        }
        // Sample the notification area once up front so the first paint has
        // icons; after this the heartbeat re-samples (never the repaint path).
        crate::tray::sample_mirror();
        let tray = crate::tray::items();
        // Say which source answered. In replace mode the sample above is a
        // deliberate no-op, so an unqualified "mirrored count=0" read like a
        // broken mirror and sent the reader chasing the wrong module.
        if crate::tray_host::is_installed() {
            tracing::info!(
                count = tray.len(),
                "notification area hosted; strip fed by registered icons"
            );
        } else {
            tracing::info!(count = tray.len(), "notification area mirrored");
        }
        for icon in &tray {
            tracing::debug!(pid = icon.pid, tooltip = %icon.tooltip, "tray icon");
        }

        window_snapshot::log_snapshot();
        if show_settings {
            crate::settings_ui::show();
        }
        crate::appearance::refresh();
        let usable = taskbar_ready
            && settings_ready
            && tray_ready
            && (standalone_launch_ready() || crate::shell_swap::foreign_shell_running());
        if !usable {
            tracing::error!(
                taskbar_ready,
                settings_ready,
                tray_ready,
                launcher_ready,
                run_ready,
                "essential startup capability check failed"
            );
        }
        let mut on_ready = Some(on_ready);
        PostMessageW(Some(hwnd), COMMIT_STARTUP_MESSAGE, WPARAM(0), LPARAM(0))?;
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            match ret.0 {
                -1 => {
                    return Err(
                        format!("GetMessageW failed (Win32 error {})", GetLastError().0).into(),
                    );
                }
                0 => break, // WM_QUIT
                _ => {}
            }
            // Commit only once the message pump has actually received work.
            // All startup protocols consume this same essential-capability result.
            if msg.hwnd == hwnd && msg.message == COMMIT_STARTUP_MESSAGE {
                if let Some(ready) = on_ready.take() {
                    complete_startup(usable, ready, || {
                        STARTUP_READY.set(true);
                        crate::update_install::confirm_startup();
                    })?;
                    if STARTUP_TAKEOVER.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        PostMessageW(Some(hwnd), EXECUTE_TAKEOVER_MESSAGE, WPARAM(0), LPARAM(0))?;
                    }
                }
                continue;
            }
            if crate::settings_ui::filter(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        tracing::info!("message loop exited");
        Ok(())
    }
}

fn refresh_ui() {
    crate::appearance::refresh();
    crate::taskbar::refresh();
}

/// Window procedure for the shell message window.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the guard only runs the body and degrades to the default handler
    // on a panic; all handles come straight from Win32.
    guard_abi(
        "window::wndproc",
        || unsafe { wndproc_inner(hwnd, msg, wparam, lparam) },
        || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    )
}

/// Inner implementation of [`wndproc`], wrapped by the abort-safety guard.
unsafe extern "system" fn wndproc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        crate::window_icon_worker::READY_MESSAGE => {
            if crate::window_icon_worker::publish() {
                crate::taskbar::refresh_icons();
            }
            LRESULT(0)
        }
        STARTUP_READY_MESSAGE => LRESULT(isize::from(STARTUP_READY.get())),
        SHELL_RESULT_MESSAGE => {
            crate::settings_ui::complete_shell_operation();
            LRESULT(0)
        }
        TRAY_TRANSITION_MESSAGE => {
            if wparam.0 == 0 {
                crate::tray_host::shutdown();
                crate::taskbar::refresh();
                return LRESULT(1);
            }
            let ready = unsafe { GetModuleHandleW(None) }
                .ok()
                .is_some_and(|m| crate::tray_host::install(HINSTANCE(m.0)).is_ok());
            crate::taskbar::refresh();
            LRESULT(isize::from(ready && crate::tray_host::is_installed()))
        }
        TAKEOVER_MESSAGE => {
            if !STARTUP_READY.get() {
                return LRESULT(0);
            }
            LRESULT(isize::from(
                unsafe { PostMessageW(Some(hwnd), EXECUTE_TAKEOVER_MESSAGE, WPARAM(0), LPARAM(0)) }
                    .is_ok(),
            ))
        }
        EXECUTE_TAKEOVER_MESSAGE => {
            crate::settings_ui::execute_takeover();
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY
            if wparam.0 == crate::run_dialog::HOTKEY_ID =>
        {
            crate::run_dialog::show();
            LRESULT(0)
        }
        SETTINGS_MESSAGE => {
            crate::settings_ui::show();
            LRESULT(1)
        }
        WM_CLOSE => {
            if !crate::settings_ui::allow_exit() {
                return LRESULT(0);
            }
            tracing::info!("WM_CLOSE received; destroying window");
            // A close request can arrive inside TrackPopupMenu's nested loop.
            // Destroying this message window does not end a menu owned by a bar;
            // cancel it on the UI thread before posting the application's quit.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::EndMenu() };
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            tracing::info!("WM_DESTROY received; posting quit");
            session::unregister(hwnd);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            let event = session::event_name(wparam.0);
            tracing::info!(event_code = wparam.0, event, "session change");
            LRESULT(0)
        }
        WM_QUERYENDSESSION => {
            tracing::info!(
                lparam = lparam.0,
                "WM_QUERYENDSESSION: allowing session end"
            );
            LRESULT(1) // TRUE: do not block shutdown/logoff
        }
        WM_ENDSESSION => {
            tracing::info!(
                ending = wparam.0 != 0,
                lparam = lparam.0,
                "WM_ENDSESSION received"
            );
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == crate::window_events::REFRESH_TIMER_ID {
                // Debounced taskbar refresh triggered by a window event.
                let _ = unsafe { KillTimer(Some(hwnd), crate::window_events::REFRESH_TIMER_ID) };
                crate::taskbar::refresh();
            } else {
                tracing::debug!(timer_id = wparam.0, "heartbeat: message pump alive");
                window_snapshot::log_snapshot();
                // The mirror read is cross-process, so it rides the heartbeat
                // rather than the repaint path (0.1.3 plan §2.3).
                crate::tray::sample_mirror();
                crate::taskbar::refresh();
                crate::shell_swap::check_foreign_shell();
                if crate::tray_host::is_installed() && crate::shell_swap::foreign_shell_running() {
                    crate::tray_host::shutdown();
                    crate::taskbar::refresh();
                }
                crate::settings_ui::complete_shell_operation();
                crate::metrics::log_sample();
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Console control handler: forwards Ctrl+C / break / close to the message
/// thread by posting `WM_CLOSE`, keeping all shutdown work on one thread.
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    // SAFETY: the guard only runs the body and degrades to the system default
    // handler on a panic; the handle comes straight from our own storage.
    guard_abi(
        "window::console_ctrl_handler",
        || unsafe { console_ctrl_handler_inner(ctrl_type) },
        || BOOL(0),
    )
}

/// Inner implementation of [`console_ctrl_handler`], wrapped by the guard.
unsafe extern "system" fn console_ctrl_handler_inner(ctrl_type: u32) -> BOOL {
    // This handler runs on a different OS thread, so it only reads the handle and
    // posts to the message thread — no shell state is touched here.
    if let Some(hwnd) = hub::shell_hwnd() {
        tracing::info!(ctrl_type, "console control event; posting WM_CLOSE");
        if let Err(err) = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) } {
            tracing::warn!(%err, "console control: could not post WM_CLOSE; shutdown may stall");
        }
    }
    // Report handled so the default handler does not terminate immediately,
    // giving the message thread a chance to unwind gracefully.
    BOOL(1)
}

fn heartbeat_interval() -> std::time::Duration {
    let secs = std::env::var("MINICON_HEARTBEAT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_HEARTBEAT_SECS)
        .max(1);
    std::time::Duration::from_secs(secs)
}

/// Every entry consumes the same result; no success protocol runs after failure.
fn complete_startup(
    usable: bool,
    ready: impl FnOnce(bool) -> Result<(), String>,
    confirm: impl FnOnce(),
) -> Result<(), String> {
    ready(usable)?;
    if !usable {
        return Err("Essential startup capabilities are unavailable".into());
    }
    confirm();
    Ok(())
}
#[cfg(test)]
mod readiness_tests {
    use super::*;
    use std::cell::RefCell;
    #[test]
    #[ignore = "owned popup on a private desktop; use isolated_tray_probe.py --test menu-close"]
    fn native_close_ends_active_popup() {
        use std::sync::mpsc;
        use std::time::Duration;
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::WindowsAndMessaging::*;
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        assert_eq!(std::env::var("MINICON_NO_TASKBAR").as_deref(), Ok("1"));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (ended_tx, ended_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (shell, owner, thread): (isize, isize, u32) =
                ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                if unsafe { FindWindowW(PCWSTR(wide("#32768").as_ptr()), PCWSTR::null()) }.is_ok() {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "popup did not open");
                std::thread::sleep(Duration::from_millis(10));
            }
            unsafe { PostMessageW(Some(HWND(shell as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
                .unwrap();
            let natural = ended_rx.recv_timeout(Duration::from_secs(2)).is_ok();
            if !natural {
                // Rescue only this owned private popup so a regression fails promptly.
                unsafe {
                    PostMessageW(
                        Some(HWND(owner as *mut _)),
                        WM_CANCELMODE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                }
                .unwrap();
                ended_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) }.unwrap();
            natural
        });
        run(false, |usable| {
            assert!(usable);
            let owner = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(wide("STATIC").as_ptr()),
                    PCWSTR(wide("Owned menu exit fixture").as_ptr()),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    0,
                    0,
                    320,
                    200,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap();
            ready_tx
                .send((
                    hub::shell_hwnd().unwrap().0 as isize,
                    owner.0 as isize,
                    unsafe { GetCurrentThreadId() },
                ))
                .unwrap();
            let selected = unsafe {
                crate::win_util::popup_menu(
                    owner,
                    &[crate::win_util::MenuRow::command(1, "Owned command")],
                    20,
                    20,
                )
            };
            assert_eq!(selected, None);
            ended_tx.send(()).unwrap();
            unsafe { DestroyWindow(owner) }.unwrap();
            Ok(())
        })
        .unwrap();
        let natural = worker.join().unwrap();
        crate::settings_ui::shutdown();
        crate::run_dialog::shutdown();
        crate::tray_popup::shutdown();
        crate::tray_host::shutdown();
        assert!(natural, "exit left TrackPopupMenu waiting for input");
    }
    #[test]
    #[ignore = "private desktop with owned hotkey competitor; use isolated_tray_probe.py --test collision"]
    fn native_rejects_all_launch_hotkeys_taken() {
        use windows::Win32::UI::WindowsAndMessaging::*;
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        assert_eq!(std::env::var("MINICON_NO_TASKBAR").as_deref(), Ok("1"));
        let companion = std::env::var_os("MINICON_TEST_COMPANION").is_some();
        let class = if companion {
            "Shell_TrayWnd"
        } else {
            "minicon_fixture_hotkeys"
        };
        let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "shell_swap::tests::private_foreign_host",
                "--nocapture",
            ])
            .env("MINICON_TEST_OCCUPY_LAUNCH_KEYS", "1")
            .env(
                "MINICON_TEST_ONLY_HOTKEYS",
                if companion { "0" } else { "1" },
            )
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let competitor = loop {
            if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR(wide(class).as_ptr()), PCWSTR::null()) } {
                break hwnd;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let mut competitor_ready = 0;
        while std::time::Instant::now() < deadline {
            let _ = unsafe {
                SendMessageTimeoutW(
                    competitor,
                    WM_APP + 219,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG,
                    100,
                    Some(&mut competitor_ready),
                )
            };
            if competitor_ready == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(competitor_ready, 1);
        let before = crate::shell_registry::current_raw().unwrap();
        let result = run(false, |usable| {
            assert_eq!(usable, companion);
            if companion {
                assert!(!standalone_launch_ready());
                assert!(crate::shell_swap::take_over().is_err());
                assert_eq!(crate::shell_registry::current_raw().unwrap(), before);
                unsafe { PostMessageW(crate::hub::shell_hwnd(), WM_CLOSE, WPARAM(0), LPARAM(0)) }
                    .unwrap();
            }
            Ok(())
        });
        assert_eq!(result.is_ok(), companion);
        assert_eq!(STARTUP_READY.get(), companion);
        assert!(!crate::launcher_ui::reachable());
        unsafe { PostMessageW(Some(competitor), WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap();
        assert!(owner.wait().unwrap().success());
        crate::settings_ui::shutdown();
        crate::run_dialog::shutdown();
        crate::tray_popup::shutdown();
        crate::tray_host::shutdown();
    }
    #[test]
    #[ignore = "private-desktop startup/UI worker fixture; use isolated_tray_probe.py --test startup"]
    fn native_startup_and_worker_host_transition() {
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        assert_eq!(std::env::var("MINICON_NO_TASKBAR").as_deref(), Ok("1"));
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(ready_for_handover());
            assert!(crate::tray_host::is_installed());
            transition_tray(false).unwrap();
            assert!(!crate::tray_host::is_installed());
            transition_tray(true).unwrap();
            assert!(crate::tray_host::is_installed());
            let hwnd = hub::shell_hwnd().unwrap();
            unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap();
        });
        run(false, |usable| {
            assert!(usable);
            sender.send(()).unwrap();
            Ok(())
        })
        .unwrap();
        worker.join().unwrap();
        crate::settings_ui::shutdown();
        crate::run_dialog::shutdown();
        crate::tray_popup::shutdown();
        crate::tray_host::shutdown();
    }
    #[test]
    fn failure_never_acknowledges_even_if_a_caller_accepts_it() {
        let events = RefCell::new(Vec::new());
        assert!(
            complete_startup(
                false,
                |usable| {
                    assert!(!usable);
                    events.borrow_mut().push("ready-rejected");
                    Ok(())
                },
                || events.borrow_mut().push("receipt")
            )
            .is_err()
        );
        assert_eq!(*events.borrow(), ["ready-rejected"]);
    }
    #[test]
    fn caller_failure_preserves_recovery_and_prevents_all_success_signals() {
        let events = RefCell::new(Vec::new());
        assert!(
            complete_startup(
                true,
                |_| Err("handoff failure".into()),
                || events.borrow_mut().push("receipt")
            )
            .is_err()
        );
        assert!(events.borrow().is_empty());
    }
    #[test]
    fn only_usable_and_accepted_startup_confirms_after_ready() {
        let events = RefCell::new(Vec::new());
        complete_startup(
            true,
            |usable| {
                assert!(usable);
                events.borrow_mut().push("ready");
                Ok(())
            },
            || events.borrow_mut().push("receipt"),
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["ready", "receipt"]);
    }
}
