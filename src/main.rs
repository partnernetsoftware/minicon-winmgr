mod config;
mod dwm;
mod grouping;
mod launcher;
mod logging;
mod monitor;
mod session;
mod shell_registry;
mod single_instance;
mod taskbar;
mod win_util;
mod window;
mod window_snapshot;

/// Second-confirmation token required before any Shell-registry write. The
/// default companion run never needs it, so the registry stays untouched.
const CONFIRM_ENV: &str = "MINICON_CONFIRM_SHELL";

/// Default install path of Cairo Desktop (override with `MINICON_CAIRO_PATH`).
const CAIRO_DEFAULT: &str = r"C:\Program Files\Cairo Shell\CairoDesktop.exe";

enum Cli {
    RunShell,
    ShellStatus,
    ShellList,
    ShellSet(Option<String>),
    ShellReplace,
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
        Cli::ShellRollback(prev) => cmd_shell_rollback(prev),
        Cli::Launch(query) => cmd_launch(query),
        Cli::LaunchRun(query) => cmd_launch_run(query),
        Cli::Help => print_help(),
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
    let _instance = match single_instance::InstanceGuard::acquire("minicon-winmgr.singleton") {
        Ok(guard) => guard,
        Err(err) => {
            tracing::warn!(%err, "refusing to start a second instance");
            eprintln!("minicon-winmgr: {err}");
            std::process::exit(1);
        }
    };
    tracing::info!("single-instance guard acquired");

    if let Err(err) = window::run() {
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
        eprintln!("minicon-winmgr: --shell-set needs a target (explorer|cairo|minicon|<path>)");
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
        "cairo" => Some(cairo_path()),
        "minicon" | "minicon-winmgr" => Some(std::env::current_exe()?.display().to_string()),
        _ => Some(target.to_string()),
    })
}

fn cairo_path() -> String {
    std::env::var("MINICON_CAIRO_PATH").unwrap_or_else(|_| CAIRO_DEFAULT.to_string())
}

/// The built-in profile table: (name, resolved Shell value).
fn builtin_profiles() -> [(&'static str, Option<String>); 3] {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string());
    [
        ("explorer", None),
        ("cairo", Some(cairo_path())),
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
           minicon-winmgr --shell-set <t>     set Shell to explorer|cairo|minicon|<path>\n\
                                              (needs {CONFIRM_ENV}=yes)\n\
           minicon-winmgr --shell-replace     set Shell to this exe (needs {CONFIRM_ENV}=yes)\n\
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
           MINICON_CAIRO_PATH=path  Cairo Desktop exe path (default {CAIRO_DEFAULT})\n\
           RUST_LOG=...             tracing filter (default info)\n"
    );
}
