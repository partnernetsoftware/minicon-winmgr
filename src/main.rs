// GUI subsystem: no console window is created, so double-clicking (or being
// launched as the login shell) never flashes a console. CLI subcommands attach
// to the parent console on demand (see `attach_console_if_needed`).
#![windows_subsystem = "windows"]

mod bar_tip;
mod config;
mod dwm;
mod folder_stack;
mod grouping;
mod hotkeys;
mod hover_preview;
mod launcher;
mod launcher_ui;
mod logging;
mod metrics;
mod monitor;
mod quick_launch;
mod recents;
mod run_dialog;
mod session;
mod settings_ui;
mod shell_launch;
mod shell_registry;
mod shell_swap;
mod single_instance;
mod start_menu;
mod task_list;
mod taskbar;
mod tray;
mod tray_bar;
mod tray_host;
mod win_util;
mod window;
mod window_events;
mod window_icon;
mod window_snapshot;

/// Second-confirmation token required before any Shell-registry write. The
/// default companion run never needs it, so the registry stays untouched.
const CONFIRM_ENV: &str = "MINICON_CONFIRM_SHELL";

/// Env var pointing at an alternative shell executable (the `alt` profile).
/// No path is baked in — the profile is only usable when this is set.
const ALT_SHELL_ENV: &str = "MINICON_ALT_SHELL";

enum Cli {
    RunShell,
    ShellStatus,
    ShellList,
    ShellSet(Option<String>),
    ShellReplace,
    ShellTakeover,
    ShellGiveBack,
    ShellRollback(Option<String>),
    Launch(Option<String>),
    LaunchRun(Option<String>),
    Help,
}

impl Cli {
    fn parse(args: impl Iterator<Item = String>) -> Cli {
        let mut args = args.peekable();
        match args.next().as_deref() {
            None => Cli::RunShell,
            Some("--shell-status") => Cli::ShellStatus,
            Some("--shell-list") => Cli::ShellList,
            Some("--shell-set") => Cli::ShellSet(args.next()),
            Some("--shell-replace") => Cli::ShellReplace,
            Some("--shell-takeover") => Cli::ShellTakeover,
            Some("--shell-give-back") => Cli::ShellGiveBack,
            Some("--shell-rollback") => Cli::ShellRollback(args.next()),
            Some("--launch") => Cli::Launch(args.next()),
            Some("--launch-run") => Cli::LaunchRun(args.next()),
            Some("--help") | Some("-h") => Cli::Help,
            Some(other) => {
                eprintln!("minicon-winmgr: unknown argument '{other}'");
                Cli::Help
            }
        }
    }
}

fn main() {
    // CLI invocations may need a console for their output; the shell run must
    // stay console-free, so only attach when arguments are present.
    if std::env::args_os().nth(1).is_some() {
        attach_console_if_needed();
    }

    let _log_guard = logging::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "minicon-winmgr starting (companion mode)"
    );

    match Cli::parse(std::env::args().skip(1)) {
        Cli::RunShell => run_companion(),
        Cli::ShellStatus => cmd_shell_status(),
        Cli::ShellList => cmd_shell_list(),
        Cli::ShellSet(target) => cmd_shell_set(target),
        Cli::ShellReplace => cmd_shell_replace(),
        Cli::ShellTakeover => cmd_shell_takeover(),
        Cli::ShellGiveBack => cmd_shell_give_back(),
        Cli::ShellRollback(prev) => cmd_shell_rollback(prev),
        Cli::Launch(query) => cmd_launch(query),
        Cli::LaunchRun(query) => cmd_launch_run(query),
        Cli::Help => print_help(),
    }
}

/// Because we build as a GUI-subsystem binary there is no console by default.
/// If we were launched from an interactive terminal (no inherited std handle),
/// attach to the parent console so `println!`/`eprintln!` are visible. When the
/// parent redirects our output to a pipe, Rust's std handles already work and we
/// leave them alone.
fn attach_console_if_needed() {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };

    // SAFETY: standard console APIs; all failures are non-fatal.
    unsafe {
        let have_stdout = GetStdHandle(STD_OUTPUT_HANDLE)
            .map(|h| !h.is_invalid())
            .unwrap_or(false);
        if have_stdout {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if let Ok(file) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                use std::os::windows::io::AsRawHandle;
                let raw = file.as_raw_handle() as isize;
                let _ = SetStdHandle(std_handle, HANDLE(raw as *mut core::ffi::c_void));
                std::mem::forget(file); // keep the handle open for the process lifetime
            }
        }
    }
}

