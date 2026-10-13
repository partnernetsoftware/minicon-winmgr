//! Settings dialog (placeholder content for now).
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Opened from the taskbar's settings button (left of the menu button).
//! Self-contained: owns its window class and a single reused window; the
//! placeholders will be replaced by real settings later.
//!
//! Style: a plain framed window (caption + system menu) drawn with GDI to match
//! the taskbar's dark theme. Content is a static table of `label / value` rows
//! driven by [`sections`], so replacing the placeholders later is a data change.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DrawTextW, EndPaint, FillRect, FrameRect, HBRUSH, HDC, HGDIOBJ, InvalidateRect,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetForegroundWindow,
    GetSystemMetrics, IDYES, IsWindowVisible, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_OK, MB_YESNO,
    MessageBoxW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_PAINT, WNDCLASS_STYLES, WNDCLASSW, WS_CAPTION, WS_OVERLAPPED, WS_SYSMENU,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

// --- layout / identity ----------------------------------------------------
const WIN_W: i32 = 420;
const WIN_H: i32 = 420;
const MARGIN: i32 = 22;
const TITLE_H: i32 = 30;
const ROW_H: i32 = 42;
const ACTION_H: i32 = 30;
const ACTION_GAP: i32 = 8;
const FOOTER_H: i32 = 20;

const CLASS_NAME: &str = "minicon_winmgr_settings";
const WINDOW_TITLE: &str = "设置";

// --- process-lifetime UI state --------------------------------------------
// Handle stored as `isize`; the message thread is the sole user, so this is
// safe by construction.
static SETTINGS: AtomicIsize = AtomicIsize::new(0);

// --- pure data model (unit-testable, no Win32) ----------------------------

/// A placeholder settings row: `(section label, current value)`.
///
/// The value column renders as muted "not yet wired up" text; swapping this
/// table for real settings is the intended extension point.
pub fn sections() -> &'static [(&'static str, &'static str)] {
    const SECTIONS: &[(&str, &str)] = &[
        ("外观", "主题"),
        ("任务栏", "显示与位置"),
        ("行为", "启动器"),
        ("快捷键", "查看"),
        ("关于", "版本"),
    ];
    SECTIONS
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
        ("替代为 Windows 外壳", Action::Replace),
        ("回滚到原外壳", Action::Rollback),
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

/// Top of the first action row, below the placeholder rows.
fn actions_top() -> i32 {
    MARGIN + TITLE_H + 8 + sections().len() as i32 * ROW_H + 12
}

/// Full-width action button rects for `client`, one per [`actions`] entry.
/// Pure (no Win32) so it can be unit-tested.
fn action_rects(client: RECT) -> Vec<(RECT, Action)> {
    let width = (client.right - client.left) - 2 * MARGIN;
    let top0 = actions_top();
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
            WIN_W,
            WIN_H,
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

/// Accent brush, used for action-button outlines.
fn accent_brush() -> isize {
    static B: OnceLock<isize> = OnceLock::new();
    // SAFETY: solid brushes are process-lifetime GDI objects, never freed.
    *B.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(theme().accent)).0 as isize })
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

fn window() -> Option<HWND> {
    existing(&SETTINGS)
}

/// Creates the (hidden) settings window class + window.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if window().is_some() {
        return Ok(()); // already initialised
    }
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
            WIN_W,
            WIN_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        SETTINGS.store(hwnd.0 as isize, Ordering::SeqCst);

        tracing::info!(
            hwnd = ?hwnd,
            rows = sections().len(),
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
    } else {
        show();
    }
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

/// Path of the file holding the shell value captured before a replacement.
fn backup_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("shell-backup.txt"))
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

/// Runs an action button.
fn run_action(action: Action) {
    match action {
        Action::Replace => do_replace(),
        Action::Rollback => do_rollback(),
    }
}

fn do_replace() {
    let Ok(exe) = std::env::current_exe() else {
        info_box("无法获取当前程序路径。");
        return;
    };
    let exe = exe.to_string_lossy().to_string();
    // Capture the current value once, so rollback can restore it faithfully.
    match crate::shell_registry::current_shell() {
        Ok(Some(cur)) if cur == exe => {
            info_box("已经是我们作为外壳了。");
            return;
        }
        Ok(cur) => {
            if let (Some(path), Some(value)) = (backup_path(), cur) {
                let _ = std::fs::write(&path, value);
                tracing::info!(backup = %path.display(), "shell backup written");
            }
        }
        Err(err) => {
            tracing::warn!(%err, "could not read current shell; proceeding without backup");
        }
    }
    match crate::shell_registry::replace(&exe) {
        Ok(()) => {
            tracing::info!(%exe, "settings: replaced shell");
            info_box("已设为 Windows 外壳。\n下一次登录（或注销重登）后生效。");
        }
        Err(err) => {
            tracing::warn!(%err, "settings: shell replace failed");
            info_box(&format!("设置外壳失败：{err}"));
        }
    }
}

