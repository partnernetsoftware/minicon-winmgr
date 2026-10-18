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
    struct Process(windows::Win32::Foundation::HANDLE);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let mut owned = Vec::new();
    for (_, pid) in &targets {
        if owned.iter().any(|(seen, _): &(u32, Process)| seen == pid) {
            continue;
        }
        if !crate::single_instance::same_user_session(*pid) {
            return Err("Cannot verify the previous shell's account and session".into());
        }
        // Pin the exact process before sending any close request. Never reopen
        // the PID for termination after a wait, where it could have been reused.
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                false,
                *pid,
            )
        }?;
        owned.push((*pid, Process(process)));
    }
    for (window, expected) in &targets {
        let mut actual = 0;
        unsafe {
            GetWindowThreadProcessId(*window, Some(&mut actual));
        }
        if actual != *expected {
            return Err("The previous shell window changed during handover".into());
        }
    }
    for (window, _) in &targets {
        unsafe { PostMessageW(Some(*window), WM_CLOSE, WPARAM(0), LPARAM(0)) }?;
    }
    let mut stopped = 0;
    for (_, process) in owned {
        if unsafe { WaitForSingleObject(process.0, SHELL_STOP_GRACE_MS) } != WAIT_OBJECT_0 {
            unsafe { TerminateProcess(process.0, 0) }?;
            if unsafe { WaitForSingleObject(process.0, SHELL_STOP_GRACE_MS) } != WAIT_OBJECT_0 {
                return Err("The previous desktop shell did not stop".into());
            }
        }
        stopped += 1;
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !foreign_shell_running() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !foreign_shell_running() {
        return Err("Explorer was started but its desktop host did not become available".into());
    }
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

    let mut name: Vec<u16> = crate::win_util::wide(exe);
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

pub struct Takeover {
    pub exe: String,
    pub stopped: usize,
}
/// A takeover must run on the persistent UI/message thread, never a short CLI process.
pub fn take_over() -> Result<Takeover, Box<dyn Error>> {
    if self_is_shell() {
        return Err("Minicon already manages this session".into());
    }
    if crate::hub::shell_hwnd().is_none() {
        return Err("A ready Minicon interface is required before takeover".into());
    }
    if matches!(
        std::env::var("MINICON_TRAY_HOST").as_deref(),
        Ok("0") | Ok("false")
    ) {
        return Err("Notification hosting is explicitly disabled; takeover was cancelled".into());
    }
    let operation = crate::shell_recovery::begin()?;
    let before = shell_registry::current_raw()?;
    let restore_explorer = foreign_shell_is_explorer();
    operation.ensure_original(&before)?;
    let exe = std::env::current_exe()?.display().to_string();
    let target = Some(shell_registry::RawShell::string(&format!("\"{exe}\"")));
    operation.write(&before, target.as_ref())?;
    let result = (|| -> Result<usize, Box<dyn Error>> {
        let stopped = stop_shell()?;
        if foreign_shell_running() {
            return Err("The previous desktop host is still running".into());
        }
        crate::window::transition_tray(true)?;
        Ok(stopped)
    })();
    match result {
        Ok(stopped) => Ok(Takeover { exe, stopped }),
        Err(error) => {
            let _ = crate::window::transition_tray(false);
            let restored = operation.write(&target, before.as_ref()).is_ok();
            let desktop = if restored && restore_explorer {
                start_shell().is_ok() && foreign_shell_running()
            } else {
                foreign_shell_running()
            };
            Err(format!("Takeover failed: {error}. Login settings restored: {restored}; external desktop host observed: {desktop}. Minicon settings remain available.").into())
        }
    }
}
/// Class names identify a shell window, not the executable we may restart.
fn foreign_shell_is_explorer() -> bool {
    use windows::Win32::System::Threading::{PROCESS_NAME_WIN32, QueryFullProcessImageNameW};
    let Some(root) = std::env::var_os("SystemRoot") else {
        return false;
    };
    let explorer = std::path::PathBuf::from(root)
        .join("explorer.exe")
        .to_string_lossy()
        .to_ascii_lowercase();
    let targets = stoppable(&shell_hits(), std::process::id());
    if targets.is_empty() {
        return false;
    }
    targets.into_iter().all(|(_, pid)| {
        let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return false;
        };
        let mut text = vec![0u16; 32768];
        let mut len = text.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(text.as_mut_ptr()),
                &mut len,
            )
        };
        let _ = unsafe { CloseHandle(process) };
        result.is_ok()
            && String::from_utf16_lossy(&text[..len as usize]).to_ascii_lowercase() == explorer
    })
}
fn explorer_setting(value: &Option<shell_registry::RawShell>) -> bool {
    let Some(raw) = value else {
        return true;
    };
    let Ok(text) = raw.text() else {
        return false;
    };
    let text = text.trim().trim_matches('"');
    text.eq_ignore_ascii_case("explorer.exe")
        || std::env::var("SystemRoot")
            .ok()
            .is_some_and(|r| text.eq_ignore_ascii_case(&format!(r"{r}\explorer.exe")))
}
pub struct GiveBack {
    pub restored: Option<String>,
    /// True only after an external Explorer host has actually appeared.
    pub started: bool,
}
/// Restore next-sign-in settings. Do not launch an unknown third-party command.
pub fn give_back() -> Result<GiveBack, Box<dyn Error>> {
    let operation = crate::shell_recovery::begin()?;
    let original = operation.original()?;
    let current = shell_registry::current_raw()?;
    if current != original && !crate::shell_recovery::is_product(&current) {
        return Err("Login settings changed outside Minicon; restoration was cancelled".into());
    }
    operation.write(&current, original.as_ref())?;
    let started = if !shell_running() && explorer_setting(&original) {
        start_shell()?;
        foreign_shell_running()
    } else {
        false
    };
    Ok(GiveBack {
        restored: original.map(|v| v.text()).transpose()?,
        started,
    })
}

