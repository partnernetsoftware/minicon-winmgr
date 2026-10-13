//! Notification-area HOST (replacement mode).
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Only meaningful when *we* are the shell: the host creates the `Shell_TrayWnd`
//! window so that `Shell_NotifyIcon` from other apps delivers their icons to us.
//! It installs automatically when the per-user `Shell` value points at this exe,
//! or when `MINICON_TRAY_HOST=1`; `MINICON_TRAY_HOST=0` force-disables it. In
//! companion mode (another shell owns the tray) it stays inert so we never
//! hijack the real notification area.
//!
//! Wire format (Win10/11 64-bit): apps call `Shell_NotifyIconW`, which posts a
//! `WM_COPYDATA` (`dwData == 1`) carrying a `SHELLTRAYDATA` blob:
//!   offset 0 : `DWORD dwMessage`  (NIM_ADD=0 / NIM_MODIFY=1 / NIM_DELETE=2)
//!   offset 4 : `NOTIFYICONDATAW nid`
//! `NOTIFYICONDATAW` (64-bit) field offsets relative to the blob's offset 4:
//!   0 cbSize, 8 hWnd, 16 uID, 20 uFlags, 24 uCallbackMessage, 32 hIcon,
//!   40 szTip[128] (256 bytes).

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_BROADCAST, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, WM_COPYDATA, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

// `Shell_NotifyIcon` message codes carried in `SHELLTRAYDATA.dwMessage`.
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;

// `WM_COPYDATA.dwData` marker used by the notification-area protocol.
const SHELLTRAYDATA_MARKER: usize = 1;

// ERROR_CLASS_ALREADY_EXISTS.
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

// Number of UTF-16 units in `NOTIFYICONDATAW::szTip`.
const TIP_CHARS: usize = 128;

/// One hosted tray icon.
pub struct TrayIconHandle {
    /// Icon uid as registered by the owning app (NOTIFYICONDATA.uID).
    pub uid: u32,
    /// Owner window handle (as isize).
    pub hwnd: isize,
    /// Callback message the owner expects for tray events.
    pub callback_msg: u32,
    /// Tooltip text (may be empty).
    pub tooltip: String,
    /// Icon handle (as isize).
    pub hicon: isize,
}

/// Fields decoded from one `WM_COPYDATA` notification-area payload.
struct ParsedIcon {
    /// NIM_ADD / NIM_MODIFY / NIM_DELETE.
    dw_message: u32,
    uid: u32,
    hwnd: isize,
    callback_msg: u32,
    hicon: isize,
    tooltip: String,
}

/// The in-process table of hosted icons, keyed by (uid, hwnd).
fn icons_store() -> &'static Mutex<Vec<TrayIconHandle>> {
    static S: OnceLock<Mutex<Vec<TrayIconHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// HWND of the sink window we created, or 0 when the host is not installed.
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

/// Installs the tray host when `MINICON_TRAY_HOST=1`; otherwise a no-op.
///
/// When installed we create a hidden `Shell_TrayWnd` window (falling back to a
/// private `TinyShellTraySink` class if the former cannot be created) so that
/// other processes' `Shell_NotifyIcon` calls reach [`tray_wndproc`]. Every
/// failure path logs and returns gracefully — this never panics.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if !host_requested() {
        tracing::debug!(
            "tray host disabled (not the shell and MINICON_TRAY_HOST unset); staying inert"
        );
        return Ok(());
    }

    // SAFETY: standard Win32 window setup. All `wide` buffers are kept in locals
    // that outlive the calls they feed, and `tray_wndproc` is a valid
    // `extern "system"` function.
    let hwnd = unsafe { create_sink(hinstance) };
    let Some(hwnd) = hwnd else {
        tracing::warn!("tray host window could not be created; continuing without it");
        return Ok(());
    };

    TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    tracing::info!(hwnd = ?hwnd, "tray host sink window created");

    // Tell already-running apps (and ones that start later) to re-register their
    // icons, since the shell may have restarted.
    // SAFETY: pure message registration + a fire-and-forget broadcast.
    unsafe {
        let name = wide("TaskbarCreated");
        let msg = RegisterWindowMessageW(PCWSTR(name.as_ptr()));
        if msg != 0 {
            let _ = PostMessageW(Some(HWND_BROADCAST), msg, WPARAM(0), LPARAM(0));
            tracing::debug!(msg, "broadcast TaskbarCreated");
        }
    }
    Ok(())
}

/// Current hosted icons (empty when the host is not installed).
pub fn icons() -> Vec<TrayIconHandle> {
    let guard = match icons_store().lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!("tray icon table lock poisoned; returning recovered snapshot");
            poisoned.into_inner()
        }
    };
    guard
        .iter()
        .map(|i| TrayIconHandle {
            uid: i.uid,
            hwnd: i.hwnd,
            callback_msg: i.callback_msg,
            tooltip: i.tooltip.clone(),
            hicon: i.hicon,
        })
        .collect()
}

