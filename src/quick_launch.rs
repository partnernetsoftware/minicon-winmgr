//! Quick-launch buttons for the left of the taskbar.
//!
//! INTERFACE FROZEN — the signatures below are relied on by the taskbar; do not
//! change them, and do not edit other files to accommodate this module.
//!
//! Pins are a **user-curated** list: the taskbar's per-window right-click menu
//! adds an entry via [`add`] (or drops it via [`remove`]). The list persists
//! next to the executable as `label<TAB>path` lines (env override
//! `MINICON_QUICKLAUNCH_FILE`), so it survives restarts. An empty list means no
//! pins — the strip is simply absent.
//!
//! Geometry is pure and unit-tested; the taskbar owns the HDC and hit-testing.

use std::path::PathBuf;

use windows::Win32::Foundation::RECT;

/// Square button edge, in logical pixels.
pub const CELL: i32 = 26;
/// Gap between adjacent buttons, in logical pixels.
pub const GAP: i32 = 4;
/// Maximum number of pinned buttons. Past this the taskbar greys the "add"
/// entry out (see [`is_full`]).
pub const MAX_PINS: usize = 8;

/// Environment variable overriding the pin file location.
const PINS_ENV: &str = "MINICON_QUICKLAUNCH_FILE";

/// Default pin file name, placed next to the executable.
const PINS_FILE_NAME: &str = "quick-launch.txt";

/// One pinned quick-launch entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// Display label captured when the pin was created (window title, trimmed).
    pub label: String,
    /// Executable path handed to [`launch_path`].
    pub path: String,
}

/// Resolves the pin file path: `MINICON_QUICKLAUNCH_FILE` if set, else
/// `<exe_dir>/quick-launch.txt`, else `./quick-launch.txt`.
fn pins_file() -> PathBuf {
    if let Some(path) = std::env::var_os(PINS_ENV)
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|parent| parent.join(PINS_FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(PINS_FILE_NAME))
}

/// Parses `label<TAB>path` records, skipping blank and malformed lines. The
/// path may contain spaces, so the fields split on the first tab only.
fn parse(contents: &str) -> Vec<Pin> {
    contents
        .lines()
        .filter_map(|line| {
            let (label, path) = line.split_once('\t')?;
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            Some(Pin {
                label: label.trim().to_owned(),
                path: path.to_owned(),
            })
        })
        .collect()
}

/// Serializes pins to `label<TAB>path` lines.
fn serialize(pins: &[Pin]) -> String {
    let mut out = String::new();
    for pin in pins {
        out.push_str(&pin.label);
        out.push('\t');
        out.push_str(&pin.path);
        out.push('\n');
    }
    out
}

/// Whether `pins` already holds `path` (case-insensitive, as Windows paths are).
fn contains(pins: &[Pin], path: &str) -> bool {
    pins.iter().any(|p| p.path.eq_ignore_ascii_case(path))
}

/// Appends `label`/`path` unless it is a duplicate or the cap is reached.
/// Returns whether the list changed. Pure core of [`add`].
fn push(pins: &mut Vec<Pin>, label: &str, path: &str) -> bool {
    if path.is_empty() || pins.len() >= MAX_PINS {
        return false;
    }
    if contains(pins, path) {
        return false;
    }
    pins.push(Pin {
        label: label.to_owned(),
        path: path.to_owned(),
    });
    true
}

/// Loads the persisted pins, capped at [`MAX_PINS`]. Best effort: a missing or
/// unreadable file yields an empty list.
fn load() -> Vec<Pin> {
    let contents = match std::fs::read_to_string(pins_file()) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::debug!(error = %err, "no quick-launch pin file");
            return Vec::new();
        }
    };
    let mut pins = parse(&contents);
    pins.truncate(MAX_PINS);
    pins
}

/// Writes `pins` back to disk, best effort.
fn save(pins: &[Pin]) {
    if let Err(err) = std::fs::write(pins_file(), serialize(pins)) {
        tracing::warn!(error = %err, "failed to persist quick-launch pins");
    }
}

