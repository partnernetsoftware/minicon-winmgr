//! Window-icon lookup, so a task button can show *which application* it is
//! before the user reads a single character of the title.
//!
//! `WM_GETICON` is the cheap path; windows that answer nothing fall back to the
//! icon the class registered. Both are handles the window or its class owns, so
//! we never destroy them — we only cache the numbers and hand them to GDI for
//! drawing during a `WM_PAINT`.
//!
//! Icon handles live in the target process's address space as far as we are
//! concerned: sending `WM_GETICON` is a cross-process call, so it is bounded by
//! a short timeout and must not run while holding the taskbar locks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, HICON, ICON_BIG, ICON_SMALL,
    ICON_SMALL2, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETICON,
};
use windows::core::PCWSTR;

/// An opaque `HICON` as a plain integer; `0` means "no icon available".
pub type Icon = isize;

/// How long we wait for a window to answer `WM_GETICON`, in milliseconds.
/// A window that is hung must never stall the bar's paint path.
const ICON_TIMEOUT_MS: u32 = 50;

fn cache() -> &'static Mutex<HashMap<isize, Icon>> {
    static CACHE: OnceLock<Mutex<HashMap<isize, Icon>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Forgets every cached handle.
///
/// Called at the top of each taskbar refresh: window handles are recycled by
/// the system, so a stale entry would put one application's icon on another
/// application's button.
pub fn begin_refresh() {
    crate::win_util::lock_or_recover(cache()).clear();
}

/// Sends `WM_GETICON` with a bounded wait, returning `0` on failure.
fn send_icon(hwnd: HWND, which: usize) -> Icon {
    let mut result: usize = 0;
    // SAFETY: `result` outlives the call, and the timeout bounds a wedged
    // target so the message thread can never block on it.
    let lr = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_GETICON,
            WPARAM(which),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            ICON_TIMEOUT_MS,
            Some(&mut result as *mut usize),
        )
    };
    if lr.0 == 0 { 0 } else { result as Icon }
}

/// Returns the icon handle for `hwnd`, cached. `0` means "draw no icon".
pub fn for_window(hwnd: HWND) -> Icon {
    let key = hwnd.0 as isize;
    if let Some(hit) = crate::win_util::lock_or_recover(cache()).get(&key) {
        return *hit;
    }
    // Prefer the small icon (16px, what the bar actually draws), then the
    // legacy small slot, and only then let GDI downscale the big one.
    let mut icon = send_icon(hwnd, ICON_SMALL2 as usize);
    if icon == 0 {
        icon = send_icon(hwnd, ICON_SMALL as usize);
    }
    if icon == 0 {
        icon = send_icon(hwnd, ICON_BIG as usize);
    }
    if icon == 0 {
        // Many windows never answer WM_GETICON but still register a class icon.
        // SAFETY: `GetClassLongPtrW` only reads; a wrong handle yields 0.
        unsafe {
            icon = GetClassLongPtrW(hwnd, GCLP_HICONSM) as Icon;
            if icon == 0 {
                icon = GetClassLongPtrW(hwnd, GCLP_HICON) as Icon;
            }
        }
    }
    crate::win_util::lock_or_recover(cache()).insert(key, icon);
    icon
}

/// Shared ownership keeps a paint snapshot valid across cache refreshes.
#[derive(Clone)]
pub(crate) struct PathIcon(Arc<OwnedIcon>);
struct OwnedIcon(Icon);
impl PathIcon {
    fn new(raw: Icon) -> Self {
        Self(Arc::new(OwnedIcon(raw)))
    }
    pub(crate) fn raw(&self) -> Icon {
        self.0.0
    }
}
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                let _ = DestroyIcon(HICON(self.0 as *mut _));
            }
        }
    }
}
const MAX_PATH_ICONS: usize = 64;
const MAX_PATH_KEY_BYTES: usize = 256 * 1024;
fn path_cache() -> &'static Mutex<HashMap<String, PathIcon>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathIcon>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Owned small icon, or an empty icon. The key cache is bounded; pin and paint
/// snapshots retain their own references even after cache entries are dropped.
pub fn for_path(path: &str) -> PathIcon {
    if path.is_empty() || path.len() > MAX_PATH_KEY_BYTES {
        return PathIcon::new(0);
    }
    if let Some(hit) = crate::win_util::lock_or_recover(path_cache())
        .get(path)
        .cloned()
    {
        return hit;
    }
    let file = crate::win_util::wide(path);
    let mut small = HICON(std::ptr::null_mut());
    let extracted = unsafe { ExtractIconExW(PCWSTR(file.as_ptr()), 0, None, Some(&mut small), 1) };
    let icon = PathIcon::new(if extracted == 1 { small.0 as Icon } else { 0 });
    let mut cache = crate::win_util::lock_or_recover(path_cache());
    if cache.len() >= MAX_PATH_ICONS
        || cache.keys().map(String::len).sum::<usize>() + path.len() > MAX_PATH_KEY_BYTES
    {
        cache.clear();
    }
    cache.insert(path.to_owned(), icon.clone());
    icon
}

/// Drop cache references, not icons still owned by a pin or paint snapshot.
pub fn clear_path_icons() {
    crate::win_util::lock_or_recover(path_cache()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "isolated native resource experiment; run resources layer"]
    fn owned_path_icon_outlives_cache_eviction() {
        use windows::Win32::Graphics::Gdi::DeleteObject;
        use windows::Win32::UI::WindowsAndMessaging::{
            CopyIcon, GetIconInfo, ICONINFO, IDI_APPLICATION, LoadIconW,
        };
        let shared = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap() };
        let raw = unsafe { CopyIcon(shared).unwrap() }.0 as Icon;
        let pin = PathIcon::new(raw);
        crate::win_util::lock_or_recover(path_cache())
            .insert("isolated-fixture".into(), pin.clone());
        clear_path_icons();
        let mut info = ICONINFO::default();
        unsafe {
            assert!(GetIconInfo(HICON(raw as *mut _), &mut info).is_ok());
            if !info.hbmColor.is_invalid() {
                let _ = DeleteObject(info.hbmColor.into());
            }
            if !info.hbmMask.is_invalid() {
                let _ = DeleteObject(info.hbmMask.into());
            }
        }
        drop(pin);
        assert!(unsafe { GetIconInfo(HICON(raw as *mut _), &mut ICONINFO::default()) }.is_err());
    }
}
