//! Pinned "folder stacks" — folders the user can expand from the Start menu.
//!
//! A stack is just a path the user pinned; clicking it lists its children
//! (files *and* sub-folders, one level deep) so common destinations are
//! reachable without opening an Explorer window. This module is the data layer
//! plus the pure logic: parsing the config value, naming entries, listing a
//! directory. No window, no GDI, no thread — `list_dir` is synchronous IO with
//! a `limit`, meant to be called straight from the message thread.
//!
//! Opening anything goes through [`crate::shell_launch::open`]. That is not
//! stylistic: a bare `explorer.exe` decides there is no shell and creates its
//! own taskbar, stealing the session from us. `shell_launch` rewrites it into
//! a file-browser invocation, so it must stay the only way we open a path.

/// One clickable row: what to show and what to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackEntry {
    /// Name shown in the menu (usually the last path segment).
    pub label: String,
    /// Full path handed to [`open`].
    pub path: String,
}

/// Splits a `folders = a;b;c` config value into its paths.
///
/// Segments are trimmed and empty ones dropped; duplicates are removed while
/// the first occurrence keeps its position. Whitespace-only values yield an
/// empty list.
pub fn parse_folders(raw: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for segment in raw.split(';') {
        let path = segment.trim();
        if path.is_empty() || seen.iter().any(|p| p == path) {
            continue;
        }
        seen.push(path.to_string());
    }
    seen
}

/// Turns already-parsed paths into rows, in pin order.
///
/// Labels come from [`child_label`], so each row reads like Explorer's own
/// name for the folder. `config::Config::folders` holds the parsed paths (see
/// [`parse_folders`]); this just pairs each with its display name.
pub fn entries(paths: &[String]) -> Vec<StackEntry> {
    paths
        .iter()
        .map(|path| StackEntry {
            label: child_label(path),
            path: path.clone(),
        })
        .collect()
}

/// Lists the children of `path` — files and folders alike — sorted by name.
///
/// Never recursive and never unbounded: at most `limit` entries come back
/// (`limit == 0` returns nothing). Any IO problem (missing folder, no
/// permission) is logged at `debug` and treated as "empty", so a bad pin can
/// never panic the shell. Entries whose path is not valid UTF-8 are skipped.
pub fn list_dir(path: &str, limit: usize) -> Vec<StackEntry> {
    if limit == 0 {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!(%path, error = %err, "folder stack: cannot read directory");
            return Vec::new();
        }
    };

    let mut out: Vec<StackEntry> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!(%path, error = %err, "folder stack: skipping unreadable entry");
                continue;
            }
        };
        let Some(child) = entry.path().to_str().map(str::to_string) else {
            tracing::debug!(%path, "folder stack: skipping non-UTF-8 entry");
            continue;
        };
        out.push(StackEntry {
            label: child_label(&child),
            path: child,
        });
    }

    // Case-insensitive key (Explorer-like); `sort_by_key` is stable, so equally
    // named entries keep directory order.
    out.sort_by_key(|e| e.label.to_lowercase());
    out.truncate(limit);
    out
}

/// Opens `path` (folder or file) the shell-safe way.
///
/// Delegates to [`crate::shell_launch::open`] so a bare `explorer.exe` is
/// rewritten before it can claim the shell. Never call `ShellExecuteW` or
/// spawn a process from here.
pub fn open(path: &str) {
    if crate::shell_launch::open(path) {
        tracing::debug!(%path, "folder stack: opened");
    } else {
        tracing::debug!(%path, "folder stack: launch failed");
    }
}

