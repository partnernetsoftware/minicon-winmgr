//! Settings dialog.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Opened from the taskbar's settings button (left of the menu button).
//! Self-contained: owns its window class and a single reused window.
//!
//! Style: a plain framed window (caption + system menu) drawn with GDI to match
//! the taskbar's dark theme. Content is a list of rows driven by [`build_rows`]:
//! headings, read-only info, and — the reason this window exists — real
//! single-choice radios and checkboxes implemented as pure geometry + hit
//! testing (there are no stock controls on a GDI-drawn surface).
//!
//! Applying a setting updates the live configuration, persists the single
//! changed key to `minicon-settings.txt`, and refreshes the taskbar so merge
//! changes take effect without a restart.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen, CreateSolidBrush,
    DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DrawTextW, Ellipse, EndPaint, FillRect, FrameRect, GetStockObject, HBRUSH, HDC, HGDIOBJ,
    InvalidateRect, NULL_BRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetForegroundWindow, GetSystemMetrics, IDYES,
    IsWindow, IsWindowVisible, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_OK, MB_YESNO, MessageBoxW,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_PAINT, WNDCLASS_STYLES,
    WNDCLASSW, WS_CAPTION, WS_OVERLAPPED, WS_SYSMENU,
};
use windows::core::PCWSTR;

use crate::config::{self, GroupBy, MergeStrategy};
use crate::win_util::wide;

// --- layout / identity ----------------------------------------------------
const WIN_W: i32 = 460;
/// Client-area height the row layout is computed against.
const WIN_H: i32 = 720;
/// Non-client frame of the settings window (caption + borders). `WIN_W`/`WIN_H`
/// describe the **client** area the layout maths uses, but `CreateWindowExW`
/// takes the **window** size — so the frame must be added on creation, or the
/// last action button is clipped off-screen (a real, shipped bug).
const FRAME_W: i32 = 16;
const FRAME_H: i32 = 44;
const MARGIN: i32 = 22;
const TITLE_H: i32 = 30;
const ROW_H: i32 = 28;
const ACTION_H: i32 = 30;
const ACTION_GAP: i32 = 8;
/// Left inset of the control glyph (radio dot / checkbox) inside a row.
const GLYPH_LEFT: i32 = 5;
/// Left inset of the row text, clear of the control glyph.
const TEXT_LEFT: i32 = 26;

const CLASS_NAME: &str = "minicon_winmgr_settings";
const WINDOW_TITLE: &str = "设置";

// --- process-lifetime UI state --------------------------------------------
// Handle stored as `isize`; the message thread is the sole user, so this is
// safe by construction.
static SETTINGS: AtomicIsize = AtomicIsize::new(0);
/// Module handle remembered from [`install`] so the window can be re-created
/// later if it is ever destroyed (see [`toggle`]).
static HINST: AtomicIsize = AtomicIsize::new(0);

// --- pure data model (unit-testable, no Win32) ----------------------------

/// What clicking a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingAction {
    SetMerge(MergeStrategy),
    SetGroupBy(GroupBy),
    ToggleShowTaskbar,
    ToggleShowTray,
    ToggleHoverPreview,
}

/// One row in the settings list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A section label; not interactive.
    Heading(&'static str),
    /// A radio option; `enabled` is false when its group is inapplicable.
    Radio {
        text: &'static str,
        on: bool,
        enabled: bool,
        action: SettingAction,
    },
    /// A checkbox; `enabled` is false when a prerequisite is unmet.
    Toggle {
        text: &'static str,
        on: bool,
        enabled: bool,
        action: SettingAction,
    },
    /// A read-only `label: value` row.
    Info { text: &'static str, value: String },
}

/// Whether a settings row is currently usable.
///
/// The single source of truth for "is this row clickable right now": when the
/// taskbar is hidden, every option that only makes sense while the bar is on
/// screen is greyed out and inert. The taskbar toggle itself, and the shell
/// take-over / give-back action buttons (which are not rows here), stay usable.
///
/// Pure so the dependency rules can be unit-tested without any Win32 state.
pub fn row_enabled(action: SettingAction, show_taskbar: bool) -> bool {
    match action {
        SettingAction::ToggleShowTaskbar => true,
        SettingAction::SetMerge(_)
        | SettingAction::SetGroupBy(_)
        | SettingAction::ToggleShowTray
        | SettingAction::ToggleHoverPreview => show_taskbar,
    }
}

/// Dependency note appended to a greyed row's label, when the taskbar is the
/// prerequisite that is unmet. Pure, so the wording is testable.
fn row_note(action: SettingAction, show_taskbar: bool) -> Option<&'static str> {
    (!row_enabled(action, show_taskbar)).then_some("（需先显示任务栏）")
}

