# minicon-winmgr

[Language index](readme.md) · **English** · [简体中文](readme.zh.md)

**Stability first. A minimal footprint.**

A lightweight Windows desktop shell built with Rust and Win32, designed as a replacement for the system desktop shell. Stability, memory safety, and low memory use guide the design: everyday actions stay direct, and optional capabilities are enabled when needed. The current default is companion mode; replacing the desktop shell requires explicit takeover.

## Why minicon-winmgr

- **Stability at the core.** Rust's ownership and type systems reduce memory-error risks, with explicit handle and resource-lifetime management at Win32 boundaries. Failed settings saves preserve the original file and support retries; shutdown restores the work area. Full release regression checks provide reproducible evidence of reliability.
- **A lean native foundation.** Rust and Win32 without an embedded browser engine; optional features initialize on demand. Version 0.1.10 adds bounded resource handling and optional diagnostics. [Measurements](docs/validation_0_1_10.md) describe their actual scenarios; longer observations remain separate work.
- **Minimal by default, optional by choice.** Everyday paths stay focused on the taskbar and settings. Thumbnail previews are off by default and do not create a preview window; companion mode leaves Explorer's notification host in place. Optional capabilities remain a user choice, keeping extra controls and default overhead down.

Product principle: **Optional features you do not need should not load by default.** In 0.1.8, the legacy menu is not initialized by default, previews do not create a window until enabled, and Run creates its window and workers on first use. The notification host initializes according to shell identity and explicit configuration. See the [0.1.8 design and acceptance](docs/archive/plan_0_1_8.md).