/// Prints ranked Start Menu matches for a query.
fn cmd_launch(query: Option<String>) {
    let Some(query) = query else {
        eprintln!("minicon-winmgr: --launch needs a query");
        std::process::exit(2);
    };
    let items = launcher::discover_shortcuts();
    let matches = launcher::search(&items, &query, 20);
    if matches.is_empty() {
        println!(
            "no matches for '{query}' ({} shortcuts indexed)",
            items.len()
        );
        return;
    }
    for s in matches {
        println!("{}  —  {}", s.name, s.path);
    }
}

/// Launches the best Start Menu match for a query.
fn cmd_launch_run(query: Option<String>) {
    let Some(query) = query else {
        eprintln!("minicon-winmgr: --launch-run needs a query");
        std::process::exit(2);
    };
    let items = launcher::discover_shortcuts();
    let Some(best) = launcher::search(&items, &query, 1).into_iter().next() else {
        eprintln!("minicon-winmgr: no match for '{query}'");
        std::process::exit(1);
    };
    tracing::info!(name = %best.name, path = %best.path, "launching shortcut");
    if let Err(err) = launcher::launch(&best.path) {
        eprintln!("minicon-winmgr: launch failed: {err}");
        std::process::exit(1);
    }
    println!("launched {}", best.name);
}

/// Normal companion run: single-instance guard, then the message loop.
fn run_companion() {
    // When Windows launches us as the login shell there is no console; detach
    // any inherited console so no stray window appears. CLI subcommands return
    // before this point, so they keep their console.
    if shell_registry::is_self_shell() {
        // SAFETY: FreeConsole is harmless if no console is attached.
        let _ = unsafe { windows::Win32::System::Console::FreeConsole() };
        tracing::info!("running as the shell; console detached");
    }

    let _instance = match single_instance::InstanceGuard::acquire("minicon-winmgr.singleton") {
        Ok(guard) => guard,
        Err(err) => {
            tracing::warn!(%err, "refusing to start a second instance");
            eprintln!("minicon-winmgr: {err}");
            std::process::exit(1);
        }
    };
    tracing::info!("single-instance guard acquired");

    // Undo a work area left shrunk by a previous run that was killed before it
    // could restore it (task manager / `shellctl stop`).
    crate::taskbar::recover_workarea();

    let result = window::run();
    // The message loop only closes the message-only window, so the taskbar
    // windows never see WM_DESTROY and would not restore the reserved work
    // area. Tear them down explicitly.
    crate::taskbar::shutdown();
    if let Err(err) = result {
        tracing::error!(%err, "message loop terminated with error");
        std::process::exit(1);
    }

    tracing::info!("minicon-winmgr stopped");
}

/// Read-only: report the current per-user Shell value.
fn cmd_shell_status() {
    match shell_registry::current_shell() {
        Ok(Some(v)) => {
            tracing::info!(shell = %v, "current Winlogon Shell value");
            println!("current Shell = {v}");
        }
        Ok(None) => {
            tracing::info!("Winlogon Shell value not set (default: explorer.exe)");
            println!("current Shell = <not set> (default: explorer.exe)");
        }
        Err(err) => {
            tracing::error!(%err, "failed to read Winlogon Shell value");
            eprintln!("minicon-winmgr: {err}");
            std::process::exit(1);
        }
    }
}

/// Lists the known shell profiles and which one is currently configured.
fn cmd_shell_list() {
    let current = shell_registry::current_shell().ok().flatten();
    let rows = builtin_profiles();
    println!("{:<10} {:<10} shell path", "target", "kind");
    for (name, path) in &rows {
        let (kind, shown) = match path {
            None => ("default", "(not set -> explorer.exe)".to_string()),
            Some(p) => ("override", p.clone()),
        };
        let mark = if is_current(&current, path) {
            "  <- current"
        } else {
            ""
        };
        println!("{name:<10} {kind:<10} {shown}{mark}");
    }
    println!();
    println!("switch with: minicon-winmgr --shell-set <target|path>   (needs {CONFIRM_ENV}=yes)");
}

