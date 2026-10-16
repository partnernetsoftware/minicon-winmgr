//! Owned icon snapshots. Window queries run on one bounded background worker;
//! the message thread only reads completed snapshots and paints owned handles.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::{
    CopyImage, DestroyIcon, HICON, IMAGE_ICON, LR_DEFAULTCOLOR,
};
use windows::core::PCWSTR;

pub type Icon = isize;

/// Cache-only lookup: never sends messages to another application.
pub fn for_window(hwnd: HWND) -> SharedIcon {
    crate::window_icon_worker::cached(hwnd)
}

/// Shared ownership keeps a paint snapshot valid across cache refreshes.
#[derive(Clone, Default)]
pub(crate) struct SharedIcon(Option<Arc<OwnedIcon>>);
struct OwnedIcon(Icon);
impl SharedIcon {
    pub(crate) fn copy_window_icon(raw: Icon) -> Self {
        if raw == 0 {
            return Self::default();
        }
        // No LR_COPYRETURNORG: this is an owned 32px snapshot, never the
        // source window's borrowed HICON. Failure degrades to a text initial.
        let copy = unsafe { CopyImage(HANDLE(raw as *mut _), IMAGE_ICON, 32, 32, LR_DEFAULTCOLOR) };
        Self::new(copy.map_or(0, |handle| handle.0 as Icon))
    }
    fn new(raw: Icon) -> Self {
        Self((raw != 0).then(|| Arc::new(OwnedIcon(raw))))
    }
    pub(crate) fn raw(&self) -> Icon {
        self.0.as_ref().map_or(0, |icon| icon.0)
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
fn path_cache() -> &'static Mutex<HashMap<String, SharedIcon>> {
    static CACHE: OnceLock<Mutex<HashMap<String, SharedIcon>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Owned small icon, or an empty icon. The key cache is bounded; pin and paint
/// snapshots retain their own references even after cache entries are dropped.
pub fn for_path(path: &str) -> SharedIcon {
    if path.is_empty() || path.len() > MAX_PATH_KEY_BYTES {
        return SharedIcon::new(0);
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
    let icon = SharedIcon::new(if extracted == 1 { small.0 as Icon } else { 0 });
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
        let pin = SharedIcon::new(raw);
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
