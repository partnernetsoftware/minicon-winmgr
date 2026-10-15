//! Single-instance guard backed by a named Win32 mutex.
//!
//! The mutex name is session-local (no `Global\` prefix), which is what a
//! per-session shell wants: one shell instance per interactive session.

use std::fmt;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

/// Owns the singleton mutex for as long as it is alive; releases it on drop.
pub struct InstanceGuard {
    handle: HANDLE,
}

#[derive(Debug)]
pub enum AcquireError {
    /// Another instance already holds the mutex.
    AlreadyRunning,
    /// The mutex could not be created; carries the raw OS error code.
    Create(u32),
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcquireError::AlreadyRunning => {
                write!(
                    f,
                    "another instance already holds the single-instance mutex"
                )
            }
            AcquireError::Create(code) => write!(f, "CreateMutexW failed (os error {code})"),
        }
    }
}

impl std::error::Error for AcquireError {}

impl InstanceGuard {
    /// Attempts to become the singleton holder of `name`.
    pub fn acquire(name: &str) -> Result<Self, AcquireError> {
        let wide: Vec<u16> = crate::win_util::wide(name);

        // SAFETY: `wide` is a valid null-terminated UTF-16 buffer that outlives
        // the call; all other arguments are plain values.
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) };

        match handle {
            Ok(handle) => {
                // CreateMutexW succeeds even when the mutex already exists; the
                // pre-existing state is signaled via ERROR_ALREADY_EXISTS.
                if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                    let _ = unsafe { CloseHandle(handle) };
                    Err(AcquireError::AlreadyRunning)
                } else {
                    Ok(InstanceGuard { handle })
                }
            }
            Err(err) => Err(AcquireError::Create(err.code().0 as u32)),
        }
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Only grant foreground permission to a process with our user SID and session.
pub(crate) fn same_user_session(pid: u32) -> bool {
    use windows::Win32::Security::{
        EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    fn user(process: HANDLE) -> Option<Vec<usize>> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
        let token = Owned(token);
        let mut bytes = 0;
        let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut bytes) };
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                bytes,
                &mut bytes,
            )
        }
        .ok()?;
        Some(buffer)
    }
    let mut ours = 0;
    let mut theirs = 0;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut ours) }.is_err()
        || unsafe { ProcessIdToSessionId(pid, &mut theirs) }.is_err()
        || ours != theirs
    {
        return false;
    }
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    let process = Owned(process);
    let (Some(our_user), Some(their_user)) =
        (user(unsafe { GetCurrentProcess() }), user(process.0))
    else {
        return false;
    };
    // Aligned buffers own both TOKEN_USER structures and their embedded SID data.
    unsafe {
        EqualSid(
            (*(our_user.as_ptr().cast::<TOKEN_USER>())).User.Sid,
            (*(their_user.as_ptr().cast::<TOKEN_USER>())).User.Sid,
        )
    }
    .is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn settings_identity_accepts_self_and_rejects_invalid_process() {
        assert!(super::same_user_session(unsafe {
            windows::Win32::System::Threading::GetCurrentProcessId()
        }));
        assert!(!super::same_user_session(u32::MAX));
    }
}
