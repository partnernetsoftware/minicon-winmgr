> 2026-10-15 归档：整理前 README，包含 M0–M11 历史叙述。当前入口见 [README](../../readme.md)。

﻿# minicon-winmgr

A minimal, reliability-focused Windows shell / session manager written in Rust.

## Motivation

- The stock Windows shell (`explorer.exe`) is often unstable for long-lived company desktop sessions.
- Third-party desktop shells improve the experience but can still freeze in our environment.
- This project rebuilds a Windows shell in **Rust** and adds **company-oriented session management** on top of a lightweight, observable taskbar.

## Direction

- Shell replacement / companion focused on reliability and session lifecycle.
- Session management for company workstations (start, recover, isolate, audit-friendly operations).
- Iterate as a normal git repo; **no release packaging or code signing** in early phases.
- Requirements, thinking tree, and the milestone plan live in [`docs/prd.md`](../prd.md).

## Status

- **M0 — observability**: tracing to rolling file + stderr, panic hook, single-instance guard.
- **M1 — message loop**: Win32 message-only window, message pump, graceful Ctrl+C shutdown.
- **M2 — window awareness**: `EnumWindows` snapshot with class/title/pid/visibility.
- **M3 — taskbar host**: self-painted, text-first 36px bar (**shown by default**, `MINICON_NO_TASKBAR=1` to hide) — settings/menu/quick-launch/list buttons, one button per window, clock. Keyboard nav via **Alt+`** then ←/→ + Enter. See [`docs/prd_4_1.md`](../prd_4_1.md).
- **M4 — session lifecycle**: WTS session-change notifications + `WM_QUERYENDSESSION/ENDSESSION` handling.
- **M5 — controlled shell switch**: `--shell-status` / `--shell-list` are read-only; writes (`--shell-set` / `--shell-replace` / `--shell-takeover` / `--shell-give-back` / `--shell-rollback`) require an explicit confirmation token (`MINICON_CONFIRM_SHELL=yes`). Runs in companion mode by default and leaves the registry untouched. **Takeover / give-back are immediate**: they write the registry *and* hand the desktop over in the current session (stop the running shell / detached-launch `explorer.exe`), no sign-out needed — see [`docs/prd_3_1.md`](../prd_3_1.md) §1b.
- **M6/M7 — shell surface**: task buttons (one per window by default; optional per-app grouping), one bar per monitor, notification-area mirror + (replacement-mode) tray host, start menu (apps + Places + power) and Alt+Space launcher, AppBar docking.
- **M8 — stability + input**: periodic memory/handle/GDI metrics, launch-frequency recents feeding ranking, window-snap hotkeys, Win+R run dialog, and a mirrored tray-icon strip on the bar's right zone.
- **M9 — taskbar enhancements**: **quick-launch pins** (user-curated, left of the bar — right-click a window button and pick *添加到快速启动*; the list persists in `quick-launch.txt` beside the exe), a window-list button (popup of every task group — removed from the UI in v0.1.2), and `Win+* → Ctrl+Alt+*` fallback for the snap hotkeys.
- **M10 — hover preview (opt-in)**: hovering a task button for ~350ms shows a live thumbnail popup (`PrintWindow`), anchored above the bar and flipped/clamped to the monitor. **Off by default**; enable with `MINICON_HOVER_PREVIEW=1`.
- **M11 — taskbar polish**: a **settings button** (left of the menu dot) opens a settings window with **real GDI-drawn controls** — window **merge policy** (never / when the bar is full / always), **grouping basis** (by application exe path, default, or by pid), show-taskbar / notification-strip / hover-preview toggles — persisted to `minicon-settings.txt` beside the exe (key-level upsert + `.bak`; the hand-written `minicon.conf` is never rewritten), plus **接管桌面（立即生效）** / **还原系统桌面（立即生效）** actions (takeover writes the registry *and* stops the running shell now; give-back restores the backed-up value and re-launches `explorer.exe`). Task buttons are **one per window by default** (matching Alt+Tab); `MINICON_GROUP_TASKS=1` remains as an env override (≡ `taskbar.group=always`). When the bar gets crowded it steps down a **density ladder** (full → compact, with an 8px hysteresis dead zone) before folding the tail behind a `»` overflow button — the clock zone is never covered. The bar anchors to the **full monitor bounds** so it sits flush to the physical bottom, and when AppBar docking is unavailable (no explorer broker) we **shrink the work area ourselves** (`SPI_SETWORKAREA`) so maximized windows stop above the bar. The reserved area is restored on exit **and** (because a `WM_DESTROY` never fires on the normal path) recovered on the next start from a persisted `minicon-workarea.state`, so a crash or hard kill no longer leaves a dead strip at the screen bottom. A second pass adds an **icon-only** densest tier (title dropped; a hover bubble names the button), keyboard **1..9** jump inside navigation, a **group right-click menu** (open / restore / minimize / close all, pin all), a toggleable **notification-area strip** (`taskbar.tray`) — fully live in replace mode (real icons, and clicks forwarded back to the owning app as notify-icon callbacks) and an explicit Windows-managed explanation in companion mode — **pinned folder stacks**, and a **toggleable start menu** (`taskbar.menu`, **off by default**). In **v0.1.2** both the start-menu entry dot and the window-list button were **retired from the UI** — the settings button is now the only control left of the task strip.

**Release notes**: `CHANGELOG.md` (historical file, absent from the current tree) — **v0.1.5** (2026-10-15): real notification icons, version-aware callbacks, a paged hidden-icons panel, and sound/network settings shortcuts. Companion mode keeps Explorer's notification host and does not paint inert application placeholders.
Delivery, compatibility and validation limits: [`docs/plan_0_1_5.md`](../plan_0_1_5.md). Keyboard: **Win+B** opens the notification panel; use arrows, Home/End, Tab, Enter/Space, Shift+F10 and Esc. Power/input-method status icons are not implemented; physical mixed-DPI multi-monitor validation remains outstanding. Later roadmap: [`docs/plan_0_2_0.md`](../plan_0_2_0.md).

### Hotkeys

| Keys | Action |
|------|--------|
| **Alt+`** | focus the taskbar, then ←/→ + Enter to switch windows, Esc to cancel |
| **Alt+Space** | jump-to-app launcher (ranked by launch frequency) |
| **Ctrl+Alt+R** | run dialog (falls back here when another shell already owns **Win+R**) |
| **Win+←/→/↑** | snap the focused window (falls back to **Ctrl+Alt+←/→/↑** when taken) |

