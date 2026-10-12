//! DWM window effects for the taskbar: rounded corners and an optional acrylic
//! backdrop, both best-effort with graceful degradation.
//!
//! - Rounded corners use `DWMWA_WINDOW_CORNER_PREFERENCE` (Windows 11). On older
//!   systems the call fails and corners stay square — no regression.
//! - Acrylic uses `DWMWA_SYSTEMBACKDROP_TYPE` + `DwmExtendFrameIntoClientArea`.
//!   It is opt-in (`MINICON_ACRYLIC=1`) because glass areas must be painted in
//!   pure black to let the backdrop show; if the OS does not composite a
//!   backdrop, that paint would read as black, so we never enable it silently.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_TRANSIENTWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::UI::Controls::MARGINS;

/// Requests rounded window corners. Logs and degrades to square if unsupported.
pub fn apply_rounded_corners(hwnd: HWND) {
    let pref = DWMWCP_ROUND;
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            std::ptr::from_ref(&pref).cast(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        )
    };
    match result {
        Ok(()) => tracing::info!("DWM rounded corners applied"),
        Err(err) => tracing::warn!(%err, "DWM rounded corners unsupported; using square corners"),
    }
}

/// Enables an acrylic (transient-window) backdrop. Returns `true` only when both
/// the backdrop attribute and the frame extension succeed, so callers can fall
/// back to the opaque look otherwise.
pub fn enable_acrylic(hwnd: HWND) -> bool {
    let backdrop = DWMSBT_TRANSIENTWINDOW;
    let backdrop_ok = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            std::ptr::from_ref(&backdrop).cast(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
    }
    .is_ok();

    // -1 margins extend the frame over the whole client area, turning
    // black-painted pixels into backdrop.
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    let frame_ok = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) }.is_ok();

    if backdrop_ok && frame_ok {
        tracing::info!("DWM acrylic backdrop enabled");
        true
    } else {
        tracing::warn!(backdrop_ok, frame_ok, "DWM acrylic backdrop unavailable");
        false
    }
}