/// Builds the settings rows for the given configuration snapshot.
pub fn build_rows(cfg: &config::Config) -> Vec<Row> {
    let show = cfg.show_taskbar;
    // The grouping key only matters when merging is on — and, like the merge
    // strategy itself, only while the taskbar is actually shown.
    let by_enabled = row_enabled(SettingAction::SetGroupBy(GroupBy::Exe), show)
        && cfg.merge != MergeStrategy::Never;
    vec![
        Row::Heading("任务栏"),
        Row::Toggle {
            text: "显示任务栏（重启后生效）",
            on: cfg.show_taskbar,
            enabled: row_enabled(SettingAction::ToggleShowTaskbar, show),
            action: SettingAction::ToggleShowTaskbar,
        },
        Row::Toggle {
            text: "显示通知图标条（实验性）",
            on: cfg.show_tray,
            enabled: row_enabled(SettingAction::ToggleShowTray, show),
            action: SettingAction::ToggleShowTray,
        },
        // No row for the start menu: its taskbar entry was retired (the
        // window-list button already covers "find any window"), so there is
        // nothing left to toggle. The feature is still reachable by hand —
        // config key `taskbar.menu` / env `MINICON_START_MENU` — and re-enabling
        // it there brings the entry slot back.
        Row::Heading("窗口合并"),
        Row::Radio {
            text: MergeStrategy::Never.label(),
            on: cfg.merge == MergeStrategy::Never,
            enabled: row_enabled(SettingAction::SetMerge(MergeStrategy::Never), show),
            action: SettingAction::SetMerge(MergeStrategy::Never),
        },
        Row::Radio {
            text: MergeStrategy::WhenFull.label(),
            on: cfg.merge == MergeStrategy::WhenFull,
            enabled: row_enabled(SettingAction::SetMerge(MergeStrategy::WhenFull), show),
            action: SettingAction::SetMerge(MergeStrategy::WhenFull),
        },
        Row::Radio {
            text: MergeStrategy::Always.label(),
            on: cfg.merge == MergeStrategy::Always,
            enabled: row_enabled(SettingAction::SetMerge(MergeStrategy::Always), show),
            action: SettingAction::SetMerge(MergeStrategy::Always),
        },
        Row::Heading("合并依据"),
        Row::Radio {
            text: GroupBy::Exe.label(),
            on: cfg.group_by == GroupBy::Exe,
            enabled: by_enabled,
            action: SettingAction::SetGroupBy(GroupBy::Exe),
        },
        Row::Radio {
            text: GroupBy::Pid.label(),
            on: cfg.group_by == GroupBy::Pid,
            enabled: by_enabled,
            action: SettingAction::SetGroupBy(GroupBy::Pid),
        },
        Row::Heading("行为"),
        Row::Toggle {
            text: "悬停显示缩略图预览",
            on: cfg.hover_preview,
            enabled: row_enabled(SettingAction::ToggleHoverPreview, show),
            action: SettingAction::ToggleHoverPreview,
        },
        Row::Heading("外观"),
        Row::Info {
            text: "主题色",
            value: "深色（主题改动需重启生效）".to_string(),
        },
        Row::Heading("快捷键"),
        Row::Info {
            text: "唤出键",
            value: cfg.hotkey.clone(),
        },
        Row::Heading("关于"),
        Row::Info {
            text: "版本",
            value: env!("CARGO_PKG_VERSION").to_string(),
        },
    ]
}

/// An actionable row at the bottom of the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Make this exe the Windows shell (`HKCU\...\Winlogon\Shell`).
    Replace,
    /// Restore the previously recorded shell (or the default explorer).
    Rollback,
}

/// The action buttons, in display order.
pub fn actions() -> &'static [(&'static str, Action)] {
    &[
        ("接管桌面（立即生效）", Action::Replace),
        ("还原系统桌面（立即生效）", Action::Rollback),
    ]
}

/// Screen-space origin that centres a `win_w`×`win_h` window. Pure so it can be
/// unit-tested without any Win32 state.
fn centered_origin_for(screen_w: i32, screen_h: i32, win_w: i32, win_h: i32) -> (i32, i32) {
    (
        ((screen_w - win_w) / 2).max(0),
        ((screen_h - win_h) / 2).max(0),
    )
}

/// Toggles a boolean preference (pure, so the checkbox behavior is testable).
fn toggle_flag(v: bool) -> bool {
    !v
}

/// Equal-height row rects for `n` rows inside `client`, stacked from the top.
/// Pure (no Win32) so it can be unit-tested.
fn row_rects(client: RECT, n: usize) -> Vec<RECT> {
    let width = (client.right - client.left) - 2 * MARGIN;
    let mut top = MARGIN + TITLE_H + 8;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(RECT {
            left: MARGIN,
            top,
            right: MARGIN + width,
            bottom: top + ROW_H,
        });
        top += ROW_H;
    }
    out
}

/// Top of the action-button stack, below `n` rows.
fn actions_top(n: usize) -> i32 {
    MARGIN + TITLE_H + 8 + n as i32 * ROW_H + 16
}

/// Full-width action button rects for `client`, one per [`actions`] entry.
fn action_rects(client: RECT, n_rows: usize) -> Vec<(RECT, Action)> {
    let width = (client.right - client.left) - 2 * MARGIN;
    let top0 = actions_top(n_rows);
    actions()
        .iter()
        .enumerate()
        .map(|(i, (_, action))| {
            let top = top0 + i as i32 * (ACTION_H + ACTION_GAP);
            (
                RECT {
                    left: MARGIN,
                    top,
                    right: MARGIN + width,
                    bottom: top + ACTION_H,
                },
                *action,
            )
        })
        .collect()
}

