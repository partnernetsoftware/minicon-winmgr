//! Normalize startup before config, singleton ownership or window creation.
//! A suspended replacement is verified before any of its code can execute.
use std::os::windows::ffi::OsStrExt;
use windows::{
    Win32::{
        Foundation::*,
        Security::*,
        System::{
            Environment::{FreeEnvironmentStringsW, GetCommandLineW, GetEnvironmentStringsW},
            Threading::*,
        },
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, PWSTR},
};

// SID_AND_ATTRIBUTES group flags from winnt.h.
const SE_GROUP_ENABLED: u32 = 0x00000004;
const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x00000010;
const SE_GROUP_INTEGRITY: u32 = 0x00000020;

struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
#[derive(Debug)]
pub(crate) struct Status {
    pub elevated: bool,
    pub integrity: u32,
    pub administrator: bool,
}
impl Status {
    fn ordinary(&self) -> bool {
        !self.elevated && !self.administrator && self.integrity <= 0x2000
    }
}
fn fixed<T: Default>(token: HANDLE, kind: TOKEN_INFORMATION_CLASS) -> Result<T, String> {
    let mut value = T::default();
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(
            token,
            kind,
            Some((&mut value as *mut T).cast()),
            size_of::<T>() as u32,
            &mut bytes,
        )
    }
    .map_err(|e| {
        crate::ui_format!(
            "读取启动令牌失败：{e}",
            "Cannot read the startup token: {e}"
        )
    })?;
    Ok(value)
}
fn variable(token: HANDLE, kind: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>, String> {
    let mut bytes = 0;
    let _ = unsafe { GetTokenInformation(token, kind, None, 0, &mut bytes) };
    if bytes == 0 {
        return Err(crate::i18n::text("无法读取启动令牌信息长度").into());
    }
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token,
            kind,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
            &mut bytes,
        )
    }
    .map_err(|e| {
        crate::ui_format!(
            "读取启动令牌信息失败：{e}",
            "Cannot read startup token information: {e}"
        )
    })?;
    Ok(buffer)
}
fn token(process: HANDLE, access: TOKEN_ACCESS_MASK) -> Result<Owned, String> {
    let mut handle = HANDLE::default();
    unsafe { OpenProcessToken(process, access, &mut handle) }.map_err(|e| {
        crate::ui_format!(
            "无法检查进程权限：{e}",
            "Cannot inspect process privileges: {e}"
        )
    })?;
    Ok(Owned(handle))
}
fn snapshot(token: HANDLE) -> Result<Status, String> {
    let elevation: TOKEN_ELEVATION = fixed(token, TokenElevation)?;
    let label = variable(token, TokenIntegrityLevel)?;
    let sid = unsafe {
        (*(label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()))
            .Label
            .Sid
    };
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err(crate::i18n::text("进程完整性级别无效").into());
    }
    let integrity = unsafe { *GetSidSubAuthority(sid, u32::from(count - 1)) };
    let groups = variable(token, TokenGroups)?;
    let groups = unsafe { &*groups.as_ptr().cast::<TOKEN_GROUPS>() };
    let mut administrator = false;
    for index in 0..groups.GroupCount as usize {
        let group = unsafe { &*groups.Groups.as_ptr().add(index) };
        if group.Attributes & SE_GROUP_ENABLED != 0
            && group.Attributes & SE_GROUP_USE_FOR_DENY_ONLY == 0
            && unsafe { IsWellKnownSid(group.Sid, WinBuiltinAdministratorsSid).as_bool() }
        {
            administrator = true;
        }
    }
    Ok(Status {
        elevated: elevation.TokenIsElevated != 0,
        integrity,
        administrator,
    })
}
pub(crate) fn description() -> &'static str {
    static LABEL: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    crate::i18n::text(LABEL.get_or_init(|| match current() {
        Ok(s) if s.ordinary() => "普通权限",
        Ok(_) => "管理员权限",
        Err(_) => "权限状态未知",
    }))
}

