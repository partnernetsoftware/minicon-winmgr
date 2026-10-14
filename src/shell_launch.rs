//! Shell-safe launching — the one place that starts things for the user.
//!
//! ## Why this module exists
//!
//! `explorer.exe` has two personalities: it is the file browser *and* the
//! shell. Which one it becomes depends on its command line. Started **without
//! arguments** it finds no `Shell_TrayWnd`, assumes the shell is missing, and
//! creates the desktop plus its own taskbar — which silently pushes our bar
//! out and hands the session back to Explorer. Started **with an argument**
//! (a path or a folder moniker) it just opens a browsing window.
//!
//! So a shell replacement must never let a bare `explorer.exe` escape. Every
//! "open" we originate — quick-launch pins, start-menu places, the run dialog,
//! the launcher — goes through [`open`], which rewrites the dangerous cases
//! into safe ones (see [`plan`]).
//!
//! The mapping itself is a pure function so the rule can be unit-tested
//! without a display.

use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use crate::win_util::wide;

/// The "This PC" folder, expressed as a CLSID Explorer can open.
///
/// Used as the argument whenever something asked for Explorer with nothing to
/// open: an argument is what keeps Explorer in file-browser mode.
pub const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// What to actually run for a requested target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// File/document handed to `ShellExecuteW` (`lpFile`).
    pub file: String,
    /// Arguments (`lpParameters`); empty means "none".
    pub params: String,
}

/// Whether `target` is Explorer started with no argument — the invocation that
/// makes it claim the shell.
///
/// A space anywhere in the target is treated as "arguments already present",
/// which is the conservative reading: we only rewrite what we are sure of.
pub fn is_bare_explorer(target: &str) -> bool {
    let t = target.trim().trim_matches('"');
    if t.is_empty() || t.contains(' ') {
        return false;
    }
    let name = t.rsplit(['\\', '/']).next().unwrap_or(t);
    name.eq_ignore_ascii_case("explorer.exe") || name.eq_ignore_ascii_case("explorer")
}

/// The sub-folder of the user profile behind a `shell:` moniker, if known.
///
/// Known monikers become real paths so Explorer can be given a concrete
/// argument; unknown ones keep the moniker itself (still an argument, still
/// safe — only a *bare* Explorer is dangerous).
fn home_subfolder(moniker: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("personal", "Documents"),
        ("documents", "Documents"),
        ("downloads", "Downloads"),
        ("mypictures", "Pictures"),
        ("pictures", "Pictures"),
        ("mymusic", "Music"),
        ("music", "Music"),
        ("myvideo", "Videos"),
        ("videos", "Videos"),
        ("desktop", "Desktop"),
    ];
    KNOWN
        .iter()
        .find(|(m, _)| m.eq_ignore_ascii_case(moniker))
        .map(|(_, sub)| *sub)
}

/// Path of `explorer.exe`, from `%SystemRoot%` when available.
fn explorer_exe(system_root: Option<&str>) -> String {
    let root = system_root.unwrap_or(r"C:\Windows");
    format!(r"{root}\explorer.exe")
}

/// Turns a requested target into a shell-safe [`Launch`].
///
/// `home` is the user profile (`%USERPROFILE%`) and `system_root` is
/// `%SystemRoot%`; both are optional so the mapping is testable.
pub fn plan(target: &str, home: Option<&str>, system_root: Option<&str>) -> Launch {
    let t = target.trim();
    if t.is_empty() {
        return Launch {
            file: String::new(),
            params: String::new(),
        };
    }

    // `shell:<moniker>` — opened by Explorer, so it must carry an argument.
    if let Some(moniker) = t.get(0..6).filter(|p| p.eq_ignore_ascii_case("shell:")) {
        let rest = t[moniker.len()..].trim_matches('/').trim();
        let arg = match home_subfolder(rest) {
            Some(sub) => match home {
                Some(h) => format!(r"{h}\{sub}"),
                None => t.to_string(),
            },
            None if rest.eq_ignore_ascii_case("mycomputerfolder") => THIS_PC.to_string(),
            None => t.to_string(),
        };
        return Launch {
            file: explorer_exe(system_root),
            params: arg,
        };
    }

    // Bare Explorer: give it somewhere to go so it stays a file browser.
    if is_bare_explorer(t) {
        return Launch {
            file: t.trim_matches('"').to_string(),
            params: THIS_PC.to_string(),
        };
    }

    Launch {
        file: t.to_string(),
        params: String::new(),
    }
}

