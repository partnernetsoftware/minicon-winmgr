//! Immediate shell handover for the *current* session.
//!
//! Windows only re-reads `HKCU\...\Winlogon\Shell` at logon, so pointing the
//! registry at us is not enough to "take over now": the shell process that is
//! already running has to be stopped too. Conversely, handing the desktop back
//! has to restart it without a sign-out.
//!
//! Everything here is scoped to the calling session's desktop, because the
//! shell is found by enumerating *top-level windows* on the current desktop
//! (`Shell_TrayWnd` / `Shell_SecondaryTrayWnd` / `Progman`). Windows in other
//! sessions — other logged-on users — are never enumerated, so they can never
//! be touched.
//!
//! Ordering mirrors `docs/prd_3_1.md`: the registry is always written *before*
//! the running shell is stopped, so a crash in between leaves the machine in a
//! consistent state re-taken at the next logon.

use std::error::Error;
use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};
use windows::core::BOOL;

use crate::shell_registry;

/// How long we let the shell exit on its own after `WM_CLOSE`, before forcing.
const SHELL_STOP_GRACE_MS: u32 = 1500;

/// Last observed foreign-shell state, for transition-only logging.
static FOREIGN_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Window classes owned by the interactive shell of a session.
fn is_shell_class(class: &str) -> bool {
    matches!(
        class,
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman"
    )
}

/// Shell windows and their owning processes, gathered in one enumeration.
#[derive(Default)]
struct ShellHits {
    /// `(window, owning pid)` for every shell-class top-level window.
    entries: Vec<(HWND, u32)>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: the guard only runs the body; enumeration continues on a panic,
    // so one unreadable window is skipped rather than aborting the scan.
    crate::win_util::guard_abi(
        "shell_swap::collect",
        || unsafe { collect_inner(hwnd, lparam) },
        || BOOL(1),
    )
}

