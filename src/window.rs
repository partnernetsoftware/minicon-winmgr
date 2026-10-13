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

use std::sync::atomic::{AtomicIsize, Ordering};

use crate::session;
use crate::win_util::wide;
use crate::window_snapshot;

use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_QUERYENDSESSION, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASS_STYLES, WNDCLASSW,
};
use windows::core::{BOOL, PCWSTR};

const CLASS_NAME: &str = "minicon_winmgr_message_window";
const WINDOW_TITLE: &str = "minicon-winmgr (message-only)";
const HEARTBEAT_TIMER_ID: usize = 1;
const DEFAULT_HEARTBEAT_SECS: u64 = 30;

/// HWND of the shell message window, shared with the console control handler
/// (which runs on a separate OS thread). 0 means "not yet created".
static SHELL_HWND: AtomicIsize = AtomicIsize::new(0);

/// Runs the shell message loop until a quit message is received.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
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

        SHELL_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
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

        // Window-management hotkeys (Win+Left/Right/Up).
        if let Err(err) = crate::hotkeys::install(hinstance) {
            tracing::error!(%err, "window hotkeys init failed; continuing without them");
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
        let tray = crate::tray::enumerate();
        tracing::info!(count = tray.len(), "notification area mirrored");
        for icon in &tray {
            tracing::debug!(pid = icon.pid, tooltip = %icon.tooltip, "tray icon");
        }

        let hosted = crate::tray_host::icons();
        if !hosted.is_empty() {
            tracing::info!(count = hosted.len(), "tray icons hosted");
        }
        window_snapshot::log_snapshot();
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
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        tracing::info!("message loop exited");
        Ok(())
    }
}

/// Window procedure for the shell message window.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
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
            tracing::debug!(timer_id = wparam.0, "heartbeat: message pump alive");
            window_snapshot::log_snapshot();
            crate::taskbar::refresh();
            crate::metrics::log_sample();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Console control handler: forwards Ctrl+C / break / close to the message
/// thread by posting `WM_CLOSE`, keeping all shutdown work on one thread.
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    let raw = SHELL_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        tracing::info!(ctrl_type, "console control event; posting WM_CLOSE");
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
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