fn do_rollback() {
    let restored = backup_path().and_then(|p| std::fs::read_to_string(p).ok());
    let restored = restored
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Confirm, defaulting to "No" — a wrong rollback needs a re-login to undo.
    let message = match &restored {
        Some(v) => format!("回滚外壳到：\n{v}\n\n确定？"),
        None => "回滚外壳到系统默认（explorer）？\n\n确定？".to_string(),
    };
    let text = wide(&message);
    let caption = wide("回滚外壳");
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
    match crate::shell_registry::restore(restored.as_deref()) {
        Ok(()) => {
            tracing::info!(?restored, "settings: shell rolled back");
            info_box("已回滚。注销重登后生效（或立即运行 explorer.exe）。");
        }
        Err(err) => {
            tracing::warn!(%err, "settings: shell rollback failed");
            info_box(&format!("回滚失败：{err}"));
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

/// Full window repaint: background, title, the placeholder rows, and the footer.
fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `hwnd` is our live window; `ps` receives the paint state.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client = RECT::default();
    // SAFETY: `hwnd` is live; `client` receives its client rect.
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let t = theme();

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

        // Placeholder rows: label left, muted value right.
        let old_body = SelectObject(hdc, HGDIOBJ(body_font() as *mut core::ffi::c_void));
        let label_left = MARGIN;
        let value_right = client.right - MARGIN;
        let label_right = client.left + (client.right - client.left) / 2;
        let mut top = MARGIN + TITLE_H + 8;
        for (label, value) in sections() {
            let mid = top + ROW_H / 2;
            let label_rect = RECT {
                left: label_left,
                top: mid - 10,
                right: label_right,
                bottom: mid + 10,
            };
            draw_line(hdc, &label_rect, label, t.text, DT_LEFT);

            let value_rect = RECT {
                left: label_right,
                top: mid - 10,
                right: value_right,
                bottom: mid + 10,
            };
            draw_line(hdc, &value_rect, value, t.muted, DT_RIGHT);

            top += ROW_H;
        }

        // Action buttons.
        for (rect, action) in action_rects(client) {
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

        // Footer note.
        let footer_rect = RECT {
            left: MARGIN,
            top: client.bottom - MARGIN - FOOTER_H,
            right: value_right,
            bottom: client.bottom - MARGIN,
        };
        draw_line(
            hdc,
            &footer_rect,
            "（占位内容，待接入真实设置）",
            t.muted,
            DT_LEFT,
        );

        SelectObject(hdc, old_body);
        let _ = EndPaint(hwnd, &ps);
    }

    tracing::debug!(rows = sections().len(), "settings window painted");
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
            if let Some(action) = hit_action(&action_rects(client), x, y) {
                run_action(action);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
            hide();
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is our live window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            SETTINGS.store(0, Ordering::SeqCst);
            tracing::info!("settings window destroyed");
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
    use super::{Action, action_rects, actions, centered_origin_for, hit_action, sections};
    use windows::Win32::Foundation::RECT;

    #[test]
    fn sections_has_expected_rows() {
        assert_eq!(sections().len(), 5);
    }

    #[test]
    fn section_rows_are_non_empty() {
        for (label, value) in sections() {
            assert!(!label.is_empty(), "section label must not be empty");
            assert!(!value.is_empty(), "section value must not be empty");
        }
    }

    #[test]
    fn centers_window_on_screen() {
        assert_eq!(centered_origin_for(1920, 1080, 420, 420), (750, 330));
    }

    #[test]
    fn clamps_when_window_larger_than_screen() {
        assert_eq!(centered_origin_for(400, 300, 420, 420), (0, 0));
    }

    #[test]
    fn has_replace_and_rollback_actions() {
        let kinds: Vec<Action> = actions().iter().map(|(_, a)| *a).collect();
        assert!(kinds.contains(&Action::Replace));
        assert!(kinds.contains(&Action::Rollback));
    }

    #[test]
    fn action_rects_stack_without_overlap_and_hit_test() {
        let client = RECT {
            left: 0,
            top: 0,
            right: 420,
            bottom: 420,
        };
        let rects = action_rects(client);
        assert_eq!(rects.len(), actions().len());
        assert!(rects[0].0.bottom <= rects[1].0.top, "rows must not overlap");
        // A point inside the first button resolves to its action.
        let (r0, a0) = rects[0];
        let mid = ((r0.left + r0.right) / 2, (r0.top + r0.bottom) / 2);
        assert_eq!(hit_action(&rects, mid.0, mid.1), Some(a0));
        assert_eq!(hit_action(&rects, 0, 0), None);
    }
}
