# minicon-winmgr

A minimal, reliability-focused Windows shell / session manager written in Rust.

## Motivation

- The stock Windows shell (`explorer.exe`) is often unstable for long-lived company desktop sessions.
- Third-party desktop shells improve the experience but can still freeze in our environment.
- This project rebuilds a Windows shell in **Rust** and adds **company-oriented session management** on top of a lightweight, observable taskbar.

## Direction

- Shell replacement / companion focused on reliability and session lifecycle.
- Session management for company workstations (start, recover, isolate, audit-friendly operations).
- Iterate as a normal git repo; **no release packaging or code signing** in early phases.
- Requirements, thinking tree, and the milestone plan live in [`docs/prd.md`](docs/prd.md).

## Status

- **M0 — observability**: tracing to rolling file + stderr, panic hook, single-instance guard.
- **M1 — message loop**: Win32 message-only window, message pump, graceful Ctrl+C shutdown.
- **M2 — window awareness**: `EnumWindows` snapshot with class/title/pid/visibility.
- **M3 — taskbar host**: self-painted, text-first 36px bar (**shown by default**, `MINICON_NO_TASKBAR=1` to hide) — brand dot, task buttons (hover + active), click-to-focus, task count + clock. Keyboard nav via **Alt+`** then ←/→ + Enter. See [`docs/ui-ux.md`](docs/ui-ux.md).
- **M4 — session lifecycle**: WTS session-change notifications + `WM_QUERYENDSESSION/ENDSESSION` handling.
- **M5 — controlled shell switch**: `--shell-status` / `--shell-list` are read-only; writes (`--shell-set` / `--shell-replace` / `--shell-rollback`) require an explicit confirmation token (`MINICON_CONFIRM_SHELL=yes`). Runs in companion mode by default and leaves the registry untouched.
- **M6/M7 — shell surface**: per-app grouped tasks, one bar per monitor, notification-area mirror + (replacement-mode) tray host, start menu (apps + Places + power) and Alt+Space launcher, AppBar docking.
- **M8 — stability + input**: periodic memory/handle/GDI metrics, launch-frequency recents feeding ranking, window-snap hotkeys, Win+R run dialog, and a mirrored tray-icon strip on the bar's right zone.
- **M9 — taskbar enhancements**: quick-launch pins (the most-used shortcuts, left of the bar), a window-list button (popup of every task group), and `Win+* → Ctrl+Alt+*` fallback for the snap hotkeys.
- **M10 — hover preview (opt-in)**: hovering a task button for ~350ms shows a live thumbnail popup (`PrintWindow`), anchored above the bar and flipped/clamped to the monitor. **Off by default**; enable with `MINICON_HOVER_PREVIEW=1`.

### Hotkeys

| Keys | Action |
|------|--------|
| **Alt+`** | focus the taskbar, then ←/→ + Enter to switch windows, Esc to cancel |
| **Alt+Space** | jump-to-app launcher (ranked by launch frequency) |
| **Ctrl+Alt+R** | run dialog (falls back here when another shell already owns **Win+R**) |
| **Win+←/→/↑** | snap the focused window (falls back to **Ctrl+Alt+←/→/↑** when taken) |

All hotkeys degrade gracefully with a log line when another shell already owns the combo.

Replacement readiness gate and rollback: [`docs/replacement-readiness.md`](docs/replacement-readiness.md).

## Entry points

Unified shell switcher (view / start / switch between the Windows default shell, an alternative shell, and minicon-winmgr):

```powershell
.\scripts\shellctl.ps1 status                    # current per-user Shell value
.\scripts\shellctl.ps1 list                      # known profiles + which is current
.\scripts\shellctl.ps1 start  -Target minicon    # launch a shell now (no registry change)
.\scripts\shellctl.ps1 stop   -Target minicon
.\scripts\shellctl.ps1 restart -Target alt
.\scripts\shellctl.ps1 switch -Target minicon -Confirm   # write Shell + swap processes (guarded)
```

`scripts\shellctl.cmd` is a double-click wrapper. The `alt` profile points at whatever
`MINICON_ALT_SHELL` names (no path is baked in).

Direct binary flags:

| Flag | Effect |
|------|--------|
| *(none)* | companion run (message loop; registry untouched) |
| `--shell-status` | print per-user `Shell` value (read-only) |
| `--shell-list` | list profiles + current |
| `--shell-set <explorer\|alt\|minicon\|path>` | set `Shell` (needs `MINICON_CONFIRM_SHELL=yes`) |
| `--shell-replace` / `--shell-rollback <value\|default>` | guarded writes (same token) |
| `--launch <query>` | list ranked Start Menu matches |
| `--launch-run <query>` | launch the best Start Menu match |
| `--help` | usage |

Env: `MINICON_NO_TASKBAR=1`, `MINICON_ACRYLIC=1`, `MINICON_HOVER_PREVIEW=1`, `MINICON_HEARTBEAT_SECS=N`, `MINICON_LOG_DIR=path`,
`MINICON_ALT_SHELL=path`, `MINICON_TRAY_HOST=1|0`, `MINICON_CONFIG=path`, `RUST_LOG=...`.

Config file (`minicon.conf`, or `MINICON_CONFIG`): `key=value`, `#`/`;` comments.
Keys: `theme.bg|surface|hover|accent|text|muted` (`#RRGGBB`), `hotkey`, `show_taskbar`.

## Modules

| Module | Role |
|--------|------|
| `logging` / `single_instance` | M0 observability + single-instance guard |
| `window` / `window_snapshot` | M1 message loop, M2 window enumeration |
| `taskbar` / `dwm` | M3 self-painted bar, AppBar docking, rounded corners + acrylic |
| `session` | M4 WTS session lifecycle |
| `shell_registry` | M5 controlled Shell switch |
| `grouping` / `launcher` / `monitor` / `config` | task grouping, Start Menu search, monitors, theme/config |
| `tray` / `launcher_ui` | M6: notification-area mirror, Alt+Space launcher popup |
| `tray_host` / `start_menu` | M7: replacement-mode tray host, ORB start menu |
| `metrics` / `hotkeys` / `recents` | M8: stability sampling, snap hotkeys, launch-frequency store |
| `run_dialog` / `tray_bar` | M8: Win+R run dialog, tray-icon strip in the bar's right zone |
| `quick_launch` / `task_list` | M9: pinned quick-launch buttons, window-list popup |
| `hover_preview` | M10: hover thumbnail popup (`PrintWindow`, throttled by `WM_TIMER`) |

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `clippy -D warnings`, and `cargo test` on Windows.

## Build

```bat
cargo build
```

If your toolchain lives outside the default location, set `RUSTUP_HOME` and `CARGO_HOME`
to your own paths and put `%CARGO_HOME%\bin` on `PATH`.