/// Opens `target` the shell-safe way. Returns whether it was launched.
pub fn open(target: &str) -> bool {
    let launch = plan(
        target,
        std::env::var("USERPROFILE").ok().as_deref(),
        std::env::var("SystemRoot").ok().as_deref(),
    );
    if launch.file.is_empty() {
        return false;
    }
    if is_bare_explorer(target) {
        tracing::warn!(
            from = %target.trim(),
            to = %launch.params,
            "explorer started bare would have taken the shell; redirected"
        );
    }

    let file = wide(&launch.file);
    let params = wide(&launch.params);
    // SAFETY: both buffers outlive the call; no owner window.
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(wide("open").as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns <= 32 on failure (no instance / no association).
    if code.0 as i32 > 32 {
        tracing::info!(target, "launched (shell-safe)");
        true
    } else {
        tracing::warn!(target, code = code.0 as i32, "shell-safe launch failed");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{Launch, THIS_PC, is_bare_explorer, plan};

    fn launch(file: &str, params: &str) -> Launch {
        Launch {
            file: file.to_string(),
            params: params.to_string(),
        }
    }

    #[test]
    fn bare_explorer_is_recognised() {
        assert!(is_bare_explorer("explorer"));
        assert!(is_bare_explorer("explorer.exe"));
        assert!(is_bare_explorer(r"C:\Windows\explorer.exe"));
        assert!(is_bare_explorer(r"c:\windows\EXPLORER.EXE"));
        assert!(is_bare_explorer(r#""C:\Windows\explorer.exe""#));
    }

    #[test]
    fn explorer_with_arguments_is_left_alone() {
        assert!(!is_bare_explorer(r"explorer.exe C:\"));
        assert!(!is_bare_explorer(r"explorer.exe shell:Downloads"));
        assert!(!is_bare_explorer(r"C:\Program Files\Some Explorer\app.exe"));
    }

    #[test]
    fn other_programs_are_not_explorer() {
        assert!(!is_bare_explorer("notepad"));
        assert!(!is_bare_explorer(r"C:\Windows\System32\notepad.exe"));
        assert!(!is_bare_explorer(""));
    }

    #[test]
    fn bare_explorer_gets_a_folder_to_open() {
        let got = plan(r"C:\Windows\explorer.exe", None, Some(r"C:\Windows"));
        assert_eq!(got.file, r"C:\Windows\explorer.exe");
        assert_eq!(got.params, THIS_PC);
    }

    #[test]
    fn explorer_with_its_own_argument_is_untouched() {
        let got = plan(r"explorer.exe D:\dev", None, None);
        assert_eq!(got.file, r"explorer.exe D:\dev");
        assert!(got.params.is_empty());
    }

    #[test]
    fn shell_monikers_become_real_paths() {
        let got = plan("shell:Downloads", Some(r"C:\Users\u"), Some(r"C:\Windows"));
        assert_eq!(got.file, r"C:\Windows\explorer.exe");
        assert_eq!(got.params, r"C:\Users\u\Downloads");
    }

    #[test]
    fn unknown_monikers_are_kept_as_arguments() {
        let got = plan("shell:RecycleBinFolder", None, Some(r"C:\Windows"));
        assert_eq!(got.file, r"C:\Windows\explorer.exe");
        assert_eq!(got.params, "shell:RecycleBinFolder");
    }

    #[test]
    fn this_pc_moniker_maps_to_a_clsid() {
        let got = plan("shell:MyComputerFolder", None, Some(r"C:\Windows"));
        assert_eq!(got.params, THIS_PC);
    }

    #[test]
    fn moniker_without_home_falls_back_to_the_moniker() {
        let got = plan("shell:Personal", None, Some(r"C:\Windows"));
        assert_eq!(got.params, "shell:Personal");
    }

    #[test]
    fn ordinary_targets_pass_through() {
        let got = plan(r"C:\Windows\System32\notepad.exe", None, None);
        assert_eq!(got, launch(r"C:\Windows\System32\notepad.exe", ""));
    }

    #[test]
    fn empty_target_launches_nothing() {
        assert!(plan("   ", None, None).file.is_empty());
    }

    #[test]
    fn explorer_path_defaults_to_the_windows_directory() {
        let got = plan("explorer", None, None);
        assert_eq!(got.file, "explorer");
        assert_eq!(got.params, THIS_PC);
    }
}