unsafe extern "system" fn collect_inner(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut ShellHits` we passed to `EnumWindows`; it
    // outlives the enumeration.
    let hits = unsafe { &mut *(lparam.0 as *mut ShellHits) };
    let mut buf = [0u16; 64];
    // SAFETY: `hwnd` is a live top-level window; `buf` is ours.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return BOOL(1);
    }
    let class = String::from_utf16_lossy(&buf[..len as usize]);
    if is_shell_class(&class) {
        let mut pid = 0u32;
        // SAFETY: same as above.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        hits.entries.push((hwnd, pid));
    }
    BOOL(1)
}

fn shell_hits() -> ShellHits {
    let mut hits = ShellHits::default();
    // SAFETY: `collect` only reads the window class and the owning pid; `hits`
    // outlives the call.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut hits as *mut _ as isize));
    }
    hits
}

/// Whether an interactive shell is running in this session right now.
pub fn shell_running() -> bool {
    !shell_hits().entries.is_empty()
}

/// Whether **this process** owns a shell window in the current session, i.e. we
/// are already the interactive shell.
///
/// This is not the same as [`shell_running`]: when we host the notification
/// area our tray host owns a `Shell_TrayWnd`, so a naive "is a shell running?"
/// says yes while the only shell is us. Any handover must use *this* check, not
/// that one — stopping our own shell stops us.
pub fn self_is_shell() -> bool {
    let me = std::process::id();
    shell_hits().entries.iter().any(|(_, pid)| *pid == me)
}

/// Whether a shell **other than us** owns this session.
///
/// A `Shell_TrayWnd` we do not own means Explorer (or another shell) has taken
/// the session back — most often because something started `explorer.exe`
/// without arguments. Detecting it is cheap and safe; acting on it is a
/// product decision, so this only reports.
pub fn foreign_shell_running() -> bool {
    let me = std::process::id();
    shell_hits()
        .entries
        .iter()
        .any(|(_, pid)| *pid != me && *pid != 0)
}

/// Logs foreign-shell transitions only (the heartbeat runs every 30s and must
/// not repeat the same warning forever).
pub fn check_foreign_shell() {
    let present = foreign_shell_running();
    let seen = FOREIGN_SEEN.swap(present, std::sync::atomic::Ordering::Relaxed);
    match (seen, present) {
        (false, true) => tracing::warn!(
            "a foreign shell window appeared — Explorer may have taken the session \
             (a bare `explorer.exe` start does this); reclaim with --shell-takeover"
        ),
        (true, false) => tracing::info!("foreign shell window is gone; we own the session again"),
        _ => {}
    }
}

/// The shell windows/pids a handover may stop: every shell-class window except
/// this process's own, and except windows whose owner cannot be resolved.
///
/// Pure, so the "never target ourselves" rule is unit-tested. It matters
/// because when we host the notification area our tray host owns a
/// `Shell_TrayWnd`: without this filter a takeover would force-terminate our
/// own process, taking the user's bar down with it.
fn stoppable(hits: &ShellHits, me: u32) -> Vec<(HWND, u32)> {
    hits.entries
        .iter()
        .copied()
        .filter(|(_, pid)| *pid != 0 && *pid != me)
        .collect()
}

/// Stops this session's shell process(es): a graceful `WM_CLOSE` first (what
/// Explorer treats as "shut the shell down"), then a forced termination if it
/// is still alive after [`SHELL_STOP_GRACE_MS`].
///
/// Only **foreign** shells are ever targeted (see [`stoppable`]); our own shell
/// window is skipped, so this can never stop the caller.
///
/// Returns how many processes were stopped; `0` means no foreign shell was
/// running in this session, which is a success, not an error.
pub fn stop_shell() -> Result<usize, Box<dyn Error>> {
    let targets = stoppable(&shell_hits(), std::process::id());
    if targets.is_empty() {
        return Ok(0);
    }
    // Ask nicely first: Explorer shuts the shell down on WM_CLOSE.
    for (window, _) in &targets {
        // SAFETY: `window` is a live shell window on our desktop owned by
        // another process.
        unsafe {
            let _ = PostMessageW(Some(*window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    let mut stopped = 0;
    for (_, pid) in targets {
        // SAFETY: plain process handles; every handle is closed before returning.
        unsafe {
            let Ok(handle) = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            ) else {
                continue;
            };
            let waited = WaitForSingleObject(handle, SHELL_STOP_GRACE_MS);
            let mut alive = waited != WAIT_OBJECT_0;
            if alive {
                // Graceful close did not take; force it so the handover is real.
                if let Ok(force) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    let _ = TerminateProcess(force, 0);
                    let _ = CloseHandle(force);
                    alive = WaitForSingleObject(handle, SHELL_STOP_GRACE_MS) != WAIT_OBJECT_0;
                }
            }
            let _ = CloseHandle(handle);
            if !alive {
                stopped += 1;
                tracing::info!(pid, "shell process stopped");
            } else {
                tracing::warn!(pid, "shell process refused to stop");
            }
        }
    }
    Ok(stopped)
}

/// Starts Explorer again if this session has no shell. Returns whether a new
/// shell was launched.
///
/// The new shell is spawned **detached** with no inherited handles: a shell must
/// outlive whoever launched it, and handing it our pipes/console would tie the
/// two processes together (and can take the launcher down with it).
pub fn start_shell() -> Result<bool, Box<dyn Error>> {
    if shell_running() {
        return Ok(false);
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let exe = format!(r"{root}\explorer.exe");
    if !std::path::Path::new(&exe).exists() {
        return Err(format!("找不到系统外壳：{exe}").into());
    }
    spawn_detached(&exe)?;
    tracing::info!(%exe, "launched the system shell");
    Ok(true)
}

/// Launches `exe` with no inherited handles, in its own process group.
fn spawn_detached(exe: &str) -> Result<(), Box<dyn Error>> {
    use windows::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    use windows::core::PWSTR;

    let mut name: Vec<u16> = exe.encode_utf16().chain(std::iter::once(0)).collect();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: `name` is a valid NUL-terminated wide string that outlives the
    // call; `startup`/`info` are owned locals. Both returned handles are closed.
    unsafe {
        CreateProcessW(
            None,
            Some(PWSTR(name.as_mut_ptr())),
            None,
            None,
            false,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            None,
            None,
            &startup,
            &mut info,
        )?;
        let _ = CloseHandle(info.hProcess);
        let _ = CloseHandle(info.hThread);
    }
    Ok(())
}

/// Path of the file holding the previous `Shell` value, written before the
/// first takeover so [`give_back`] can restore it faithfully. An *empty* file
/// records "the value was not set"; a **missing** file means "this program has
/// never taken over", and [`give_back`] refuses to touch the registry then.
pub fn backup_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("shell-backup.txt"))
}

/// Records the pre-takeover shell exactly once. A pre-existing backup is never
/// overwritten, so the *original* value survives repeated takeovers.
fn record_backup(current: Option<&str>) {
    let Some(path) = backup_path() else {
        return;
    };
    if path.exists() {
        return;
    }
    if let Err(err) = std::fs::write(&path, current.unwrap_or("")) {
        tracing::warn!(path = %path.display(), %err, "shell backup write failed");
    }
}

/// Interprets a backup file's contents. Pure, so the "missing / empty / value"
/// distinction that guards [`give_back`] is unit-tested.
///
/// - `None` file contents → `None` (never recorded: give-back must refuse)
/// - blank contents → `Some(None)` (recorded as "the value was not set")
/// - non-blank contents → `Some(Some(value))`
fn interpret_backup(raw: Option<String>) -> Option<Option<String>> {
    let raw = raw?;
    let trimmed = raw.trim().to_string();
    Some((!trimmed.is_empty()).then_some(trimmed))
}

/// The taken-over state needed by [`give_back`], or `None` when no takeover was
/// ever recorded.
fn recorded_backup() -> Option<Option<String>> {
    interpret_backup(std::fs::read_to_string(backup_path()?).ok())
}

/// The outcome of a takeover, for reporting back to the user/CLI.
pub struct Takeover {
    pub exe: String,
    /// Shell processes stopped in this session (`0` = none was running).
    pub stopped: usize,
    /// Whether the shell auto-restart was disabled (needs elevation).
    pub auto_restart_disabled: bool,
}

/// Replaces the shell with this executable **and** takes over immediately.
///
/// Refuses when we already are the shell: there is nothing to hand over, and
/// stopping the session's shell would stop *us* (we own the tray host's
/// `Shell_TrayWnd`).
pub fn take_over() -> Result<Takeover, Box<dyn Error>> {
    if self_is_shell() {
        return Err("本程序已经在充当桌面外壳，无需接管".into());
    }
    let exe = std::env::current_exe()?.display().to_string();

    // 1. Record the current value once (empty file = "was not set"), so a later
    //    give-back restores the user's machine exactly. A failed write is not
    //    fatal, but it is logged because it makes give-back refuse later.
    match shell_registry::current_shell() {
        Ok(cur) => record_backup(cur.as_deref()),
        Err(err) => tracing::warn!(%err, "could not read current shell; taking over anyway"),
    }

    // 2. Registry first (persistent intent), then the runtime handover.
    shell_registry::replace(&exe)?;

    // 3. Stop Windows from re-launching a shell we just replaced. Best effort:
    //    this lives under HKLM and needs elevation.
    let auto_restart_disabled = match shell_registry::set_auto_restart(false) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(%err, "could not disable shell auto-restart (needs elevation)");
            false
        }
    };

    // 4. Actually take the desktop: stop this session's shell.
    let stopped = stop_shell()?;
    Ok(Takeover {
        exe,
        stopped,
        auto_restart_disabled,
    })
}

/// The outcome of handing the desktop back to the system shell.
pub struct GiveBack {
    /// The value restored (`None` = the value was cleared, i.e. the Windows
    /// default shell).
    pub restored: Option<String>,
    /// Whether `explorer.exe` had to be launched.
    pub started: bool,
}

/// Restores the recorded shell **and** gives the desktop back immediately.
///
/// Refuses to touch the registry when no takeover was recorded, so a shell
/// configured outside this program is never silently cleared.
pub fn give_back() -> Result<GiveBack, Box<dyn Error>> {
    let Some(restored) = recorded_backup() else {
        return Err("没有记录到接管前的桌面外壳，无法还原（本程序未曾接管过桌面）".into());
    };

    shell_registry::restore(restored.as_deref())?;
    // Re-enable auto-restart so the system shell is resilient again.
    if let Err(err) = shell_registry::set_auto_restart(true) {
        tracing::warn!(%err, "could not re-enable shell auto-restart");
    }
    let started = start_shell()?;
    Ok(GiveBack { restored, started })
}

#[cfg(test)]
mod tests {
    use super::{ShellHits, interpret_backup, is_shell_class, stoppable};

    fn hwnd(id: usize) -> windows::Win32::Foundation::HWND {
        windows::Win32::Foundation::HWND(id as *mut core::ffi::c_void)
    }

    #[test]
    fn a_handover_never_targets_our_own_shell() {
        let me = 4242;
        let hits = ShellHits {
            entries: vec![
                (hwnd(1), me),  // our tray-host Shell_TrayWnd
                (hwnd(2), 100), // explorer's shell
                (hwnd(3), 0),   // unknown owner: not attributable, skipped
                (hwnd(4), 100), // explorer's secondary tray (same pid)
            ],
        };
        let pids: Vec<u32> = stoppable(&hits, me).into_iter().map(|(_, p)| p).collect();
        assert_eq!(pids, vec![100, 100]);
        assert!(stoppable(&hits, me).iter().all(|(_, p)| *p != me));
    }

    #[test]
    fn nothing_is_stoppable_when_only_we_are_the_shell() {
        let me = 7;
        let hits = ShellHits {
            entries: vec![(hwnd(1), me)],
        };
        assert!(stoppable(&hits, me).is_empty());
    }

    #[test]
    fn shell_classes_are_recognised() {
        assert!(is_shell_class("Shell_TrayWnd"));
        assert!(is_shell_class("Shell_SecondaryTrayWnd"));
        assert!(is_shell_class("Progman"));
    }

    #[test]
    fn other_windows_are_not_shell_windows() {
        // Our own windows and unrelated shell-adjacent classes must not match.
        for class in [
            "minicon_winmgr_taskbar",
            "minicon_winmgr_settings",
            "WorkerW",
            "Shell_SecondaryTray",
            "ProgmanX",
            "TaskManagerWindow",
            "",
        ] {
            assert!(!is_shell_class(class), "{class} must not be a shell window");
        }
    }

    #[test]
    fn missing_backup_means_never_taken_over() {
        // No file at all → give-back must refuse rather than clear the registry.
        assert_eq!(interpret_backup(None), None);
    }

    #[test]
    fn empty_backup_records_an_unset_shell() {
        assert_eq!(interpret_backup(Some(String::new())), Some(None));
        assert_eq!(interpret_backup(Some("  \n".to_string())), Some(None));
    }

    #[test]
    fn non_empty_backup_records_the_value() {
        assert_eq!(
            interpret_backup(Some(" C:\\Windows\\explorer.exe \n".to_string())),
            Some(Some("C:\\Windows\\explorer.exe".to_string()))
        );
    }
}
