//! Task grouping: collect task windows into per-application groups.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.

use std::collections::HashMap;

use crate::window_snapshot::WindowInfo;

/// A set of task windows belonging to the same application.
pub struct TaskGroup {
    /// Stable identity of the group (owning process id as a string).
    pub key: String,
    /// Windows in this group, in enumeration order.
    pub windows: Vec<WindowInfo>,
}

/// Groups windows by their owning process id.
///
/// Groups appear in the order their first window is seen; windows keep their
/// input (enumeration) order within each group.
pub fn group_by_pid(windows: Vec<WindowInfo>) -> Vec<TaskGroup> {
    let mut groups: Vec<TaskGroup> = Vec::new();
    let mut index: HashMap<u32, usize> = HashMap::new();

    for window in windows {
        match index.get(&window.pid) {
            Some(&i) => groups[i].windows.push(window),
            None => {
                index.insert(window.pid, groups.len());
                groups.push(TaskGroup {
                    key: window.pid.to_string(),
                    windows: vec![window],
                });
            }
        }
    }

    groups
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