/// Whether we should host the tray:
/// - explicitly opted in via `MINICON_TRAY_HOST=1`, or
/// - we *are* the configured shell (then apps deliver icons to us), auto-enabled
///   so replacement mode works without extra env setup. `MINICON_TRAY_HOST=0`
///   force-disables either way.
fn host_requested() -> bool {
    match std::env::var("MINICON_TRAY_HOST") {
        Ok(v) if v == "1" => true,
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
        _ => crate::shell_registry::is_self_shell(),
    }
}

/// Creates the tray sink window: `Shell_TrayWnd` first, then a private class.
///
/// # Safety
/// Calls Win32 window APIs; all class-name buffers stay alive across the calls.
unsafe fn create_sink(hinstance: HINSTANCE) -> Option<HWND> {
    // SAFETY: forwarded from the caller.
    unsafe {
        let primary = wide("Shell_TrayWnd");
        let empty = wide("");
        register_class(hinstance, PCWSTR(primary.as_ptr()), "Shell_TrayWnd");

        if let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(primary.as_ptr()),
            PCWSTR(empty.as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinstance),
            None,
        ) {
            return Some(hwnd);
        }
        tracing::warn!(
            error = GetLastError().0,
            "CreateWindowEx(Shell_TrayWnd) failed; trying TinyShellTraySink"
        );

        let fallback = wide("TinyShellTraySink");
        register_class(hinstance, PCWSTR(fallback.as_ptr()), "TinyShellTraySink");
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(fallback.as_ptr()),
            PCWSTR(empty.as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinstance),
            None,
        )
        .ok()
    }
}

/// Registers `class_name` with [`tray_wndproc`], tolerating an existing class.
///
/// # Safety
/// `class_name` must be a valid NUL-terminated UTF-16 class-name buffer.
unsafe fn register_class(hinstance: HINSTANCE, class_name: PCWSTR, label: &str) {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(tray_wndproc),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    // SAFETY: `wc` and its class-name buffer outlive the call.
    if unsafe { RegisterClassW(&wc) } == 0 {
        let err = unsafe { GetLastError().0 };
        if err == ERROR_CLASS_ALREADY_EXISTS {
            tracing::debug!(label, "tray window class already exists");
        } else {
            tracing::warn!(label, error = err, "RegisterClassW failed");
        }
    }
}

/// Splits a `SHELLTRAYDATA` blob into its `(dwMessage, NOTIFYICONDATAW)` parts.
///
/// Pure: no Win32 calls, no allocation beyond the decoded tooltip — safe to unit
/// test with synthetic buffers. Returns `None` when the blob is too short to
/// even hold `dwMessage`. Missing trailing fields decode to zero/empty so that
/// the shorter `NIM_DELETE` payloads are still handled.
fn parse_shell_tray_data(raw: &[u8]) -> Option<ParsedIcon> {
    if raw.len() < 4 {
        return None;
    }
    let dw_message = read_u32(raw, 0)?;
    let nid = &raw[4..];
    Some(ParsedIcon {
        dw_message,
        hwnd: read_i64(nid, 8),
        uid: read_u32(nid, 16).unwrap_or(0),
        callback_msg: read_u32(nid, 24).unwrap_or(0),
        hicon: read_i64(nid, 32),
        tooltip: read_tooltip(nid, 40),
    })
}