All hotkeys degrade gracefully with a log line when another shell already owns the combo.

Replacement readiness gate and rollback: [`docs/prd_3_1.md`](../prd_3_1.md). The full PRD is [`docs/prd.md`](../prd.md) (with `prd_*.md` sub-documents).

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
| `--shell-takeover` | replace the shell **and** take the desktop over now (same token) |
| `--shell-give-back` | restore the recorded shell **and** bring the desktop back now (same token) |
| `--launch <query>` | list ranked Start Menu matches |
| `--launch-run <query>` | launch the best Start Menu match |
| `--help` | usage |

Env vars: full, authoritative list in [`docs/prd_1_1.md`](../prd_1_1.md) §环境变量 / 开关 (single source of truth; not duplicated here).

Config file (`minicon.conf`, or `MINICON_CONFIG`): `key=value`, `#`/`;` comments.
Keys and defaults: authoritative table in [`docs/prd_6_1.md`](../prd_6_1.md) §6 (single source of truth; not duplicated here).
The settings window writes `minicon-settings.txt` (or `MINICON_SETTINGS`); precedence is **env > minicon-settings.txt > minicon.conf > defaults**.

## Modules

| Module | Role |
|--------|------|
| `logging` / `single_instance` | M0 observability + single-instance guard |
| `window` / `window_snapshot` | M1 message loop, M2 window enumeration |
| `taskbar` / `dwm` | M3 self-painted bar, AppBar docking, rounded corners + acrylic |
| `session` | M4 WTS session lifecycle |
| `shell_registry` / `shell_swap` | M5 controlled Shell switch; immediate in-session handover (stop / re-launch the shell) |
| `shell_launch` | the single, shell-safe "open" exit: never lets a bare `explorer.exe` reclaim the shell |
| `grouping` / `launcher` / `monitor` / `config` | task grouping, Start Menu search, monitors, theme/config |
| `tray` / `launcher_ui` | M6: notification-area mirror, Alt+Space launcher popup |
| `tray_host` / `start_menu` | M7: replacement-mode tray host, ORB start menu (its UI entry was retired 2026-10-14; still reachable via the `taskbar.menu` / `MINICON_START_MENU` escape hatch) |
| `folder_stack` | pinned folder rows in the start menu; expands to a bounded child popup |
| `metrics` / `hotkeys` / `recents` | M8: stability sampling, snap hotkeys, launch-frequency store |
| `run_dialog` / `tray_bar` | M8: Win+R run dialog, tray-icon strip in the bar's right zone |
| `quick_launch` / `task_list` | M9: user-curated quick-launch pins (persisted), window-list popup |
| `hover_preview` | M10: hover thumbnail popup (`PrintWindow`, throttled by `WM_TIMER`) |
| `settings_ui` | M11: settings window (real GDI-drawn controls) opened from the bar's settings button |

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `clippy -D warnings`, and `cargo test` on Windows.

## Build

```bat
cargo build
```

If your toolchain lives outside the default location, set `RUSTUP_HOME` and `CARGO_HOME`
to your own paths and put `%CARGO_HOME%\bin` on `PATH`.
