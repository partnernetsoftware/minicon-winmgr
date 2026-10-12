# minicon-winmgr
Rust Windows shell / session manager (Cairo Desktop-class).
## Motivation
- Stock `explorer.exe` is often unstable for long-lived company desktop sessions.
- [Cairo Desktop](https://github.com/cairoshell/cairoshell) improves the shell experience but still frequently freezes in our environment.
- This project rebuilds a Windows shell in **Rust**, and adds **company-oriented Windows session management** on top of a Cairo-class desktop shell.
## Direction
- Win-shell replacement / companion focused on reliability and session lifecycle.
- Session management for company workstations (start, recover, isolate, audit-friendly operations).
- Iterate as a normal git repo; **no release packaging or code signing** in early phases.
- Product requirements, benchmark feature tree, and the milestone plan live in [`docs/prd.md`](docs/prd.md).

## Status
Early phases, **companion mode** (does not touch the Winlogon `Shell` registry value).
- **M0 — observability** done: tracing to rolling file + stderr, panic hook, single-instance guard.
- **M1 — message loop** done: Win32 message-only window, message pump, graceful Ctrl+C shutdown.
- **M2 — window awareness** done: `EnumWindows` snapshot with class/title/pid/visibility.
- **M3 — taskbar host** done: self-painted, text-first 36px bar (**shown by default**, `MINICON_NO_TASKBAR=1` to hide) — brand dot, task buttons (hover + active), click-to-focus, task count + clock. Keyboard nav via **Alt+`** then ←/→ + Enter. See [`docs/ui-ux.md`](docs/ui-ux.md).
- **M4 — session lifecycle** done: WTS session-change notifications + `WM_QUERYENDSESSION/ENDSESSION` handling.
- **M5 — shell replacement** implemented as a *controlled* switch: `--shell-status` is read-only; `--shell-replace` / `--shell-rollback` require a second confirmation token (`MINICON_CONFIRM_SHELL=yes`) and were **not** executed. The default run never touches the registry.

See `docs/prd.md` for the benchmark feature tree, thinking tree, and full milestone plan.

## Entry points

Unified shell switcher (view / start / switch between Windows explorer, Cairo Desktop, minicon-winmgr):

```powershell
.\scripts\shellctl.ps1 status                    # current per-user Shell value
.\scripts\shellctl.ps1 list                      # known profiles + which is current
.\scripts\shellctl.ps1 start  -Target minicon    # launch a shell now (no registry change)
.\scripts\shellctl.ps1 stop   -Target minicon
.\scripts\shellctl.ps1 restart -Target cairo
.\scripts\shellctl.ps1 switch -Target minicon -Confirm   # write Shell + swap processes (guarded)
```

`scripts\shellctl.cmd` is a double-click wrapper. Direct binary flags:

| Flag | Effect |
|------|--------|
| *(none)* | companion run (message loop; registry untouched) |
| `--shell-status` | print per-user `Shell` value (read-only) |
| `--shell-list` | list profiles + current |
| `--shell-set <explorer\|cairo\|minicon\|path>` | set `Shell` (needs `MINICON_CONFIRM_SHELL=yes`) |
| `--shell-replace` / `--shell-rollback <value\|default>` | guarded writes (same token) |
| `--launch <query>` | list ranked Start Menu matches |
| `--launch-run <query>` | launch the best Start Menu match |
| `--help` | usage |

Env: `MINICON_NO_TASKBAR=1` (hide bar), `MINICON_ACRYLIC=1` (DWM acrylic backdrop, best-effort), `MINICON_HEARTBEAT_SECS=N`, `MINICON_LOG_DIR=path`, `MINICON_CAIRO_PATH=path`, `MINICON_CONFIG=path` (config file), `RUST_LOG=...`.

Config file (`minicon.conf`, or `MINICON_CONFIG`): `key=value`, `#`/`;` comments. Keys: `theme.bg|surface|hover|accent|text|muted` (`#RRGGBB`), `hotkey`, `show_taskbar`.

## Modules

| Module | Role |
|--------|------|
| `logging` / `single_instance` | M0 observability + single-instance guard |
| `window` / `window_snapshot` | M1 message loop, M2 window enumeration |
| `taskbar` / `dwm` | M3 self-painted bar, rounded corners + acrylic |
| `session` | M4 WTS session lifecycle |
| `shell_registry` | M5 controlled Shell switch |
| `grouping` / `launcher` / `monitor` / `config` | task grouping, Start Menu search, monitors, theme/config |
| `tray` / `launcher_ui` | M6: notification-area mirror, Alt+Space launcher popup |
| `tray_host` / `start_menu` | M7: replacement-mode tray host (`MINICON_TRAY_HOST=1`), ORB start menu |

Reference implementation notes (ideas worth borrowing): [`docs/reference-tinywm.md`](docs/reference-tinywm.md). Replacement readiness gate: [`docs/replacement-readiness.md`](docs/replacement-readiness.md).

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `clippy -D warnings`, and `cargo test` on Windows.

## Build
```bat
set RUSTUP_HOME=C:\Users\wjc2022\.rustup
set CARGO_HOME=C:\Users\wjc2022\.cargo
set PATH=C:\Users\wjc2022\.cargo\bin;%PATH%
cargo build
```
