//! Overflow / group popup: a menu listing every taskbar group.
//!
//! Opened from the taskbar's `»` overflow button and from a grouped task
//! button. Shows a `TrackPopupMenu` with one item per group and returns the
//! chosen window handle (None if dismissed).
//!
//! The old dedicated "window-list" button (≡) was retired in v0.1.2; this
//! module survived it because the two entry points above still need a list.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

/// Rows per column before the popup wraps into a new column.
///
/// A single `TrackPopupMenu` column that runs past the screen bottom is a known
/// failure mode with 30+ windows. Wrapping into columns at a fixed row count
/// keeps the popup on screen without any custom scrolling surface. 24 rows at
/// the default menu item height is comfortably inside a 1080p work area.
pub const PER_COLUMN: usize = 24;

/// Whether entry `i` starts a new (vertically separated) column. Pure, so the
/// wrap points are unit-tested.
fn starts_new_column(i: usize, per_col: usize) -> bool {
    per_col > 0 && i > 0 && i.is_multiple_of(per_col)
}

/// One row in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Group label (title, with `(+N)` when grouped).
    pub label: String,
    /// Window to activate when the row is chosen.
    pub hwnd: isize,
}

/// Shows the list popup anchored under `anchor`; returns the chosen hwnd.
pub fn popup(anchor: HWND, entries: &[Entry]) -> Option<isize> {
    if entries.is_empty() {
        return None;
    }

    // Rows are 1-based menu ids, matching `entries`; the wrap points come from
    // `starts_new_column`. Labels are widened (and copied) inside `popup_menu`.
    let rows: Vec<crate::win_util::MenuRow> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            // Wrap into a fresh column every `PER_COLUMN` rows so a long list
            // never runs off the bottom of the screen.
            if starts_new_column(i, PER_COLUMN) {
                crate::win_util::MenuRow::first_in_column(i + 1, &entry.label)
            } else {
                crate::win_util::MenuRow::command(i + 1, &entry.label)
            }
        })
        .collect();

    // Prefer the cursor; fall back to the bottom-left of the anchor.
    // SAFETY: `anchor` is a live window owned by this process (the taskbar).
    let fallback = unsafe {
        let mut rc = RECT::default();
        GetWindowRect(anchor, &mut rc).ok().map(|()| rc)
    };
    // SAFETY: `anchor` is our live bar window; `popup_menu` owns the menu's
    // whole lifetime (create → append → foreground → track → destroy).
    let id = unsafe {
        let (x, y) = crate::win_util::cursor_pos_or(fallback);
        crate::win_util::popup_menu(anchor, &rows, x, y)
    };

    id.and_then(|id| entries.get(id - 1).map(|entry| entry.hwnd))
}

#[cfg(test)]
mod tests {
    use super::{PER_COLUMN, starts_new_column};

    #[test]
    fn first_column_never_breaks() {
        // The very first row must not start with a column break.
        assert!(!starts_new_column(0, PER_COLUMN));
        for i in 1..PER_COLUMN {
            assert!(!starts_new_column(i, PER_COLUMN));
        }
    }

    #[test]
    fn breaks_land_on_every_column_boundary() {
        assert!(starts_new_column(PER_COLUMN, PER_COLUMN));
        assert!(starts_new_column(PER_COLUMN * 2, PER_COLUMN));
        assert!(starts_new_column(PER_COLUMN * 3, PER_COLUMN));
        assert!(!starts_new_column(PER_COLUMN + 1, PER_COLUMN));
    }

    #[test]
    fn zero_width_column_never_breaks() {
        // Defensive: a non-positive width would otherwise divide by zero.
        assert!(!starts_new_column(5, 0));
    }
}
