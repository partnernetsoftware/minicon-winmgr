//! Quick-launch buttons for the left of the taskbar.
//!
//! Pins are a **user-curated** list: the taskbar's per-window right-click menu
//! adds an entry via [`add`] (or drops it via [`remove`]). The list persists
//! next to the executable as `label<TAB>path` lines (env override
//! `MINICON_QUICKLAUNCH_FILE`), so it survives restarts. An empty list means no
//! pins — the strip is simply absent.
//!
//! Geometry is pure and unit-tested; the taskbar owns the HDC and hit-testing.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
const MAX_PIN_FILE_BYTES: usize = 1024 * 1024;

use windows::Win32::Foundation::RECT;

/// Square button edge, in logical pixels.
pub const CELL: i32 = 26;
/// Gap between adjacent buttons, in logical pixels.
pub const PIN_GAP: i32 = 4;
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
        .take(MAX_PINS)
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

fn read_pin_file(path: &Path) -> io::Result<Option<String>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut contents = String::new();
    file.take(MAX_PIN_FILE_BYTES as u64 + 1)
        .read_to_string(&mut contents)?;
    if contents.len() > MAX_PIN_FILE_BYTES {
        return Err(io::Error::other("pin file exceeds size limit"));
    }
    Ok(Some(contents))
}

/// Display may degrade to an empty strip. Mutations must propagate read errors
/// instead of treating an unreadable/corrupt file as an empty writable list.
fn load() -> Vec<Pin> {
    read_pin_file(&pins_file())
        .ok()
        .flatten()
        .map(|text| parse(&text))
        .unwrap_or_default()
}

fn mutate_file(path: &Path, change: impl FnOnce(&mut Vec<Pin>) -> bool) -> io::Result<bool> {
    mutate_file_with(path, change, || Ok(()))
}
fn mutate_file_with(
    path: &Path,
    change: impl FnOnce(&mut Vec<Pin>) -> bool,
    before_replace: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    let original = read_pin_file(path)?;
    let mut pins = parse(original.as_deref().unwrap_or(""));
    if !change(&mut pins) {
        return Ok(false);
    }
    let contents = serialize(&pins);
    if contents.len() > MAX_PIN_FILE_BYTES {
        return Err(io::Error::other("pin file exceeds size limit"));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.{serial}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        before_replace()?;
        if read_pin_file(path)? != original {
            return Err(io::Error::other("pin file changed externally"));
        }
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        use windows::core::PCWSTR;
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_WRITE_THROUGH
                    | if original.is_some() {
                        MOVEFILE_REPLACE_EXISTING
                    } else {
                        Default::default()
                    },
            )
        }
        .map_err(io::Error::other)?;
        Ok(true)
    })();
    let _ = std::fs::remove_file(&temporary);
    result
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
    match mutate_file(&pins_file(), |pins| push(pins, label, path)) {
        Ok(changed) => changed,
        Err(error) => {
            tracing::warn!(%error, "quick-launch save failed; existing file retained");
            false
        }
    }
}

/// Remove only after reading the existing file successfully; replacement is atomic.
pub fn remove(path: &str) -> bool {
    match mutate_file(&pins_file(), |pins| {
        let before = pins.len();
        pins.retain(|pin| !pin.path.eq_ignore_ascii_case(path));
        pins.len() != before
    }) {
        Ok(changed) => changed,
        Err(error) => {
            tracing::warn!(%error, "quick-launch save failed; existing file retained");
            false
        }
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
    use super::{MAX_PINS, PIN_GAP, Pin, contains, hit, layout, parse, push, serialize};

    #[test]
    fn pin_mutations_reject_oversized_input_and_preserve_original_on_failure() {
        use super::*;
        let folder = std::env::temp_dir().join(format!(
            "winmgr-pins-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("pins.txt");
        let original = b"Editor\tC:\\Examples\\editor.exe\n";
        std::fs::write(&path, original).unwrap();
        assert!(
            mutate_file_with(
                &path,
                |pins| push(pins, "Second", "second.exe"),
                || Err(io::Error::other("injected replace failure"))
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
        assert!(mutate_file(&path, |pins| push(pins, "Second", "second.exe")).unwrap());
        assert_eq!(load_test(&path).len(), 2);
        let external = b"External\texternal.exe\n";
        assert!(
            mutate_file_with(
                &path,
                |pins| push(pins, "Third", "third.exe"),
                || { std::fs::write(&path, external) }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), external);
        use std::os::windows::fs::OpenOptionsExt;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(mutate_file(&path, |pins| push(pins, "Blocked", "blocked.exe")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), external);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
        drop(lock);
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(mutate_file(&path, |pins| push(pins, "Third", "third.exe")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe]);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PIN_FILE_BYTES as u64 + 1).unwrap();
        drop(file);
        assert!(mutate_file(&path, |pins| push(pins, "Third", "third.exe")).is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            MAX_PIN_FILE_BYTES as u64 + 1
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&folder).unwrap();
        fn load_test(path: &Path) -> Vec<Pin> {
            parse(read_pin_file(path).unwrap().as_deref().unwrap())
        }
        let many = (0..100)
            .map(|i| format!("Item{i}\tpath{i}\n"))
            .collect::<String>();
        assert_eq!(parse(&many).len(), MAX_PINS);
    }

    #[test]
    fn empty_has_no_rects() {
        assert!(layout(0, 0, 26, PIN_GAP, 36).is_empty());
    }

    #[test]
    fn non_positive_cell_has_no_rects() {
        assert!(layout(0, 3, 0, PIN_GAP, 36).is_empty());
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
        let rects = layout(0, 2, 26, PIN_GAP, 36);
        // (36 - 26) / 2 == 5
        for r in &rects {
            assert_eq!(r.top, 5);
            assert_eq!(r.bottom, 31);
        }
    }

    #[test]
    fn hit_reports_containing_index() {
        let rects = layout(0, 3, 26, PIN_GAP, 36);
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
        let rects = layout(0, 3, 26, PIN_GAP, 36);
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
