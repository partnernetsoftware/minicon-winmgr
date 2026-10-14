//! The notification-icon strip: **geometry, painting and click forwarding**.
//!
//! This module owns the single description of where a cell is. The painter, the
//! hit test and the hover test all go through [`Layout`], so a click can never
//! land on a different icon than the one drawn under the cursor — the bug that
//! two independent copies of the same arithmetic used to invite.
//!
//! It knows nothing about *where* icons come from: that is [`crate::tray`]'s
//! job. Icons that arrive without an owner (the mirrored shape, §2.1 of the
//! 0.1.3 plan) are painted as deliberately faint placeholders and
//! [`forward`] refuses to send anything for them.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, HDC};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CopyImage, DI_NORMAL, DestroyIcon, DrawIconEx, GetCursorPos,
    GetWindowThreadProcessId, HICON, IMAGE_ICON, IsWindow, LR_DEFAULTCOLOR, PostMessageW,
};

use crate::tray::TrayItem;
use crate::win_util::{self, Brush};

/// Square cell edge, in logical pixels.
pub(crate) const CELL: i32 = 28;
/// Gap between adjacent cells, in logical pixels.
pub(crate) const GAP: i32 = 2;

/// Width the strip occupies for `count` icons (0 when empty).
///
/// Pure and saturating: never negative, and a negative `gap` is treated as 0 so
/// the width can never disagree with where [`Layout::cell_rect`] puts the cells
/// (a negative gap would otherwise shrink the strip while cells still abut).
pub(crate) fn strip_width(count: usize, cell: i32, gap: i32) -> i32 {
    if count == 0 || cell <= 0 {
        return 0;
    }
    let n = count as i32;
    let gap = gap.max(0);
    n * cell + (n - 1) * gap
}

/// Geometry of the strip: `cell`-px squares, `gap` px apart, vertically centred
/// in `top..top + height`, right-aligned so the last cell ends flush at
/// `right_x` (index 0 is leftmost).
///
/// Pure value type — no Win32 state — so both the layout maths and the hit test
/// are unit-testable directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) top: i32,
    pub(crate) right_x: i32,
    pub(crate) height: i32,
    pub(crate) cell: i32,
    pub(crate) gap: i32,
}

impl Layout {
    /// A layout using the standard square/gap sizes, matching the painted strip.
    pub(crate) const fn new(top: i32, right_x: i32, height: i32) -> Self {
        Self {
            top,
            right_x,
            height,
            cell: CELL,
            gap: GAP,
        }
    }

    /// Width the strip occupies for `count` icons (0 when empty or degenerate).
    pub(crate) fn strip_width(&self, count: usize) -> i32 {
        strip_width(count, self.cell, self.gap)
    }

    /// Rectangle of the cell at `index`, or `None` for an out-of-range index or
    /// a degenerate layout. Arithmetic is done in `i64` and range-checked so a
    /// pathological strip can never overflow/panic.
    pub(crate) fn cell_rect(&self, index: usize, count: usize) -> Option<RECT> {
        if count == 0 || self.cell <= 0 || index >= count {
            return None;
        }
        let cell = self.cell as i64;
        let gap = self.gap.max(0) as i64;
        let width = self.strip_width(count) as i64;
        let left = self.right_x as i64 - width + index as i64 * (cell + gap);
        let top = self.top as i64 + (self.height as i64 - cell) / 2;
        Some(RECT {
            left: i32::try_from(left).ok()?,
            top: i32::try_from(top).ok()?,
            right: i32::try_from(left + cell).ok()?,
            bottom: i32::try_from(top + cell).ok()?,
        })
    }

    /// Index of the cell containing `(x, y)`, or `None` when the point falls in
    /// a gap, outside the strip, or the strip is empty.
    ///
    /// Pure: the coordinate → icon-index mapping, unit-testable on its own.
    pub(crate) fn hit_test(&self, count: usize, x: i32, y: i32) -> Option<usize> {
        (0..count).find(|&i| {
            self.cell_rect(i, count)
                .is_some_and(|r| win_util::rect_contains(r, x, y))
        })
    }
}

/// A mouse action the strip forwards to an icon's owner.
///
/// Exactly the four events a notification icon has to receive, and no more:
/// hover is deliberately **not** forwarded (our strip and the owner's own
/// coordinates have nothing in common, and spamming another process with
/// `WM_MOUSEMOVE` would only confuse it). Right-button down and up are kept
/// apart because some apps open their menu on down, others finalise on up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseEvent {
    LeftDown,
    LeftUp,
    LeftDoubleClick,
    RightDown,
    RightUp,
    KeyboardSelect,
    KeyboardMenu,
}

