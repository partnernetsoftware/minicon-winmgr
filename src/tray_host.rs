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
//!
//! Click forwarding: for each registered icon we remember the owner triple
//! `(hWnd, uCallbackMessage, uID)`. When a click lands on one of our strip
//! cells we post the matching mouse message back to `hWnd` with
//! `wParam = uID` and `lParam = <mouse message>` (the documented notify-icon
//! callback shape), so the owning app reacts as if the real tray was clicked.
//! The coordinate → icon and icon+event → message mappings are pure functions
//! (see [`StripLayout::hit_test`] and [`forward_target`]) so they stay testable
//! without touching a live window; the only effectful step is the guarded
//! `PostMessageW` in [`forward_at`].

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_BROADCAST, IsWindow, PostMessageW, RegisterClassW,
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

// Mouse messages posted back in the notify-icon callback's `lParam`. Defined
// locally so we never need a new `windows` feature for them.
//
// `allow(dead_code)`: the whole click-forwarding surface below is the wiring
// API the taskbar calls once it routes strip clicks here; until then it has no
// in-crate caller.
#[allow(dead_code)]
const WM_MOUSEMOVE: u32 = 0x0200;
#[allow(dead_code)]
const WM_LBUTTONDOWN: u32 = 0x0201;
#[allow(dead_code)]
const WM_LBUTTONDBLCLK: u32 = 0x0203;
#[allow(dead_code)]
const WM_RBUTTONDOWN: u32 = 0x0204;

// Cell geometry defaults for [`StripLayout`], matching `tray_bar`.
//
// `allow(dead_code)`: `StripLayout::new` is the entry point the taskbar will
// call once the strip is painted from the *hosted* icons; until that display
// wiring lands nothing in the crate builds a layout, so the defaults read as
// unused. See `docs/prd_1_1.md` (tray click forwarding).
#[allow(dead_code)]
const HIT_CELL: i32 = 18;
#[allow(dead_code)]
const HIT_GAP: i32 = 2;

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

/// One cell of a horizontal icon strip, in client/logical pixels.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl CellRect {
    /// Half-open containment: `right`/`bottom` edges are exclusive.
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// Geometry of the tray strip: `cell`-px squares, `gap` px apart, vertically
/// centred in `top..top + height`, right-aligned so the last cell ends flush at
/// `right_x` (index 0 is leftmost). Mirrors what the taskbar paints.
///
/// Pure value type — no Win32 state — so both the layout maths and the hit test
/// can be unit-tested directly.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripLayout {
    pub top: i32,
    pub right_x: i32,
    pub height: i32,
    pub cell: i32,
    pub gap: i32,
}

impl StripLayout {
    /// A layout using the standard square/gap sizes, matching the painted strip.
    #[allow(dead_code)]
    pub const fn new(top: i32, right_x: i32, height: i32) -> Self {
        Self {
            top,
            right_x,
            height,
            cell: HIT_CELL,
            gap: HIT_GAP,
        }
    }

    /// Width the strip occupies for `count` icons (0 when empty or degenerate).
    ///
    /// Saturating against overflow: returns 0 instead of wrapping.
    pub fn strip_width(&self, count: usize) -> i32 {
        if count == 0 || self.cell <= 0 {
            return 0;
        }
        let n = count as i64;
        let cell = self.cell as i64;
        let gap = self.gap.max(0) as i64;
        to_i32(n * cell + (n - 1) * gap).unwrap_or(0)
    }

    /// Rectangle of the cell at `index`, or `None` for an out-of-range index or
    /// a degenerate layout. Arithmetic is done in `i64` and range-checked so a
    /// pathological strip can never overflow/panic.
    pub fn cell_rect(&self, index: usize, count: usize) -> Option<CellRect> {
        if count == 0 || self.cell <= 0 || index >= count {
            return None;
        }
        let cell = self.cell as i64;
        let gap = self.gap.max(0) as i64;
        let width = self.strip_width(count) as i64;
        let left = self.right_x as i64 - width + index as i64 * (cell + gap);
        let top = self.top as i64 + (self.height as i64 - cell) / 2;
        Some(CellRect {
            left: to_i32(left)?,
            top: to_i32(top)?,
            right: to_i32(left + cell)?,
            bottom: to_i32(top + cell)?,
        })
    }

    /// Index of the cell containing `(x, y)`, or `None` when the point falls in
    /// a gap, outside the strip, or the strip is empty.
    ///
    /// Pure: the coordinate → icon-index mapping, unit-testable on its own.
    pub fn hit_test(&self, count: usize, x: i32, y: i32) -> Option<usize> {
        (0..count).find(|&i| self.cell_rect(i, count).is_some_and(|r| r.contains(x, y)))
    }
}

/// Narrows an `i64` to `i32`, or `None` when it would not fit.
#[allow(dead_code)]
fn to_i32(v: i64) -> Option<i32> {
    i32::try_from(v).ok()
}

/// A mouse action to forward to a hosted icon's owner.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Move,
    LeftDown,
    LeftDoubleClick,
    RightDown,
}

