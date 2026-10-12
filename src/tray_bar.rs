//! Tray icon strip rendered in the taskbar's right zone.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! The taskbar's paint path calls [`paint`] to draw one square cell per
//! mirrored notification icon, right-aligned, and [`strip_width`] to reserve
//! horizontal space before the clock. Geometry lives here (pure, unit-tested);
//! the taskbar owns the HDC and the icon list.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HGDIOBJ,
};

use crate::tray::TrayIcon;

/// Square cell edge, in logical pixels.
pub const CELL: i32 = 18;
/// Gap between adjacent cells, in logical pixels.
pub const GAP: i32 = 2;

/// Width the strip occupies for `count` icons (0 when empty).
///
/// Pure and saturating: never negative.
pub fn strip_width(count: usize, cell: i32, gap: i32) -> i32 {
    if count == 0 || cell <= 0 {
        return 0;
    }
    let n = count as i32;
    n * cell + (n - 1) * gap
}

/// Rectangle for the cell at `index` (0 = leftmost) within a strip of `count`
/// cells right-aligned ending at `right_x`, vertically centred in `top..top +
/// height`.
///
/// Pure geometry: no Win32 calls, fully unit-testable. `cell` is the square
/// edge and `gap` the space between neighbours.
fn cell_rect(
    index: usize,
    count: usize,
    top: i32,
    right_x: i32,
    height: i32,
    cell: i32,
    gap: i32,
) -> RECT {
    let width = strip_width(count, cell, gap);
    let left = right_x - width + index as i32 * (cell + gap);
    let y = top + (height - cell) / 2;
    RECT {
        left,
        top: y,
        right: left + cell,
        bottom: y + cell,
    }
}

/// Muted placeholder colour derived from a process id.
///
/// The palette is deliberately desaturated so the strip reads as a quiet,
/// text-first surface (matching the bar's muted accent language). The modulo
/// keeps the mapping stable per pid, so the same process always gets the same
/// hue — enough to tell multiple icons apart at a glance until real icon
/// bitmaps are available.
fn pid_color(pid: u32) -> COLORREF {
    const PALETTE: [(u8, u8, u8); 6] = [
        (0x54, 0x5C, 0x6A), // slate
        (0x6A, 0x5C, 0x54), // taupe
        (0x54, 0x6A, 0x5C), // sage
        (0x5C, 0x54, 0x6A), // mauve
        (0x6A, 0x54, 0x5C), // wine
        (0x54, 0x5F, 0x6A), // steel
    ];
    let (r, g, b) = PALETTE[(pid % PALETTE.len() as u32) as usize];
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Paints `icons` right-aligned ending at `right_x`, vertically centred in a
/// band of `height` starting at `top`. Returns the width actually used.
///
/// Each icon is drawn as a flat placeholder square (we have no bitmaps yet),
/// coloured by [`pid_color`]. Right-to-left: the last entry ends flush against
/// `right_x`.
///
/// SAFETY: `hdc` must be a valid device context for the duration of the call.
pub unsafe fn paint(hdc: HDC, top: i32, right_x: i32, height: i32, icons: &[TrayIcon]) -> i32 {
    let count = icons.len();
    if count == 0 {
        return 0;
    }
    let width = strip_width(count, CELL, GAP);

    // SAFETY: `hdc` is a live device context per the caller's contract. Every
    // brush created below is selected only transiently by `FillRect` (never
    // into the DC) and deleted before we move on, so no GDI handle is leaked
    // and no brush is in use when deleted.
    unsafe {
        for (index, icon) in icons.iter().enumerate() {
            let rect = cell_rect(index, count, top, right_x, height, CELL, GAP);
            let brush: HBRUSH = CreateSolidBrush(pid_color(icon.pid));
            FillRect(hdc, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }

    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(strip_width(0, 18, 2), 0);
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
    fn three_cells_are_right_aligned_and_evenly_spaced() {
        // count=3, cell=18, gap=2 -> width 58; last cell ends flush at right_x.
        let r0 = cell_rect(0, 3, 0, 100, 36, 18, 2);
        let r1 = cell_rect(1, 3, 0, 100, 36, 18, 2);
        let r2 = cell_rect(2, 3, 0, 100, 36, 18, 2);
        assert_eq!((r0.left, r0.right), (42, 60));
        assert_eq!((r1.left, r1.right), (62, 80));
        assert_eq!((r2.left, r2.right), (82, 100));
        assert_eq!(strip_width(3, 18, 2), 58);
    }

    #[test]
    fn cells_are_vertically_centred() {
        // height 36, cell 18 -> 9px above and below.
        let r = cell_rect(0, 1, 4, 100, 36, 18, 2);
        assert_eq!((r.top, r.bottom), (4 + 9, 4 + 9 + 18));
    }

    #[test]
    fn colour_is_stable_and_palette_bounded() {
        // Same pid -> same colour; six distinct buckets within the palette.
        assert_eq!(pid_color(42), pid_color(42));
        assert_eq!(pid_color(6), pid_color(0));
        assert_ne!(pid_color(0), pid_color(1));
    }
}
