//! Settings entry points and the separately confirmed desktop-management actions.
pub(crate) use crate::settings_panel::{allow_exit, filter, install, show, shutdown, toggle_from};
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

    match crate::shell_swap::take_over() {
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
            if !report.auto_restart_disabled {
                message.push_str(
                    "注意：未能关闭系统外壳自动重启（需要管理员权限），\
                     Windows 可能随后重新拉起 explorer。\n",
                );
            }
            message.push_str(crate::i18n::text(
                "\n可用「还原系统桌面（立即生效）」一键恢复。",
            ));
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
    // Confirm, defaulting to Cancel — this hands the desktop back to Explorer.
    // When *we* are the shell the handover completes at the next sign-in (our
    // own bar is holding the session and cannot restart Explorer under itself),
    // so say so instead of over-promising.
    let we_are_shell = crate::shell_swap::self_is_shell();
    let tail = if we_are_shell {
        crate::i18n::text("当前正是本程序在充当桌面外壳，切换将在下次登录时完成。")
    } else {
        crate::i18n::text("将立即重新启动 explorer。")
    };
    // Distinguish "no takeover was ever recorded" (give-back refuses, so we stop
    // up front rather than promising a restore we will not perform) from a
    // recorded-but-empty value (which restores the Windows default shell).
    let backed_up = crate::shell_swap::backup_path().and_then(|p| std::fs::read_to_string(p).ok());
    let message = match backed_up
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(v) => crate::ui_format!(
            "还原系统桌面：把外壳还原为\n{v}\n{tail}\n\n确定？",
            "Restore the desktop shell to:\n{v}\n{tail}\n\nContinue?"
        ),
        None if backed_up.is_some() => {
            crate::ui_format!(
                "还原系统桌面：把外壳还原为系统默认（explorer）\n{tail}\n\n确定？",
                "Restore the default desktop shell (Explorer).\n{tail}\n\nContinue?"
            )
        }
        None => {
            info_box(crate::i18n::text(
                "没有记录到接管前的桌面外壳（本程序未曾接管过桌面），无法还原。",
            ));
            return;
        }
    };
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
    match crate::shell_swap::give_back() {
        Ok(report) => {
            tracing::info!(
                restored = ?report.restored,
                started = report.started,
                "settings: shell given back"
            );
            let mut message = if we_are_shell {
                String::from(crate::i18n::text("已还原系统桌面设置。\n"))
            } else {
                String::from(crate::i18n::text("已还原系统桌面（立即生效）。\n"))
            };
            message.push_str(if report.started {
                crate::i18n::text("已重新启动 explorer。")
            } else if we_are_shell {
                crate::i18n::text("下次登录将由 explorer 接管桌面。")
            } else {
                crate::i18n::text("本会话已有桌面外壳在运行。")
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
