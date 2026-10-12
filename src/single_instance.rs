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
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

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
