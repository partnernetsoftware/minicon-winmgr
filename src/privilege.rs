//! Normalize startup before config, singleton ownership or window creation.
//! A suspended replacement is verified before any of its code can execute.
use std::os::windows::ffi::OsStrExt;
use windows::{
    Win32::{
        Foundation::*,
        Security::{Authorization, *},
        System::{
            Environment::{FreeEnvironmentStringsW, GetCommandLineW, GetEnvironmentStringsW},
            ProcessStatus::EnumProcesses,
            RemoteDesktop::ProcessIdToSessionId,
            Threading::*,
        },
        UI::WindowsAndMessaging::*,
    },
    core::{HRESULT, PCWSTR, PWSTR},
};

// SID_AND_ATTRIBUTES group flags from winnt.h.
const SE_GROUP_ENABLED: u32 = 0x00000004;
const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x00000010;

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
    /// `TOKEN_ELEVATION_TYPE`: 1 = Default, 2 = Full, 3 = Limited.
    pub elevation_type: u32,
}
impl Status {
    fn ordinary(&self) -> bool {
        !self.elevated
            && !self.administrator
            && self.integrity <= 0x2000
            // A Full-type token is an elevated token — or, when it carries
            // medium integrity, the artifact `CreateRestrictedToken` leaves
            // behind. A normal user process is Default (UAC off, or a user
            // with no split token) or Limited (the filtered token). Accepting
            // Full here is what let a restricted token pass as "ordinary",
            // and every console child of such a process then fails to
            // initialise (0xc0000142, no conhost) while GUI children work.
            && self.elevation_type != 2
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
    let elevation_type = fixed::<u32>(token, TokenElevationType)?;
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
        elevation_type,
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

/// `TokenUser` of the current process. Callers read the SID out of the buffer
/// while it is still alive.
fn current_user() -> Option<Vec<usize>> {
    let token = token(unsafe { GetCurrentProcess() }, TOKEN_QUERY).ok()?;
    variable(token.0, TokenUser).ok()
}

/// Account name of the current process token, e.g. `wjc2022`.
fn account_name() -> Option<String> {
    let user = current_user()?;
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut name = vec![0u16; 256];
    let mut domain = vec![0u16; 256];
    let mut kind = SID_NAME_USE::default();
    let mut name_len = name.len() as u32;
    let mut domain_len = domain.len() as u32;
    let resolved = unsafe {
        LookupAccountSidW(
            None,
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    };
    let read = |name: &[u16], len: u32| String::from_utf16_lossy(&name[..len as usize]);
    match resolved {
        Ok(()) => Some(read(&name, name_len)),
        // Only a too-small buffer is worth retrying; Windows reports the size
        // it needs, so one pass covers any real account name.
        Err(error)
            if error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
                && name_len as usize > name.len() =>
        {
            name = vec![0u16; name_len as usize];
            domain = vec![0u16; domain_len as usize];
            let mut retry_name = name.len() as u32;
            let mut retry_domain = domain.len() as u32;
            unsafe {
                LookupAccountSidW(
                    None,
                    sid,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut retry_name,
                    Some(PWSTR(domain.as_mut_ptr())),
                    &mut retry_domain,
                    &mut kind,
                )
            }
            .ok()
            .map(|()| read(&name, retry_name))
        }
        Err(_) => None,
    }
}

/// SID as text (`S-1-5-21-…-1001`): still unique per account, used when the
/// account name cannot be resolved.
fn sid_text() -> Option<String> {
    let user = current_user()?;
    let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    let mut raw = PWSTR::null();
    unsafe { Authorization::ConvertSidToStringSidW(sid, &mut raw) }.ok()?;
    if raw.is_null() {
        return None;
    }
    let mut length = 0usize;
    unsafe {
        while *raw.0.add(length) != 0 && length < 256 {
            length += 1;
        }
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(raw.0, length) });
    unsafe { LocalFree(Some(HLOCAL(raw.0.cast()))) };
    Some(text)
}

/// Identity used to keep per-account state apart: the account name, the SID
/// when the name cannot be resolved, and a fixed name as the last resort.
///
/// One executable can be started by several Windows accounts — every
/// interactive session runs its own shell instance, and they all resolve to
/// the same install directory. Anything written next to the executable (logs)
/// must therefore be segmented per account, or two accounts' lines interleave
/// in one file.
pub(crate) fn account_identity() -> String {
    account_name()
        .or_else(sid_text)
        .unwrap_or_else(|| "user".to_string())
}
fn ordinary_token(original: HANDLE) -> Result<Owned, String> {
    if let Some(primary) = linked_token(original) {
        return Ok(primary);
    }
    tracing::warn!(
        "startup token has no usable linked token; falling back to a peer process token"
    );
    if let Some(peer) = peer_token(original) {
        return Ok(peer);
    }
    Err(crate::i18n::text(
        "系统未能提供普通权限令牌；请从普通用户会话启动，或以普通用户身份重新登录。",
    )
    .into())
}

/// The filtered twin of an elevated token: the same user, the same session,
/// medium integrity and no administrative groups. Duplicated to a primary
/// token it is exactly what a normal user process runs with.
fn linked_token(original: HANDLE) -> Option<Owned> {
    let linked = fixed::<TOKEN_LINKED_TOKEN>(original, TokenLinkedToken)
        .inspect_err(|error| {
            tracing::warn!(%error, "startup token has no linked token");
        })
        .ok()?;
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
    .is_err()
    {
        tracing::warn!(
            error = %windows::core::Error::from_thread(),
            "linked token could not be duplicated to a primary token"
        );
        return None;
    }
    let primary = Owned(primary);
    match snapshot(primary.0) {
        Ok(status) if status.ordinary() => Some(primary),
        Ok(_) => {
            tracing::warn!("linked token is not an ordinary user token; ignoring it");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "linked token could not be inspected; ignoring it");
            None
        }
    }
}

/// How many same-session processes to inspect before giving up. Explorer
/// normally appears within the first few; the cap keeps a pathological process
/// table from delaying startup.
const PEER_SCAN_LIMIT: usize = 64;

/// A primary token borrowed from another process of the same user in the same
/// session.
///
/// Explorer's own token is the filtered token the user's desktop is built on,
/// so relaunching from it produces a process indistinguishable from a normal
/// user process. This is the fallback when the linked token cannot be
/// duplicated — some administrative policies deny that duplication, and the
/// previous fallback (`CreateRestrictedToken`) left a Full-elevation-type
/// token behind that broke every console child of the shell.
fn peer_token(original: HANDLE) -> Option<Owned> {
    let session = fixed::<u32>(original, TokenSessionId).ok()?;
    let mut pids = vec![0u32; 4096];
    let mut needed = 0u32;
    let listed = unsafe {
        EnumProcesses(
            pids.as_mut_ptr(),
            std::mem::size_of_val(&pids) as u32,
            &mut needed,
        )
    };
    if listed.is_err() || needed == 0 {
        return None;
    }
    let count = (needed as usize / std::mem::size_of::<u32>()).min(pids.len());
    let me = std::process::id();
    let candidates: Vec<u32> = pids[..count]
        .iter()
        .copied()
        .filter(|pid| *pid != 0 && *pid != me)
        .filter(|pid| peer_session(*pid) == Some(session))
        .take(PEER_SCAN_LIMIT)
        .collect();
    // Explorer first: its token is the session's own. A second pass accepts
    // any other same-user process, which covers machines where no Explorer
    // window is open.
    for explorer_only in [true, false] {
        for &pid in &candidates {
            if explorer_only && !is_explorer(pid) {
                continue;
            }
            if let Some(primary) = peer_primary(pid, original) {
                tracing::info!(pid, explorer = explorer_only, "relaunching with a peer process token");
                return Some(primary);
            }
        }
    }
    None
}

fn peer_session(pid: u32) -> Option<u32> {
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(pid, &mut session) }
        .ok()
        .map(|()| session)
}