/// The current pins, in display order.
pub fn pins() -> Vec<Pin> {
    load()
}

/// Whether `path` is already pinned.
pub fn is_pinned(path: &str) -> bool {
    contains(&load(), path)
}

/// Whether the pin cap is reached, so the taskbar can grey the "add" entry.
pub fn is_full() -> bool {
    load().len() >= MAX_PINS
}

/// Pins `path` under `label`, persisting on success. Returns whether the list
/// changed (false for a duplicate, an empty path, or when [`MAX_PINS`] is
/// reached).
pub fn add(label: &str, path: &str) -> bool {
    let mut pins = load();
    if push(&mut pins, label, path) {
        save(&pins);
        tracing::info!(path, "quick-launch: pin added");
        true
    } else {
        false
    }
}

/// Drops the pin whose path matches, persisting on success. Returns whether the
/// list changed.
pub fn remove(path: &str) -> bool {
    let mut pins = load();
    let before = pins.len();
    pins.retain(|p| !p.path.eq_ignore_ascii_case(path));
    if pins.len() == before {
        false
    } else {
        save(&pins);
        tracing::info!(path, "quick-launch: pin removed");
        true
    }
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
        .position(|r| crate::win_util::rect_contains(*r, x, y))
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
    use super::{GAP, MAX_PINS, Pin, contains, hit, layout, parse, push, serialize};

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

    #[test]
    fn parse_skips_blank_and_malformed_lines() {
        let text = "Editor\tC:\\app\\editor.exe\n\nno-tab-here\nTerm\tC:\\a b\\term.exe\n";
        assert_eq!(
            parse(text),
            vec![
                Pin {
                    label: "Editor".into(),
                    path: "C:\\app\\editor.exe".into()
                },
                Pin {
                    label: "Term".into(),
                    path: "C:\\a b\\term.exe".into()
                },
            ]
        );
    }

    #[test]
    fn parse_rejects_empty_path() {
        assert!(parse("Label\t   \n").is_empty());
    }

    #[test]
    fn serialize_round_trips_through_parse() {
        let pins = vec![
            Pin {
                label: "编辑器".into(),
                path: "C:\\Program Files\\Ed\\ed.exe".into(),
            },
            Pin {
                label: "Term".into(),
                path: "C:\\t.exe".into(),
            },
        ];
        assert_eq!(parse(&serialize(&pins)), pins);
    }

    #[test]
    fn push_adds_new_entry() {
        let mut pins = Vec::new();
        assert!(push(&mut pins, "Ed", "C:\\e.exe"));
        assert_eq!(pins.len(), 1);
    }

    #[test]
    fn push_ignores_duplicate_path_case_insensitively() {
        let mut pins = vec![Pin {
            label: "Ed".into(),
            path: "C:\\Ed.exe".into(),
        }];
        assert!(!push(&mut pins, "Other", "c:\\ed.exe"));
        assert_eq!(pins.len(), 1);
    }

    #[test]
    fn push_ignores_empty_path() {
        let mut pins = Vec::new();
        assert!(!push(&mut pins, "X", ""));
        assert!(pins.is_empty());
    }

    #[test]
    fn push_respects_the_cap() {
        let mut pins = Vec::new();
        for i in 0..MAX_PINS {
            assert!(push(&mut pins, "n", &format!("C:\\app{i}.exe")));
        }
        assert!(!push(&mut pins, "one more", "C:\\extra.exe"));
        assert_eq!(pins.len(), MAX_PINS);
    }

    #[test]
    fn contains_matches_case_insensitively() {
        let pins = vec![Pin {
            label: "Ed".into(),
            path: "C:\\Ed.exe".into(),
        }];
        assert!(contains(&pins, "c:\\ed.exe"));
        assert!(!contains(&pins, "c:\\other.exe"));
    }
}