impl MouseEvent {
    /// The `WM_*` mouse message the owner expects in the callback's `lParam`.
    pub const fn lparam(self) -> u32 {
        match self {
            MouseEvent::Move => WM_MOUSEMOVE,
            MouseEvent::LeftDown => WM_LBUTTONDOWN,
            MouseEvent::LeftDoubleClick => WM_LBUTTONDBLCLK,
            MouseEvent::RightDown => WM_RBUTTONDOWN,
        }
    }
}

/// The four-tuple posted back to the icon's owning window.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForwardTarget {
    pub hwnd: isize,
    pub msg: u32,
    pub wparam: usize,
    pub lparam: isize,
}

/// Pure mapping: an icon + mouse event → the message to post
/// (`None` when the icon lacks a usable `hWnd`/callback message, meaning the
/// caller should skip it).
#[allow(dead_code)]
pub fn forward_target(icon: &TrayIconHandle, event: MouseEvent) -> Option<ForwardTarget> {
    if icon.hwnd == 0 || icon.callback_msg == 0 {
        return None;
    }
    Some(ForwardTarget {
        hwnd: icon.hwnd,
        msg: icon.callback_msg,
        wparam: icon.uid as usize,
        lparam: event.lparam() as isize,
    })
}

/// Snapshot of an icon's forwarding triple, captured under the store lock so we
/// never hold the lock across an `IsWindow`/`PostMessageW` call.
#[allow(dead_code)]
fn forward_snapshot() -> Vec<TrayIconHandle> {
    let guard = match icons_store().lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!("tray icon table lock poisoned; forwarding from recovered snapshot");
            poisoned.into_inner()
        }
    };
    guard
        .iter()
        .map(|i| TrayIconHandle {
            uid: i.uid,
            hwnd: i.hwnd,
            callback_msg: i.callback_msg,
            tooltip: String::new(),
            hicon: 0,
        })
        .collect()
}

/// Posts `event` for `snapshot[index]`. `true` iff a message was delivered.
/// Validates the owner window first; never panics.
#[allow(dead_code)]
fn forward_from(snapshot: &[TrayIconHandle], event: MouseEvent, index: usize) -> bool {
    let Some(icon) = snapshot.get(index) else {
        tracing::debug!(index, "tray click outside the hosted icon range; ignoring");
        return false;
    };
    let Some(target) = forward_target(icon, event) else {
        tracing::debug!(
            uid = icon.uid,
            "hosted icon has no hWnd/callback message; skipping forward"
        );
        return false;
    };
    post_target(target, event)
}

/// The effectful step: verify `target.hwnd` is a live window and post the
/// callback. Any invalid handle or post failure is logged and skipped.
#[allow(dead_code)]
fn post_target(target: ForwardTarget, event: MouseEvent) -> bool {
    // SAFETY: `HWND` is a plain handle wrapper; a stale handle is exactly what
    // `IsWindow` is designed to reject.
    let hwnd = HWND(target.hwnd as *mut core::ffi::c_void);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        tracing::debug!(
            hwnd = target.hwnd,
            "hosted icon owner window is gone; dropping forward"
        );
        return false;
    }
    // SAFETY: `hwnd` was just confirmed live; the message/wparams are plain
    // values. `PostMessageW` is asynchronous and never blocks the message pump.
    match unsafe {
        PostMessageW(
            Some(hwnd),
            target.msg,
            WPARAM(target.wparam),
            LPARAM(target.lparam),
        )
    } {
        Ok(()) => {
            tracing::debug!(
                hwnd = target.hwnd,
                msg = target.msg,
                uid = target.wparam,
                lparam = target.lparam,
                event = ?event,
                "forwarded tray input to icon owner"
            );
            true
        }
        Err(err) => {
            tracing::debug!(
                hwnd = target.hwnd,
                msg = target.msg,
                error = %err,
                "PostMessageW to icon owner failed; dropping forward"
            );
            false
        }
    }
}

/// Hit-tests `(x, y)` against `layout` and forwards the matching mouse event to
/// the icon's owner. Returns `true` iff a message was posted.
///
/// Best-effort: the index is resolved against the registration order of hosted
/// icons, so a strip that paints them in a different order (or with different
/// geometry) may mis-target. Every miss is a logged no-op, never a panic.
#[allow(dead_code)]
pub fn forward_at(event: MouseEvent, layout: StripLayout, x: i32, y: i32) -> bool {
    let snapshot = forward_snapshot();
    match layout.hit_test(snapshot.len(), x, y) {
        Some(index) => forward_from(&snapshot, event, index),
        None => false,
    }
}

