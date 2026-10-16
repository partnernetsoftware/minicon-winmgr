//! Explicit, graceful handoff between different executables in this session.
use crate::{
    single_instance::{AcquireError, InstanceGuard},
    win_util::wide,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
        },
        UI::WindowsAndMessaging::{
            FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE, IDOK, MB_DEFBUTTON2,
            MB_ICONQUESTION, MB_OKCANCEL, MessageBoxW, PostMessageW, WM_CLOSE,
        },
    },
    core::PCWSTR,
};

const MUTEX: &str = "minicon-winmgr.singleton";
struct Existing {
    handle: HANDLE,
    hwnd: HWND,
    pid: u32,
    path: PathBuf,
    hash: String,
}
impl Drop for Existing {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// The mutex is dropped before a failed startup attempts the old executable.
pub(crate) struct Session {
    guard: Option<InstanceGuard>,
    rollback: Option<Existing>,
}
impl Session {
    pub(crate) fn ready(&mut self, usable: bool) -> Result<(), String> {
        if self.rollback.is_some() && !usable {
            return Err(error(
                "The replacement could not create its essential interface. Restoring the previous executable.",
                "新实例未能创建必要界面，将尝试恢复原程序。",
            ));
        }
        self.rollback = None;
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.guard = None;
        if let Some(old) = self.rollback.take() {
            use std::os::windows::process::CommandExt;
            // Never restart a file that changed while the confirmation was open.
            if crate::update_stream::hash_file(&old.path).ok().as_ref() != Some(&old.hash) {
                tracing::error!("instance fallback executable changed; restart refused");
                return;
            }
            // A competing launch may have acquired the singleton meanwhile.
            if let Ok(guard) = InstanceGuard::acquire(MUTEX) {
                drop(guard);
                match std::process::Command::new(&old.path)
                    .current_dir(old.path.parent().unwrap_or(std::path::Path::new(".")))
                    .creation_flags(0x00000008 | 0x00000200 | 0x01000000)
                    .spawn()
                {
                    Ok(_) => tracing::warn!(
                        "new instance failed before readiness; old executable restarted"
                    ),
                    Err(error) => tracing::error!(%error, "old instance restart failed"),
                }
            }
        }
    }
}

fn error(en: &'static str, zh: &'static str) -> String {
    if crate::i18n::language() == crate::i18n::Language::En {
        en
    } else {
        zh
    }
    .into()
}

fn discover() -> Result<Existing, String> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let hwnd = unsafe {
            FindWindowExW(
                Some(HWND_MESSAGE),
                None,
                PCWSTR(wide("minicon_winmgr_message_window").as_ptr()),
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
                return Err(error(
                    "The running instance belongs to another user or session.",
                    "运行中的实例不属于当前用户会话。",
                ));
            }
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                )
            }
            .map_err(|_| {
                error(
                    "Cannot identify the running instance.",
                    "无法识别运行中的实例。",
                )
            })?;
            let mut old = Existing {
                handle,
                hwnd,
                pid,
                path: PathBuf::new(),
                hash: String::new(),
            };
            let mut buffer = vec![0u16; 32768];
            let mut length = buffer.len() as u32;
            unsafe {
                QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_WIN32,
                    windows::core::PWSTR(buffer.as_mut_ptr()),
                    &mut length,
                )
            }
            .map_err(|_| {
                error(
                    "Cannot identify the running executable.",
                    "无法识别运行中的程序文件。",
                )
            })?;
            use std::os::windows::ffi::OsStringExt;
            old.path = std::ffi::OsString::from_wide(&buffer[..length as usize]).into();
            old.hash = crate::update_stream::hash_file(&old.path).map_err(|_| {
                error(
                    "Cannot verify the running executable. It has been left running.",
                    "无法验证运行中的程序文件，已保留原实例。",
                )
            })?;
            return Ok(old);
        }
        if Instant::now() >= end {
            return Err(error(
                "The existing instance is not ready. Try again shortly.",
                "已有实例尚未就绪，请稍后重试。",
            ));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

pub(crate) fn acquire(show_settings: bool) -> Result<Option<Session>, String> {
    match InstanceGuard::acquire(MUTEX) {
        Ok(guard) => {
            return Ok(Some(Session {
                guard: Some(guard),
                rollback: None,
            }));
        }
        Err(AcquireError::AlreadyRunning) => {}
        Err(error) => return Err(error.to_string()),
    }
    // Confirmation precedes window creation, so initialize the saved UI language here.
    let _ = crate::config::current();
    let old = discover()?;
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let hash = crate::update_stream::hash_file(&current).map_err(|_| {
        error(
            "Cannot verify this executable. The running instance was not changed.",
            "无法验证当前程序，未更改运行中的实例。",
        )
    })?;
    if old.hash == hash {
        if show_settings {
            crate::window::request_settings()?;
        }
        return Ok(None);
    }
    let message = if crate::i18n::language() == crate::i18n::Language::En {
        format!(
            "Another winmgr executable is running. Switch to this one?\n\nRunning:\n{}\n\nNew: {}\n{}\n\nThe existing instance will close normally. Sign-in shell settings will stay as they are.",
            old.path.display(),
            env!("CARGO_PKG_VERSION"),
            current.display()
        )
    } else {
        format!(
            "检测到另一份 winmgr 正在运行，是否切换到当前程序？\n\n当前运行：\n{}\n\n即将运行：{}\n{}\n\n原实例将正常退出；不会更改登录外壳设置。",
            old.path.display(),
            env!("CARGO_PKG_VERSION"),
            current.display()
        )
    };
    let title = error("Switch running winmgr?", "切换运行中的 winmgr？");
    if unsafe {
        MessageBoxW(
            None,
            PCWSTR(wide(&message).as_ptr()),
            PCWSTR(wide(&title).as_ptr()),
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        )
    } != IDOK
    {
        return Ok(None);
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(old.hwnd, Some(&mut pid));
    }
    if pid != old.pid || unsafe { WaitForSingleObject(old.handle, 0) } == WAIT_OBJECT_0 {
        return Err(error(
            "The running instance changed. Please try again.",
            "运行中的实例已变化，请重新启动当前程序。",
        ));
    }
    unsafe { PostMessageW(Some(old.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.map_err(|_| {
        error(
            "Cannot ask the old instance to close.",
            "无法请求原实例退出。",
        )
    })?;
    if unsafe { WaitForSingleObject(old.handle, 15000) } != WAIT_OBJECT_0 {
        return Err(error(
            "The old instance has not exited. Resolve any unsaved-settings prompt and try again. It was not forcibly stopped.",
            "原实例尚未退出。请处理未保存设置等提示后重试；没有强制结束原实例。",
        ));
    }
    let guard = InstanceGuard::acquire(MUTEX).map_err(|_| {
        error(
            "Another instance started during handoff. Please try again.",
            "交接期间另一个实例已启动，请重试。",
        )
    })?;
    Ok(Some(Session {
        guard: Some(guard),
        rollback: Some(old),
    }))
}
