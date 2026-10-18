//! Notification-area **host protocol** (replacement mode only).
//!
//! Only meaningful when *we* are the shell: the host creates the `Shell_TrayWnd`
//! window so that `Shell_NotifyIcon` from other apps delivers their icons to us.
//! It installs automatically when the per-user `Shell` value points at this exe,
//! or when `MINICON_TRAY_HOST=1`; `MINICON_TRAY_HOST=0` force-disables it. In
//! companion mode (another shell owns the tray) it stays inert so we never
//! hijack the real notification area.
//!
//! Observed Windows wire format (including callers using the native x64 API):
//! `WM_COPYDATA(dwData=1)` carries magic `0x34753423`, operation, then a
//! 32-bit marshalled NOTIFYICONDATAW. HWND/HICON on this wire are DWORDs,
//! not native pointers. See the real Shell_NotifyIcon probe in the 0.1.5 plan.
//!
//! This module answers exactly one question — *which icons exist* — and hands
//! them to [`crate::tray`] as [`TrayIconHandle`]s. Geometry, painting and click
//! forwarding live in `tray_bar`; keeping them out of here is what stopped the
//! same cell arithmetic existing in three places (0.1.3 plan §2.4).

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    CopyImage, CreateWindowExW, DefWindowProcW, DestroyIcon, FindWindowW, HICON, HWND_BROADCAST,
    IMAGE_ICON, IsWindow, LR_DEFAULTCOLOR, PostMessageW, RegisterClassW, RegisterWindowMessageW,
    SetTimer, WM_COPYDATA, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::win_util::{ERROR_CLASS_ALREADY_EXISTS, guard_abi, lock_or_recover, wide};

// `Shell_NotifyIcon` message codes carried in `SHELLTRAYDATA.dwMessage`.
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETFOCUS: u32 = 3;
const NIM_SETVERSION: u32 = 4;

// `WM_COPYDATA.dwData` marker used by the notification-area protocol.
const SHELLTRAYDATA_MARKER: usize = 1;
const SHELLTRAYDATA_MAGIC: u32 = 0x3475_3423;

// Number of UTF-16 units in `NOTIFYICONDATAW::szTip`.
const TIP_CHARS: usize = 128;

// Malformed senders must remain diagnosable without flooding the shell log.
fn warn_payload(cb: u32, reason: &str) {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let emit = {
        let mut last = lock_or_recover(LAST.get_or_init(|| Mutex::new(None)));
        if last.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) {
            false
        } else {
            *last = Some(Instant::now());
            true
        }
    };
    if emit {
        tracing::warn!(cb, reason, "tray sink: rejected payload (rate limited)");
    }
}

/// One hosted tray icon: everything a click needs to be forwarded back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrayIconHandle {
    /// Icon uid as registered by the owning app (NOTIFYICONDATA.uID).
    pub(crate) uid: u32,
    /// Owner window handle (as isize).
    pub(crate) hwnd: isize,
    /// Callback message the owner expects for tray events.
    pub(crate) callback_msg: u32,
    /// Tooltip text (may be empty).
    pub(crate) tooltip: String,
    /// Icon handle (as isize). Owned by the **registering process**.
    pub(crate) hicon: isize,
    pub(crate) version: u32,
    pub(crate) hidden: bool,
    pub(crate) guid: Option<[u8; 16]>,
    pub(crate) image: Option<Arc<IconImage>>,
    owner_pid: u32,
}

/// Snapshot references keep a copied icon alive after registration updates/deletes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IconImage(pub(crate) isize);
impl Drop for IconImage {
    fn drop(&mut self) {
        let _ = unsafe { DestroyIcon(HICON(self.0 as *mut _)) };
    }
}
fn copy_icon(raw: isize) -> Option<Arc<IconImage>> {
    if raw == 0 {
        return None;
    }
    let copy =
        unsafe { CopyImage(HANDLE(raw as *mut _), IMAGE_ICON, 0, 0, LR_DEFAULTCOLOR) }.ok()?;
    (!copy.is_invalid()).then(|| Arc::new(IconImage(copy.0 as isize)))
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
    flags: u32,
    state: u32,
    state_mask: u32,
    version: u32,
    guid: Option<[u8; 16]>,
}

