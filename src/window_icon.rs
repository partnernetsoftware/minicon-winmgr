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
use std::sync::{Mutex, OnceLock};

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

/// Icons extracted from executable paths, keyed by path. Unlike the window
/// cache these handles are **ours** (the shell hands out a fresh copy), so
/// [`clear_path_icons`] must destroy them.
fn path_cache() -> &'static Mutex<HashMap<String, Icon>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Icon>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns a small icon for the executable at `path`, cached. `0` means "draw
/// no icon" (the file is missing, is not an executable, or has no icon).
pub fn for_path(path: &str) -> Icon {
    if path.is_empty() {
        return 0;
    }
    if let Some(hit) = crate::win_util::lock_or_recover(path_cache()).get(path) {
        return *hit;
    }
    let file = crate::win_util::wide(path);
    let mut small = HICON(std::ptr::null_mut());
    // SAFETY: `small` outlives the call; asking only for the small slot. A
    // missing or icon-less file simply extracts nothing.
    let extracted = unsafe { ExtractIconExW(PCWSTR(file.as_ptr()), 0, None, Some(&mut small), 1) };
    let icon = if extracted > 0 { small.0 as Icon } else { 0 };
    crate::win_util::lock_or_recover(path_cache()).insert(path.to_owned(), icon);
    icon
}

/// Frees every cached path icon and clears the cache.
///
/// Called at the top of each taskbar refresh: these handles are ours, so
/// dropping the map without destroying them would leak one icon per pin per
/// refresh. Handles from the window cache are not touched — we never own those.
pub fn clear_path_icons() {
    let mut cache = crate::win_util::lock_or_recover(path_cache());
    for icon in cache.drain().map(|(_, icon)| icon) {
        if icon != 0 {
            // SAFETY: the handle came from `ExtractIconExW` and is only ever
            // passed to `DrawIconEx` (never selected into a DC), so it is safe
            // to free.
            unsafe {
                let _ = DestroyIcon(HICON(icon as *mut core::ffi::c_void));
            }
        }
    }
}
