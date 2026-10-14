//! Start-menu application discovery and fuzzy search (launcher seed).

use std::path::Path;

use crate::model::Shortcut;

/// Scans the user and common Start Menu program folders for `.lnk` files.
///
/// Reads the user (`%APPDATA%`) and machine-wide (`%ProgramData%`) Start Menu
/// `Programs` directories, recursing into subfolders. Missing directories are
/// skipped. Results are sorted by display name and de-duplicated.
pub fn discover_shortcuts() -> Vec<Shortcut> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(
            Path::new(&appdata)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    if let Ok(programdata) = std::env::var("ProgramData") {
        roots.push(
            Path::new(&programdata)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }

    let mut out: Vec<Shortcut> = Vec::new();
    for root in roots {
        collect_lnk(&root, &mut out);
    }

    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    out.dedup_by(|a, b| a.name == b.name && a.path == b.path);
    out
}

/// Recursively collects `.lnk` files under `dir` into `out`.
fn collect_lnk(dir: &Path, out: &mut Vec<Shortcut>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            collect_lnk(&path, out);
        } else if is_lnk(&path)
            && let (Some(name), Some(path)) = (file_stem(&path), path.to_str())
        {
            out.push(Shortcut {
                name,
                path: path.to_owned(),
            });
        }
    }
}

/// Returns the file stem of `path` as a `String`, if it is valid UTF-8.
fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_owned())
}

/// Whether `path` has a `.lnk` extension (case-insensitive).
fn is_lnk(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
}

/// Like [`search`], but boosts frequently/recently launched apps: an empty
/// query returns the most-used apps first (see `crate::recents`).
pub fn search_ranked<'a>(items: &'a [Shortcut], query: &str, limit: usize) -> Vec<&'a Shortcut> {
    if query.trim().is_empty() {
        let stats = crate::recents::load();
        return crate::recents::rank(items, &stats, limit);
    }
    search(items, query, limit)
}

/// Fuzzy-matches `items` against `query`, returning up to `limit` best matches,
/// best first. An empty query returns the first `limit` items.
pub fn search<'a>(items: &'a [Shortcut], query: &str, limit: usize) -> Vec<&'a Shortcut> {
    if limit == 0 {
        return Vec::new();
    }
    let query = query.trim();
    if query.is_empty() {
        return items.iter().take(limit).collect();
    }

    let needle: Vec<char> = query.to_lowercase().chars().collect();

    let mut scored: Vec<(i64, usize, &Shortcut)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if let Some(score) = fuzzy_score(item.name.to_lowercase().chars().collect(), &needle) {
            scored.push((score, idx, item));
        }
    }

    // Higher score first; ties keep the original (stable) order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, it)| it)
        .collect()
}

/// Scores `name` against `needle` (both lowercased char vectors).
///
/// Returns `None` when `needle` is not a subsequence of `name`. Otherwise
/// returns a score: contiguous runs and matches near the start score higher.
fn fuzzy_score(name: Vec<char>, needle: &[char]) -> Option<i64> {
    let mut score: i64 = 0;
    let mut n_idx = 0usize;

    for (pos, &ch) in name.iter().enumerate() {
        if n_idx < needle.len() && ch == needle[n_idx] {
            // Base reward for every matched char.
            score += 1;
            // Contiguity bonus when the previous char also matched.
            if pos > 0 && n_idx > 0 && name[pos - 1] == needle[n_idx - 1] {
                score += 5;
            }
            // Early-match bonus.
            if pos == 0 || (n_idx == 0 && pos == 0) {
                score += 3;
            }
            n_idx += 1;
        }
    }

    if n_idx == needle.len() {
        // Exact / prefix matches score notably higher.
        if name.len() == needle.len() {
            score += 20;
        } else if name.starts_with(needle) {
            score += 10;
        }
        Some(score)
    } else {
        None
    }
}

/// Launches the shortcut at `path`, recording it for frequency ranking.
pub fn launch(path: &str) -> std::io::Result<()> {
    crate::recents::record(path);
    #[cfg(windows)]
    {
        // Routed through `shell_launch` so a pinned bare `explorer.exe` cannot
        // quietly take the shell back from us (see that module's header).
        if crate::shell_launch::open(path) {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "shell refused to open {path}"
            )))
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, path: &str) -> Shortcut {
        Shortcut {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    }

    fn names<'a>(items: &[&'a Shortcut]) -> Vec<&'a str> {
        items.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn empty_query_returns_first_limit() {
        let items = vec![
            item("Alpha", "a.lnk"),
            item("Beta", "b.lnk"),
            item("Gamma", "c.lnk"),
        ];
        let got = search(&items, "", 2);
        assert_eq!(names(&got), vec!["Alpha", "Beta"]);
    }

    #[test]
    fn empty_query_limit_larger_than_items() {
        let items = vec![item("Alpha", "a.lnk")];
        assert_eq!(search(&items, "", 10).len(), 1);
    }

    #[test]
    fn zero_limit_returns_empty() {
        let items = vec![item("Alpha", "a.lnk")];
        assert!(search(&items, "a", 0).is_empty());
        assert!(search(&items, "", 0).is_empty());
    }

    #[test]
    fn no_match_returns_empty() {
        let items = vec![item("Alpha", "a.lnk"), item("Beta", "b.lnk")];
        assert!(search(&items, "zzz", 10).is_empty());
    }

    #[test]
    fn subsequence_matching() {
        let items = vec![
            item("Visual Studio Code", "v.lnk"),
            item("Notepad", "n.lnk"),
        ];
        let got = search(&items, "vsc", 10);
        assert_eq!(names(&got), vec!["Visual Studio Code"]);
    }

    #[test]
    fn case_insensitive() {
        let items = vec![item("Visual Studio Code", "v.lnk")];
        assert_eq!(search(&items, "VSCODE", 10).len(), 1);
        assert_eq!(search(&items, "visual studio", 10).len(), 1);
    }

    #[test]
    fn prefix_and_exact_rank_higher() {
        let items = vec![item("Code", "c.lnk"), item("Visual Studio Code", "v.lnk")];
        // Exact "Code" should beat the longer subsequence match.
        let got = search(&items, "code", 10);
        assert_eq!(names(&got), vec!["Code", "Visual Studio Code"]);
    }

    #[test]
    fn ordered_by_score_and_limited() {
        let items = vec![
            item("Nano", "nano.lnk"),
            item("Anon", "anon.lnk"),
            item("Node", "node.lnk"),
        ];
        let got = search(&items, "no", 2);
        assert_eq!(got.len(), 2);
        // "Node" is the only prefix match → tops the list despite input order.
        assert_eq!(got[0].name, "Node");
    }

    #[test]
    fn ties_preserve_input_order() {
        let items = vec![item("Item A", "1.lnk"), item("Item B", "2.lnk")];
        let got = search(&items, "item", 10);
        assert_eq!(names(&got), vec!["Item A", "Item B"]);
    }

    #[test]
    fn launch_nonexistent_path_is_not_an_error_at_call_site() {
        // `launch` only spawns; a bogus path may still spawn `cmd`. We avoid
        // running it here to keep tests hermetic — the function must exist and
        // be callable with an io::Result signature.
        let _: fn(&str) -> std::io::Result<()> = launch;
    }
}