/// The in-process table of hosted icons, keyed by (uid, hwnd).
fn icons_store() -> &'static Mutex<Vec<TrayIconHandle>> {
    static S: OnceLock<Mutex<Vec<TrayIconHandle>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// HWND of the sink window we created, or 0 when the host is not installed.
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
#[cfg(test)]
pub(crate) static FAIL_NEXT_INSTALL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether we are currently hosting the notification area.
///
/// This is what decides the strip's whole behaviour: hosting means real icons
/// and forwardable clicks; not hosting means the inert mirrored shape
/// (0.1.3 plan §2.1).
pub(crate) fn is_installed() -> bool {
    TRAY_HWND.load(Ordering::SeqCst) != 0
}

/// Installs the tray host when we are the shell (or `MINICON_TRAY_HOST=1`);
/// otherwise a no-op.
///
/// When installed we create a hidden `Shell_TrayWnd` window (failing if the public
/// host cannot be created) so that
/// other processes' `Shell_NotifyIcon` calls reach [`tray_wndproc`]. Every
/// failure path logs and returns gracefully — this never panics.
pub(crate) fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if is_installed() {
        return Ok(());
    }
    #[cfg(test)]
    if FAIL_NEXT_INSTALL.swap(false, Ordering::SeqCst) {
        return Err("Injected notification host creation failure".into());
    }
    if !host_requested() {
        tracing::debug!(
            "tray host disabled (not the shell and MINICON_TRAY_HOST unset); staying inert"
        );
        return Ok(());
    }
    // Even the diagnostic opt-in must not introduce a second notification
    // host alongside Explorer (or another shell) and redirect its clients.
    if unsafe { FindWindowW(PCWSTR(wide("Shell_TrayWnd").as_ptr()), PCWSTR::null()) }.is_ok() {
        tracing::info!("existing notification host retained; companion mode");
        return Ok(());
    }

    // SAFETY: standard Win32 window setup. All `wide` buffers are kept in locals
    // that outlive the calls they feed, and `tray_wndproc` is a valid
    // `extern "system"` function.
    let hwnd = unsafe { create_sink(hinstance) };
    let Some(hwnd) = hwnd else {
        return Err(format!(
            "Public notification host creation failed (Win32 {})",
            unsafe { GetLastError().0 }
        )
        .into());
    };

    TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    if unsafe { FindWindowW(PCWSTR(wide("Shell_TrayWnd").as_ptr()), PCWSTR::null()) }.ok()
        != Some(hwnd)
    {
        shutdown();
        return Err("The public notification host is not discoverable".into());
    }
    let _ = unsafe { SetTimer(Some(hwnd), 1, 1000, None) };
    tracing::info!(hwnd = ?hwnd, "tray host sink window created");

    // Tell already-running apps (and ones that start later) to re-register their
    // icons, since the shell may have restarted.
    // SAFETY: pure message registration + a fire-and-forget broadcast.
    unsafe {
        let name = wide("TaskbarCreated");
        let msg = RegisterWindowMessageW(PCWSTR(name.as_ptr()));
        if msg != 0 {
            if let Err(error) = PostMessageW(Some(HWND_BROADCAST), msg, WPARAM(0), LPARAM(0)) {
                shutdown();
                return Err(error.into());
            }
            tracing::debug!(msg, "broadcast TaskbarCreated");
        } else {
            shutdown();
            return Err("TaskbarCreated registration failed".into());
        }
    }
    Ok(())
}

pub(crate) fn shutdown() {
    let raw = TRAY_HWND.swap(0, Ordering::SeqCst);
    if raw != 0 {
        let _ =
            unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(HWND(raw as *mut _)) };
    }
    lock_or_recover(icons_store()).clear();
}

