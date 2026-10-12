//! Notification-area (system tray) mirroring.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Best-effort: the notification area lives in another process
//! (`Shell_TrayWnd` → `ToolbarWindow32`), so enumeration requires reading the
//! toolbar's buttons out of that process. If it cannot be read, return empty and
//! log — never panic.

use std::ffi::c_void;
use std::mem::size_of;

use tracing::{debug, warn};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};
use windows::Win32::UI::Controls::{TB_BUTTONCOUNT, TB_GETBUTTON, TB_GETBUTTONTEXTW, TBBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SMTO_NORMAL,
    SendMessageTimeoutW,
};
use windows::core::PCWSTR;

use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};

/// Per-call timeout for cross-process toolbar messages (ms).
const TOOLBAR_TIMEOUT_MS: u32 = 2_000;
/// Size of the scratch region we map inside the tray host process.
const REMOTE_BLOCK: usize = 4_096;
/// Upper bound on tooltip characters we will read back.
const TOOLTIP_CHARS: usize = 512;

/// One icon found in the notification area.
pub struct TrayIcon {
    /// Tooltip text shown for the icon (may be empty).
    pub tooltip: String,
    /// Owning process id when it can be resolved (0 if unknown).
    pub pid: u32,
}

/// Enumerates the current notification-area icons, best effort.
pub fn enumerate() -> Vec<TrayIcon> {
    let Some(toolbar) = find_tray_toolbar() else {
        debug!("notification-area toolbar window not found");
        return Vec::new();
    };

    let mut pid: u32 = 0;
    // SAFETY: `toolbar` is a live HWND (checked by FindWindow*); `pid` is a
    // valid out-pointer for the duration of the call.
    let _thread = unsafe { GetWindowThreadProcessId(toolbar, Some(&mut pid)) };
    if pid == 0 {
        warn!("tray toolbar has no owning process; cannot read icons");
        return Vec::new();
    }

    let access =
        PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION;
    let process = match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => handle,
        Err(err) => {
            warn!(pid, error = %err, "OpenProcess on tray host failed; returning no icons");
            return Vec::new();
        }
    };

    let icons = collect_icons(toolbar, process);

    // SAFETY: `process` is a valid handle from OpenProcess; closing once is correct.
    unsafe {
        let _ = CloseHandle(process);
    }
    icons
}

/// Walks the shell window hierarchy to find the toolbar that hosts the icons.
///
/// Primary path: `Shell_TrayWnd` → `TrayNotifyWnd` → `SysPager` → `ToolbarWindow32`.
/// Fallback path: `NotifyIconOverflowWindow` → `ToolbarWindow32`.
fn find_tray_toolbar() -> Option<HWND> {
    // SAFETY: all calls are read-only window lookups; PCWSTR buffers stay alive
    // (in locals) across the call.
    unsafe {
        if let Some(toolbar) = find_nested_toolbar("Shell_TrayWnd", &["TrayNotifyWnd", "SysPager"])
        {
            return Some(toolbar);
        }
        if let Some(toolbar) = find_nested_toolbar("NotifyIconOverflowWindow", &[]) {
            return Some(toolbar);
        }
    }
    None
}

/// Finds `ToolbarWindow32` nested under the given top-level window via the
/// provided intermediate child classes.
///
/// # Safety
/// Caller must ensure window calls are valid (always true for the shell classes).
unsafe fn find_nested_toolbar(top_class: &str, intermediates: &[&str]) -> Option<HWND> {
    let top_cls = wide(top_class);
    // SAFETY: `top_cls` is a NUL-terminated UTF-16 buffer kept alive here.
    let mut current = unsafe { FindWindowW(PCWSTR(top_cls.as_ptr()), PCWSTR::null()) }.ok()?;

    for class in intermediates {
        let cls = wide(class);
        // SAFETY: `current` is a valid HWND from FindWindow*; `cls` stays alive.
        current =
            unsafe { FindWindowExW(Some(current), None, PCWSTR(cls.as_ptr()), PCWSTR::null()) }
                .ok()?;
    }

    let tb_cls = wide("ToolbarWindow32");
    // SAFETY: `current` is valid; `tb_cls` stays alive across the call.
    unsafe { FindWindowExW(Some(current), None, PCWSTR(tb_cls.as_ptr()), PCWSTR::null()).ok() }
}