impl MouseEvent {
    /// The `WM_*` mouse message the owner expects in its callback's `lParam`.
    pub(crate) const fn lparam(self) -> u32 {
        // Literals rather than the `windows` tokens: the values are part of the
        // notify-icon protocol and pinning them keeps a transposed constant
        // from silently sending the wrong button.
        match self {
            MouseEvent::LeftDown => 0x0201, // WM_LBUTTONDOWN
            MouseEvent::LeftUp => 0x0202,
            MouseEvent::LeftDoubleClick => 0x0203, // WM_LBUTTONDBLCLK
            MouseEvent::RightDown => 0x0204,       // WM_RBUTTONDOWN
            MouseEvent::RightUp => 0x0205,         // WM_RBUTTONUP
            MouseEvent::KeyboardSelect => 0x0401,
            MouseEvent::KeyboardMenu => 0x007b,
        }
    }
}

/// Paints `items` using `layout`, returning the width actually used.
///
/// Items that carry an icon handle are drawn from a **copy** of it (see
/// [`draw_owned_icon`]); the rest get a faint placeholder square. Right-to-left:
/// the last entry ends flush against `layout.right_x`.
///
/// # Safety
/// `hdc` must be a valid device context for the duration of the call.
pub(crate) unsafe fn paint(hdc: HDC, layout: &Layout, items: &[TrayItem]) -> i32 {
    let count = items.len();
    if count == 0 {
        return 0;
    }
    let width = layout.strip_width(count);

    // SAFETY: `hdc` is a live device context per the caller's contract. Every
    // brush and icon copy created below is destroyed before this function
    // returns, and neither is selected into the DC (GDI only reads them).
    unsafe {
        for (index, item) in items.iter().enumerate() {
            let Some(rect) = layout.cell_rect(index, count) else {
                continue;
            };
            let inset = ((rect.right - rect.left - 20) / 2).max(0);
            let image_rect = RECT {
                left: rect.left + inset,
                top: rect.top + inset,
                right: rect.right - inset,
                bottom: rect.bottom - inset,
            };
            let icon = item.image.as_ref().map_or(item.hicon, |image| image.0);
            draw_real_icon(hdc, image_rect, icon, index).unwrap_or_else(|| {
                fill_placeholder(hdc, image_rect, item.pid);
            });
        }
    }

    width
}