/// The display name of a path: its last segment.
///
/// Trailing separators are ignored, so `D:\dev\` reads `dev` and `C:\` reads
/// `C:`. An empty path yields an empty name.
pub fn child_label(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{child_label, entries, list_dir, parse_folders};
    use std::path::PathBuf;

    /// Unique scratch directory for one test; callers remove it when done.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "minicon-folder-stack-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn labels(entries: &[super::StackEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.label.as_str()).collect()
    }

    #[test]
    fn empty_value_has_no_folders() {
        assert!(parse_folders("").is_empty());
    }

    #[test]
    fn separator_only_value_has_no_folders() {
        assert!(parse_folders(";;").is_empty());
        assert!(parse_folders(";").is_empty());
    }

    #[test]
    fn whitespace_only_value_has_no_folders() {
        assert!(parse_folders("  ").is_empty());
        assert!(parse_folders(" ; \t; ").is_empty());
    }

    #[test]
    fn segments_are_trimmed_and_empties_dropped() {
        let got = parse_folders("  C:\\a ;\tD:\\b\n;;C:\\c");
        assert_eq!(got, vec!["C:\\a", "D:\\b", "C:\\c"]);
    }

    #[test]
    fn duplicates_are_dropped_keeping_first_position() {
        let got = parse_folders(r"C:\b;C:\a;C:\b;C:\a;D:\c;C:\b");
        assert_eq!(got, vec![r"C:\b", r"C:\a", r"D:\c"]);
    }

    #[test]
    fn pin_order_is_preserved() {
        let got = parse_folders("one;two;three");
        assert_eq!(got, vec!["one", "two", "three"]);
    }

    #[test]
    fn label_is_the_last_segment() {
        assert_eq!(child_label(r"C:\Users\a\Downloads"), "Downloads");
        assert_eq!(child_label(r"D:\dev"), "dev");
        assert_eq!(child_label("relative/name"), "name");
    }

    #[test]
    fn trailing_separator_is_ignored() {
        assert_eq!(child_label(r"D:\dev\"), "dev");
        assert_eq!(child_label(r"D:\dev\\"), "dev");
        assert_eq!(child_label("a/b/"), "b");
    }

    #[test]
    fn root_paths_keep_their_drive() {
        assert_eq!(child_label(r"C:\"), "C:");
        assert_eq!(child_label("C:"), "C:");
    }

    #[test]
    fn empty_path_has_an_empty_label() {
        assert_eq!(child_label(""), "");
        assert_eq!(child_label("\\"), "");
    }

    #[test]
    fn entries_carry_path_and_label() {
        let paths = parse_folders(r" C:\Users\a\Downloads ; D:\dev ; C:\Users\a\Downloads ");
        let got = entries(&paths);
        assert_eq!(
            got,
            vec![
                super::StackEntry {
                    label: "Downloads".to_string(),
                    path: r"C:\Users\a\Downloads".to_string(),
                },
                super::StackEntry {
                    label: "dev".to_string(),
                    path: r"D:\dev".to_string(),
                },
            ]
        );
    }

    #[test]
    fn whitespace_only_config_yields_no_entries() {
        assert!(entries(&parse_folders("  ")).is_empty());
        assert!(entries(&[]).is_empty());
    }

    #[test]
    fn lists_files_and_folders_sorted_by_name() {
        let dir = scratch("list");
        for name in ["zeta.txt", "Alpha.txt", "gamma.txt"] {
            std::fs::write(dir.join(name), b"x").expect("write file");
        }
        std::fs::create_dir(dir.join("beta")).expect("create subdir");

        let got = list_dir(&dir.to_string_lossy(), 10);
        assert_eq!(
            labels(&got),
            vec!["Alpha.txt", "beta", "gamma.txt", "zeta.txt"]
        );
        assert!(
            got.iter()
                .all(|e| e.path.starts_with(&dir.to_string_lossy().to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_truncates_the_listing() {
        let dir = scratch("limit");
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(dir.join(name), b"x").expect("write file");
        }
        let path = dir.to_string_lossy().to_string();

        assert_eq!(list_dir(&path, 2).len(), 2);
        assert_eq!(labels(&list_dir(&path, 2)), vec!["a.txt", "b.txt"]);
        assert_eq!(list_dir(&path, 100).len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_limit_lists_nothing() {
        let dir = scratch("zero");
        std::fs::write(dir.join("a.txt"), b"x").expect("write file");
        assert!(list_dir(&dir.to_string_lossy(), 0).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_path_lists_nothing() {
        let missing = std::env::temp_dir().join(format!(
            "minicon-folder-stack-test-{}-missing",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(list_dir(&missing.to_string_lossy(), 10).is_empty());
    }

    #[test]
    fn a_file_is_not_a_listable_directory() {
        let dir = scratch("file");
        let file = dir.join("plain.txt");
        std::fs::write(&file, b"x").expect("write file");
        assert!(list_dir(&file.to_string_lossy(), 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