/// Counts the toolbar buttons and reads each one out of the host process.
fn collect_icons(toolbar: HWND, process: HANDLE) -> Vec<TrayIcon> {
    // SAFETY: `toolbar` is a live toolbar HWND; `process` is a valid handle with
    // VM read/write/operation access obtained by the caller.
    unsafe {
        let mut count: usize = 0;
        let ok = SendMessageTimeoutW(
            toolbar,
            TB_BUTTONCOUNT,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_NORMAL,
            TOOLBAR_TIMEOUT_MS,
            Some(&mut count),
        );
        if ok.0 == 0 {
            warn!("TB_BUTTONCOUNT timed out on tray toolbar");
            return Vec::new();
        }
        if count == 0 {
            debug!("notification area is empty");
            return Vec::new();
        }

        let remote = VirtualAllocEx(
            process,
            None,
            REMOTE_BLOCK,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote.is_null() {
            warn!("VirtualAllocEx inside tray host failed; returning no icons");
            return Vec::new();
        }

        let icons = read_buttons(toolbar, process, remote, count);

        let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
        icons
    }
}

/// Reads `count` `TBBUTTON`s plus their tooltip text via the shared remote block.
///
/// # Safety
/// `remote` must be a live allocation of at least [`REMOTE_BLOCK`] bytes inside
/// `process`, and `process` must have been opened with the VM access rights.
unsafe fn read_buttons(
    toolbar: HWND,
    process: HANDLE,
    remote: *mut c_void,
    count: usize,
) -> Vec<TrayIcon> {
    // Place the tooltip scratch buffer after the TBBUTTON, aligned to 2 bytes.
    let text_remote = {
        let base = remote as usize + size_of::<TBBUTTON>() + 8;
        (base + 1) & !1
    } as *mut c_void;

    let mut icons = Vec::new();
    for index in 0..count {
        let mut button = TBBUTTON::default();
        let mut sent: usize = 0;

        // SAFETY: message is a plain toolbar query; `remote` points into the
        // target process, which the docs require for TB_GETBUTTON.
        let ok = unsafe {
            SendMessageTimeoutW(
                toolbar,
                TB_GETBUTTON,
                WPARAM(index),
                LPARAM(remote as isize),
                SMTO_ABORTIFHUNG | SMTO_NORMAL,
                TOOLBAR_TIMEOUT_MS,
                Some(&mut sent),
            )
        };
        if ok.0 == 0 {
            debug!(index, "TB_GETBUTTON timed out; skipping button");
            continue;
        }

        // SAFETY: `remote` holds a TBBUTTON written by the target; the local
        // buffer is sized exactly `size_of::<TBBUTTON>()`.
        if !unsafe {
            read_exact(
                process,
                remote as *const c_void,
                (&mut button as *mut TBBUTTON).cast::<c_void>(),
                size_of::<TBBUTTON>(),
            )
        } {
            debug!(index, "failed to read TBBUTTON from tray host; skipping");
            continue;
        }

        // SAFETY: `text_remote` is inside the same live remote allocation.
        let tooltip = unsafe { read_tooltip(toolbar, process, text_remote, button.idCommand) };
        // pid cannot be derived reliably from a TBBUTTON (dwData carries the
        // icon id, not a process id), so report unknown.
        icons.push(TrayIcon { tooltip, pid: 0 });
    }
    icons
}

/// Fetches the tooltip text for one button via `TB_GETBUTTONTEXTW`.
///
/// # Safety
/// `text_remote` must be a scratch buffer of at least [`TOOLTIP_CHARS`] UTF-16
/// units inside `process`.
unsafe fn read_tooltip(
    toolbar: HWND,
    process: HANDLE,
    text_remote: *mut c_void,
    command: i32,
) -> String {
    let mut length: usize = 0;
    // SAFETY: standard toolbar query; wParam is the button command id.
    let ok = unsafe {
        SendMessageTimeoutW(
            toolbar,
            TB_GETBUTTONTEXTW,
            WPARAM(command as usize),
            LPARAM(text_remote as isize),
            SMTO_ABORTIFHUNG | SMTO_NORMAL,
            TOOLBAR_TIMEOUT_MS,
            Some(&mut length),
        )
    };
    if ok.0 == 0 || length == 0 {
        return String::new();
    }

    let chars = length.min(TOOLTIP_CHARS - 1);
    let mut buffer = vec![0u16; chars];

    // SAFETY: `text_remote` was written by the target with `length` UTF-16
    // units; `buffer` is sized to the (bounded) character count.
    let read_ok = unsafe {
        read_exact(
            process,
            text_remote as *const c_void,
            buffer.as_mut_ptr().cast::<c_void>(),
            chars * size_of::<u16>(),
        )
    };
    if !read_ok {
        debug!(command, "failed to read tooltip buffer from tray host");
        return String::new();
    }

    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Reads exactly `size` bytes from `address` in `process` into `dst`.
///
/// # Safety
/// `dst` must be valid for writes of `size` bytes; `process` must grant VM read.
unsafe fn read_exact(
    process: HANDLE,
    address: *const c_void,
    dst: *mut c_void,
    size: usize,
) -> bool {
    let mut read: usize = 0;
    // SAFETY: forwarded from the caller's contract above.
    let ok = unsafe { ReadProcessMemory(process, address, dst, size, Some(&mut read)) };
    ok.is_ok() && read == size
}

/// Encodes `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_does_not_panic() {
        // The result may legitimately be empty (headless session, access denied,
        // no tray). The contract under test is only that enumeration never
        // panics or aborts.
        let icons = enumerate();
        eprintln!("tray::enumerate() -> {} icon(s)", icons.len());
        let _ = icons.len();
    }
}
