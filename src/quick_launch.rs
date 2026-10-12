//! Quick-launch buttons for the left of the taskbar.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Pins are the most-used Start Menu entries (from `crate::recents`), capped at
//! [`MAX_PINS`]; an empty history yields no pins. Geometry is pure and
//! unit-tested; the taskbar owns the HDC and hit-testing.

use windows::Win32::Foundation::RECT;

/// Square button edge, in logical pixels.
pub const CELL: i32 = 26;
/// Gap between adjacent buttons, in logical pixels.
pub const GAP: i32 = 4;
/// Maximum number of pinned buttons.
pub const MAX_PINS: usize = 5;

/// One pinned quick-launch entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// Display label (shortcut file stem).
    pub label: String,
    /// Target path passed to `crate::launcher::launch`.
    pub path: String,
}

/// Resolves the current pins: the most-used Start Menu shortcuts, capped at
/// [`MAX_PINS`]. Empty when there is no usage history.
pub fn pins() -> Vec<Pin> {
    let stats = crate::recents::load();
    if stats.is_empty() {
        return Vec::new();
    }
    let items = crate::launcher::discover_shortcuts();
    crate::recents::rank(&items, &stats, MAX_PINS)
        .into_iter()
        .map(|s| Pin {
            label: s.name.clone(),
            path: s.path.clone(),
        })
        .collect()
}

/// Left-to-right button rects for `n` buttons starting at `left`, vertically
/// centred within a bar of `bar_h`. Returns empty for `n == 0`.
pub fn layout(left: i32, n: usize, cell: i32, gap: i32, bar_h: i32) -> Vec<RECT> {
    if n == 0 || cell <= 0 {
        return Vec::new();
    }
    let top = (bar_h - cell) / 2;
    (0..n)
        .map(|i| {
            let x = left + i as i32 * (cell + gap);
            RECT {
                left: x,
                top,
                right: x + cell,
                bottom: top + cell,
            }
        })
        .collect()
}

/// Index of the button containing `(x, y)`, if any.
pub fn hit(rects: &[RECT], x: i32, y: i32) -> Option<usize> {
    rects
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

/// Launches the shortcut at `path`, logging the outcome.
pub fn launch_path(path: &str) {
    tracing::info!(path, "quick-launch: launching pin");
    if let Err(err) = crate::launcher::launch(path) {
        tracing::warn!(path, error = %err, "quick-launch: launch failed");
    }
}

#[cfg(test)]
mod tests {
    use super::{GAP, MAX_PINS, hit, layout};

    #[test]
    fn empty_has_no_rects() {
        assert!(layout(0, 0, 26, GAP, 36).is_empty());
    }

    #[test]
    fn non_positive_cell_has_no_rects() {
        assert!(layout(0, 3, 0, GAP, 36).is_empty());
    }

    #[test]
    fn rects_flow_left_to_right_with_gap() {
        let rects = layout(10, 3, 26, 4, 36);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].left, 10);
        assert_eq!(rects[1].left, 10 + 30);
        assert_eq!(rects[2].left, 10 + 60);
        for r in &rects {
            assert_eq!(r.right, r.left + 26);
        }
    }

    #[test]
    fn rects_are_vertically_centred() {
        let rects = layout(0, 2, 26, GAP, 36);
        // (36 - 26) / 2 == 5
        for r in &rects {
            assert_eq!(r.top, 5);
            assert_eq!(r.bottom, 31);
        }
    }

    #[test]
    fn hit_reports_containing_index() {
        let rects = layout(0, 3, 26, GAP, 36);
        // Centre of the second cell.
        assert_eq!(hit(&rects, 30 + 13, 5 + 13), Some(1));
        // Left edge of the first cell is inside; its right edge is not, and the
        // slot between two cells (the gap) belongs to neither.
        assert_eq!(hit(&rects, 0, 5), Some(0));
        assert_eq!(hit(&rects, 26, 5), None);
        assert_eq!(hit(&rects, 30, 5), Some(1));
    }

    #[test]
    fn hit_misses_outside() {
        let rects = layout(0, 3, 26, GAP, 36);
        assert_eq!(hit(&rects, -1, 5), None);
        assert_eq!(hit(&rects, 0, 4), None); // above the bar
        assert_eq!(hit(&rects, 10_000, 5), None);
        assert_eq!(hit(&[], 0, 0), None);
    }

    #[test]
    fn max_pins_is_positive() {
        const { assert!(MAX_PINS > 0) };
    }
}