/// Draws a foreign icon by first copying it into this process.
///
/// The handle belongs to the process that registered the icon. Touching it
/// beyond `DrawIconEx` would be a correctness bug: destroying it would remove
/// the icon from the owning app's own tray. So the copy is what gets destroyed,
/// and the original is only ever read.
///
/// Returns `false` when there is nothing to draw, so the caller can fall back to
/// the placeholder.
///
/// # Safety
/// `hdc` must be a valid device context; `hicon` is untrusted and may be stale.
unsafe fn draw_real_icon(hdc: HDC, rect: RECT, hicon: isize, index: usize) -> Option<()> {
    if hicon == 0 {
        return None;
    }
    let size = (rect.right - rect.left).min(rect.bottom - rect.top).max(0);
    if size == 0 {
        return None;
    }

    // SAFETY: `CopyImage` only reads the source handle and returns a new handle
    // owned by this process.
    let copy = match unsafe {
        CopyImage(
            HANDLE(hicon as *mut c_void),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    } {
        Ok(copy) => copy,
        Err(err) => {
            tracing::debug!(index, %err, "tray icon copy failed; drawing the placeholder instead");
            return None;
        }
    };
    if copy.0.is_null() {
        tracing::debug!(
            index,
            "tray icon copy came back null; drawing the placeholder instead"
        );
        return None;
    }

    // SAFETY: `copy` is a live icon handle we own; `hdc` is live per contract.
    let _ = unsafe {
        DrawIconEx(
            hdc,
            rect.left,
            rect.top,
            HICON(copy.0),
            size,
            size,
            0,
            None,
            DI_NORMAL,
        )
    };
    // SAFETY: our own copy, no longer referenced.
    let _ = unsafe { DestroyIcon(HICON(copy.0)) };
    Some(())
}

/// Fills one cell with the muted placeholder for `pid`.
///
/// # Safety
/// `hdc` must be a valid device context.
unsafe fn fill_placeholder(hdc: HDC, rect: RECT, pid: u32) {
    // SAFETY: the brush is owned by the returned guard and destroyed on drop;
    // `FillRect` does not keep it.
    if let Some(brush) = unsafe { Brush::solid(placeholder_color(pid)) } {
        // SAFETY: `hdc` is live per contract; the brush is valid for the call.
        unsafe {
            FillRect(hdc, &rect, brush.handle());
        }
    }
}

/// Muted placeholder colour derived from a process id.
///
/// The palette is desaturated **and** blended most of the way towards the bar
/// background, which is deliberate: this colour is only ever used for icons we
/// could not get a real handle for, and the product decision (0.1.3 plan §2.1)
/// is that those cells must read as inert rather than impersonate a real icon.
/// The modulo keeps the mapping stable per pid, so the same process always gets
/// the same hue.
fn placeholder_color(pid: u32) -> COLORREF {
    const PALETTE: [(u8, u8, u8); 6] = [
        (0x54, 0x5C, 0x6A), // slate
        (0x6A, 0x5C, 0x54), // taupe
        (0x54, 0x6A, 0x5C), // sage
        (0x5C, 0x54, 0x6A), // mauve
        (0x6A, 0x54, 0x5C), // wine
        (0x54, 0x5F, 0x6A), // steel
    ];
    /// How much of the bar background is mixed in, in percent.
    const BG_MIX: u32 = 62;

    let (r, g, b) = PALETTE[(pid % PALETTE.len() as u32) as usize];
    // `Theme` tokens are `COLORREF`-style `0x00BBGGRR` values.
    let bg = crate::config::theme().bg;
    let mix = |c: u8, bg: u8| ((c as u32 * (100 - BG_MIX) + bg as u32 * BG_MIX) / 100) as u8;
    let r = mix(r, (bg & 0xFF) as u8);
    let g = mix(g, ((bg >> 8) & 0xFF) as u8);
    let b = mix(b, ((bg >> 16) & 0xFF) as u8);
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Posts `event` back to `item`'s owner, as if the real notification icon had
/// been clicked.
///
/// Returns whether a message was delivered. Items without an owner (the
/// mirrored shape) and owners whose window has gone away are logged no-ops —
/// never a panic, never a modal error.
pub(crate) fn forward(item: &TrayItem, event: MouseEvent) -> bool {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    forward_at(item, event, point)
}

pub(crate) fn forward_at(item: &TrayItem, event: MouseEvent, point: POINT) -> bool {
    let Some(owner) = item.owner else {
        tracing::debug!("tray strip is in its inert shape; click not forwarded");
        return false;
    };
    // Pre-NOTIFYICON_VERSION clients expect the legacy right-button pair for
    // keyboard activation/menu requests, rather than NIN_KEYSELECT/CONTEXTMENU.
    if owner.version < 3 && matches!(event, MouseEvent::KeyboardSelect | MouseEvent::KeyboardMenu) {
        let down = forward_at(item, MouseEvent::RightDown, point);
        let up = forward_at(item, MouseEvent::RightUp, point);
        return down && up;
    }

    // SAFETY: `HWND` is a plain handle wrapper, and a stale handle is exactly
    // what `IsWindow` exists to reject.
    let hwnd = HWND(owner.hwnd as *mut c_void);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        tracing::debug!(
            hwnd = owner.hwnd,
            "tray icon owner window is gone; dropping forward"
        );
        return false;
    }

    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid != item.pid {
        return false;
    }
    // The user explicitly selected this app's notification icon. Permit its
    // posted callback to activate a menu without forcing focus back to us.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
    let Some((wparam, lparam)) = callback(owner.uid, owner.version, event, point.x, point.y) else {
        return true;
    };
    // SAFETY: `hwnd` was just confirmed live; message/wparams are plain values.
    // `PostMessageW` is asynchronous, so a hung owner cannot stall our bar.
    match unsafe {
        PostMessageW(
            Some(hwnd),
            owner.callback_msg,
            WPARAM(wparam),
            LPARAM(lparam),
        )
    } {
        Ok(()) => {
            tracing::debug!(
                hwnd = owner.hwnd,
                uid = owner.uid,
                msg = owner.callback_msg,
                event = ?event,
                "forwarded tray input to the icon's owner"
            );
            true
        }
        Err(err) => {
            tracing::debug!(
                hwnd = owner.hwnd,
                uid = owner.uid,
                %err,
                "PostMessageW to the tray icon owner failed; dropping forward"
            );
            false
        }
    }
}

fn callback(uid: u32, version: u32, event: MouseEvent, x: i32, y: i32) -> Option<(usize, isize)> {
    if version >= 4 {
        let notification = match event {
            MouseEvent::LeftDown | MouseEvent::RightDown => return None,
            MouseEvent::LeftUp => 0x0400,
            MouseEvent::RightUp | MouseEvent::KeyboardMenu => 0x007b,
            other => other.lparam(),
        };
        Some((
            ((x as u16 as u32) | ((y as u16 as u32) << 16)) as usize,
            (notification | ((uid & 0xffff) << 16)) as isize,
        ))
    } else {
        Some((uid as usize, event.lparam() as isize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::TrayOwner;

    fn item(owner: Option<TrayOwner>) -> TrayItem {
        TrayItem {
            tooltip: String::from("tip"),
            pid: 0,
            hicon: 0,
            owner,
            hidden: false,
            image: None,
        }
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(strip_width(0, 18, 2), 0);
    }

    #[test]
    fn version_four_callbacks_pack_identity_and_signed_screen_coordinates() {
        let (point, event) = callback(0x1234, 4, MouseEvent::RightUp, -120, -30).unwrap();
        assert_eq!(point as u16 as i16, -120);
        assert_eq!((point >> 16) as u16 as i16, -30);
        assert_eq!(event as u32, 0x1234_007b);
        assert_eq!(callback(7, 4, MouseEvent::LeftDown, 0, 0), None);
        assert_eq!(
            callback(7, 4, MouseEvent::LeftUp, 0, 0),
            Some((0, 0x0007_0400))
        );
        assert_eq!(callback(7, 0, MouseEvent::LeftUp, 0, 0), Some((7, 0x0202)));
    }

    #[test]
    fn single_cell_has_no_gaps() {
        assert_eq!(strip_width(1, 18, 2), 18);
    }

    #[test]
    fn gaps_between_cells_only() {
        assert_eq!(strip_width(3, 18, 2), 3 * 18 + 2 * 2);
    }

    #[test]
    fn non_positive_cell_is_zero() {
        assert_eq!(strip_width(4, 0, 2), 0);
    }

    #[test]
    fn layout_width_matches_the_free_function() {
        let l = Layout::new(0, 100, 36);
        assert_eq!(l.strip_width(0), 0);
        assert_eq!(l.strip_width(1), CELL);
        assert_eq!(l.strip_width(3), 3 * CELL + 2 * GAP);
        assert_eq!(l.strip_width(3), strip_width(3, CELL, GAP));
    }

    #[test]
    fn three_cells_are_right_aligned_and_evenly_spaced() {
        // Three 28px hit cells with 2px gaps, flush at right_x.
        let l = Layout::new(0, 100, 36);
        let r0 = l.cell_rect(0, 3).unwrap();
        let r1 = l.cell_rect(1, 3).unwrap();
        let r2 = l.cell_rect(2, 3).unwrap();
        assert_eq!((r0.left, r0.right), (12, 40));
        assert_eq!((r1.left, r1.right), (42, 70));
        assert_eq!((r2.left, r2.right), (72, 100));
        assert_eq!(l.strip_width(3), 88);
    }

    #[test]
    fn cells_are_vertically_centred() {
        // Height 36, cell 28: 4px above and below.
        let l = Layout::new(4, 100, 36);
        let r = l.cell_rect(0, 1).unwrap();
        assert_eq!((r.top, r.bottom), (8, 36));
    }

    #[test]
    fn placeholder_colour_is_stable_and_palette_bounded() {
        // Same pid -> same colour; six distinct buckets within the palette.
        assert_eq!(placeholder_color(42), placeholder_color(42));
        assert_eq!(placeholder_color(6), placeholder_color(0));
        assert_ne!(placeholder_color(0), placeholder_color(1));
    }

    // --- hit testing -----------------------------------------------------

    #[test]
    fn hit_test_is_right_aligned_and_skips_gaps() {
        // Three 28px cells ending flush at x=100.
        let l = Layout::new(0, 100, 36);
        assert_eq!(l.hit_test(3, 20, 18), Some(0));
        assert_eq!(l.hit_test(3, 50, 18), Some(1));
        assert_eq!(l.hit_test(3, 90, 18), Some(2));
        // A gap column between the first and second cells.
        assert_eq!(l.hit_test(3, 41, 18), None);
        // Left of the strip.
        assert_eq!(l.hit_test(3, 11, 18), None);
        // Right edge is exclusive.
        assert_eq!(l.hit_test(3, 100, 18), None);
    }

    #[test]
    fn hit_test_respects_the_vertical_band() {
        let l = Layout::new(4, 100, 36);
        // Cell 28 centred in 36 with origin 4: rows [8, 36).
        assert_eq!(l.hit_test(1, 90, 7), None);
        assert_eq!(l.hit_test(1, 90, 8), Some(0));
        assert_eq!(l.hit_test(1, 90, 35), Some(0));
        assert_eq!(l.hit_test(1, 90, 36), None);
    }

    #[test]
    fn hit_test_edge_cases_never_match() {
        let l = Layout::new(0, 100, 36);
        assert_eq!(l.hit_test(0, 50, 18), None); // empty strip
        let degenerate = Layout {
            cell: 0,
            ..Layout::new(0, 100, 36)
        };
        assert_eq!(degenerate.hit_test(3, 50, 18), None); // zero cell
    }

    #[test]
    fn cell_rect_rejects_out_of_range_and_handles_overflow() {
        let l = Layout::new(0, 100, 36);
        assert!(l.cell_rect(3, 3).is_none());
        assert!(l.cell_rect(0, 0).is_none());
        // A strip anchored at i32::MIN underflows the left edge for 2 cells;
        // the i64 maths + range check must yield None, not a panic.
        let under = Layout::new(0, i32::MIN, 36);
        assert!(under.cell_rect(0, 2).is_none());
    }

    #[test]
    fn geometry_and_hit_test_agree_at_every_cell_centre() {
        // The invariant the whole strip hangs on: the icon a click resolves to is
        // the icon drawn under the cursor. Checking every cell centre (and the
        // row just above/below the band) at several strip sizes pins it.
        for count in 1..=8usize {
            let l = Layout::new(7, 1_500, 36);
            for index in 0..count {
                let r = l.cell_rect(index, count).expect("cell in range");
                let cx = (r.left + r.right) / 2;
                let cy = (r.top + r.bottom) / 2;
                assert_eq!(l.hit_test(count, cx, cy), Some(index));
                // Every pixel of the cell belongs to that cell.
                assert_eq!(l.hit_test(count, r.left, r.top), Some(index));
                assert_eq!(l.hit_test(count, r.right - 1, r.bottom - 1), Some(index));
                // And the gap columns resolve to nobody.
                if index + 1 < count {
                    assert_eq!(l.hit_test(count, r.right, cy), None, "gap column");
                }
            }
        }
    }

    #[test]
    fn a_negative_gap_is_treated_as_zero() {
        // `gap` comes from a constant today, but a negative value must not make
        // cells overlap (or the hit test ambiguous).
        let l = Layout {
            gap: -4,
            ..Layout::new(0, 100, 36)
        };
        assert_eq!(l.strip_width(3), 3 * CELL);
        let r0 = l.cell_rect(0, 3).unwrap();
        let r1 = l.cell_rect(1, 3).unwrap();
        assert_eq!(r1.left, r0.right, "cells abut, they do not overlap");
    }

    // --- forwarding ------------------------------------------------------

    #[test]
    fn mouse_events_map_to_the_documented_lparams() {
        // Pin the literal values: a transposed token here would silently send
        // the wrong button and no owner menu would ever appear.
        assert_eq!(MouseEvent::LeftDown.lparam(), 0x0201);
        assert_eq!(MouseEvent::LeftDoubleClick.lparam(), 0x0203);
        assert_eq!(MouseEvent::RightDown.lparam(), 0x0204);
        assert_eq!(MouseEvent::RightUp.lparam(), 0x0205);
        // Down and up must stay distinct — apps opening on down and finishing
        // on up need the two separate messages.
        assert_ne!(MouseEvent::RightDown.lparam(), MouseEvent::RightUp.lparam());
    }

    #[test]
    fn forward_never_touches_an_inert_item() {
        // The product guarantee of the mirrored shape: no owner, no message.
        for event in [
            MouseEvent::LeftDown,
            MouseEvent::LeftDoubleClick,
            MouseEvent::RightDown,
            MouseEvent::RightUp,
        ] {
            assert!(!forward(&item(None), event), "{event:?} must not forward");
        }
    }

    #[test]
    fn forward_drops_a_dead_owner_window() {
        // This handle is not a window, so `IsWindow` rejects it and the forward
        // is a logged no-op rather than a post into the void.
        let stale = item(Some(TrayOwner {
            hwnd: 0x1234,
            uid: 7,
            callback_msg: 0x0400,
            version: 0,
        }));
        assert!(!forward(&stale, MouseEvent::LeftDown));
    }
}
