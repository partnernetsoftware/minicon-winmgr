//! Settings entry points and the separately confirmed desktop-management actions.
pub(crate) use crate::settings_panel::{filter, install, show, shutdown, toggle_from};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
static SHELL_BUSY: AtomicBool = AtomicBool::new(false);
enum ShellResult {
    Takeover(Result<crate::shell_swap::Takeover, String>),
    Restore(Result<crate::shell_swap::GiveBack, String>),
}
static SHELL_RESULT: OnceLock<Mutex<Option<ShellResult>>> = OnceLock::new();
pub(crate) fn shell_busy() -> bool {
    SHELL_BUSY.load(Ordering::SeqCst)
}
pub(crate) fn allow_exit() -> bool {
    !shell_busy() && crate::settings_panel::allow_exit()
}
fn start_shell_operation(takeover: bool) {
    if SHELL_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::settings_panel::refresh_shell_status();
    let thread = std::thread::Builder::new()
        .name("desktop-handover".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(|| if takeover {
                ShellResult::Takeover(crate::shell_swap::take_over().map_err(|e| e.to_string()))
            } else {
                ShellResult::Restore(crate::shell_swap::give_back().map_err(|e| e.to_string()))
            }).unwrap_or_else(|_| if takeover { ShellResult::Takeover(Err("Desktop operation failed unexpectedly. Check current login settings before retrying.".into())) } else { ShellResult::Restore(Err("Desktop restoration failed unexpectedly. Check current login settings before retrying.".into())) });
            *crate::win_util::lock_or_recover(SHELL_RESULT.get_or_init(|| Mutex::new(None))) =
                Some(result);
            if let Some(hwnd) = crate::hub::shell_hwnd() {
                let _ = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        Some(hwnd),
                        crate::window::SHELL_RESULT_MESSAGE,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    )
                };
            }
        });
    if let Err(error) = thread {
        SHELL_BUSY.store(false, Ordering::SeqCst);
        crate::settings_panel::refresh_shell_status();
        info_box(&error.to_string());
    }
}
pub(crate) fn complete_shell_operation() {
    let result =
        crate::win_util::lock_or_recover(SHELL_RESULT.get_or_init(|| Mutex::new(None))).take();
    if let Some(result) = result {
        SHELL_BUSY.store(false, Ordering::SeqCst);
        crate::settings_panel::refresh_shell_status();
        match result {
            ShellResult::Takeover(result) => show_takeover_result(result),
            ShellResult::Restore(result) => show_restore_result(result),
        }
    }
}
use crate::win_util::wide;
use windows::Win32::UI::WindowsAndMessaging::{
    IDOK, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_OK, MB_OKCANCEL, MessageBoxW,
};
use windows::core::PCWSTR;
fn info_box(message: &str) {
    unsafe {
        MessageBoxW(
            crate::settings_panel::owner(),
            PCWSTR(wide(message).as_ptr()),
            PCWSTR(wide(crate::i18n::text("设置")).as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}
pub(crate) fn do_replace() {
    if shell_busy() {
        return;
    }
    // Already the shell? Nothing to hand over — and stopping the session's
    // shell would stop *us* (we own the tray host's `Shell_TrayWnd`). Refuse
    // before the confirmation so the user is never asked to approve a no-op.
    if crate::shell_swap::self_is_shell() {
        tracing::info!("settings: takeover skipped, already running as the shell");
        info_box(crate::i18n::text(
            "当前已经是 minicon 在充当桌面外壳，无需接管。",
        ));
        return;
    }
    // Taking over is immediate now: the desktop shell that is running in this
    // session gets stopped, not just the registry value written. That closes
    // the user's desktop, so it is confirmed first (default: Cancel).
    let text = wide(if crate::i18n::language() == crate::i18n::Language::En {
        "Replace the desktop shell with minicon-winmgr now?\n\nThe current shell (Explorer) will close, including its taskbar and desktop icons. Minicon will provide the taskbar and remain the shell at the next sign-in.\n\nContinue?"
    } else {
        "接管桌面：立即把当前桌面外壳换成 minicon-winmgr。\n\n\
         现在运行的桌面外壳（explorer）会立刻关闭，任务栏与桌面图标随之消失，\
         由 minicon 的任务栏接管。下一次登录仍使用 minicon。\n\n\
         确定？"
    });
    let caption = wide(crate::i18n::text("接管桌面"));
    // SAFETY: both buffers outlive the call.
    let answer = unsafe {
        MessageBoxW(
            crate::settings_panel::owner(),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OKCANCEL | MB_ICONINFORMATION | MB_DEFBUTTON2,
        )
    };
    if answer != IDOK {
        return;
    }

    execute_takeover();
}

pub(crate) fn execute_takeover() {
    start_shell_operation(true);
}
fn show_takeover_result(result: Result<crate::shell_swap::Takeover, String>) {
    match result {
        Ok(report) => {
            tracing::info!(
                exe = %report.exe,
                stopped = report.stopped,
                "settings: shell taken over"
            );
            let mut message = String::from(crate::i18n::text("已接管桌面（立即生效）。\n"));
            if report.stopped == 0 {
                message.push_str(crate::i18n::text(
                    "本会话没有正在运行的桌面外壳，无需关闭。\n",
                ));
            } else {
                message.push_str(&crate::ui_format!(
                    "已关闭 {} 个桌面外壳进程。\n",
                    "Stopped {} desktop shell process(es).\n",
                    report.stopped
                ));
            }
            message.push_str(if crate::i18n::language() == crate::i18n::Language::En {
                "\nRestore pre-takeover settings changes the next sign-in; Minicon continues managing this session."
            } else {
                "\n恢复接管前设置会更改下次登录；当前会话继续由 Minicon 管理。"
            });
            info_box(&message);
        }
        Err(err) => {
            tracing::warn!(%err, "settings: shell takeover failed");
            info_box(&crate::ui_format!(
                "接管桌面失败：{err}",
                "Desktop takeover failed: {err}"
            ));
        }
    }
}

pub(crate) fn do_rollback() {
    if shell_busy() {
        return;
    }
    // Confirm, defaulting to Cancel — this hands the desktop back to Explorer.
    // When *we* are the shell the handover completes at the next sign-in (our
    // own bar is holding the session and cannot restart Explorer under itself),
    // so say so instead of over-promising.
    let original = match crate::shell_recovery::original_text() {
        Ok(value) => value,
        Err(error) => {
            info_box(&crate::ui_format!(
                "恢复记录不可用：{error}。旧记录不会被自动采用；可用显式确认的 --shell-rollback <原值|default> 手工选择下次登录设置。",
                "Recovery record unavailable: {error}. Legacy records are not adopted automatically. An explicitly confirmed --shell-rollback <value|default> can select next-sign-in settings."
            ));
            return;
        }
    };
    let target = original.as_deref().unwrap_or("Windows default (Explorer)");
    let message = crate::ui_format!(
        "恢复接管前的下次登录设置：\n{target}\n当前会话由现有宿主管理；不会自动执行第三方外壳命令。\n\n确定？",
        "Restore pre-takeover settings for the next sign-in:\n{target}\nThe existing host continues to manage this session. Third-party shell commands are not run automatically.\n\nContinue?"
    );
    let text = wide(&message);
    let caption = wide(crate::i18n::text("还原系统桌面"));
    // SAFETY: both buffers outlive the call.
    let answer = unsafe {
        MessageBoxW(
            crate::settings_panel::owner(),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OKCANCEL | MB_ICONINFORMATION | MB_DEFBUTTON2,
        )
    };
    if answer != IDOK {
        return;
    }
    start_shell_operation(false);
}
fn show_restore_result(result: Result<crate::shell_swap::GiveBack, String>) {
    let we_are_shell = crate::shell_swap::self_is_shell();
    match result {
        Ok(report) => {
            tracing::info!(
                restored = ?report.restored,
                started = report.started,
                "settings: shell given back"
            );
            let mut message = String::from(crate::i18n::text("已还原系统桌面设置。\n"));
            message.push_str(if report.started {
                crate::i18n::text("已重新启动 explorer。")
            } else if we_are_shell {
                if crate::i18n::language() == crate::i18n::Language::En { "Next sign-in will use the pre-takeover settings. Minicon continues managing this session." } else { "下次登录使用接管前设置，当前会话继续由 Minicon 管理。" }
            } else if crate::shell_swap::foreign_shell_running() {
                crate::i18n::text("本会话已有桌面外壳在运行。")
            } else if crate::i18n::language() == crate::i18n::Language::En {
                "Next-sign-in settings restored. No desktop host is currently observed; third-party commands were not started."
            } else {
                "已恢复下次登录设置。当前未检测到桌面宿主，未启动第三方命令。"
            });
            message.push_str(crate::i18n::text("\n若桌面外观未恢复，注销重登即可。"));
            info_box(&message);
        }
        Err(err) => {
            tracing::warn!(%err, "settings: shell give-back failed");
            info_box(&crate::ui_format!(
                "还原失败：{err}",
                "Desktop restoration failed: {err}"
            ));
        }
    }
}

pub(crate) fn shell_status_text() -> String {
    let en = crate::i18n::language() == crate::i18n::Language::En;
    if shell_busy() {
        return if en {
            "Desktop handover in progress… Settings remain available."
        } else {
            "桌面交接正在执行…设置仍可使用。"
        }
        .into();
    }
    let session = if crate::shell_swap::self_is_shell() {
        if en { "Minicon" } else { "Minicon 管理" }
    } else if crate::shell_swap::foreign_shell_running() {
        if en {
            "With existing desktop"
        } else {
            "与现有桌面共同运行"
        }
    } else {
        if en {
            "No desktop host detected"
        } else {
            "未检测到桌面宿主"
        }
    };
    let login = match crate::shell_registry::current_raw() {
        Ok(None) => {
            if en {
                "Windows default"
            } else {
                "Windows 默认桌面"
            }
        }
        Ok(value) if crate::shell_recovery::is_product(&value) => "Minicon",
        Ok(_) => {
            if en {
                "Other configured shell"
            } else {
                "其他外壳设置"
            }
        }
        Err(_) => {
            if en {
                "Unavailable"
            } else {
                "无法读取"
            }
        }
    };
    let recovery = if crate::shell_recovery::original_text().is_ok() {
        if en {
            "Account-bound record verified"
        } else {
            "账户恢复记录已校验"
        }
    } else {
        if en {
            "No verified record; legacy files not adopted"
        } else {
            "无已校验记录；不自动采用旧文件"
        }
    };
    if en {
        format!("This session: {session}\nNext sign-in: {login}\nRecovery: {recovery}")
    } else {
        format!("当前会话：{session}\n下次登录：{login}\n恢复依据：{recovery}")
    }
}