/// Forwards `event` directly to the icon at `index` (callers that already did
/// their own hit test). Returns `true` iff a message was posted.
#[allow(dead_code)]
pub fn forward_to(event: MouseEvent, index: usize) -> bool {
    let snapshot = forward_snapshot();
    forward_from(&snapshot, event, index)
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

    // --- click-forwarding pure logic -------------------------------------

    fn icon(uid: u32, hwnd: isize, cb: u32) -> TrayIconHandle {
        TrayIconHandle {
            uid,
            hwnd,
            callback_msg: cb,
            tooltip: String::new(),
            hicon: 0,
        }
    }

    #[test]
    fn mouse_event_maps_to_the_documented_lparam() {
        assert_eq!(MouseEvent::Move.lparam(), WM_MOUSEMOVE);
        assert_eq!(MouseEvent::LeftDown.lparam(), WM_LBUTTONDOWN);
        assert_eq!(MouseEvent::LeftDoubleClick.lparam(), WM_LBUTTONDBLCLK);
        assert_eq!(MouseEvent::RightDown.lparam(), WM_RBUTTONDOWN);
    }

    #[test]
    fn forward_target_packs_the_triple() {
        let ic = icon(7, 0x1234, 0x0400);
        let t = forward_target(&ic, MouseEvent::LeftDown).expect("valid triple");
        assert_eq!(t.hwnd, 0x1234);
        assert_eq!(t.msg, 0x0400);
        assert_eq!(t.wparam, 7);
        assert_eq!(t.lparam, WM_LBUTTONDOWN as isize);

        // Event only changes lParam, never the owner triple.
        let dbl = forward_target(&ic, MouseEvent::LeftDoubleClick).unwrap();
        assert_eq!(dbl.hwnd, t.hwnd);
        assert_eq!(dbl.msg, t.msg);
        assert_eq!(dbl.wparam, t.wparam);
        assert_eq!(dbl.lparam, WM_LBUTTONDBLCLK as isize);
    }

    #[test]
    fn forward_target_skips_icons_without_owner_data() {
        // Missing hWnd, missing callback message, or both => no forward.
        assert!(forward_target(&icon(1, 0, 0x0400), MouseEvent::LeftDown).is_none());
        assert!(forward_target(&icon(1, 0x99, 0), MouseEvent::RightDown).is_none());
        assert!(forward_target(&icon(1, 0, 0), MouseEvent::Move).is_none());
    }

    #[test]
    fn strip_width_matches_the_painted_strip() {
        let l = StripLayout::new(0, 100, 36);
        assert_eq!(l.strip_width(0), 0);
        assert_eq!(l.strip_width(1), HIT_CELL);
        assert_eq!(l.strip_width(3), 3 * HIT_CELL + 2 * HIT_GAP);
    }

    #[test]
    fn hit_test_is_right_aligned_and_skips_gaps() {
        // 3 cells of 18 + 2 gaps of 2 = 58 wide, ending flush at x=100.
        let l = StripLayout::new(0, 100, 36);
        // Cell centres: index 0 -> ~[42,60), 1 -> [62,80), 2 -> [82,100).
        assert_eq!(l.hit_test(3, 50, 18), Some(0));
        assert_eq!(l.hit_test(3, 70, 18), Some(1));
        assert_eq!(l.hit_test(3, 90, 18), Some(2));
        // A gap column (between 60 and 62) hits nothing.
        assert_eq!(l.hit_test(3, 61, 18), None);
        // Left of the strip.
        assert_eq!(l.hit_test(3, 41, 18), None);
        // Right edge is exclusive.
        assert_eq!(l.hit_test(3, 100, 18), None);
    }

    #[test]
    fn hit_test_respects_the_vertical_band() {
        let l = StripLayout::new(4, 100, 36);
        // cell 18 centred in 36 -> rows [13, 31).
        assert_eq!(l.hit_test(1, 90, 12), None);
        assert_eq!(l.hit_test(1, 90, 13), Some(0));
        assert_eq!(l.hit_test(1, 90, 30), Some(0));
        assert_eq!(l.hit_test(1, 90, 31), None);
    }

    #[test]
    fn hit_test_edge_cases_never_match() {
        let l = StripLayout::new(0, 100, 36);
        assert_eq!(l.hit_test(0, 50, 18), None); // empty strip
        let degenerate = StripLayout {
            cell: 0,
            ..StripLayout::new(0, 100, 36)
        };
        assert_eq!(degenerate.hit_test(3, 50, 18), None); // zero cell
    }

    #[test]
    fn cell_rect_rejects_out_of_range_and_handles_overflow() {
        let l = StripLayout::new(0, 100, 36);
        assert!(l.cell_rect(3, 3).is_none());
        // A strip anchored at i32::MIN underflows the left edge for 2 cells;
        // the i64 maths + range check must yield None, not a panic.
        let under = StripLayout::new(0, i32::MIN, 36);
        assert!(under.cell_rect(0, 2).is_none());
    }

    #[test]
    fn forward_to_skips_invalid_owner_and_out_of_range() {
        // No icons registered, and index 0 is out of range => false, no panic.
        assert!(!forward_to(MouseEvent::LeftDown, 0));
        // A layout hit can never turn into a post while the table is empty.
        let l = StripLayout::new(0, 100, 36);
        assert!(!forward_at(MouseEvent::LeftDown, l, 50, 18));
    }
}
