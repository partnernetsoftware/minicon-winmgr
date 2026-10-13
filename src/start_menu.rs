//! Start menu / ORB — a categorized application menu popup.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Self-contained: owns its window class and wndproc, reuses `crate::launcher`
//! for discovery/launch. `toggle()` is called by the taskbar's brand-dot click.
//!
//! Layout: a borderless top-most popup holding three stacked `LISTBOX`es of the
//! same visual style — the discovered Start Menu shortcuts (top), a `Places`
//! navigation list (middle) and a `Power` list (bottom). Each list box is
//! subclassed so `Enter` / `Esc` are handled without a dialog manager (the list
//! owns keyboard focus).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, GWLP_WNDPROC, GetSystemMetrics, HMENU, IDYES,
    IsChild, LB_ADDSTRING, LB_GETCOUNT, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, LBN_DBLCLK,
    LBS_NOTIFY, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO, MessageBoxW, RegisterClassW, SM_CYSCREEN,
    SW_HIDE, SW_SHOW, SW_SHOWNORMAL, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    ShowWindow, WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_COMMAND, WM_DESTROY,
    WM_KEYDOWN, WM_KILLFOCUS, WNDCLASS_STYLES, WNDCLASSW, WNDPROC, WS_BORDER, WS_CHILD,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::PCWSTR;

use crate::launcher::Shortcut;
use crate::win_util::wide;

// --- geometry / ids -------------------------------------------------------
const ORB_W: i32 = 320;
const ORB_H: i32 = 440;
const MARGIN: i32 = 6;
/// Gap between the three stacked list boxes.
const GAP: i32 = 4;
/// Nominal list-box row height (Win32 `LISTBOX` default item height in pixels).
const ROW_H: i32 = 16;

const ID_LIST: u16 = 2001;
const ID_PLACES: u16 = 2002;
const ID_POWER: u16 = 2003;

const CLASS_NAME: &str = "minicon_winmgr_start_menu";
const WINDOW_TITLE: &str = "minicon start menu";
/// Light background so the popup is not a black rectangle before controls fill.
const BG_COLOR: COLORREF = COLORREF(0x00FF_FFFF);

// --- process-lifetime UI state --------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static ORB: AtomicIsize = AtomicIsize::new(0);
static LIST: AtomicIsize = AtomicIsize::new(0);
static PLACES: AtomicIsize = AtomicIsize::new(0);
static POWER: AtomicIsize = AtomicIsize::new(0);
static ORIG_LIST_PROC: AtomicIsize = AtomicIsize::new(0);
static ORIG_PLACES_PROC: AtomicIsize = AtomicIsize::new(0);
static ORIG_POWER_PROC: AtomicIsize = AtomicIsize::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the shortcut/place/power lists have been discovered and pushed into
/// the list boxes.
static LOADED: AtomicBool = AtomicBool::new(false);

/// Discovered Start Menu shortcuts, in list-box row order.
static ITEMS: Mutex<Vec<Shortcut>> = Mutex::new(Vec::new());

// --- pure data models (unit-testable, no Win32) ---------------------------

/// A navigable location shown in the `Places` section.
///
/// `target` is the argument handed to `ShellExecuteW` — a `shell:` moniker such
/// as `shell:MyComputerFolder`, or a URI scheme like `ms-settings:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// Localised display label.
    pub label: &'static str,
    /// `ShellExecuteW` target (shell moniker or URI).
    pub target: &'static str,
}

/// The fixed set of `Places` entries, in display order.
pub fn places() -> &'static [Place] {
    const PLACES: &[Place] = &[
        Place {
            label: "计算机",
            target: "shell:MyComputerFolder",
        },
        Place {
            label: "文档",
            target: "shell:Personal",
        },
        Place {
            label: "下载",
            target: "shell:Downloads",
        },
        Place {
            label: "图片",
            target: "shell:MyPictures",
        },
        Place {
            label: "设置",
            target: "ms-settings:",
        },
    ];
    PLACES
}

/// A concrete power transition. Kept separate from the label so execution and
/// testing can reason about it without parsing strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCommand {
    /// `shutdown.exe /l` — sign the current user out.
    LogOff,
    /// `shutdown.exe /r /t 0` — restart immediately.
    Restart,
    /// `shutdown.exe /s /t 0` — power off immediately.
    Shutdown,
}

