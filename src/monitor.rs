//! Monitor enumeration (work areas) for multi-monitor support.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::core::BOOL;

/// `MONITORINFOF_PRIMARY` from `winuser.h` (not re-exported by the crate).
const MONITORINFOF_PRIMARY: u32 = 0x1;

/// A monitor's work area (excludes taskbars), full bounds, and primary flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Full monitor rectangle `(x, y, w, h)` including any taskbars — used to
    /// anchor our bar flush to the physical screen edge.
    pub bounds: (i32, i32, i32, i32),
    pub primary: bool,
}

/// Enumerates all monitors with their work areas, primary first.
pub fn enumerate() -> Vec<MonitorInfo> {
    let mut out: Vec<MonitorInfo> = Vec::new();
    // SAFETY: `enum_proc` only pushes into the Vec behind this pointer, and the
    // Vec outlives the synchronous EnumDisplayMonitors call.
    let lparam = LPARAM(&mut out as *mut Vec<MonitorInfo> as isize);
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), lparam);
    }
    out.sort_by_key(|m| !m.primary); // primary first
    out
}

unsafe extern "system" fn enum_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: `lparam` was set by `enumerate` to a valid `*mut Vec<MonitorInfo>`.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let work = info.rcWork;
        let bounds = info.rcMonitor;
        out.push(MonitorInfo {
            x: work.left,
            y: work.top,
            width: work.right - work.left,
            height: work.bottom - work.top,
            bounds: (
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
            ),
            primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    BOOL(1) // keep enumerating
}

#[cfg(test)]
mod tests {
    use super::enumerate;

    #[test]
    fn reports_at_least_one_monitor_with_one_primary() {
        let monitors = enumerate();
        assert!(!monitors.is_empty(), "expected at least one monitor");
        assert_eq!(
            monitors.iter().filter(|m| m.primary).count(),
            1,
            "exactly one monitor should be primary"
        );
        assert!(
            monitors.iter().all(|m| m.width > 0 && m.height > 0),
            "work areas should have positive size"
        );
        assert!(monitors[0].primary, "primary monitor should sort first");
    }
}
