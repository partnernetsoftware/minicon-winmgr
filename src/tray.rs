//! Notification-area **data source**.
//!
//! One model, two runtime shapes (see `docs/plan_0_1_3.md` §2.1):
//!
//! * **replacement mode** (we are the shell): other processes deliver their
//!   icons to our own `Shell_TrayWnd` sink (`tray_host`), so we hold the owner
//!   triple and clicks can be *forwarded*. The data is pushed to us; there are
//!   no cross-process reads at all.
//! * **companion mode** (another shell owns the tray): all we can do is read
//!   the neighbour's toolbar. That yields tooltips but no owner window and no
//!   icon handle, so those items are painted as inert placeholders and can
//!   never be forwarded.
//!
//! The mirror read is the expensive one (cross-process, two `SendMessageTimeout`
//! calls per icon against a process we do not control), so it is **sampled on
//! the heartbeat into a cache** and never runs on the bar's repaint path. See
//! [`sample_mirror`].
//!
//! Geometry, painting and forwarding deliberately live elsewhere
//! (`tray_bar`): this module answers *what* is in the notification area, not
//! where it goes or what a click does.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;
use std::sync::OnceLock;

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

use crate::win_util::{lock_or_recover, wide};

/// Per-call timeout for cross-process toolbar messages (ms).
///
/// Deliberately small. This runs against a process we do not control, and the
/// read is `TB_BUTTONCOUNT` plus two messages per icon; at the old 2000 ms a
/// hung explorer could stall our message thread for seconds. 250 ms is plenty
/// for a healthy shell and keeps us responsive when it is not.
const TOOLBAR_TIMEOUT_MS: u32 = 250;
/// Size of the scratch region we map inside the tray host process.
const REMOTE_BLOCK: usize = 4_096;
/// Upper bound on tooltip characters we will read back.
const TOOLTIP_CHARS: usize = 512;

/// The owner triple a forwarded click has to be posted back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayOwner {
    /// Owner window handle, as `isize` (belongs to *another* process).
    pub hwnd: isize,
    /// Icon uid as registered by the owning app (`NOTIFYICONDATA.uID`).
    pub uid: u32,
    /// Callback message the owner expects for tray events.
    pub callback_msg: u32,
}

/// One icon in the notification area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// Tooltip text (`szTip`); may be empty.
    pub tooltip: String,
    /// Owning process id when known (0 = unknown). Only used to pick the
    /// placeholder colour on the degraded path.
    pub pid: u32,
    /// Icon handle **owned by another process** (0 = none). Never destroyed
    /// here: the painter copies it into this process first.
    pub hicon: isize,
    /// Present iff a click on this icon can reach the app that registered it.
    /// Always `None` on the mirror path.
    pub owner: Option<TrayOwner>,
}

impl TrayItem {
    /// Whether a click on this icon can be forwarded to its owner.
    pub const fn is_forwardable(&self) -> bool {
        self.owner.is_some()
    }
}

/// The strip's contents: hosted icons when we own the tray, else the mirror.
///
/// Hosted icons are read on demand because the host pushes them asynchronously;
/// mirror icons come from the cache [`sample_mirror`] fills. Cheap either way —
/// no cross-process work happens here.
pub fn items() -> Vec<TrayItem> {
    if crate::tray_host::is_installed() {
        return crate::tray_host::handles()
            .into_iter()
            .map(from_handle)
            .collect();
    }
    lock_or_recover(mirror_cache()).clone()
}

/// Converts a hosted registration into the strip's model.
fn from_handle(handle: crate::tray_host::TrayIconHandle) -> TrayItem {
    // A registration without an owner window or callback message can never be
    // clicked back, so it is modelled as non-forwardable rather than trusted.
    let owner = (handle.hwnd != 0 && handle.callback_msg != 0).then_some(TrayOwner {
        hwnd: handle.hwnd,
        uid: handle.uid,
        callback_msg: handle.callback_msg,
    });
    let pid = if handle.hwnd == 0 {
        0
    } else {
        // SAFETY: `handle.hwnd` came from the registering app; reading its pid
        // is a read-only query that tolerates a stale handle (returns 0).
        unsafe { crate::win_util::window_pid(HWND(handle.hwnd as *mut c_void)) }
    };
    TrayItem {
        tooltip: handle.tooltip,
        pid,
        hicon: handle.hicon,
        owner,
    }
}

/// Cached mirror sample (message thread only, like every other store here).
fn mirror_cache() -> &'static Mutex<Vec<TrayItem>> {
    static C: OnceLock<Mutex<Vec<TrayItem>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// Re-reads the neighbouring shell's notification area into the cache.
///
/// Call this from the heartbeat, never from a repaint. In replacement mode it
/// is a no-op: we own the tray, so the mirror could only ever show the
/// neighbour's icons (there is no neighbour) and the read would be pure cost.
pub fn sample_mirror() {
    if crate::tray_host::is_installed() {
        return;
    }
    let items = read_mirror();
    debug!(
        count = items.len(),
        "sampled the mirrored notification area"
    );
    *lock_or_recover(mirror_cache()) = items;
}

/// Reads the current notification-area icons off the neighbour shell.
fn read_mirror() -> Vec<TrayItem> {
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
fn collect_icons(toolbar: HWND, process: HANDLE) -> Vec<TrayItem> {
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
) -> Vec<TrayItem> {
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
        // A mirrored icon is inert: `dwData` carries the icon id, not the
        // owner window, so there is nothing to forward a click to — and the
        // toolbar's image list is not readable without doubling the
        // cross-process surface, so there is no handle to draw either.
        icons.push(TrayItem {
            tooltip,
            pid: 0,
            hicon: 0,
            owner: None,
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mirror_does_not_panic() {
        // The result may legitimately be empty (headless session, access denied,
        // no tray). The contract under test is only that reading the neighbour's
        // notification area never panics or aborts.
        let icons = read_mirror();
        eprintln!("tray::read_mirror() -> {} icon(s)", icons.len());
        assert!(
            icons.iter().all(|i| i.owner.is_none()),
            "mirror items are inert"
        );
    }

    #[test]
    fn mirror_items_are_never_forwardable() {
        // The whole "won't hijack, won't mis-forward" guarantee rests on this:
        // every mirror item carries no owner, so a click can only ever be a
        // logged no-op.
        for item in read_mirror() {
            assert!(!item.is_forwardable(), "mirror item claims an owner");
            assert_eq!(item.hicon, 0, "mirror item claims an icon handle");
        }
    }

    #[test]
    fn items_falls_back_to_the_mirror_when_the_host_is_absent() {
        // No host was installed in this test process, so `items()` must serve
        // the (possibly empty) mirror cache rather than panicking or reaching
        // for hosted state that does not exist.
        assert!(!crate::tray_host::is_installed());
        let items = items();
        assert!(items.iter().all(|i| !i.is_forwardable()));
    }

    #[test]
    fn is_forwardable_reflects_the_owner() {
        let mut item = TrayItem {
            tooltip: String::new(),
            pid: 0,
            hicon: 0,
            owner: None,
        };
        assert!(!item.is_forwardable());
        item.owner = Some(TrayOwner {
            hwnd: 0x1234,
            uid: 1,
            callback_msg: 0x0400,
        });
        assert!(item.is_forwardable());
    }
}