Version: **[0.1.12](https://github.com/partnernetsoftware/minicon-winmgr/releases/tag/v0.1.12)**. See the [release notes](docs/releases/v0.1.12.md), [product requirements](docs/prd.md), and [implementation plan](docs/plan_0_1_12.md). Linked design and specification documents are currently primarily in Chinese.

Version 0.1.12 verifies recovery records before takeover, installs the notification host during runtime handover, and uses one essential startup-readiness gate. Desktop management distinguishes this session, next sign-in and recovery status.

Version 0.1.11 strengthens ordinary-privilege startup, formats the clock as `Www yyyy-mm-dd HH:MM`, and previews Run candidates in the input. Version 0.1.10 remembers your layout, lets you resize a side taskbar, opens the Recycle Bin from the clock panel, and asks before switching from another running build. It fixes the bottom gap under maximized windows after moving the taskbar to a side. Manual Check / Update / Restart, English/Chinese UI, Light/Dark/Auto themes and on-demand features remain. Elevated launches restart as the same user with standard privileges; the application does not request UAC. See the [release evidence and coverage limits](docs/validation_0_1_10.md).

## Settings preview

Actual native Win32 windows, captured from 0.1.10. Language changes apply immediately and are saved. Auto theme follows the Windows application theme.

| English · Light | English · Dark |
|---|---|
| ![English light settings](docs/images/settings_en_light.png) | ![English dark settings](docs/images/settings_en_dark.png) |

| Chinese · Light | Chinese · Dark |
|---|---|
| ![Chinese light settings](docs/images/settings_zh_light.png) | ![Chinese dark settings](docs/images/settings_zh_dark.png) |

Screenshots render only this application's windows.

See the [UI design](docs/design.md) and [test layers](docs/testing.md).

## Capabilities

- Taskbars on multiple monitors; window activation/minimization; grouping by application or process; adaptive density and overflow menus; pinned shortcuts and folder entries.
- Alt+Space launcher and Win+R Run dialog, with Ctrl+Alt+R as the registration fallback. Run supports history, path suggestions, Chinese IME, and launch-error feedback.
- Real notification icons in replacement mode, versioned callbacks, a paged hidden-icon panel, and application re-registration recovery. Companion mode retains Explorer's notification host without drawing inert application placeholders.
- Session cleanup, logs and resource metrics, work-area reservation/restoration, and explicit shell takeover and rollback.

Settings is the remaining entry on the leading side of the taskbar. The start-menu dot and window-list button were retired in 0.1.2; the legacy menu remains behind a hidden switch. Sound/network entries open system settings; power/input-method status icons are not implemented. Settings uses native controls with keyboard navigation, scrolling, configuration-source information, and save retries.

## Download

Download the Windows x64 executable and `SHA256SUMS.txt` from [GitHub Releases](https://github.com/partnernetsoftware/minicon-winmgr/releases/latest). Normal use does not require administrator privileges. Exit the old instance normally before upgrading, and retain your personal configuration. Binaries are not yet code-signed.

## Update from settings

Use **Check → Update → Restart**. Checking is manual; startup and opening settings do not contact an update service. Update downloads and verifies the stable Windows x64 release while your current version keeps running. Restart becomes available after verification and after settings are saved. It restarts winmgr only; your applications remain open.

Keep winmgr in a folder your normal account can write. If that folder is read-only, download a release and replace the executable manually. Updates do not request UAC. A temporary helper keeps the previous executable as a backup, waits for the old instance to exit, then checks the new instance's startup receipt. Failed replacement/startup attempts restore the old executable when the filesystem permits it.

Staging and backups are kept in `.winmgr-update-*` folders beside the executable. After confirming the new version works, older completed backup folders can be removed manually. Normal launches do not scan these folders or resume a pending update. Technical failure details are available in Configuration & about and logs.

## Build, use, and configure

```powershell
cargo build
New-Item -ItemType Directory -Force .\target\debug\live | Out-Null
Copy-Item .\target\debug\minicon-winmgr.exe .\target\debug\live\minicon-winmgr.exe
.\target\debug\live\minicon-winmgr.exe
```

If an instance is running, `minicon-winmgr.exe --settings` opens its settings window. This also works with the taskbar disabled. During development, build first, exit the old instance, then copy and run the live executable to avoid locking Cargo's build artifacts.

Click the clock to open the notification panel, which includes Date & time settings. Double-click the clock to open those system settings directly.

Alt+backtick focuses the taskbar; use arrow keys to select, Home/End to reach either end, Enter/Space to activate, and Esc to return to the previous window. Win+B opens the notification panel, supporting arrow keys, Home/End, Tab, Enter/Space, Shift+F10, and Esc. Window snapping uses Win+arrow, with Ctrl+Alt+arrow as the fallback when registration conflicts. Actual hotkey registration results are currently recorded mainly in logs.

Preferences are saved automatically to `~/winmgr.cnf` (`%USERPROFILE%\winmgr.cnf`) and restored on launch without arguments. Drag the inner edge of a left/right taskbar to resize its width; the chosen width is saved too. The previous executable-adjacent `minicon-settings.txt` is read when the new file is absent and copied into it on the first successful settings save; the old file remains untouched. Configuration precedence: environment variables > `winmgr.cnf` > `minicon.conf` > defaults. `MINICON_CONFIG` and `MINICON_SETTINGS` override file paths. Settings writes are asynchronous, update individual keys, and create backups. Failed saves preserve the original file, display status, and can be retried; unrelated handwritten configuration is retained. Valid environment overrides are read-only in settings, with a short fixed-setting hint. Technical source details are available in Configuration & About. The authoritative key table is maintained in the [taskbar and configuration specification](docs/prd_6_1.md).

Taskbar visibility changes the next-start preference; the panel also reports the current state. Hover previews can be toggled at runtime. Run expands defined `%VARIABLE%` references in both targets and arguments, preserves unknown variables, and suggests specifying an editor when a file's default application is unavailable.

Shell takeover immediately closes the current session's desktop shell and changes the current user's shell for subsequent logins. Read the [takeover and rollback specification](docs/prd_3_1.md) first, and retain rollback records. Command-line changes require explicit `MINICON_CONFIRM_SHELL=yes`; settings uses OK/Cancel with Cancel as the default. Ordinary development does not require takeover. In 0.1.12, takeover requires a verified account-bound recovery record and a reachable Minicon launch entry. Restore pre-takeover settings changes the next sign-in; Minicon continues managing the current session. Legacy `shell-backup.txt` files are not adopted automatically. See the specification for explicit manual recovery when no verified record exists.

## Development and documentation

- [Code and documentation map](docs/code-map.md): module boundaries, gaps, and evidence.
- [Implementation plan](docs/archive/plan_0_1_6.md): settings usability and feedback, including the 0.1.7 release follow-up.
- [0.1.8 design and delivery](docs/archive/plan_0_1_8.md): on-demand startup and daily interactions.
- [0.1.9 update design](docs/archive/plan_0_1_9.md): Check / Update / Restart.
- [Future capabilities](docs/prd_5_1.md): unscheduled requirements and prerequisites.
- [Archive](docs/archive/README.md): historical plans, roadmaps, and documentation.

Use `python scripts/test.py` for quick development checks. `--layer unit --filter config::tests` runs one test module; `--layer appearance --restore-exe <normal-running-exe>` or `--layer settings --restore-exe <normal-running-exe>` runs the corresponding desktop checks on an isolated copy. Add `--candidate-exe <existing-exe>` to reuse a build. Before a release, `--layer release` runs the full automated suite against an optimized build. See [test layers](docs/testing.md) for scope and environment requirements. Release validation builds in `target/release-validation` to avoid locks from older instances. Windows CI runs `quality`, the isolated language/concurrency unit matrix and bounded `fault` checks. A manually triggered candidate workflow runs the optimized unit matrix and builds the file for desktop/resource acceptance. The tag workflow verifies provenance and promotes that exact tested file into a draft; publication follows evidence and screenshot review. Code signing is not included. For custom toolchain locations, set `RUSTUP_HOME`/`CARGO_HOME` and add Cargo's bin directory to PATH.

See the [release acceptance records](docs/validation.md), including [0.1.12 evidence](docs/validation_0_1_12.md). Historical 0.1.6 evidence remains in its [plan](docs/archive/plan_0_1_6.md) and [acceptance data](docs/validation.md). Physical mixed-DPI monitors, monitor hot-plugging, screen-reader speech, high-contrast display, and hour-long stability remain outside the current coverage.