/// The action a row *would* trigger, even when the row is currently disabled.
/// Used to tell "no action here" (headings, info) from "disabled right now".
fn row_declared_action(rows: &[Row], i: usize) -> Option<SettingAction> {
    match rows.get(i)? {
        Row::Radio { action, .. } | Row::Toggle { action, .. } => Some(*action),
        _ => None,
    }
}

/// The action a row triggers, or `None` when it is not clickable (headings,
/// info rows, and any row whose prerequisites are unmet).
fn row_action(rows: &[Row], i: usize) -> Option<SettingAction> {
    match rows.get(i)? {
        Row::Radio {
            action, enabled, ..
        }
        | Row::Toggle {
            action, enabled, ..
        } if *enabled => Some(*action),
        _ => None,
    }
}

/// Index of the row rect containing `(x, y)`, clickable or not.
fn hit_row_rect(rects: &[RECT], x: i32, y: i32) -> Option<usize> {
    rects
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

/// Index of the clickable row containing `(x, y)`, if any.
#[cfg(test)]
fn hit_row(rows: &[Row], rects: &[RECT], x: i32, y: i32) -> Option<usize> {
    hit_row_rect(rects, x, y).filter(|&i| row_action(rows, i).is_some())
}

/// Action whose button contains `(x, y)`, if any.
fn hit_action(rects: &[(RECT, Action)], x: i32, y: i32) -> Option<Action> {
    rects
        .iter()
        .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
        .map(|(_, a)| *a)
}

fn centered_origin() -> (i32, i32) {
    // SAFETY: pure metric queries.
    unsafe {
        centered_origin_for(
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
            WIN_W + FRAME_W,
            WIN_H + FRAME_H,
        )
    }
}

// --- GDI resources --------------------------------------------------------

fn theme() -> crate::config::Theme {
    static T: OnceLock<crate::config::Theme> = OnceLock::new();
    *T.get_or_init(|| crate::config::load().theme)
}

fn hbrush(v: isize) -> HBRUSH {
    HBRUSH(v as *mut core::ffi::c_void)
}

fn bg_brush() -> isize {
    static B: OnceLock<isize> = OnceLock::new();
    // SAFETY: solid brushes are process-lifetime GDI objects, never freed.
    *B.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(theme().bg)).0 as isize })
}

/// Accent brush, used for control glyphs and action-button outlines.
fn accent_brush() -> isize {
    static B: OnceLock<isize> = OnceLock::new();
    // SAFETY: solid brushes are process-lifetime GDI objects, never freed.
    *B.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(theme().accent)).0 as isize })
}

/// 1px accent pen for the hollow outline of unselected radios / checkboxes.
fn accent_pen() -> isize {
    static P: OnceLock<isize> = OnceLock::new();
    // SAFETY: the pen is a process-lifetime GDI object, never freed.
    *P.get_or_init(|| unsafe { CreatePen(PS_SOLID, 1, COLORREF(theme().accent)).0 as isize })
}

/// Creates a Segoe UI font at `height` px and `weight` (e.g. 400 normal, 700 bold).
fn make_font(height: i32, weight: i32) -> isize {
    let face = wide("Segoe UI");
    // SAFETY: `face` outlives the call; the font is process-lifetime.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            PCWSTR(face.as_ptr()),
        )
        .0 as isize
    }
}

fn title_font() -> isize {
    static F: OnceLock<isize> = OnceLock::new();
    *F.get_or_init(|| make_font(-17, 700))
}

fn body_font() -> isize {
    static F: OnceLock<isize> = OnceLock::new();
    *F.get_or_init(|| make_font(-13, 400))
}

// --- window plumbing ------------------------------------------------------

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

/// The live settings window, if any.
///
/// A stored handle that no longer names a window is treated as "gone": the
/// window is only ever destroyed by a `WM_DESTROY` we did not intend (see
/// [`toggle`]), so stale handles must not make the settings button dead.
fn window() -> Option<HWND> {
    existing(&SETTINGS).filter(|h| unsafe { IsWindow(Some(*h)).as_bool() })
}

/// Creates the (hidden) settings window class + window.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if window().is_some() {
        return Ok(()); // already initialised
    }
    HINST.store(hinstance.0 as isize, Ordering::SeqCst);
    // SAFETY: standard Win32 window setup; every buffer outlives its call and
    // `wndproc` is a valid `extern "system"` function. The class brush comes
    // from the theme and is process-lifetime.
    unsafe {
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);
        let class_brush = CreateSolidBrush(COLORREF(theme().bg));

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )
            .unwrap_or_default(),
            hbrBackground: class_brush,
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            let code = GetLastError().0;
            // ERROR_CLASS_ALREADY_EXISTS (1410) is fine on a repeated init.
            if code != 1410 {
                return Err(
                    format!("RegisterClassW(settings_ui) failed (Win32 error {code})").into(),
                );
            }
        }

        let (x, y) = centered_origin();
        // Hidden on creation: no `WS_VISIBLE`.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0),
            x,
            y,
            WIN_W + FRAME_W,
            WIN_H + FRAME_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        SETTINGS.store(hwnd.0 as isize, Ordering::SeqCst);

        tracing::info!(
            hwnd = ?hwnd,
            rows = build_rows(&config::current()).len(),
            "settings window created (hidden)"
        );
    }
    Ok(())
}

