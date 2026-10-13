//! Window-list button: a popup listing every taskbar group.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Opened from the taskbar's list button. Shows a `TrackPopupMenu` with one
//! item per group and returns the chosen window handle (None if dismissed).

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetWindowRect, MF_MENUBARBREAK,
    MF_STRING, SetForegroundWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TRACK_POPUP_MENU_FLAGS, TrackPopupMenu,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

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

    // SAFETY: the popup menu is created, used and destroyed entirely within this
    // call. `anchor` is a live window owned by this process (the taskbar). Each
    // `wide` buffer outlives its `AppendMenuW` call, which copies the string.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return None;
        };

        for (i, entry) in entries.iter().enumerate() {
            let label = wide(&entry.label);
            // Wrap into a fresh column every `PER_COLUMN` rows so a long list
            // never runs off the bottom of the screen (see `starts_new_column`).
            let flags = if starts_new_column(i, PER_COLUMN) {
                MF_STRING | MF_MENUBARBREAK
            } else {
                MF_STRING
            };
            // Menu ids are 1-based; `(i + 1)` is at most `entries.len()`.
            let _ = AppendMenuW(menu, flags, i + 1, PCWSTR(label.as_ptr()));
        }

        // Prefer the cursor; fall back to the bottom-left of the anchor.
        let mut pt = POINT::default();
        let (x, y) = match GetCursorPos(&mut pt) {
            Ok(()) => (pt.x, pt.y),
            Err(_) => {
                let mut rc = RECT::default();
                if GetWindowRect(anchor, &mut rc).is_ok() {
                    (rc.left, rc.bottom)
                } else {
                    (0, 0)
                }
            }
        };

        // Required so the popup dismisses correctly when clicking away.
        let _ = SetForegroundWindow(anchor);

        // With `TPM_RETURNCMD` the return value is the chosen command id (0 if
        // the menu was dismissed without a selection).
        let cmd = TrackPopupMenu(
            menu,
            TRACK_POPUP_MENU_FLAGS(popup_flags()),
            x,
            y,
            Some(0),
            anchor,
            None,
        );

        // SAFETY: `menu` is no longer referenced after this point.
        let _ = DestroyMenu(menu);

        let id = cmd.0;
        if id >= 1 && (id as usize) <= entries.len() {
            Some(entries[id as usize - 1].hwnd)
        } else {
            None
        }
    }
}

/// Menu flags used for the popup (kept as a fn so it can be asserted in tests).
pub fn popup_flags() -> u32 {
    (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN).0
}

#[cfg(test)]
mod tests {
    use super::{PER_COLUMN, TPM_RETURNCMD, popup_flags, starts_new_column};

    #[test]
    fn flags_include_return_cmd() {
        assert_ne!(popup_flags() & TPM_RETURNCMD.0, 0);
    }

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