/// Current hosted icons (empty when the host is not installed).
pub(crate) fn handles() -> Vec<TrayIconHandle> {
    lock_or_recover(icons_store()).clone()
}

/// Cancel an in-flight input when the icon was removed, replaced or renegotiated
/// while the user held the button. The caller releases this lock before posting.
pub(crate) fn callback_is_current(
    hwnd: isize,
    uid: u32,
    message: u32,
    version: u32,
    pid: u32,
) -> bool {
    lock_or_recover(icons_store()).iter().any(|icon| {
        icon.hwnd == hwnd
            && icon.uid == uid
            && icon.callback_msg == message
            && icon.version == version
            && icon.owner_pid == pid
    })
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

/// Creates the publicly discoverable `Shell_TrayWnd`; no private fallback.
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
        None
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

/// Validates the wire envelope and decodes only the declared marshalled payload.
fn parse_shell_tray_data(raw: &[u8]) -> Option<ParsedIcon> {
    if raw.len() < 8 + 12 || read_u32(raw, 0)? != SHELLTRAYDATA_MAGIC {
        return None;
    }
    let dw_message = read_u32(raw, 4)?;
    let nid = &raw[8..];
    let size = read_u32(nid, 0)? as usize;
    if size < 12 || nid.len() < size {
        return None;
    }
    let nid = &nid[..size];
    Some(ParsedIcon {
        dw_message,
        hwnd: read_u32(nid, 4)? as isize,
        uid: read_u32(nid, 8)?,
        flags: read_u32(nid, 12).unwrap_or(0),
        callback_msg: read_u32(nid, 16).unwrap_or(0),
        hicon: read_u32(nid, 20).unwrap_or(0) as isize,
        tooltip: read_tooltip(nid, 24),
        state: read_u32(nid, 280).unwrap_or(0),
        state_mask: read_u32(nid, 284).unwrap_or(0),
        version: read_u32(nid, 800).unwrap_or(0),
        guid: if read_u32(nid, 12).unwrap_or(0) & 0x20 != 0 {
            Some(nid.get(936..952)?.try_into().ok()?)
        } else {
            None
        },
    })
}

/// Reads a little-endian `u32` at `off`, or `None` if out of range.
fn read_u32(buf: &[u8], off: usize) -> Option<u32> {
    let bytes = buf.get(off..off + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
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
///
/// Any change re-arms the taskbar's debounced refresh, so an icon that appears
/// (or disappears) shows up on the strip within the debounce window instead of
/// waiting for the 30 s heartbeat.
fn apply(parsed: ParsedIcon) -> bool {
    let same = |icon: &TrayIconHandle| match parsed.guid {
        Some(guid) => icon.guid == Some(guid),
        None => icon.uid == parsed.uid && icon.hwnd == parsed.hwnd,
    };
    let mut accepted = true;
    let changed = {
        let mut guard = lock_or_recover(icons_store());
        match parsed.dw_message {
            NIM_DELETE => {
                let before = guard.len();
                guard.retain(|i| !same(i));
                let removed = before - guard.len();
                if removed > 0 {
                    tracing::info!(removed, uid = parsed.uid, "tray icon unregistered");
                }
                accepted = removed > 0;
                removed > 0
            }
            NIM_ADD | NIM_MODIFY => {
                let handle = TrayIconHandle {
                    uid: parsed.uid,
                    hwnd: parsed.hwnd,
                    callback_msg: parsed.callback_msg,
                    tooltip: parsed.tooltip,
                    hicon: parsed.hicon,
                    version: 0,
                    hidden: parsed.flags & 8 != 0 && parsed.state_mask & parsed.state & 1 != 0,
                    guid: parsed.guid,
                    image: if parsed.flags & 2 != 0 {
                        copy_icon(parsed.hicon)
                    } else {
                        None
                    },
                    owner_pid: unsafe { crate::win_util::window_pid(HWND(parsed.hwnd as *mut _)) },
                };
                let changed = match guard.iter_mut().find(|i| same(i)) {
                    Some(existing) => {
                        let handle = TrayIconHandle {
                            hicon: if parsed.flags & 2 != 0 {
                                handle.hicon
                            } else {
                                existing.hicon
                            },
                            callback_msg: if parsed.flags & 1 != 0 {
                                handle.callback_msg
                            } else {
                                existing.callback_msg
                            },
                            tooltip: if parsed.flags & 4 != 0 {
                                handle.tooltip
                            } else {
                                existing.tooltip.clone()
                            },
                            version: existing.version,
                            hidden: if parsed.flags & 8 != 0 && parsed.state_mask & 1 != 0 {
                                handle.hidden
                            } else {
                                existing.hidden
                            },
                            image: if parsed.flags & 2 != 0 {
                                handle.image
                            } else {
                                existing.image.clone()
                            },
                            ..handle
                        };
                        let changed = *existing != handle;
                        *existing = handle;
                        changed
                    }
                    None => {
                        guard.push(handle);
                        true
                    }
                };
                if changed {
                    // INFO, not DEBUG: this is the one line that answers "did
                    // an app actually hand us an icon?" — the question that
                    // otherwise costs a log dive through the whole strip.
                    tracing::debug!(
                        uid = parsed.uid,
                        total = guard.len(),
                        "tray icon registered"
                    );
                }
                changed
            }
            NIM_SETVERSION => {
                if let Some(icon) = guard.iter_mut().find(|i| same(i)) {
                    accepted = parsed.version <= 4;
                    if accepted {
                        icon.version = parsed.version;
                    }
                    accepted
                } else {
                    accepted = false;
                    false
                }
            }
            NIM_SETFOCUS => {
                accepted = guard.iter().any(same);
                false
            }
            other => {
                accepted = false;
                tracing::debug!(other, "ignoring unknown NIM message");
                false
            }
        }
    };
    if changed {
        crate::window_events::request_refresh();
    }
    accepted
}

/// Window procedure of the hidden tray sink.
unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the guard only runs the body and degrades to the default handler
    // on a panic; all handles come straight from Win32.
    guard_abi(
        "tray_host::tray_wndproc",
        || unsafe { tray_wndproc_inner(hwnd, msg, wparam, lparam) },
        || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    )
}

/// Un-guarded window procedure body; only reached through [`tray_wndproc`].
unsafe extern "system" fn tray_wndproc_inner(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TIMER && wparam.0 == 1 {
        let removed = {
            let mut icons = lock_or_recover(icons_store());
            let before = icons.len();
            icons.retain(|icon| {
                let owner = HWND(icon.hwnd as *mut _);
                unsafe {
                    IsWindow(Some(owner)).as_bool()
                        && crate::win_util::window_pid(owner) == icon.owner_pid
                }
            });
            before != icons.len()
        };
        if removed {
            crate::window_events::request_refresh();
        }
        return LRESULT(0);
    }
    if msg == WM_COPYDATA {
        // SAFETY: for WM_COPYDATA the OS guarantees `lparam` points to a live
        // `COPYDATASTRUCT` valid for the duration of this call.
        unsafe {
            let cds = lparam.0 as *const COPYDATASTRUCT;
            if !cds.is_null() {
                let cds = &*cds;
                if cds.dwData != SHELLTRAYDATA_MARKER {
                    tracing::debug!(marker = cds.dwData, "tray sink: unrelated WM_COPYDATA");
                } else if cds.lpData.is_null() || cds.cbData < 4 {
                    warn_payload(cds.cbData, "tray payload too small");
                } else {
                    let raw =
                        std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                    // A payload that reaches us but does not parse used to be
                    // dropped without a trace — indistinguishable from "no app
                    // ever registered", which is the exact ambiguity that ate
                    // a session during the 0.1.3 acceptance run.
                    match parse_shell_tray_data(raw) {
                        Some(parsed) => return LRESULT(apply(parsed) as isize),
                        None => {
                            warn_payload(cds.cbData, "unparseable tray payload");
                        }
                    }
                }
            }
        }
        return LRESULT(0);
    }
    // Nothing else is *expected* here, but shell32 decides whether to accept a
    // `Shell_NotifyIcon` registration by talking to the tray window, and a
    // refused registration is otherwise invisible from our side. At DEBUG so
    // "why did Shell_NotifyIcon return FALSE?" has an answer in the log.
    tracing::debug!(
        msg,
        wparam = wparam.0,
        lparam = lparam.0,
        "tray sink: message"
    );
    // SAFETY: standard default handling for every other message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "private-desktop notification protocol fixture; use isolated_tray_probe.py"]
    fn native_public_host_lifecycle() {
        use super::*;
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::{Shell::*, WindowsAndMessaging::*};
        assert_eq!(
            std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(),
            Ok("1"),
            "Private desktop launcher is required"
        );
        let instance = HINSTANCE(
            unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
                .unwrap()
                .0,
        );
        assert!(
            unsafe { FindWindowW(PCWSTR(wide("Shell_TrayWnd").as_ptr()), PCWSTR::null()) }.is_err()
        );
        install(instance).unwrap();
        assert!(is_installed());
        assert!(handles().is_empty()); // An empty session is ready.
        install(instance).unwrap(); // Idempotent.
        let owner = unsafe {
            CreateWindowExW(
                Default::default(),
                PCWSTR(wide("STATIC").as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .unwrap();
        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: owner,
            uID: 49,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_APP + 211,
            hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap(),
            ..Default::default()
        };
        for (to, from) in icon.szTip.iter_mut().zip("private fixture".encode_utf16()) {
            *to = from;
        }
        assert!(unsafe { Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_ADD, &icon) }.as_bool());
        assert_eq!(handles().len(), 1);
        let item = crate::tray::items().into_iter().next().unwrap();
        assert!(item.is_forwardable());
        assert!(item.image.is_some());
        assert!(crate::tray_bar::forward_at(
            &item,
            crate::tray_bar::MouseEvent::LeftUp,
            POINT { x: 10, y: 10 }
        ));
        let mut message = MSG::default();
        assert!(
            unsafe {
                PeekMessageW(
                    &mut message,
                    Some(owner),
                    icon.uCallbackMessage,
                    icon.uCallbackMessage,
                    PM_REMOVE,
                )
            }
            .as_bool()
        );
        assert_eq!(message.lParam.0 as u32, WM_LBUTTONUP);
        icon.uFlags = NIF_TIP;
        icon.szTip.fill(0);
        for (to, from) in icon.szTip.iter_mut().zip("changed".encode_utf16()) {
            *to = from;
        }
        assert!(
            unsafe { Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_MODIFY, &icon) }.as_bool()
        );
        assert_eq!(handles()[0].tooltip, "changed");
        assert!(
            unsafe { Shell_NotifyIconW(windows::Win32::UI::Shell::NIM_DELETE, &icon) }.as_bool()
        );
        assert!(handles().is_empty());
        assert!(!crate::tray_bar::forward_at(
            &item,
            crate::tray_bar::MouseEvent::LeftUp,
            POINT::default()
        ));
        shutdown();
        assert!(!is_installed());
        assert!(
            unsafe { FindWindowW(PCWSTR(wide("Shell_TrayWnd").as_ptr()), PCWSTR::null()) }.is_err()
        );
        let _ = unsafe { DestroyWindow(owner) };
    }
    use super::*;

    /// Native x64 callers also send DWORD handles in this marshalled envelope.
    fn blob(dw_message: u32, hwnd: i64, uid: u32, cb_msg: u32, hicon: i64, tip: &str) -> Vec<u8> {
        let mut v = vec![0u8; 8 + 956];
        v[0..4].copy_from_slice(&SHELLTRAYDATA_MAGIC.to_le_bytes());
        v[4..8].copy_from_slice(&dw_message.to_le_bytes());
        let nid = 8;
        v[nid..nid + 4].copy_from_slice(&956u32.to_le_bytes());
        v[nid + 4..nid + 8].copy_from_slice(&(hwnd as u32).to_le_bytes());
        v[nid + 8..nid + 12].copy_from_slice(&uid.to_le_bytes());
        v[nid + 12..nid + 16].copy_from_slice(&7u32.to_le_bytes());
        v[nid + 16..nid + 20].copy_from_slice(&cb_msg.to_le_bytes());
        v[nid + 20..nid + 24].copy_from_slice(&(hicon as u32).to_le_bytes());
        let mut tip_bytes: Vec<u8> = tip.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        tip_bytes.truncate(TIP_CHARS * 2);
        v[nid + 24..nid + 24 + tip_bytes.len()].copy_from_slice(&tip_bytes);
        v
    }

    #[test]
    fn parses_full_add_payload() {
        let raw = blob(NIM_ADD, 0x5566_7788, 7, 0x0400, 0xAA, "Hello 世界");
        let p = parse_shell_tray_data(&raw).expect("full blob should parse");
        assert_eq!(p.dw_message, NIM_ADD);
        assert_eq!(p.hwnd, 0x5566_7788);
        assert_eq!(p.uid, 7);
        assert_eq!(p.callback_msg, 0x0400);
        assert_eq!(p.hicon, 0xAA);
        assert_eq!(p.tooltip, "Hello 世界");
    }

    #[test]
    fn delete_payload_without_tip_still_parses() {
        // Only up to uID is present (typical NIM_DELETE); the rest must default.
        let mut raw = blob(NIM_DELETE, 0x1234, 9, 0, 0, "");
        raw.truncate(8 + 12);
        raw[8..12].copy_from_slice(&12u32.to_le_bytes());

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
        let tip = 8 + 24;
        let garbage = ('Z' as u16).to_le_bytes();
        raw[tip + 6..tip + 8].copy_from_slice(&garbage);
        let p = parse_shell_tray_data(&raw).unwrap();
        assert_eq!(p.tooltip, "ab");
    }

    #[test]
    fn too_short_blob_is_rejected() {
        assert!(parse_shell_tray_data(&[]).is_none());
        assert!(parse_shell_tray_data(&[0, 0, 0]).is_none());
        assert!(parse_shell_tray_data(&[2, 0, 0, 0]).is_none());
        let mut truncated = blob(NIM_ADD, 1, 1, 1, 1, "tip");
        truncated.truncate(32);
        assert!(parse_shell_tray_data(&truncated).is_none());
    }

    #[test]
    fn host_is_inactive_without_installation() {
        // Other tests mutate the global table concurrently; only installation
        // state is a meaningful process-wide invariant in this test binary.
        assert!(!is_installed());
    }

    #[test]
    fn applying_an_add_then_a_delete_round_trips_one_icon() {
        let mine = |hs: &[TrayIconHandle]| hs.iter().any(|h| h.uid == 3 && h.hwnd == 0x5555);

        apply(parse_shell_tray_data(&blob(NIM_ADD, 0x5555, 3, 0x0400, 0xBB, "x")).unwrap());
        let added = handles();
        let entry = added
            .iter()
            .find(|h| h.uid == 3 && h.hwnd == 0x5555)
            .expect("NIM_ADD must register the icon");
        assert_eq!(entry.callback_msg, 0x0400);
        assert_eq!(entry.tooltip, "x");
        assert_eq!(entry.hicon, 0xBB);

        // NIM_MODIFY with the same key replaces rather than duplicates.
        apply(parse_shell_tray_data(&blob(NIM_MODIFY, 0x5555, 3, 0x0400, 0xBB, "y")).unwrap());
        let modified = handles();
        assert!(
            modified
                .iter()
                .any(|h| h.uid == 3 && h.hwnd == 0x5555 && h.tooltip == "y"),
            "NIM_MODIFY must update the tooltip in place"
        );
        assert_eq!(
            modified
                .iter()
                .filter(|h| h.uid == 3 && h.hwnd == 0x5555)
                .count(),
            1,
            "modify must not duplicate the icon"
        );

        apply(parse_shell_tray_data(&blob(NIM_DELETE, 0x5555, 3, 0, 0, "")).unwrap());
        assert!(!mine(&handles()), "NIM_DELETE must remove the icon");
    }

    #[test]
    fn an_unknown_nim_message_changes_nothing() {
        apply(parse_shell_tray_data(&blob(99, 0x7777, 5, 0x0400, 0, "z")).unwrap());
        assert!(
            !handles().iter().any(|h| h.uid == 5 && h.hwnd == 0x7777),
            "an unknown dwMessage must be ignored"
        );
    }

    #[test]
    fn partial_updates_preserve_callback_icon_and_negotiated_version() {
        let owner = 0x77119;
        assert!(apply(
            parse_shell_tray_data(&blob(NIM_ADD, owner, 19, 0x8111, 0x33, "before")).unwrap()
        ));
        let mut version = blob(NIM_SETVERSION, owner, 19, 0, 0, "");
        version[8 + 800..8 + 804].copy_from_slice(&4u32.to_le_bytes());
        assert!(apply(parse_shell_tray_data(&version).unwrap()));
        let mut update = blob(NIM_MODIFY, owner, 19, 0, 0, "after");
        update[8 + 12..8 + 16].copy_from_slice(&4u32.to_le_bytes());
        assert!(apply(parse_shell_tray_data(&update).unwrap()));
        let current = handles()
            .into_iter()
            .find(|i| i.hwnd == owner as isize && i.uid == 19)
            .unwrap();
        assert_eq!(
            (current.callback_msg, current.hicon, current.version),
            (0x8111, 0x33, 4)
        );
        assert_eq!(current.tooltip, "after");
        assert!(apply(
            parse_shell_tray_data(&blob(NIM_DELETE, owner, 19, 0, 0, "")).unwrap()
        ));
    }

    #[test]
    fn guid_identity_updates_hidden_state_without_duplicating_or_reordering() {
        let guid = [0x42u8; 16];
        let make = |operation, uid, flags: u32, state| {
            let mut raw = blob(operation, 0x77120, uid, 0x8112, 0, "guid");
            raw[8 + 12..8 + 16].copy_from_slice(&(flags | 0x20u32).to_le_bytes());
            raw[8 + 280..8 + 284].copy_from_slice(&u32::to_le_bytes(state));
            raw[8 + 284..8 + 288].copy_from_slice(&1u32.to_le_bytes());
            raw[8 + 936..8 + 952].copy_from_slice(&guid);
            parse_shell_tray_data(&raw).unwrap()
        };
        assert!(apply(make(NIM_ADD, 20, 7, 0)));
        assert!(apply(make(NIM_MODIFY, 21, 8, 1)));
        let entries = handles();
        let matches: Vec<_> = entries.iter().filter(|i| i.guid == Some(guid)).collect();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].hidden);
        assert_eq!(matches[0].callback_msg, 0x8112);
        assert!(apply(make(NIM_DELETE, 0, 0, 0)));
    }

    #[test]
    fn modify_for_an_unseen_icon_registers_it() {
        // Apps do call NIM_MODIFY for an icon we never saw an ADD for (e.g. they
        // started before us). Treating it as an upsert is what keeps such icons
        // in the strip at all.
        let missing = |hs: &[TrayIconHandle]| !hs.iter().any(|h| h.uid == 77 && h.hwnd == 0x8888);
        assert!(missing(&handles()), "precondition: not registered yet");

        apply(parse_shell_tray_data(&blob(NIM_MODIFY, 0x8888, 77, 0x0400, 0xCC, "late")).unwrap());

        let all = handles();
        let entry = all
            .iter()
            .find(|h| h.uid == 77 && h.hwnd == 0x8888)
            .expect("NIM_MODIFY for an unseen icon must add it");
        assert_eq!(entry.tooltip, "late");

        // Clean up so a rerun of this test starts from the same state.
        apply(parse_shell_tray_data(&blob(NIM_DELETE, 0x8888, 77, 0, 0, "")).unwrap());
        assert!(missing(&handles()));
    }
}