/// Reads a little-endian `u32` at `off`, or `None` if out of range.
fn read_u32(buf: &[u8], off: usize) -> Option<u32> {
    let bytes = buf.get(off..off + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Reads a little-endian `i64` at `off` as `isize`, defaulting to 0 when absent.
fn read_i64(buf: &[u8], off: usize) -> isize {
    match buf.get(off..off + 8) {
        Some(b) => {
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            i64::from_le_bytes(a) as isize
        }
        None => 0,
    }
}

/// Decodes the NUL-terminated UTF-16 `szTip` at `off` (up to [`TIP_CHARS`]).
fn read_tooltip(buf: &[u8], off: usize) -> String {
    let Some(bytes) = buf.get(off..) else {
        return String::new();
    };
    let take = bytes.len().min(TIP_CHARS * 2);
    let units: Vec<u16> = bytes[..take]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// Applies one decoded notification to the icon table.
fn apply(parsed: ParsedIcon) {
    let mut guard = match icons_store().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    match parsed.dw_message {
        NIM_DELETE => {
            let before = guard.len();
            guard.retain(|i| !(i.uid == parsed.uid && i.hwnd == parsed.hwnd));
            tracing::debug!(
                removed = before - guard.len(),
                uid = parsed.uid,
                "NIM_DELETE"
            );
        }
        NIM_ADD | NIM_MODIFY => {
            let handle = TrayIconHandle {
                uid: parsed.uid,
                hwnd: parsed.hwnd,
                callback_msg: parsed.callback_msg,
                tooltip: parsed.tooltip,
                hicon: parsed.hicon,
            };
            match guard
                .iter_mut()
                .find(|i| i.uid == handle.uid && i.hwnd == handle.hwnd)
            {
                Some(existing) => *existing = handle,
                None => guard.push(handle),
            }
            tracing::debug!(
                uid = parsed.uid,
                total = guard.len(),
                "tray icon registered"
            );
        }
        other => {
            tracing::debug!(other, "ignoring unknown NIM message");
        }
    }
}

/// Window procedure of the hidden tray sink.
unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_COPYDATA {
        // SAFETY: for WM_COPYDATA the OS guarantees `lparam` points to a live
        // `COPYDATASTRUCT` valid for the duration of this call.
        unsafe {
            let cds = lparam.0 as *const COPYDATASTRUCT;
            if !cds.is_null() {
                let cds = &*cds;
                if cds.dwData == SHELLTRAYDATA_MARKER
                    && !cds.lpData.is_null()
                    && cds.cbData as usize >= 4
                {
                    let raw =
                        std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                    if let Some(parsed) = parse_shell_tray_data(raw) {
                        apply(parsed);
                    }
                }
            }
        }
        return LRESULT(0);
    }
    // SAFETY: standard default handling for every other message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic 64-bit `SHELLTRAYDATA` blob:
    /// `dwMessage` (4 bytes) + `NOTIFYICONDATAW`.
    fn blob(dw_message: u32, hwnd: i64, uid: u32, cb_msg: u32, hicon: i64, tip: &str) -> Vec<u8> {
        let mut v = vec![0u8; 4 + 40 + TIP_CHARS * 2];
        v[0..4].copy_from_slice(&dw_message.to_le_bytes());
        let nid = 4;
        // cbSize at nid+0.
        v[nid..nid + 4].copy_from_slice(&(40u32 + 256).to_le_bytes());
        v[nid + 8..nid + 16].copy_from_slice(&hwnd.to_le_bytes());
        v[nid + 16..nid + 20].copy_from_slice(&uid.to_le_bytes());
        v[nid + 20..nid + 24].copy_from_slice(&0x3u32.to_le_bytes()); // uFlags
        v[nid + 24..nid + 28].copy_from_slice(&cb_msg.to_le_bytes());
        v[nid + 32..nid + 40].copy_from_slice(&hicon.to_le_bytes());
        let mut tip_bytes: Vec<u8> = tip.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        tip_bytes.truncate(TIP_CHARS * 2);
        v[nid + 40..nid + 40 + tip_bytes.len()].copy_from_slice(&tip_bytes);
        v
    }

    #[test]
    fn parses_full_add_payload() {
        let raw = blob(
            NIM_ADD,
            0x1122_3344_5566_7788u64 as i64,
            7,
            0x0400,
            0xAA,
            "Hello 世界",
        );
        let p = parse_shell_tray_data(&raw).expect("full blob should parse");
        assert_eq!(p.dw_message, NIM_ADD);
        assert_eq!(p.hwnd, 0x1122_3344_5566_7788u64 as i64 as isize);
        assert_eq!(p.uid, 7);
        assert_eq!(p.callback_msg, 0x0400);
        assert_eq!(p.hicon, 0xAA);
        assert_eq!(p.tooltip, "Hello 世界");
    }

    #[test]
    fn delete_payload_without_tip_still_parses() {
        // Only up to uID is present (typical NIM_DELETE); the rest must default.
        let mut raw = vec![0u8; 4 + 20];
        raw[0..4].copy_from_slice(&NIM_DELETE.to_le_bytes());
        raw[4 + 8..4 + 16].copy_from_slice(&0x1234i64.to_le_bytes());
        raw[4 + 16..4 + 20].copy_from_slice(&9u32.to_le_bytes());

        let p = parse_shell_tray_data(&raw).expect("delete blob should parse");
        assert_eq!(p.dw_message, NIM_DELETE);
        assert_eq!(p.hwnd, 0x1234);
        assert_eq!(p.uid, 9);
        assert_eq!(p.callback_msg, 0);
        assert_eq!(p.hicon, 0);
        assert_eq!(p.tooltip, "");
    }

    #[test]
    fn tooltip_stops_at_first_nul() {
        let mut raw = blob(NIM_ADD, 1, 1, 0, 0, "ab");
        // Plant garbage after the NUL terminator (helper leaves tip+4.. as 0),
        // so the parser must still stop at the NUL right after "ab".
        let tip = 4 + 40;
        let garbage = ('Z' as u16).to_le_bytes();
        raw[tip + 6..tip + 8].copy_from_slice(&garbage);
        let p = parse_shell_tray_data(&raw).unwrap();
        assert_eq!(p.tooltip, "ab");
    }

    #[test]
    fn too_short_blob_is_rejected() {
        assert!(parse_shell_tray_data(&[]).is_none());
        assert!(parse_shell_tray_data(&[0, 0, 0]).is_none());
        // Exactly 4 bytes: dwMessage decodes, everything else defaults to zero.
        let p = parse_shell_tray_data(&[2, 0, 0, 0]).expect("4-byte blob");
        assert_eq!(p.dw_message, NIM_DELETE);
        assert_eq!(p.uid, 0);
        assert_eq!(p.hwnd, 0);
        assert_eq!(p.tooltip, "");
    }

    #[test]
    fn icons_are_empty_when_not_installed() {
        // No install happened in this test process, so the table stays empty.
        assert!(icons().is_empty());
    }
}