/// Shows or hides the settings window. If it is on screen but not focused,
/// clicking the button raises it instead of hiding it.
pub fn toggle() {
    let vis = visible();
    let fg = is_foreground();
    tracing::info!(visible = vis, foreground = fg, "settings toggle requested");
    if vis && fg {
        hide();
        return;
    }
    // The window is reused for the whole process lifetime. If it is missing —
    // never created, or destroyed behind our back — re-create it instead of
    // leaving the taskbar's settings button permanently dead.
    if window().is_none() {
        let hinst = HINST.load(Ordering::SeqCst);
        if hinst == 0 {
            tracing::warn!("settings toggle ignored: no module handle recorded");
            return;
        }
        if let Err(err) = install(HINSTANCE(hinst as *mut std::ffi::c_void)) {
            tracing::error!(%err, "settings window re-create failed");
            return;
        }
    }
    show();
}

/// Actual on-screen state (queried from Win32, not a cached flag).
fn visible() -> bool {
    window().is_some_and(|h| unsafe { IsWindowVisible(h).as_bool() })
}

/// Whether the settings window currently owns the foreground.
fn is_foreground() -> bool {
    window().is_some_and(|h| unsafe { GetForegroundWindow() == h })
}

/// Shows the window, forces it to the foreground, and repaints it.
fn show() {
    let Some(hwnd) = window() else {
        tracing::warn!("settings show ignored: window not installed");
        return;
    };
    // SAFETY: `hwnd` is our live window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let raised = crate::win_util::force_foreground(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, true);
        if raised {
            tracing::info!("settings window shown and focused");
        } else {
            tracing::warn!("settings window shown but could not take foreground");
        }
    }
}

/// Hides the window without destroying it.
fn hide() {
    let Some(hwnd) = window() else { return };
    // SAFETY: `hwnd` is our live window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    tracing::debug!("settings window hidden");
}

/// Simple modal message box (OK) for user feedback.
fn info_box(message: &str) {
    let text = wide(message);
    let caption = wide("minicon-winmgr");
    // SAFETY: both buffers outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// Applies a settings-row action: update the live config, persist the changed
/// key, then refresh the taskbar so merge changes are visible immediately.
fn run_setting(action: SettingAction, hwnd: HWND) {
    let changed = match action {
        SettingAction::SetMerge(m) => config::set_merge(m),
        SettingAction::SetGroupBy(g) => config::set_group_by(g),
        SettingAction::ToggleShowTaskbar => {
            config::set_show_taskbar(toggle_flag(config::current().show_taskbar))
        }
        SettingAction::ToggleShowTray => {
            config::set_show_tray(toggle_flag(config::current().show_tray))
        }
        SettingAction::ToggleHoverPreview => {
            config::set_hover_preview(toggle_flag(config::current().hover_preview))
        }
    };
    if changed {
        tracing::info!(?action, "setting applied");
        // Rebuild the bar: merge/grouping changes must not wait for a refresh tick.
        crate::taskbar::refresh();
        // SAFETY: `hwnd` is our live settings window.
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
    }
}

/// Runs an action button.
fn run_action(action: Action) {
    match action {
        Action::Replace => do_replace(),
        Action::Rollback => do_rollback(),
    }
}

fn do_replace() {
    // Already the shell? Nothing to hand over — and stopping the session's
    // shell would stop *us* (we own the tray host's `Shell_TrayWnd`). Refuse
    // before the confirmation so the user is never asked to approve a no-op.
    if crate::shell_swap::self_is_shell() {
        tracing::info!("settings: takeover skipped, already running as the shell");
        info_box("当前已经是 minicon 在充当桌面外壳，无需接管。");
        return;
    }
    // Taking over is immediate now: the desktop shell that is running in this
    // session gets stopped, not just the registry value written. That closes
    // the user's desktop, so it is confirmed first (default: No).
    let text = wide(
        "接管桌面：立即把当前桌面外壳换成 minicon-winmgr。\n\n\
         现在运行的桌面外壳（explorer）会立刻关闭，任务栏与桌面图标随之消失，\
         由 minicon 的任务栏接管。下一次登录仍使用 minicon。\n\n\
         确定？",
    );
    let caption = wide("接管桌面");
    // SAFETY: both buffers outlive the call.
    let answer = unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_YESNO | MB_ICONINFORMATION | MB_DEFBUTTON2,
        )
    };
    if answer != IDYES {
        return;
    }

    match crate::shell_swap::take_over() {
        Ok(report) => {
            tracing::info!(
                exe = %report.exe,
                stopped = report.stopped,
                "settings: shell taken over"
            );
            let mut message = String::from("已接管桌面（立即生效）。\n");
            if report.stopped == 0 {
                message.push_str("本会话没有正在运行的桌面外壳，无需关闭。\n");
            } else {
                message.push_str(&format!("已关闭 {} 个桌面外壳进程。\n", report.stopped));
            }
            if !report.auto_restart_disabled {
                message.push_str(
                    "注意：未能关闭系统外壳自动重启（需要管理员权限），\
                     Windows 可能随后重新拉起 explorer。\n",
                );
            }
            message.push_str("\n可用「还原系统桌面（立即生效）」一键恢复。");
            info_box(&message);
        }
        Err(err) => {
            tracing::warn!(%err, "settings: shell takeover failed");
            info_box(&format!("接管桌面失败：{err}"));
        }
    }
}