fn is_explorer(pid: u32) -> bool {
    let Some(process) = open_peer_process(pid) else {
        return false;
    };
    let mut buffer = vec![0u16; 260];
    let mut length = buffer.len() as u32;
    let named = unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    named.is_ok()
        && String::from_utf16_lossy(&buffer[..length as usize])
            .rsplit(['\\', '/'])
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("explorer.exe"))
}

/// Duplicates `pid`'s token into a primary token, but only when it is the same
/// user in the same session and an ordinary, primary, non-Full-type token.
fn peer_primary(pid: u32, original: HANDLE) -> Option<Owned> {
    let process = open_peer_process(pid)?;
    let source = token(process.0, TOKEN_QUERY | TOKEN_DUPLICATE).ok()?;
    if !same_identity(original, source.0).unwrap_or(false) {
        return None;
    }
    if fixed::<u32>(source.0, TokenType).ok()? != 1 {
        return None;
    }
    if !snapshot(source.0).ok()?.ordinary() {
        return None;
    }
    let mut primary = HANDLE::default();
    if unsafe {
        DuplicateTokenEx(
            source.0,
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    }
    .is_err()
    {
        return None;
    }
    let primary = Owned(primary);
    if snapshot(primary.0).ok()?.ordinary() {
        Some(primary)
    } else {
        None
    }
}

fn open_peer_process(pid: u32) -> Option<Owned> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .inspect_err(|error| {
            if error.code() != windows::Win32::Foundation::ERROR_ACCESS_DENIED.to_hresult() {
                tracing::debug!(pid, %error, "peer process could not be opened");
            }
        })
        .ok()?;
    Some(Owned(handle))
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
/// What [`normalize`] decided about this process.
pub(crate) enum Startup {
    /// Already ordinary — or no clean ordinary token was available, in which
    /// case this process keeps running exactly as it was started.
    Unchanged,
    /// A verified ordinary replacement is running; this process must exit.
    Replaced(u32),
}

/// None-equivalent: already ordinary, or continued as started.
pub(crate) fn normalize() -> Result<Startup, String> {
    let status = current()?;
    if status.ordinary() {
        return Ok(Startup::Unchanged);
    }
    let original = token(
        unsafe { GetCurrentProcess() },
        TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
    )?;
    let ordinary = match ordinary_token(original.0) {
        Ok(ordinary) => ordinary,
        Err(error) => {
            // No clean ordinary token exists. The previous fallback
            // synthesised one with CreateRestrictedToken, which left a
            // Full-elevation-type token behind: every console child of such a
            // shell then died with 0xc0000142 (no conhost at all) while GUI
            // children looked perfectly healthy. Exiting here instead would
            // leave the session without a shell, so continue as started and
            // record why.
            tracing::warn!(
                elevated = status.elevated,
                integrity = u64::from(status.integrity),
                elevation_type = u64::from(status.elevation_type),
                %error,
                "started with elevated privileges and no clean ordinary token was available; continuing as started"
            );
            return Ok(Startup::Unchanged);
        }
    };
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
    Ok(Startup::Replaced(info.dwProcessId))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_requires_no_admin_group_and_at_most_medium_integrity() {
        for (elevated, administrator, integrity, elevation_type, expected) in [
            (false, false, 0x2000, 1, true),
            (false, false, 0x2000, 3, true),
            (false, false, 0x3000, 1, false),
            (true, false, 0x2000, 1, false),
            (false, true, 0x2000, 1, false),
            // A Full-type token with medium integrity is the artifact
            // CreateRestrictedToken produces; it must never count as ordinary.
            (false, false, 0x2000, 2, false),
            (true, false, 0x3000, 2, false),
        ] {
            assert_eq!(
                Status {
                    elevated,
                    administrator,
                    integrity,
                    elevation_type
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
    fn the_live_process_token_is_ordinary_and_keeps_user_and_session() {
        // Whatever token this test binary runs with, it must satisfy the same
        // ordinary() rule the relaunch check applies - and the account
        // identity helper must resolve to a usable log-directory segment.
        let original = token(
            unsafe { GetCurrentProcess() },
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT,
        )
        .unwrap();
        let status = snapshot(original.0).unwrap();
        if status.ordinary() {
            assert!(same_identity(original.0, original.0).unwrap());
            assert!(account_identity().chars().any(|ch| ch.is_ascii_alphanumeric()));
        } else {
            // A deliberately elevated test runner must not be mistaken for a
            // normal user process.
            assert!(status.elevated || status.elevation_type == 2 || status.administrator);
        }
    }

    #[test]
    fn a_peer_token_is_either_ordinary_and_ours_or_absent() {
        // CI runs without an interactive session, so no peer exists there and
        // the assertions are skipped; on a desktop they exercise the whole
        // enumeration - duplicate - verify fallback chain.
        let original = token(
            unsafe { GetCurrentProcess() },
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT,
        )
        .unwrap();
        if let Some(peer) = peer_token(original.0) {
            assert!(snapshot(peer.0).unwrap().ordinary());
            assert!(same_identity(original.0, peer.0).unwrap());
        }
    }
}