impl PowerCommand {
    /// Arguments passed to `shutdown.exe` (see [`Self::target`]).
    pub fn args(self) -> &'static [&'static str] {
        match self {
            PowerCommand::LogOff => &["/l"],
            PowerCommand::Restart => &["/r", "/t", "0"],
            PowerCommand::Shutdown => &["/s", "/t", "0"],
        }
    }
}

/// A `Power` menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerAction {
    /// Localised display label.
    pub label: &'static str,
    /// The `shutdown.exe` invocation this entry runs (for logging/tests).
    pub target: &'static str,
    /// Structured command actually executed.
    pub command: PowerCommand,
}

/// The fixed set of `Power` entries, in display order.
pub fn power_actions() -> &'static [PowerAction] {
    const ACTIONS: &[PowerAction] = &[
        PowerAction {
            label: "注销",
            target: "shutdown.exe /l",
            command: PowerCommand::LogOff,
        },
        PowerAction {
            label: "重启",
            target: "shutdown.exe /r /t 0",
            command: PowerCommand::Restart,
        },
        PowerAction {
            label: "关机",
            target: "shutdown.exe /s /t 0",
            command: PowerCommand::Shutdown,
        },
    ];
    ACTIONS
}

/// Rectangle in the popup's client coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Computes the stacked layout of [apps, places, power] list boxes inside a
/// popup `orb_h` pixels tall. Pure so it can be unit-tested.
fn list_layout(orb_h: i32) -> [Rect; 3] {
    let w = ORB_W - 2 * MARGIN;
    let places_h = places().len() as i32 * ROW_H;
    let power_h = power_actions().len() as i32 * ROW_H;
    let apps_h = (orb_h - 2 * MARGIN - places_h - power_h - 2 * GAP).max(0);

    let apps = Rect {
        x: MARGIN,
        y: MARGIN,
        w,
        h: apps_h,
    };
    let places = Rect {
        x: MARGIN,
        y: MARGIN + apps_h + GAP,
        w,
        h: places_h,
    };
    let power = Rect {
        x: MARGIN,
        y: MARGIN + apps_h + GAP + places_h + GAP,
        w,
        h: power_h,
    };
    [apps, places, power]
}

// --- handle helpers -------------------------------------------------------

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn orb() -> Option<HWND> {
    existing(&ORB)
}

fn list() -> Option<HWND> {
    existing(&LIST)
}

fn places_list() -> Option<HWND> {
    existing(&PLACES)
}

fn power_list() -> Option<HWND> {
    existing(&POWER)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

/// Which of the three stacked list boxes `hwnd` is, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Apps,
    Places,
    Power,
}

fn list_kind(hwnd: HWND) -> Option<ListKind> {
    if list() == Some(hwnd) {
        Some(ListKind::Apps)
    } else if places_list() == Some(hwnd) {
        Some(ListKind::Places)
    } else if power_list() == Some(hwnd) {
        Some(ListKind::Power)
    } else {
        None
    }
}

/// The original `LISTBOX` procedure saved when `hwnd` was subclassed.
fn orig_proc_for(hwnd: HWND) -> isize {
    match list_kind(hwnd) {
        Some(ListKind::Apps) => ORIG_LIST_PROC.load(Ordering::SeqCst),
        Some(ListKind::Places) => ORIG_PLACES_PROC.load(Ordering::SeqCst),
        Some(ListKind::Power) => ORIG_POWER_PROC.load(Ordering::SeqCst),
        None => 0,
    }
}

/// Locks the items cache, recovering from a poisoned mutex (the data is a plain
/// `Vec` with no invariants a panic could break).
fn items_guard() -> std::sync::MutexGuard<'static, Vec<Shortcut>> {
    ITEMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Path for the shortcut at list-box row `index`, if any. Pure so it can be
/// unit-tested without touching Win32.
fn path_at(items: &[Shortcut], index: i32) -> Option<String> {
    if index < 0 {
        return None;
    }
    items.get(index as usize).map(|s| s.path.clone())
}

/// The `Places` entry at row `index`, if any. Pure so it can be unit-tested.
fn place_at(index: i32) -> Option<&'static Place> {
    if index < 0 {
        return None;
    }
    places().get(index as usize)
}