/// Guarded write: set the per-user Shell value to a known target or a raw path.
fn cmd_shell_set(target: Option<String>) {
    let Some(target) = target else {
        eprintln!("minicon-winmgr: --shell-set needs a target (explorer|alt|minicon|<path>)");
        std::process::exit(2);
    };
    if !confirmed() {
        refuse(
            "setting the Shell requires explicit confirmation",
            &format!("set {CONFIRM_ENV}=yes and re-run --shell-set {target}"),
        );
    }
    let value = match resolve_target(&target) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("minicon-winmgr: {err}");
            std::process::exit(1);
        }
    };
    tracing::warn!(%target, ?value, "SETTING Winlogon Shell value (confirmed)");
    let result = match &value {
        Some(v) => shell_registry::replace(v),
        None => shell_registry::restore(None),
    };
    if let Err(err) = result {
        eprintln!("minicon-winmgr: {err}");
        std::process::exit(1);
    }
    let shown = value.as_deref().unwrap_or("(default: explorer.exe)");
    println!("Shell set to {shown}");
    println!("rollback with: minicon-winmgr --shell-rollback default");
}

/// Resolves a symbolic target to a registry value (`None` = Windows default).
fn resolve_target(target: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    Ok(match target.to_ascii_lowercase().as_str() {
        "explorer" | "default" | "system" => None,
        "alt" | "altshell" => {
            Some(alt_shell_path().ok_or_else(|| {
                format!("profile 'alt' needs {ALT_SHELL_ENV}=<path to shell exe>")
            })?)
        }
        "minicon" | "minicon-winmgr" => Some(std::env::current_exe()?.display().to_string()),
        _ => Some(target.to_string()),
    })
}

/// Path of the alternative shell, from `MINICON_ALT_SHELL` (if set).
fn alt_shell_path() -> Option<String> {
    std::env::var(ALT_SHELL_ENV).ok().filter(|p| !p.is_empty())
}

/// The built-in profile table: (name, resolved Shell value).
fn builtin_profiles() -> [(&'static str, Option<String>); 3] {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string());
    [
        ("explorer", None),
        ("alt", alt_shell_path()),
        ("minicon", exe),
    ]
}

fn is_current(current: &Option<String>, path: &Option<String>) -> bool {
    match (current, path) {
        (Some(c), Some(p)) => c.eq_ignore_ascii_case(p),
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(c), None) => c.eq_ignore_ascii_case("explorer.exe"),
    }
}

/// Guarded write: point the per-user Shell at this executable.
fn cmd_shell_replace() {
    if !confirmed() {
        refuse(
            "replacing the Shell requires explicit confirmation",
            "set MINICON_CONFIRM_SHELL=yes and re-run --shell-replace",
        );
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p.display().to_string(),
        Err(err) => {
            eprintln!("minicon-winmgr: cannot resolve current exe: {err}");
            std::process::exit(1);
        }
    };
    tracing::warn!(shell = %exe, "REPLACING Winlogon Shell value (confirmed)");
    if let Err(err) = shell_registry::replace(&exe) {
        eprintln!("minicon-winmgr: {err}");
        std::process::exit(1);
    }
    println!("Shell set to {exe}");
    println!("rollback with: minicon-winmgr --shell-rollback default");
}

/// Guarded write: point the per-user Shell at this executable **and** take over
/// the current desktop now (stop the running shell in this session).
fn cmd_shell_takeover() {
    if !confirmed() {
        refuse(
            "taking over the shell requires explicit confirmation",
            "set MINICON_CONFIRM_SHELL=yes and re-run --shell-takeover",
        );
    }
    tracing::warn!("TAKING OVER the shell now (confirmed)");
    match shell_swap::take_over() {
        Ok(report) => {
            println!("Shell set to {}", report.exe);
            println!(
                "stopped {} shell process(es) in this session",
                report.stopped
            );
            if report.auto_restart_disabled {
                println!("shell auto-restart disabled");
            } else {
                println!(
                    "warn: could not disable shell auto-restart (needs elevation); \
                     Windows may relaunch explorer"
                );
            }
            println!("give the desktop back with: minicon-winmgr --shell-give-back");
        }
        Err(err) => {
            eprintln!("minicon-winmgr: {err}");
            std::process::exit(1);
        }
    }
}