pub(crate) fn current() -> Result<Status, String> {
    snapshot(token(unsafe { GetCurrentProcess() }, TOKEN_QUERY)?.0)
}
fn medium_restricted(original: HANDLE) -> Result<Owned, String> {
    let mut restricted = HANDLE::default();
    unsafe {
        CreateRestrictedToken(
            original,
            LUA_TOKEN | DISABLE_MAX_PRIVILEGE,
            None,
            None,
            None,
            &mut restricted,
        )
    }
    .map_err(|e| {
        crate::ui_format!(
            "无法生成普通权限令牌：{e}",
            "Cannot create a standard-user token: {e}"
        )
    })?;
    let restricted = Owned(restricted);
    // One aligned allocation owns both the structure and its embedded SID.
    let mut storage =
        vec![0usize; (size_of::<TOKEN_MANDATORY_LABEL>() + 68).div_ceil(size_of::<usize>())];
    let address = storage.as_mut_ptr().cast::<u8>();
    let sid = PSID(unsafe { address.add(size_of::<TOKEN_MANDATORY_LABEL>()) }.cast());
    let mut length = 68;
    unsafe {
        CreateWellKnownSid(WinMediumLabelSid, None, Some(sid), &mut length).map_err(|e| {
            crate::ui_format!(
                "无法生成普通完整性级别：{e}",
                "Cannot create a medium-integrity level: {e}"
            )
        })?;
        address
            .cast::<TOKEN_MANDATORY_LABEL>()
            .write(TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: sid,
                    Attributes: SE_GROUP_INTEGRITY,
                },
            });
        SetTokenInformation(
            restricted.0,
            TokenIntegrityLevel,
            address.cast(),
            (size_of::<TOKEN_MANDATORY_LABEL>() + length as usize) as u32,
        )
        .map_err(|e| {
            crate::ui_format!(
                "无法降低进程完整性级别：{e}",
                "Cannot lower the process integrity level: {e}"
            )
        })?;
    }
    if !snapshot(restricted.0)?.ordinary() {
        return Err(crate::i18n::text("系统未能生成普通权限令牌；请从普通用户会话启动。").into());
    }
    Ok(restricted)
}
fn ordinary_token(original: HANDLE) -> Result<Owned, String> {
    if let Ok(linked) = fixed::<TOKEN_LINKED_TOKEN>(original, TokenLinkedToken) {
        let linked = Owned(linked.LinkedToken);
        let mut primary = HANDLE::default();
        if unsafe {
            DuplicateTokenEx(
                linked.0,
                TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut primary,
            )
        }
        .is_ok()
        {
            let primary = Owned(primary);
            if snapshot(primary.0)?.ordinary() {
                return Ok(primary);
            }
        }
    }
    // Built-in Administrator / disabled UAC may have no linked token.
    // Use the documented LUA restriction, never just lower the integrity label.
    medium_restricted(original)
}
fn same_identity(first: HANDLE, second: HANDLE) -> Result<bool, String> {
    let a = variable(first, TokenUser)?;
    let b = variable(second, TokenUser)?;
    let equal = unsafe {
        EqualSid(
            (*(a.as_ptr().cast::<TOKEN_USER>())).User.Sid,
            (*(b.as_ptr().cast::<TOKEN_USER>())).User.Sid,
        )
    }
    .is_ok();
    Ok(equal && fixed::<u32>(first, TokenSessionId)? == fixed::<u32>(second, TokenSessionId)?)
}
// The native block also contains hidden per-drive entries such as =C:.
// Rebuilding it from std::env::vars_os would silently drop those entries.
unsafe fn copy_environment(address: *const u16) -> Vec<u16> {
    let mut length = 0;
    unsafe {
        while *address.add(length) != 0 || *address.add(length + 1) != 0 {
            length += 1;
        }
        std::slice::from_raw_parts(address, length + 2).to_vec()
    }
}
fn environment() -> Result<Vec<u16>, String> {
    struct Block(PWSTR);
    impl Drop for Block {
        fn drop(&mut self) {
            let _ = unsafe { FreeEnvironmentStringsW(self.0) };
        }
    }
    let block = Block(unsafe { GetEnvironmentStringsW() });
    if block.0.is_null() {
        return Err(crate::ui_format!(
            "无法读取启动环境：{}",
            "Cannot read the startup environment: {}",
            windows::core::Error::from_thread()
        ));
    }
    Ok(unsafe { copy_environment(block.0.as_ptr()) })
}
struct Child {
    process: Owned,
    thread: Owned,
    suspended: bool,
}
// Only query a message window owned by the verified child. A successful GUI
// response means assembly finished; an exiting CLI child must return success.
fn wait_ready(child: &Child, pid: u32) -> Result<(), String> {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(15) {
        if unsafe { WaitForSingleObject(child.process.0, 0) } == WAIT_OBJECT_0 {
            let mut code = 1;
            unsafe { GetExitCodeProcess(child.process.0, &mut code) }.map_err(|e| {
                crate::ui_format!(
                    "无法检查普通权限进程：{e}",
                    "Cannot inspect the standard-user process: {e}"
                )
            })?;
            return if code == 0 {
                Ok(())
            } else {
                Err(crate::ui_format!(
                    "普通权限进程启动失败（退出码 {code}）",
                    "The standard-user process failed to start (exit code {code})"
                ))
            };
        }
        let mut after = None;
        while let Ok(hwnd) = unsafe {
            FindWindowExW(
                Some(HWND_MESSAGE),
                after,
                PCWSTR(crate::win_util::wide("minicon_winmgr_message_window").as_ptr()),
                None,
            )
        } {
            let mut owner = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut owner));
            }
            if owner == pid {
                let mut ready = 0;
                let sent = unsafe {
                    SendMessageTimeoutW(
                        hwnd,
                        crate::window::STARTUP_READY_MESSAGE,
                        WPARAM(0),
                        LPARAM(0),
                        SMTO_ABORTIFHUNG,
                        100,
                        Some(&mut ready),
                    )
                };
                if sent.0 != 0 && ready == 1 {
                    return Ok(());
                }
            }
            after = Some(hwnd);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(crate::i18n::text("普通权限进程未能在 15 秒内完成启动。请从普通用户会话重试。").into())
}
impl Drop for Child {
    fn drop(&mut self) {
        if self.suspended {
            let _ = unsafe { TerminateProcess(self.process.0, 1) };
        }
    }
}
/// None means already ordinary; Some means the verified replacement was resumed.
pub(crate) fn normalize() -> Result<Option<u32>, String> {
    if current()?.ordinary() {
        return Ok(None);
    }
    let original = token(
        unsafe { GetCurrentProcess() },
        TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
    )?;
    let ordinary = ordinary_token(original.0)?;
    if !same_identity(original.0, ordinary.0)? {
        return Err(crate::i18n::text("普通权限令牌不属于当前用户会话").into());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let executable: Vec<_> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cwd: Vec<_> = cwd.as_os_str().encode_wide().chain(Some(0)).collect();
    let command = unsafe { GetCommandLineW().as_wide().to_vec() };
    let env = environment()?;
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    let mut mutable_command = command.clone();
    mutable_command.push(0);
    let first = unsafe {
        CreateProcessWithTokenW(
            ordinary.0,
            LOGON_WITH_PROFILE,
            PCWSTR(executable.as_ptr()),
            Some(PWSTR(mutable_command.as_mut_ptr())),
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(env.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &startup,
            &mut info,
        )
    };
    if let Err(first) = first {
        // Some administrative policies do not grant SeImpersonatePrivilege.
        mutable_command = command;
        mutable_command.push(0);
        unsafe {
            CreateProcessAsUserW(
                Some(ordinary.0),
                PCWSTR(executable.as_ptr()),
                Some(PWSTR(mutable_command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                Some(env.as_ptr().cast()),
                PCWSTR(cwd.as_ptr()),
                &startup,
                &mut info,
            )
        }
        .map_err(|second| {
            crate::ui_format!("无法以普通权限重新启动：{first}；备用方式：{second}。请从普通用户会话启动。","Unable to relaunch as a standard user: {first}; fallback: {second}. Start from a standard-user session.")
        })?;
    }
    let mut child = Child {
        process: Owned(info.hProcess),
        thread: Owned(info.hThread),
        suspended: true,
    };
    let child_token = token(child.process.0, TOKEN_QUERY)?;
    if !snapshot(child_token.0)?.ordinary() || !same_identity(original.0, child_token.0)? {
        return Err(
            crate::i18n::text("重新启动的权限或用户会话不符合要求，已终止该子进程。").into(),
        );
    }
    if unsafe { ResumeThread(child.thread.0) } == u32::MAX {
        return Err(crate::i18n::text("无法继续普通权限进程，已终止该子进程。").into());
    }
    child.suspended = false;
    if let Err(error) = wait_ready(&child, info.dwProcessId) {
        // Request normal teardown first, so a partial shell can restore its
        // reserved work area. This only targets the process we just created.
        let _ = unsafe { PostThreadMessageW(info.dwThreadId, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if unsafe { WaitForSingleObject(child.process.0, 2000) } != WAIT_OBJECT_0 {
            let _ = unsafe { TerminateProcess(child.process.0, 1) };
        }
        return Err(error);
    }
    Ok(Some(info.dwProcessId))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_requires_no_admin_group_and_at_most_medium_integrity() {
        for (elevated, administrator, integrity, expected) in [
            (false, false, 0x2000, true),
            (false, false, 0x3000, false),
            (true, false, 0x2000, false),
            (false, true, 0x2000, false),
        ] {
            assert_eq!(
                Status {
                    elevated,
                    administrator,
                    integrity
                }
                .ordinary(),
                expected
            );
        }
    }
    #[test]
    fn environment_preserves_unicode_and_spaces_and_double_terminates() {
        let value: Vec<_> = "=C:=C:\\张 三\0A=a=b\0MINICON_CONFIG=D:\\张 三\\a.conf\0\0"
            .encode_utf16()
            .collect();
        assert_eq!(unsafe { copy_environment(value.as_ptr()) }, value);
        assert_eq!(unsafe { copy_environment([0u16, 0].as_ptr()) }, [0, 0]);
        let native = environment().unwrap();
        assert!(native.len() >= 2);
        assert_eq!(&native[native.len() - 2..], [0, 0]);
    }
    #[test]
    fn native_lua_token_is_ordinary_and_keeps_user_and_session() {
        let original = token(
            unsafe { GetCurrentProcess() },
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT,
        )
        .unwrap();
        let limited = medium_restricted(original.0).unwrap();
        assert!(snapshot(limited.0).unwrap().ordinary());
        assert!(same_identity(original.0, limited.0).unwrap());
    }
}