/// The `Power` entry at row `index`, if any. Pure so it can be unit-tested.
fn power_at(index: i32) -> Option<&'static PowerAction> {
    if index < 0 {
        return None;
    }
    power_actions().get(index as usize)
}

/// Registers the start-menu window class and prepares (hidden) UI.
pub fn install(hinstance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    if orb().is_some() {
        return Ok(()); // already initialised
    }
    // SAFETY: standard Win32 window setup; every buffer outlives its call and
    // both wndprocs are valid `extern "system"` functions.
    unsafe {
        let class_name = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);
        // A class brush keeps the popup from flashing uninitialised pixels.
        let background = CreateSolidBrush(BG_COLOR);

        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: windows::Win32::UI::WindowsAndMessaging::LoadCursorW(
                None,
                windows::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            )
            .unwrap_or_default(),
            hbrBackground: background,
            style: WNDCLASS_STYLES::default(),
            ..Default::default()
        };
        if RegisterClassW(&wnd_class) == 0 {
            let code = GetLastError().0;
            // ERROR_CLASS_ALREADY_EXISTS (1410) is fine on a repeated init.
            if code != 1410 {
                return Err(
                    format!("RegisterClassW(start_menu) failed (Win32 error {code})").into(),
                );
            }
        }

        let (x, y) = anchor_origin();
        // Hidden on creation: no `WS_VISIBLE`.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            x,
            y,
            ORB_W,
            ORB_H,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        ORB.store(hwnd.0 as isize, Ordering::SeqCst);

        // Three uniformly styled list boxes: apps (top), places, power (bottom).
        let rects = list_layout(ORB_H);
        let ids = [ID_LIST, ID_PLACES, ID_POWER];
        let slots = [&LIST, &PLACES, &POWER];
        let orig_slots = [&ORIG_LIST_PROC, &ORIG_PLACES_PROC, &ORIG_POWER_PROC];
        for i in 0..slots.len() {
            let child = create_listbox(hwnd, hinstance, ids[i], rects[i])?;
            slots[i].store(child.0 as isize, Ordering::SeqCst);
            // Subclass each list box so Enter/Esc reach our logic even though
            // the control owns the keyboard focus.
            let previous = subclass_list(child);
            orig_slots[i].store(previous, Ordering::SeqCst);
        }

        tracing::info!(hwnd = ?hwnd, "start menu popup created (hidden)");
    }
    Ok(())
}

/// Creates one `LISTBOX` child at `rect`, mirroring the original control style.
///
/// # Safety
/// `parent`/`hinstance` must be live objects; the returned handle is owned by
/// the parent window.
unsafe fn create_listbox(
    parent: HWND,
    hinstance: HINSTANCE,
    id: u16,
    rect: Rect,
) -> windows::core::Result<HWND> {
    let list_class = wide("LISTBOX");
    let no_text = wide("");
    // SAFETY: buffers outlive the call; `parent` is our live popup.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(list_class.as_ptr()),
            PCWSTR(no_text.as_ptr()),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_VSCROLL.0 | LBS_NOTIFY as u32,
            ),
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            Some(parent),
            Some(HMENU(id as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )
    }
}

/// Installs `list_proc` as `hwnd`'s window procedure, returning the previous
/// one (0 on failure).
///
/// # Safety
/// `hwnd` must be a live window that accepts `GWLP_WNDPROC`.
unsafe fn subclass_list(hwnd: HWND) -> isize {
    let addr = list_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    // SAFETY: `addr` is a valid `extern "system"` wndproc.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, addr as usize as isize) }
}

/// Screen-space origin: left-aligned, nudged up from the bottom edge (where
/// the ORB / brand dot sits). Pure so it can be unit-tested.
fn anchor_origin_for(screen_h: i32, win_h: i32) -> (i32, i32) {
    (0, (screen_h - win_h - 48).max(0))
}

fn anchor_origin() -> (i32, i32) {
    // SAFETY: pure metric queries.
    unsafe { anchor_origin_for(GetSystemMetrics(SM_CYSCREEN), ORB_H) }
}

/// Shows the menu, lazily populating the lists, and focuses row 0.
pub fn toggle() {
    if visible() {
        hide();
    } else {
        show();
    }
}

/// Whether the popup is currently on screen.
fn visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

