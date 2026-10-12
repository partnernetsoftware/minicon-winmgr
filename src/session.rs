//! Session lifecycle (M4).
//!
//! Registers the shell window for Windows Terminal Services session-change
//! notifications so the shell can observe logon, lock/unlock, connect, and
//! disconnect events — the seed of company-oriented session management.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};

// WTS session-change event codes from `wtsapi32.h`. The windows crate exposes
// `NOTIFY_FOR_THIS_SESSION` but not these individual event values, so they are
// mirrored here (stable Win32 ABI values).
const WTS_CONSOLE_CONNECT: usize = 0x1;
const WTS_CONSOLE_DISCONNECT: usize = 0x2;
const WTS_REMOTE_CONNECT: usize = 0x3;
const WTS_REMOTE_DISCONNECT: usize = 0x4;
const WTS_SESSION_LOGON: usize = 0x5;
const WTS_SESSION_LOGOFF: usize = 0x6;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;
const WTS_SESSION_REMOTE_CONTROL: usize = 0x9;
const WTS_SESSION_CREATE: usize = 0xa;
const WTS_SESSION_TERMINATE: usize = 0xb;

/// Registers `hwnd` to receive `WM_WTSSESSION_CHANGE` for this session.
pub fn register(hwnd: HWND) -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: `hwnd` is a live window owned by this process.
    unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)? };
    tracing::info!("WTS session notifications registered");
    Ok(())
}

/// Unregisters `hwnd` from session-change notifications (best-effort).
pub fn unregister(hwnd: HWND) {
    // SAFETY: `hwnd` is a live window owned by this process.
    if let Err(err) = unsafe { WTSUnRegisterSessionNotification(hwnd) } {
        tracing::warn!(%err, "WTSUnRegisterSessionNotification failed");
    }
}

/// Maps a `WM_WTSSESSION_CHANGE` event code to a human-readable name.
pub fn event_name(code: usize) -> &'static str {
    match code {
        WTS_CONSOLE_CONNECT => "console-connect",
        WTS_CONSOLE_DISCONNECT => "console-disconnect",
        WTS_REMOTE_CONNECT => "remote-connect",
        WTS_REMOTE_DISCONNECT => "remote-disconnect",
        WTS_SESSION_LOGON => "session-logon",
        WTS_SESSION_LOGOFF => "session-logoff",
        WTS_SESSION_LOCK => "session-lock",
        WTS_SESSION_UNLOCK => "session-unlock",
        WTS_SESSION_REMOTE_CONTROL => "session-remote-control",
        WTS_SESSION_CREATE => "session-create",
        WTS_SESSION_TERMINATE => "session-terminate",
        _ => "unknown",
    }
}
