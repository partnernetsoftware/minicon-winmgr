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
thread_local! { static STARTUP_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

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
pub fn run(show_settings: bool) -> Result<(), Box<dyn std::error::Error>> {
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

        if crate::taskbar::enabled() {
            if let Err(err) = crate::taskbar::create(hinstance) {
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
        if let Err(err) = crate::launcher_ui::init(hinstance) {
            tracing::error!(%err, "launcher UI init failed; continuing without it");
        }

        // Start menu (ORB) — opened from the taskbar brand dot.
        if let Err(err) = crate::start_menu::install(hinstance) {
            tracing::error!(%err, "start menu init failed; continuing without it");
        }

        // Run dialog (Win+R).
        if let Err(err) = crate::run_dialog::install(hinstance) {
            tracing::error!(%err, "run dialog init failed; continuing without it");
        }

        // Settings window (opened from the taskbar's settings button).
        if let Err(err) = crate::settings_ui::install(hinstance) {
            tracing::error!(%err, "settings init failed; continuing without it");
        }

        // Hover window-preview popup (shown by the taskbar on hover).
        if let Err(err) = crate::hover_preview::install(hinstance) {
            tracing::error!(%err, "hover preview init failed; continuing without it");
        }

        // Tray host is inert unless MINICON_TRAY_HOST=1 (replacement mode).
        if let Err(err) = crate::tray_host::install(hinstance) {
            tracing::error!(%err, "tray host install failed; continuing without it");
        }
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
        if crate::settings_panel::owner().is_some() {
            crate::update_install::confirm_startup();
        }
        if show_settings {
            crate::settings_ui::show();
        }
        crate::appearance::refresh();
        STARTUP_READY.set(true);
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
