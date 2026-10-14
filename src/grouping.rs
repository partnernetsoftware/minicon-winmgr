//! Task grouping: collect task windows into per-application groups.

use std::collections::HashMap;

use crate::window_snapshot::WindowInfo;

/// A set of task windows belonging to the same application.
pub struct TaskGroup {
    /// Stable identity of the group (owning process id as a string).
    pub key: String,
    /// Windows in this group, in enumeration order.
    pub windows: Vec<WindowInfo>,
}

/// Groups windows by an arbitrary key computed from each window.
///
/// Groups appear in the order their first window is seen; windows keep their
/// input (enumeration) order within each group. This is the general form —
/// [`group_by_pid`] and the taskbar's exe-path grouping both build on it.
pub fn group_by<F>(windows: Vec<WindowInfo>, mut key_of: F) -> Vec<TaskGroup>
where
    F: FnMut(&WindowInfo) -> String,
{
    let mut groups: Vec<TaskGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for window in windows {
        let key = key_of(&window);
        match index.get(&key) {
            Some(&i) => groups[i].windows.push(window),
            None => {
                index.insert(key.clone(), groups.len());
                groups.push(TaskGroup {
                    key,
                    windows: vec![window],
                });
            }
        }
    }

    groups
}

/// Groups windows by their owning process id.
///
/// Retained as the canonical "pid strategy" so callers and tests share one
/// definition; the taskbar groups by exe path when configured.
pub fn group_by_pid(windows: Vec<WindowInfo>) -> Vec<TaskGroup> {
    group_by(windows, |window| window.pid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HWND;

    /// Builds a `WindowInfo` with a null hwnd; grouping only inspects `pid`.
    fn win(pid: u32, title: &str) -> WindowInfo {
        WindowInfo {
            hwnd: HWND(std::ptr::null_mut()),
            pid,
            class: String::new(),
            title: title.to_string(),
            visible: true,
        }
    }

    fn titles(group: &TaskGroup) -> Vec<&str> {
        group.windows.iter().map(|w| w.title.as_str()).collect()
    }

    #[test]
    fn empty_input_yields_no_groups() {
        assert!(group_by_pid(Vec::new()).is_empty());
    }

    #[test]
    fn single_window_yields_single_group() {
        let groups = group_by_pid(vec![win(7, "a")]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "7");
        assert_eq!(titles(&groups[0]), vec!["a"]);
    }

    #[test]
    fn same_pid_windows_are_merged() {
        let groups = group_by_pid(vec![win(1, "a"), win(1, "b"), win(1, "c")]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "1");
        assert_eq!(titles(&groups[0]), vec!["a", "b", "c"]);
    }

    #[test]
    fn group_order_is_first_appearance_and_windows_keep_input_order() {
        let groups = group_by_pid(vec![win(2, "a"), win(1, "b"), win(2, "c"), win(1, "d")]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "2");
        assert_eq!(groups[1].key, "1");
        assert_eq!(titles(&groups[0]), vec!["a", "c"]);
        assert_eq!(titles(&groups[1]), vec!["b", "d"]);
    }

    #[test]
    fn group_by_uses_the_supplied_key() {
        // Group by the first letter of the title: same-letter windows merge even
        // across pids, proving the key function — not the pid — drives grouping.
        let groups = group_by(
            vec![win(1, "alpha"), win(2, "apple"), win(3, "beta")],
            |w| w.title.chars().next().unwrap().to_string(),
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "a");
        assert_eq!(titles(&groups[0]), vec!["alpha", "apple"]);
        assert_eq!(groups[1].key, "b");
    }

    #[test]
    fn same_pid_across_gaps_groups_together() {
        let groups = group_by_pid(vec![
            win(1, "a"),
            win(2, "b"),
            win(1, "c"),
            win(2, "d"),
            win(1, "e"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "1");
        assert_eq!(titles(&groups[0]), vec!["a", "c", "e"]);
        assert_eq!(groups[1].key, "2");
        assert_eq!(titles(&groups[1]), vec!["b", "d"]);
    }
}