/// Guarded write: restore the recorded shell **and** bring the desktop back now.
fn cmd_shell_give_back() {
    if !confirmed() {
        refuse(
            "restoring the Shell requires explicit confirmation",
            "set MINICON_CONFIRM_SHELL=yes and re-run --shell-give-back",
        );
    }
    tracing::warn!("GIVING BACK the shell now (confirmed)");
    match shell_swap::give_back() {
        Ok(report) => {
            let shown = report
                .restored
                .as_deref()
                .unwrap_or("(default: explorer.exe)");
            println!("Shell restored to {shown}");
            if report.started {
                println!("launched explorer.exe");
            } else {
                println!("a shell is already running in this session");
            }
        }
        Err(err) => {
            eprintln!("minicon-winmgr: {err}");
            std::process::exit(1);
        }
    }
}

/// Guarded write: restore a previous Shell value (or delete it with "default").
fn cmd_shell_rollback(previous: Option<String>) {
    if !confirmed() {
        refuse(
            "restoring the Shell requires explicit confirmation",
            "set MINICON_CONFIRM_SHELL=yes and re-run --shell-rollback <value|default>",
        );
    }
    let target = match previous.as_deref() {
        Some("default") => None,
        Some(v) => Some(v),
        None => {
            eprintln!("minicon-winmgr: --shell-rollback needs a value or 'default'");
            std::process::exit(2);
        }
    };
    tracing::warn!(?target, "RESTORING Winlogon Shell value (confirmed)");
    if let Err(err) = shell_registry::restore(target) {
        eprintln!("minicon-winmgr: {err}");
        std::process::exit(1);
    }
    println!("Shell restored");
}

fn confirmed() -> bool {
    std::env::var(CONFIRM_ENV)
        .map(|v| v.eq_ignore_ascii_case("yes"))
        .unwrap_or(false)
}

fn refuse(what: &str, hint: &str) -> ! {
    tracing::error!(%what, "refusing unconfirmed Shell registry write");
    eprintln!("minicon-winmgr: {what}; {hint}");
    std::process::exit(3);
}

fn print_help() {
    println!(
        "minicon-winmgr — companion Windows shell / session manager\n\
         \n\
         Usage:\n\
           minicon-winmgr                     run the shell (companion mode)\n\
           minicon-winmgr --shell-status      print the per-user Winlogon Shell value\n\
           minicon-winmgr --shell-list        list known shell profiles + current\n\
           minicon-winmgr --shell-set <t>     set Shell to explorer|alt|minicon|<path>\n\
                                              (needs {CONFIRM_ENV}=yes)\n\
           minicon-winmgr --shell-replace     set Shell to this exe (needs {CONFIRM_ENV}=yes)\n\
           minicon-winmgr --shell-takeover    replace the shell AND take over now\n\
                                              (needs {CONFIRM_ENV}=yes)\n\
           minicon-winmgr --shell-give-back   restore the shell AND bring the desktop back\n\
                                              (needs {CONFIRM_ENV}=yes)\n\
           minicon-winmgr --shell-rollback <value|default>\n\
                                              restore Shell (needs {CONFIRM_ENV}=yes)\n\
           minicon-winmgr --launch <query>    list Start Menu matches for a query\n\
           minicon-winmgr --launch-run <query> launch the best Start Menu match\n\
           minicon-winmgr --help              show this help\n\
         \n\
         Taskbar keys:\n\
           Alt+`                              summon the bar and start keyboard nav\n\
           Left / Right                       move the task cursor\n\
           Enter                              switch to the selected task\n\
           Esc                                cancel navigation\n\
         \n\
         Env:\n\
           MINICON_NO_TASKBAR=1     hide the taskbar host window (shown by default)\n\
           MINICON_ACRYLIC=1        use the DWM acrylic backdrop (best-effort)\n\
           MINICON_HEARTBEAT_SECS=N heartbeat/snapshot interval (default 30)\n\
           MINICON_LOG_DIR=path     log directory (default <exe_dir>/logs)\n\
           MINICON_ALT_SHELL=path   exe for the 'alt' shell profile (optional)\n\
           MINICON_CONFIRM_SHELL=yes  required to run any --shell-* write command\n\
           RUST_LOG=...             tracing filter (default info)\n"
    );
}