#[cfg(test)]
mod tests {
    use super::{ShellHits, is_shell_class, stoppable};
    #[test]
    #[ignore = "owned foreign-host helper on a private desktop"]
    fn private_foreign_host() {
        use windows::{
            Win32::{Foundation::*, UI::WindowsAndMessaging::*},
            core::PCWSTR,
        };
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        unsafe extern "system" fn procedure(
            window: HWND,
            message: u32,
            wp: WPARAM,
            lp: LPARAM,
        ) -> LRESULT {
            if message == WM_DESTROY || message == WM_TIMER {
                unsafe {
                    PostQuitMessage(0);
                }
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(window, message, wp, lp) }
        }
        unsafe {
            let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
            let name = crate::win_util::wide("Shell_TrayWnd");
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: HINSTANCE(module.0),
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(
                Default::default(),
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(HINSTANCE(module.0)),
                None,
            )
            .unwrap();
            SetTimer(Some(hwnd), 1, 15000, None);
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                DispatchMessageW(&message);
            }
        }
    }
    #[test]
    #[ignore = "private desktop and process-local HKCU override; use isolated_tray_probe.py --test takeover"]
    fn native_private_desktop_takeover() {
        use windows::{
            Win32::{Foundation::*, System::Registry::*, UI::WindowsAndMessaging::*},
            core::PCWSTR,
        };
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        assert_eq!(std::env::var("MINICON_NO_TASKBAR").as_deref(), Ok("1"));
        assert!(std::env::var_os("MINICON_TRAY_HOST").is_none());
        struct Registry {
            key: HKEY,
            path: String,
        }
        impl Drop for Registry {
            fn drop(&mut self) {
                unsafe {
                    let _ = RegOverridePredefKey(HKEY_CURRENT_USER, None);
                    let _ = RegCloseKey(self.key);
                    assert!(self.path.starts_with(r"Software\minicon-winmgr\test-"));
                    let _ = RegDeleteTreeW(
                        HKEY_CURRENT_USER,
                        PCWSTR(crate::win_util::wide(&self.path).as_ptr()),
                    );
                }
            }
        }
        let path = format!(
            r"Software\minicon-winmgr\test-takeover-{}",
            std::process::id()
        );
        let mut key = HKEY::default();
        assert_eq!(
            unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(crate::win_util::wide(&path).as_ptr()),
                    Some(0),
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_ALL_ACCESS,
                    None,
                    &mut key,
                    None,
                )
            },
            ERROR_SUCCESS
        );
        let registry = Registry { key, path };
        assert_eq!(
            unsafe { RegOverridePredefKey(HKEY_CURRENT_USER, Some(registry.key)) },
            ERROR_SUCCESS
        );
        let mut foreign = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "shell_swap::tests::private_foreign_host",
                "--nocapture",
            ])
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !super::foreign_shell_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(super::foreign_shell_running());
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(!crate::tray_host::is_installed());
            assert_eq!(crate::shell_registry::current_raw().unwrap(), None);
            // A corrupt record must fail before either registry or host changes.
            let record = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
                .join("minicon-winmgr")
                .join("recovery")
                .join(crate::privilege::sid_text().unwrap())
                .join("original-shell.json");
            std::fs::create_dir_all(record.parent().unwrap()).unwrap();
            std::fs::write(&record, b"corrupt-fixture").unwrap();
            assert!(super::take_over().is_err());
            assert_eq!(std::fs::read(&record).unwrap(), b"corrupt-fixture");
            assert_eq!(crate::shell_registry::current_raw().unwrap(), None);
            assert!(super::foreign_shell_running());
            std::fs::remove_file(record).unwrap();
            // A later host failure must undo our registry intent without executing
            // the unknown previous shell command. The ready UI remains alive.
            crate::tray_host::FAIL_NEXT_INSTALL.store(true, std::sync::atomic::Ordering::SeqCst);
            assert!(super::take_over().is_err());
            assert_eq!(crate::shell_registry::current_raw().unwrap(), None);
            assert!(!crate::tray_host::is_installed());
            assert!(!super::foreign_shell_running());
            let mut replacement = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "shell_swap::tests::private_foreign_host",
                    "--nocapture",
                ])
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !super::foreign_shell_running() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(super::foreign_shell_running());
            let report = super::take_over().unwrap();
            assert_eq!(report.stopped, 1);
            assert!(replacement.wait().unwrap().success());
            assert!(crate::tray_host::is_installed());
            assert!(crate::shell_registry::is_self_shell());
            assert_eq!(crate::shell_recovery::original_text().unwrap(), None);
            let result = super::give_back().unwrap();
            assert!(!result.started);
            assert_eq!(result.restored, None);
            assert_eq!(crate::shell_registry::current_raw().unwrap(), None);
            assert!(crate::tray_host::is_installed()); // Current session remains ours.
            unsafe { PostMessageW(crate::hub::shell_hwnd(), WM_CLOSE, WPARAM(0), LPARAM(0)) }
                .unwrap();
        });
        crate::window::run(false, |usable| {
            assert!(usable);
            sender.send(()).unwrap();
            Ok(())
        })
        .unwrap();
        worker.join().unwrap();
        assert!(foreign.wait().unwrap().success());
        crate::settings_ui::shutdown();
        crate::run_dialog::shutdown();
        crate::tray_popup::shutdown();
        crate::tray_host::shutdown();
        drop(registry);
    }

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
}