/// Appends `labels` to the list box `list`, resetting it first. Each label is
/// copied into the control before the call returns, so the borrow need only
/// outlive this call.
///
/// # Safety
/// `list` must be a live list box.
unsafe fn fill_list<'a>(list: HWND, labels: impl IntoIterator<Item = &'a str>) {
    // SAFETY: `list` is a live list box; added strings are copied by the
    // control before each call returns.
    unsafe {
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for label in labels {
            let text = wide(label);
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
    }
}

/// Discovers shortcuts on first use and pushes app/place/power rows into the
/// list boxes.
fn ensure_items() {
    if LOADED.load(Ordering::SeqCst) {
        return;
    }
    let all = crate::launcher::discover_shortcuts();
    // Show the most-used apps first (empty query ranks by launch frequency).
    let found: Vec<crate::launcher::Shortcut> = crate::launcher::search_ranked(&all, "", all.len())
        .into_iter()
        .map(|s| crate::launcher::Shortcut {
            name: s.name.clone(),
            path: s.path.clone(),
        })
        .collect();
    tracing::info!(
        count = found.len(),
        places = places().len(),
        power = power_actions().len(),
        "start menu indexed shortcuts and built fixed sections"
    );
    // SAFETY: all three handles are our live list boxes.
    unsafe {
        if let Some(list) = list() {
            fill_list(list, found.iter().map(|s| s.name.as_str()));
        }
        if let Some(places_box) = places_list() {
            fill_list(places_box, places().iter().map(|p| p.label));
        }
        if let Some(power_box) = power_list() {
            fill_list(power_box, power_actions().iter().map(|a| a.label));
        }
    }
    *items_guard() = found;
    LOADED.store(true, Ordering::SeqCst);
}

/// Shows the popup, selects the first entry, and focuses the apps list.
fn show() {
    let Some(hwnd) = orb() else { return };
    ensure_items();
    // SAFETY: `hwnd`/`list` are our live windows.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Some(list) = list() {
            let count = SendMessageW(list, LB_GETCOUNT, None, None).0 as i32;
            if count > 0 {
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            }
            let _ = SetFocus(Some(list));
        }
    }
    VISIBLE.store(true, Ordering::SeqCst);
    tracing::info!("start menu shown");
}

/// Hides the popup without destroying it.
fn hide() {
    let Some(hwnd) = orb() else { return };
    // SAFETY: `hwnd` is our live popup window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    VISIBLE.store(false, Ordering::SeqCst);
    tracing::debug!("start menu hidden");
}

/// The selected row of the list box `list`, or `-1` when unset.
///
/// # Safety
/// `list` must be a live list box.
unsafe fn selected_index(list: HWND) -> i32 {
    // SAFETY: `list` is a live list box.
    (unsafe { SendMessageW(list, LB_GETCURSEL, None, None).0 }) as i32
}
/// Path of the currently selected app row, resolved via the module cache.
fn selected_path() -> Option<String> {
    let list = list()?;
    // SAFETY: `list` is our live list box.
    let index = unsafe { selected_index(list) };
    path_at(&items_guard(), index)
}

/// Launches the selected shortcut and hides the menu.
fn launch_selected() {
    let Some(path) = selected_path() else {
        tracing::debug!("start menu: nothing selected to launch");
        return;
    };
    match crate::launcher::launch(&path) {
        Ok(()) => tracing::info!(%path, "start menu launched shortcut"),
        Err(err) => tracing::warn!(%path, %err, "start menu launch failed"),
    }
    hide();
}

/// Opens `target` via `ShellExecuteW` (`open` verb). Returns `false` when the
/// shell reports an error (return value <= 32).
fn open_target(target: &str) -> bool {
    let operation = wide("open");
    let file = wide(target);
    // SAFETY: both wide buffers outlive the call; null `parameters`/`directory`
    // are valid for the shell `open` verb.
    unsafe {
        let result = ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        result.0 as isize > 32
    }
}

/// Opens the `Places` entry at `index` and hides the menu.
fn activate_place(index: i32) {
    let Some(place) = place_at(index) else {
        tracing::debug!("start menu: no place selected");
        return;
    };
    if open_target(place.target) {
        tracing::info!(
            label = place.label,
            target = place.target,
            "start menu opened place"
        );
    } else {
        tracing::warn!(
            label = place.label,
            target = place.target,
            "start menu open place failed"
        );
    }
    hide();
}