fn do_rollback() {
    // Confirm, defaulting to "No" — this hands the desktop back to Explorer.
    // When *we* are the shell the handover completes at the next sign-in (our
    // own bar is holding the session and cannot restart Explorer under itself),
    // so say so instead of over-promising.
    let we_are_shell = crate::shell_swap::self_is_shell();
    let tail = if we_are_shell {
        "当前正是本程序在充当桌面外壳，切换将在下次登录时完成。"
    } else {
        "将立即重新启动 explorer。"
    };
    // Distinguish "no takeover was ever recorded" (give-back refuses, so we stop
    // up front rather than promising a restore we will not perform) from a
    // recorded-but-empty value (which restores the Windows default shell).
    let backed_up = crate::shell_swap::backup_path().and_then(|p| std::fs::read_to_string(p).ok());
    let message = match backed_up
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(v) => format!("还原系统桌面：把外壳还原为\n{v}\n{tail}\n\n确定？"),
        None if backed_up.is_some() => {
            format!("还原系统桌面：把外壳还原为系统默认（explorer）\n{tail}\n\n确定？")
        }
        None => {
            info_box("没有记录到接管前的桌面外壳（本程序未曾接管过桌面），无法还原。");
            return;
        }
    };
    let text = wide(&message);
    let caption = wide("还原系统桌面");
    // SAFETY: both buffers outlive the call.
    let answer = unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_YESNO | MB_ICONINFORMATION | MB_DEFBUTTON2,
        )
    };
    if answer != IDYES {
        return;
    }
    match crate::shell_swap::give_back() {
        Ok(report) => {
            tracing::info!(
                restored = ?report.restored,
                started = report.started,
                "settings: shell given back"
            );
            let mut message = if we_are_shell {
                String::from("已还原系统桌面设置。\n")
            } else {
                String::from("已还原系统桌面（立即生效）。\n")
            };
            message.push_str(if report.started {
                "已重新启动 explorer。"
            } else if we_are_shell {
                "下次登录将由 explorer 接管桌面。"
            } else {
                "本会话已有桌面外壳在运行。"
            });
            message.push_str("\n若桌面外观未恢复，注销重登即可。");
            info_box(&message);
        }
        Err(err) => {
            tracing::warn!(%err, "settings: shell give-back failed");
            info_box(&format!("还原失败：{err}"));
        }
    }
}

// --- painting -------------------------------------------------------------

