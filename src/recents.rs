//! Launch recency / frequency stats for ranking the launcher.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Persistence is a plain-text file, one `path<TAB>count` record per line.
//! All IO is best-effort: failures are logged at `debug` and otherwise ignored.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::Shortcut;

/// Environment variable overriding the stats file location.
const RECENTS_ENV: &str = "MINICON_RECENTS_FILE";

/// Default stats file name, placed next to the executable.
const RECENTS_FILE_NAME: &str = "launch-recents.txt";

/// Resolves the stats file path: `MINICON_RECENTS_FILE` if set, else
/// `<exe_dir>/launch-recents.txt`, else `./launch-recents.txt`.
fn stats_file() -> PathBuf {
    if let Some(path) = std::env::var_os(RECENTS_ENV)
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|parent| parent.join(RECENTS_FILE_NAME)))
        .unwrap_or_else(|| PathBuf::from(RECENTS_FILE_NAME))
}

/// Records a launch of `path`, updating persisted stats (best effort).
///
/// Loads the current stats, bumps `path`'s count (creating it at 1), and writes
/// everything back. Any IO failure is logged and otherwise ignored.
pub fn record(path: &str) {
    let mut stats = load();
    match stats.iter_mut().find(|(p, _)| p == path) {
        Some((_, count)) => *count = count.saturating_add(1),
        None => stats.push((path.to_string(), 1)),
    }

    if let Err(err) = write_stats(&stats) {
        tracing::debug!(%path, error = %err, "failed to persist launch recents");
    }
}

/// Loads persisted `(path, count)` stats, best effort.
///
/// A missing or unreadable file yields an empty list. Malformed lines are
/// skipped. The file format is `path<TAB>count`; `path` may contain spaces, so
/// the fields are split on the first tab only.
pub fn load() -> Vec<(String, u32)> {
    let path = stats_file();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::debug!(file = %path.display(), error = %err, "no launch recents file");
            return Vec::new();
        }
    };

    contents.lines().filter_map(parse_line).collect()
}

/// Parses a single `path<TAB>count` line, returning `None` for malformed lines.
fn parse_line(line: &str) -> Option<(String, u32)> {
    let (path, count) = line.split_once('\t')?;
    if path.is_empty() {
        return None;
    }
    let count = count.trim().parse::<u32>().ok()?;
    Some((path.to_string(), count))
}

/// Serializes stats to `path<TAB>count` lines and writes them to disk.
fn write_stats(stats: &[(String, u32)]) -> std::io::Result<()> {
    let mut out = String::new();
    for (path, count) in stats {
        out.push_str(path);
        out.push('\t');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    std::fs::write(stats_file(), out)
}

/// Orders `items` by frequency in `stats` (higher first), then by `items`
/// order, returning at most `limit`. Pure function; used by tests.
///
/// Items absent from `stats` (and ties) keep their original relative order.
pub fn rank<'a>(items: &'a [Shortcut], stats: &[(String, u32)], limit: usize) -> Vec<&'a Shortcut> {
    if limit == 0 {
        return Vec::new();
    }

    let counts: HashMap<&str, u32> = stats.iter().map(|(p, c)| (p.as_str(), *c)).collect();

    let mut ordered: Vec<&Shortcut> = items.iter().collect();
    // `sort_by` is stable, so equal-count items retain their input order.
    ordered.sort_by(|a, b| {
        let ca = counts.get(a.path.as_str()).copied().unwrap_or(0);
        let cb = counts.get(b.path.as_str()).copied().unwrap_or(0);
        cb.cmp(&ca)
    });

    ordered.truncate(limit);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(name: &str, path: &str) -> Shortcut {
        Shortcut {
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    fn paths<'a>(items: &[&'a Shortcut]) -> Vec<&'a str> {
        items.iter().map(|s| s.path.as_str()).collect()
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(rank(&[], &[], 5).is_empty());
    }

    #[test]
    fn zero_limit_returns_empty() {
        let items = [shortcut("a", "a")];
        let stats = [("a".to_string(), 100)];
        assert!(rank(&items, &stats, 0).is_empty());
    }

    #[test]
    fn no_stats_keeps_original_order() {
        let items = [shortcut("a", "a"), shortcut("b", "b"), shortcut("c", "c")];
        let ranked = rank(&items, &[], 10);
        assert_eq!(paths(&ranked), vec!["a", "b", "c"]);
    }

    #[test]
    fn stats_sort_by_count_descending() {
        let items = [shortcut("a", "a"), shortcut("b", "b"), shortcut("c", "c")];
        let stats = [
            ("a".to_string(), 1),
            ("b".to_string(), 5),
            ("c".to_string(), 3),
        ];
        let ranked = rank(&items, &stats, 10);
        assert_eq!(paths(&ranked), vec!["b", "c", "a"]);
    }

    #[test]
    fn equal_counts_keep_stable_order() {
        let items = [shortcut("a", "a"), shortcut("b", "b"), shortcut("c", "c")];
        let stats = [
            ("a".to_string(), 7),
            ("b".to_string(), 7),
            ("c".to_string(), 7),
        ];
        let ranked = rank(&items, &stats, 10);
        assert_eq!(paths(&ranked), vec!["a", "b", "c"]);
    }

    #[test]
    fn missing_stats_rank_below_ranked_but_keep_order() {
        let items = [
            shortcut("a", "a"),
            shortcut("b", "b"),
            shortcut("c", "c"),
            shortcut("d", "d"),
        ];
        // Only `c` and `b` have stats; `a` and `d` are absent (count 0).
        let stats = [("c".to_string(), 2), ("b".to_string(), 9)];
        let ranked = rank(&items, &stats, 10);
        assert_eq!(paths(&ranked), vec!["b", "c", "a", "d"]);
    }

    #[test]
    fn limit_truncates_results() {
        let items = [shortcut("a", "a"), shortcut("b", "b"), shortcut("c", "c")];
        let stats = [
            ("a".to_string(), 3),
            ("b".to_string(), 2),
            ("c".to_string(), 1),
        ];
        let ranked = rank(&items, &stats, 2);
        assert_eq!(paths(&ranked), vec!["a", "b"]);
    }
}