/// Opens whatever `Places` row is selected in the places list box.
fn activate_selected_place() {
    let Some(list) = places_list() else { return };
    // SAFETY: `list` is our live places list box.
    let index = unsafe { selected_index(list) };
    activate_place(index);
}

/// Asks the user to confirm `action` before executing it.
///
/// Defaults the message box to **No** so that a stray `Enter` cannot power the
/// machine off. Returns `true` only on an explicit Yes.
fn confirm_power(action: &PowerAction) -> bool {
    let text = wide(&format!("确定要{}吗？", action.label));
    let caption = wide("电源确认");
    // SAFETY: buffers outlive the call; `orb()` is our (possibly hidden) popup
    // and is a valid owner window (or `None`).
    unsafe {
        MessageBoxW(
            orb(),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

/// Runs `action` via `shutdown.exe` and hides the menu.
fn run_power(action: &PowerAction) {
    match std::process::Command::new("shutdown.exe")
        .args(action.command.args())
        .spawn()
    {
        Ok(_) => tracing::warn!(
            action = action.label,
            target = action.target,
            "start menu executed power action"
        ),
        Err(err) => {
            tracing::warn!(action = action.label, target = action.target, %err, "start menu power action failed")
        }
    }
}

/// Confirms then executes the selected `Power` entry.
///
/// Always logs intent at `warn` first, so even a declined prompt leaves an
/// audit trail — a deliberate guard against accidental shutdown.
fn activate_power(index: i32) {
    let Some(action) = power_at(index) else {
        tracing::debug!("start menu: no power action selected");
        return;
    };
    tracing::warn!(
        action = action.label,
        target = action.target,
        "start menu power action requested"
    );
    if !confirm_power(action) {
        tracing::info!(action = action.label, "start menu power action declined");
        return;
    }
    run_power(action);
    hide();
}

/// Confirms then executes the selected `Power` row of the power list box.
fn activate_selected_power() {
    let Some(list) = power_list() else { return };
    // SAFETY: `list` is our live power list box.
    let index = unsafe { selected_index(list) };
    activate_power(index);
}

/// Whether `new_focus` (the `WM_KILLFOCUS` wParam) lies outside the popup.
fn focus_left_popup(new_focus: isize) -> bool {
    if new_focus == 0 {
        return true;
    }
    let Some(hwnd) = orb() else { return true };
    // SAFETY: `hwnd` is our live popup; `IsChild` is a pure query.
    unsafe { !IsChild(hwnd, hwnd_from(new_focus)).as_bool() }
}

/// Window procedure for the start-menu popup.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if code == LBN_DBLCLK {
                match id {
                    ID_LIST => launch_selected(),
                    ID_PLACES => activate_selected_place(),
                    ID_POWER => activate_selected_power(),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        // Losing activation (clicking another app) dismisses the menu.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if visible() {
                hide();
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            if visible() && focus_left_popup(wparam.0 as isize) {
                hide();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            ORB.store(0, Ordering::SeqCst);
            LIST.store(0, Ordering::SeqCst);
            PLACES.store(0, Ordering::SeqCst);
            POWER.store(0, Ordering::SeqCst);
            ORIG_LIST_PROC.store(0, Ordering::SeqCst);
            ORIG_PLACES_PROC.store(0, Ordering::SeqCst);
            ORIG_POWER_PROC.store(0, Ordering::SeqCst);
            VISIBLE.store(false, Ordering::SeqCst);
            LOADED.store(false, Ordering::SeqCst);
            tracing::info!("start menu window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Subclass procedure for the three list boxes: maps Enter/Esc to the right
/// menu action (chosen by which list received the key) and delegates everything
/// else to the original `LISTBOX` procedure.
unsafe extern "system" fn list_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_RETURN.0 {
                match list_kind(hwnd) {
                    Some(ListKind::Apps) => launch_selected(),
                    Some(ListKind::Places) => activate_selected_place(),
                    Some(ListKind::Power) => activate_selected_power(),
                    None => {}
                }
                return LRESULT(0);
            }
            if key == VK_ESCAPE.0 {
                hide();
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS if visible() && focus_left_popup(wparam.0 as isize) => hide(),
        _ => {}
    }

    let previous = orig_proc_for(hwnd);
    if previous == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `previous` is the `WNDPROC` returned by `SetWindowLongPtrW`.
    let original: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
    unsafe { CallWindowProcW(original, hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, path: &str) -> Shortcut {
        Shortcut {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    }

    #[test]
    fn path_at_resolves_rows_to_paths() {
        let items = vec![item("Alpha", "a.lnk"), item("Beta", "b.lnk")];
        assert_eq!(path_at(&items, 0).as_deref(), Some("a.lnk"));
        assert_eq!(path_at(&items, 1).as_deref(), Some("b.lnk"));
    }

    #[test]
    fn path_at_rejects_out_of_range_and_negative() {
        let items = vec![item("Alpha", "a.lnk")];
        assert_eq!(path_at(&items, -1), None);
        assert_eq!(path_at(&items, 1), None);
        assert_eq!(path_at(&[], 0), None);
    }

    #[test]
    fn anchor_origin_sits_above_bottom_edge() {
        assert_eq!(anchor_origin_for(1080, 440), (0, 592));
    }

    #[test]
    fn anchor_origin_never_negative_on_short_screens() {
        assert_eq!(anchor_origin_for(200, 440).1, 0);
    }

    #[test]
    fn places_contains_required_targets() {
        let entries = places();
        assert_eq!(entries.len(), 5, "Places should have 5 entries");
        let labels: Vec<&str> = entries.iter().map(|p| p.label).collect();
        assert!(labels.contains(&"计算机"), "missing 计算机");
        assert!(labels.contains(&"设置"), "missing 设置");
        assert!(
            entries.iter().any(|p| p.target == "shell:MyComputerFolder"),
            "计算机 must open shell:MyComputerFolder"
        );
        assert!(
            entries.iter().any(|p| p.target == "ms-settings:"),
            "设置 must open ms-settings:"
        );
        // Targets are unique and non-empty.
        for p in entries {
            assert!(!p.label.is_empty() && !p.target.is_empty());
        }
    }

    #[test]
    fn place_at_bounds_and_lookup() {
        assert_eq!(place_at(0).map(|p| p.label), Some("计算机"));
        assert_eq!(place_at(-1), None);
        assert_eq!(place_at(places().len() as i32), None);
    }

    #[test]
    fn power_actions_contains_shutdown_and_restart() {
        let actions = power_actions();
        assert_eq!(actions.len(), 3, "Power should have 3 entries");
        let labels: Vec<&str> = actions.iter().map(|a| a.label).collect();
        assert!(labels.contains(&"关机"), "missing 关机");
        assert!(labels.contains(&"重启"), "missing 重启");
        assert!(labels.contains(&"注销"), "missing 注销");
    }

    #[test]
    fn power_actions_map_to_expected_shutdown_args() {
        let by_label = |l: &str| power_actions().iter().find(|a| a.label == l).copied();
        assert_eq!(by_label("关机").unwrap().command.args(), ["/s", "/t", "0"]);
        assert_eq!(by_label("重启").unwrap().command.args(), ["/r", "/t", "0"]);
        assert_eq!(by_label("注销").unwrap().command.args(), ["/l"]);
        // Every entry advertises a shutdown.exe command line.
        for a in power_actions() {
            assert!(
                a.target.starts_with("shutdown.exe"),
                "bad target: {}",
                a.target
            );
        }
    }

    #[test]
    fn power_at_bounds_and_lookup() {
        assert_eq!(power_at(0).map(|a| a.label), Some("注销"));
        assert_eq!(power_at(-1), None);
        assert_eq!(power_at(power_actions().len() as i32), None);
    }

    #[test]
    fn list_layout_stacks_three_boxes_without_overlap() {
        let [apps, places, power] = list_layout(ORB_H);
        // Apps starts at the margin and places/power follow with GAP between.
        assert_eq!(apps.x, MARGIN);
        assert_eq!(apps.y, MARGIN);
        assert_eq!(places.y, apps.y + apps.h + GAP);
        assert_eq!(power.y, places.y + places.h + GAP);
        // Everything fits inside the popup.
        assert!(power.y + power.h <= ORB_H - MARGIN);
        // Widths match and are positive.
        assert_eq!(apps.w, ORB_W - 2 * MARGIN);
        assert!(apps.h > 0 && places.h > 0 && power.h > 0);
    }
}