/// Draws one single-line, vertically centred label into `rect` in `color`.
///
/// # Safety
/// `hdc` must be a live device context with a font selected.
unsafe fn draw_line(hdc: HDC, rect: &RECT, text: &str, color: u32, flags: DRAW_TEXT_FORMAT) {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = *rect;
    // SAFETY: `hdc` is live; `buf` outlives the call.
    unsafe {
        SetTextColor(hdc, COLORREF(color));
        DrawTextW(
            hdc,
            &mut buf,
            &mut r,
            flags | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
}

/// Draws a radio dot at the left of `r`: filled accent when `on`, hollow when
/// off.
///
/// # Safety
/// `hdc` must be a live device context.
unsafe fn draw_radio(hdc: HDC, r: &RECT, on: bool) {
    let cx = r.left + GLYPH_LEFT + 6;
    let cy = (r.top + r.bottom) / 2;
    let rad = 6;
    // SAFETY: all GDI objects are process-lifetime; `hdc` is live.
    unsafe {
        let brush = if on {
            accent_brush()
        } else {
            GetStockObject(NULL_BRUSH).0 as isize
        };
        let old_brush = SelectObject(hdc, HGDIOBJ(brush as *mut core::ffi::c_void));
        let old_pen = SelectObject(hdc, HGDIOBJ(accent_pen() as *mut core::ffi::c_void));
        let _ = Ellipse(hdc, cx - rad, cy - rad, cx + rad, cy + rad);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }
}

/// Draws a checkbox at the left of `r`: an accent frame, filled when `on`.
///
/// # Safety
/// `hdc` must be a live device context.
unsafe fn draw_checkbox(hdc: HDC, r: &RECT, on: bool) {
    let cy = (r.top + r.bottom) / 2;
    let half = 7;
    let boxr = RECT {
        left: r.left + GLYPH_LEFT,
        top: cy - half,
        right: r.left + GLYPH_LEFT + 2 * half,
        bottom: cy + half,
    };
    // SAFETY: GDI calls on a live `hdc`; brushes are process-lifetime.
    unsafe {
        FillRect(hdc, &boxr, hbrush(bg_brush()));
        FrameRect(hdc, &boxr, hbrush(accent_brush()));
        if on {
            let inner = RECT {
                left: boxr.left + 4,
                top: boxr.top + 4,
                right: boxr.right - 4,
                bottom: boxr.bottom - 4,
            };
            FillRect(hdc, &inner, hbrush(accent_brush()));
        }
    }
}

/// Full window repaint: background, title, settings rows, action buttons.
fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `hwnd` is our live window; `ps` receives the paint state.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client = RECT::default();
    // SAFETY: `hwnd` is live; `client` receives its client rect.
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let t = theme();
    let cfg = config::current();
    let show_taskbar = cfg.show_taskbar;
    let rows = build_rows(&cfg);
    let rects = row_rects(client, rows.len());

    // SAFETY: all GDI handles below are process-lifetime; the client rect is
    // ours; `client` is fully painted before `EndPaint`.
    unsafe {
        FillRect(hdc, &client, hbrush(bg_brush()));
        SetBkMode(hdc, TRANSPARENT);

        // Title.
        let old_title = SelectObject(hdc, HGDIOBJ(title_font() as *mut core::ffi::c_void));
        let title_rect = RECT {
            left: MARGIN,
            top: MARGIN,
            right: client.right - MARGIN,
            bottom: MARGIN + TITLE_H,
        };
        draw_line(hdc, &title_rect, WINDOW_TITLE, t.text, DT_LEFT);
        SelectObject(hdc, old_title);

        let old_body = SelectObject(hdc, HGDIOBJ(body_font() as *mut core::ffi::c_void));
        let value_right = client.right - MARGIN;
        for (row, r) in rows.iter().zip(rects.iter()) {
            match row {
                Row::Heading(text) => {
                    draw_line(hdc, r, text, t.muted, DT_LEFT);
                }
                Row::Info { text, value } => {
                    let label_rect = RECT {
                        right: r.right,
                        ..*r
                    };
                    draw_line(hdc, &label_rect, text, t.text, DT_LEFT);
                    let val_rect = RECT {
                        left: r.left + 120,
                        right: value_right,
                        ..*r
                    };
                    draw_line(hdc, &val_rect, value, t.muted, DT_RIGHT);
                }
                Row::Radio {
                    text,
                    on,
                    enabled,
                    action,
                } => {
                    draw_radio(hdc, r, *on);
                    let tr = RECT {
                        left: r.left + TEXT_LEFT,
                        right: r.right,
                        ..*r
                    };
                    // Disabled rows are drawn in the muted foreground and spell
                    // out the unmet prerequisite.
                    let color = if *enabled { t.text } else { t.muted };
                    let label = match row_note(*action, show_taskbar) {
                        Some(note) => format!("{text}{note}"),
                        None => (*text).to_string(),
                    };
                    draw_line(hdc, &tr, &label, color, DT_LEFT);
                }
                Row::Toggle {
                    text,
                    on,
                    enabled,
                    action,
                } => {
                    draw_checkbox(hdc, r, *on);
                    let tr = RECT {
                        left: r.left + TEXT_LEFT,
                        right: r.right,
                        ..*r
                    };
                    let color = if *enabled { t.text } else { t.muted };
                    let label = match row_note(*action, show_taskbar) {
                        Some(note) => format!("{text}{note}"),
                        None => (*text).to_string(),
                    };
                    draw_line(hdc, &tr, &label, color, DT_LEFT);
                }
            }
        }

        // Action buttons.
        for (rect, action) in action_rects(client, rows.len()) {
            FillRect(hdc, &rect, hbrush(bg_brush()));
            FrameRect(hdc, &rect, hbrush(accent_brush()));
            let label = actions()
                .iter()
                .find(|(_, a)| *a == action)
                .map(|(l, _)| *l)
                .unwrap_or("");
            let mut text_rect = rect;
            text_rect.left += 12;
            draw_line(hdc, &text_rect, label, t.text, DT_LEFT);
        }

        SelectObject(hdc, old_body);
        let _ = EndPaint(hwnd, &ps);
    }

    tracing::debug!(rows = rows.len(), "settings window painted");
}

/// Window procedure for the settings window.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        // Background is fully cleared in `WM_PAINT`.
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let x = get_x(lparam);
            let y = get_y(lparam);
            let mut client = RECT::default();
            // SAFETY: `hwnd` is live; `client` receives its client rect.
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            let rows = build_rows(&config::current());
            let rects = row_rects(client, rows.len());
            if let Some(i) = hit_row_rect(&rects, x, y) {
                if let Some(action) = row_action(&rows, i) {
                    run_setting(action, hwnd);
                    return LRESULT(0);
                }
                // A disabled setting row is inert: swallow the click (so it can
                // never fall through to the action buttons) and record why.
                if let Some(action) = row_declared_action(&rows, i) {
                    tracing::debug!(?action, "ignoring click on disabled setting row");
                    return LRESULT(0);
                }
            }
            if let Some(action) = hit_action(&action_rects(client, rows.len()), x, y) {
                run_action(action);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
            hide();
            LRESULT(0)
        }
        WM_CLOSE => {
            // Closing hides instead of destroying: the window is reused, and a
            // destroyed window leaves the settings button with nothing to show.
            hide();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn get_x(lparam: LPARAM) -> i32 {
    (lparam.0 as usize & 0xFFFF) as u16 as i16 as i32
}

fn get_y(lparam: LPARAM) -> i32 {
    ((lparam.0 as usize >> 16) & 0xFFFF) as u16 as i16 as i32
}

#[cfg(test)]
mod tests {
    use super::{
        Action, GroupBy, MergeStrategy, Row, SettingAction, action_rects, actions, build_rows,
        centered_origin_for, hit_action, hit_row, hit_row_rect, row_action, row_declared_action,
        row_enabled, row_note, row_rects, toggle_flag,
    };
    use crate::config::Config;
    use windows::Win32::Foundation::RECT;

    /// Every setting action the dialog can carry, for exhaustive checks.
    fn all_setting_actions() -> [SettingAction; 5] {
        [
            SettingAction::SetMerge(MergeStrategy::Never),
            SettingAction::SetGroupBy(GroupBy::Exe),
            SettingAction::ToggleShowTaskbar,
            SettingAction::ToggleShowTray,
            SettingAction::ToggleHoverPreview,
        ]
    }

    fn client() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: super::WIN_W,
            bottom: super::WIN_H,
        }
    }

    #[test]
    fn rows_cover_the_expected_settings() {
        let rows = build_rows(&Config::default());
        // The merge radios are present and default to "never".
        let never = rows.iter().find(
            |r| matches!(r, Row::Radio { text, .. } if *text == MergeStrategy::Never.label()),
        );
        assert!(matches!(never, Some(Row::Radio { on: true, .. })));
        // Grouping basis is disabled while merging is off.
        let exe = rows
            .iter()
            .find(|r| matches!(r, Row::Radio { text, .. } if *text == "按应用（可执行文件）"));
        assert!(matches!(exe, Some(Row::Radio { enabled: false, .. })));
    }

    #[test]
    fn grouping_basis_enables_when_merging() {
        let cfg = Config {
            merge: MergeStrategy::Always,
            ..Config::default()
        };
        let rows = build_rows(&cfg);
        let pid = rows
            .iter()
            .find(|r| matches!(r, Row::Radio { text, .. } if *text == "按进程（pid）"));
        assert!(matches!(pid, Some(Row::Radio { enabled: true, .. })));
    }

    #[test]
    fn row_rects_stack_without_overlap() {
        let rects = row_rects(client(), 4);
        assert_eq!(rects.len(), 4);
        for w in rects.windows(2) {
            assert!(w[0].bottom <= w[1].top, "rows must not overlap");
        }
    }

    #[test]
    fn hit_row_resolves_clickable_rows_and_ignores_others() {
        let cfg = Config::default();
        let rows = build_rows(&cfg);
        let rects = row_rects(client(), rows.len());
        // Find a heading (non-clickable) and click its centre: no hit.
        let heading = rows
            .iter()
            .position(|r| matches!(r, Row::Heading(_)))
            .unwrap();
        let hr = rects[heading];
        let hc = ((hr.left + hr.right) / 2, (hr.top + hr.bottom) / 2);
        assert_eq!(hit_row(&rows, &rects, hc.0, hc.1), None);
        // A merge radio resolves to its action.
        let radio = rows
            .iter()
            .position(|r| {
                matches!(
                    r,
                    Row::Radio {
                        action: SettingAction::SetMerge(_),
                        ..
                    }
                )
            })
            .unwrap();
        let rr = rects[radio];
        let rc = ((rr.left + rr.right) / 2, (rr.top + rr.bottom) / 2);
        assert_eq!(hit_row(&rows, &rects, rc.0, rc.1), Some(radio));
        // A disabled row (grouping basis with merge off) is not clickable.
        let disabled = rows
            .iter()
            .position(|r| matches!(r, Row::Radio { enabled: false, .. }))
            .unwrap();
        let dr = rects[disabled];
        let dc = ((dr.left + dr.right) / 2, (dr.top + dr.bottom) / 2);
        assert_eq!(hit_row(&rows, &rects, dc.0, dc.1), None);
    }

    #[test]
    fn row_action_is_none_for_non_interactive_rows() {
        let rows = build_rows(&Config::default());
        for (i, r) in rows.iter().enumerate() {
            let expected = match r {
                Row::Heading(_) | Row::Info { .. } => false,
                Row::Radio { enabled, .. } => *enabled,
                Row::Toggle { .. } => true,
            };
            assert_eq!(row_action(&rows, i).is_some(), expected);
        }
    }

    #[test]
    fn toggle_flag_negates() {
        assert!(toggle_flag(false));
        assert!(!toggle_flag(true));
    }

    #[test]
    fn centers_window_on_screen() {
        assert_eq!(centered_origin_for(1920, 1080, 460, 660), (730, 210));
    }

    #[test]
    fn clamps_when_window_larger_than_screen() {
        assert_eq!(centered_origin_for(400, 300, 460, 660), (0, 0));
    }

    #[test]
    fn has_replace_and_rollback_actions() {
        let kinds: Vec<Action> = actions().iter().map(|(_, a)| *a).collect();
        assert!(kinds.contains(&Action::Replace));
        assert!(kinds.contains(&Action::Rollback));
    }

    #[test]
    fn action_rects_stack_without_overlap_and_hit_test() {
        let rects = action_rects(client(), build_rows(&Config::default()).len());
        assert_eq!(rects.len(), actions().len());
        assert!(rects[0].0.bottom <= rects[1].0.top, "rows must not overlap");
        // A point inside the first button resolves to its action.
        let (r0, a0) = rects[0];
        let mid = ((r0.left + r0.right) / 2, (r0.top + r0.bottom) / 2);
        assert_eq!(hit_action(&rects, mid.0, mid.1), Some(a0));
        assert_eq!(hit_action(&rects, 0, 0), None);
    }

    /// The stock row set must fit the client area the layout assumes
    /// (`WIN_H`); the non-client frame is added on top at creation time.
    #[test]
    fn default_rows_and_actions_fit_the_client_height() {
        let rows = build_rows(&Config::default());
        let rects = action_rects(client(), rows.len());
        let last = rects.last().expect("there are action buttons").0;
        assert!(
            last.bottom + super::MARGIN <= super::WIN_H,
            "content ends at {} but the client is only {} tall",
            last.bottom + super::MARGIN,
            super::WIN_H
        );
    }

    #[test]
    fn every_row_is_usable_while_the_taskbar_is_shown() {
        for action in all_setting_actions() {
            assert!(
                row_enabled(action, true),
                "{action:?} should be usable while the taskbar is shown"
            );
            assert_eq!(row_note(action, true), None, "{action:?} needs no note");
        }
    }

    #[test]
    fn taskbar_dependent_rows_disable_without_the_taskbar() {
        for action in [
            SettingAction::SetMerge(MergeStrategy::Never),
            SettingAction::SetGroupBy(GroupBy::Exe),
            SettingAction::ToggleShowTray,
            SettingAction::ToggleHoverPreview,
        ] {
            assert!(
                !row_enabled(action, false),
                "{action:?} must be disabled without the taskbar"
            );
            assert_eq!(
                row_note(action, false),
                Some("（需先显示任务栏）"),
                "{action:?} should explain its dependency"
            );
        }
        // The taskbar toggle itself stays usable, so it can be turned back on.
        assert!(row_enabled(SettingAction::ToggleShowTaskbar, false));
        assert_eq!(row_note(SettingAction::ToggleShowTaskbar, false), None);
    }

    #[test]
    fn hiding_the_taskbar_greys_exactly_the_dependent_rows() {
        // Merge on, so the merge dependency does not mask the taskbar one.
        let cfg = Config {
            show_taskbar: false,
            merge: MergeStrategy::Always,
            ..Config::default()
        };
        let rows = build_rows(&cfg);
        for (i, r) in rows.iter().enumerate() {
            let (action, enabled) = match r {
                Row::Radio {
                    action, enabled, ..
                }
                | Row::Toggle {
                    action, enabled, ..
                } => (*action, *enabled),
                Row::Heading(_) | Row::Info { .. } => continue,
            };
            assert_eq!(
                enabled,
                row_enabled(action, false),
                "row {i} ({action:?}) disagrees with row_enabled"
            );
        }
        // Only "显示任务栏" remains interactive; every other row is inert.
        let interactive: Vec<SettingAction> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Radio {
                    action, enabled, ..
                }
                | Row::Toggle {
                    action, enabled, ..
                } if *enabled => Some(*action),
                _ => None,
            })
            .collect();
        assert_eq!(interactive, vec![SettingAction::ToggleShowTaskbar]);
    }

    #[test]
    fn disabled_rows_report_their_action_but_stay_unclickable() {
        let cfg = Config {
            show_taskbar: false,
            merge: MergeStrategy::Always,
            ..Config::default()
        };
        let rows = build_rows(&cfg);
        let rects = row_rects(client(), rows.len());
        // The "通知图标条" toggle: declared, but not clickable while greyed.
        let i = rows
            .iter()
            .position(|r| {
                matches!(
                    r,
                    Row::Toggle {
                        action: SettingAction::ToggleShowTray,
                        enabled: false,
                        ..
                    }
                )
            })
            .unwrap();
        assert_eq!(
            row_declared_action(&rows, i),
            Some(SettingAction::ToggleShowTray)
        );
        assert_eq!(row_action(&rows, i), None);
        let r = rects[i];
        let mid = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        assert_eq!(hit_row_rect(&rects, mid.0, mid.1), Some(i));
        assert_eq!(hit_row(&rows, &rects, mid.0, mid.1), None);
    }
}
