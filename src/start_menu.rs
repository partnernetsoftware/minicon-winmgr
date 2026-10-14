//! Start menu / ORB — a categorized application menu popup.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.
//!
//! Self-contained: owns its window class and wndproc, reuses `crate::launcher`
//! for discovery/launch. `toggle()` is called by the taskbar's brand-dot click.
//!
//! Layout: a borderless top-most popup holding a search `EDIT` over four
//! stacked `LISTBOX`es of the same visual style — the discovered Start Menu
//! shortcuts (top), a `Places` navigation list, a `最近使用` (recents) list and
//! a `Power` list (bottom). Every control is subclassed so `Enter` / `Esc` are
//! handled without a dialog manager (the focused control owns the keyboard, so
//! both the search field and the list boxes need their own hooks).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{
    COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::CreateSolidBrush;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    EN_CHANGE, ES_AUTOHSCROLL, GWLP_WNDPROC, GetCursorPos, GetSystemMetrics, GetWindowTextLengthW,
    GetWindowTextW, HMENU, IDYES, IsChild, LB_ADDSTRING, LB_GETCOUNT, LB_GETCURSEL,
    LB_RESETCONTENT, LB_SETCURSEL, LBN_DBLCLK, LBS_NOTIFY, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO,
    MF_SEPARATOR, MF_STRING, MessageBoxW, RegisterClassW, SM_CYSCREEN, SW_HIDE, SW_SHOW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TRACK_POPUP_MENU_FLAGS, TrackPopupMenu,
    WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_COMMAND, WM_DESTROY, WM_KEYDOWN,
    WM_KILLFOCUS, WNDCLASS_STYLES, WNDCLASSW, WNDPROC, WS_BORDER, WS_CHILD, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::PCWSTR;

use crate::launcher::Shortcut;
use crate::win_util::{lock_or_recover, wide};

// --- geometry / ids -------------------------------------------------------
const ORB_W: i32 = 320;
const ORB_H: i32 = 440;
const MARGIN: i32 = 6;
/// Height of the search `EDIT` sitting at the very top of the popup.
const SEARCH_H: i32 = 22;
/// Gap between the search field and the four stacked list boxes.
const GAP: i32 = 4;
/// Nominal list-box row height (Win32 `LISTBOX` default item height in pixels).
const ROW_H: i32 = 16;
/// How many recents the `最近使用` box shows (its height is fixed to this many
/// rows; the surplus comes out of the app box).
const RECENT_ROWS: usize = 8;
/// How many pinned-folder rows the layout reserves height for. Extra pins
/// still get their rows (the box scrolls); this only bounds the box height so
/// a long pin list cannot squeeze the app list away.
const MAX_STACK_ROWS: usize = 6;
/// Maximum children listed when a pinned folder expands.
///
/// A native popup menu cannot scroll, so a long listing runs off the screen
/// and becomes unusable. Keep it to a scannable slice; the leading
/// "打开此文件夹" row is the escape hatch to the full listing in Explorer.
const MAX_STACK_ITEMS: usize = 15;

const ID_LIST: u16 = 2001;
const ID_PLACES: u16 = 2002;
const ID_POWER: u16 = 2003;
const ID_RECENT: u16 = 2004;
const ID_SEARCH: u16 = 2005;

const CLASS_NAME: &str = "minicon_winmgr_start_menu";
const WINDOW_TITLE: &str = "minicon start menu";
/// Light background so the popup is not a black rectangle before controls fill.
const BG_COLOR: COLORREF = COLORREF(0x00FF_FFFF);

// --- process-lifetime UI state --------------------------------------------
// Handles are stored as `isize`; the message thread is the sole user, so this
// is safe by construction.
static ORB: AtomicIsize = AtomicIsize::new(0);
static SEARCH: AtomicIsize = AtomicIsize::new(0);
static LIST: AtomicIsize = AtomicIsize::new(0);
static PLACES: AtomicIsize = AtomicIsize::new(0);
static RECENT: AtomicIsize = AtomicIsize::new(0);
static POWER: AtomicIsize = AtomicIsize::new(0);
static ORIG_SEARCH_PROC: AtomicIsize = AtomicIsize::new(0);
static ORIG_LIST_PROC: AtomicIsize = AtomicIsize::new(0);
static ORIG_PLACES_PROC: AtomicIsize = AtomicIsize::new(0);
static ORIG_RECENT_PROC: AtomicIsize = AtomicIsize::new(0);
static ORIG_POWER_PROC: AtomicIsize = AtomicIsize::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the shortcut/place/recents/power lists have been discovered and
/// pushed into the list boxes.
static LOADED: AtomicBool = AtomicBool::new(false);

/// The app rows currently shown, in list-box row order (so a row index can be
/// mapped back to its shortcut path when a search is active).
static ITEMS: Mutex<Vec<Shortcut>> = Mutex::new(Vec::new());
/// The `最近使用` rows currently shown, in list-box row order. Read once per
/// `show()` (from [`crate::recents`]) — never per keystroke.
static RECENT_ITEMS: Mutex<Vec<Shortcut>> = Mutex::new(Vec::new());

/// The full discovered shortcut set, cached for the process lifetime. Read-only
/// after the first call, so it needs no lock (avoids holding one across the
/// `recents` file IO in `search_ranked`).
fn all_items() -> &'static [Shortcut] {
    static ALL: OnceLock<Vec<Shortcut>> = OnceLock::new();
    ALL.get_or_init(|| {
        let found = crate::launcher::discover_shortcuts();
        tracing::info!(
            count = found.len(),
            "start menu indexed Start Menu shortcuts"
        );
        found
    })
}

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

/// The pinned folder rows, in pin order (labels from the last path segment).
///
/// Reads the live configuration, so it reflects the `folders = a;b` value from
/// either config file.
fn pinned_entries() -> Vec<crate::folder_stack::StackEntry> {
    crate::folder_stack::entries(&crate::config::current().folders)
}

/// Rows the places list box reserves height for: the fixed places plus the
/// pinned folders, the folder part capped by [`MAX_STACK_ROWS`].
fn place_row_count() -> usize {
    places().len() + pinned_entries().len().min(MAX_STACK_ROWS)
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

/// The search field spans the top margin, directly above the app list.
fn search_rect() -> Rect {
    Rect {
        x: MARGIN,
        y: MARGIN,
        w: ORB_W - 2 * MARGIN,
        h: SEARCH_H,
    }
}

/// Computes the stacked layout of [apps, places, recent, power] list boxes
/// inside a popup `orb_h` pixels tall, below the search field. `place_rows` is
/// the number of rows the places box reserves (fixed places plus pinned
/// folders). Pure so it can be unit-tested.
fn list_layout(orb_h: i32, place_rows: usize) -> [Rect; 4] {
    let w = ORB_W - 2 * MARGIN;
    let places_h = place_rows as i32 * ROW_H;
    let recent_h = RECENT_ROWS as i32 * ROW_H;
    let power_h = power_actions().len() as i32 * ROW_H;
    // The five stacked controls leave four gaps; the search field also eats its
    // own height off the top. Whatever is left belongs to the app list.
    let apps_h = (orb_h - 2 * MARGIN - SEARCH_H - 4 * GAP - places_h - recent_h - power_h).max(0);

    let apps_y = MARGIN + SEARCH_H + GAP;
    let apps = Rect {
        x: MARGIN,
        y: apps_y,
        w,
        h: apps_h,
    };
    let places = Rect {
        x: MARGIN,
        y: apps_y + apps_h + GAP,
        w,
        h: places_h,
    };
    let recent = Rect {
        x: MARGIN,
        y: places.y + places_h + GAP,
        w,
        h: recent_h,
    };
    let power = Rect {
        x: MARGIN,
        y: recent.y + recent_h + GAP,
        w,
        h: power_h,
    };
    [apps, places, recent, power]
}

/// Case-insensitive substring match used by the search box. An empty (or
/// whitespace-only) `query` matches everything; both sides are trimmed.
///
/// Deliberately *literal*: this is the predictable gate for typed input, and
/// the fuzzy scorer in [`crate::launcher`] is kept as a fallback for queries
/// that are only a loose subsequence.
fn matches(query: &str, name: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let needle = query.to_lowercase();
    name.trim().to_lowercase().contains(needle.as_str())
}

/// The first `limit` entries of `entries`, preserving order. `limit == 0`
/// yields nothing. Pure so it can be unit-tested.
fn recent_rows(entries: &[String], limit: usize) -> Vec<String> {
    entries.iter().take(limit).cloned().collect()
}

/// Display name for a recent `path`: the matching shortcut's name when the app
/// is still discovered, otherwise the bare file name.
fn resolve_name(all: &[Shortcut], path: &str) -> String {
    all.iter()
        .find(|s| s.path == path)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_owned())
                .unwrap_or_else(|| path.to_owned())
        })
}

/// Rows for the app list under `query`. Ordering is the launcher's (its default
/// most-used ranking for an empty query, fuzzy relevance otherwise); the
/// literal substring hits ([`matches`]) are lifted ahead of the fuzzy-only ones
/// so an obvious prefix always leads, while loose subsequence matches
/// ("vsc" → "Visual Studio Code") stay reachable below.
fn app_rows(all: &[Shortcut], query: &str) -> Vec<Shortcut> {
    let mut literal: Vec<Shortcut> = Vec::new();
    let mut fuzzy: Vec<Shortcut> = Vec::new();
    for s in crate::launcher::search_ranked(all, query, all.len()) {
        let row = Shortcut {
            name: s.name.clone(),
            path: s.path.clone(),
        };
        if matches(query, &s.name) {
            literal.push(row);
        } else {
            fuzzy.push(row);
        }
    }
    literal.extend(fuzzy);
    literal
}

/// Rows for the `最近使用` list box: the most-launched recents first, each
/// resolved to its shortcut name (falling back to the bare file name when the
/// shortcut is gone), capped at `limit`. Pure apart from nothing — takes the
/// already-loaded stats so no file IO happens under a lock.
fn recent_entries(all: &[Shortcut], stats: &[(String, u32)], limit: usize) -> Vec<Shortcut> {
    // `recents` records launch counts, not timestamps, so "recent" is read as
    // "most-used" — the same order the empty-query app list uses.
    let mut ordered: Vec<&(String, u32)> = stats.iter().collect();
    // Stable sort: equal counts keep their original (file) order.
    ordered.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let rows: Vec<Shortcut> = ordered
        .into_iter()
        .map(|(path, _)| Shortcut {
            name: resolve_name(all, path),
            path: path.clone(),
        })
        .collect();
    let labels: Vec<String> = rows.iter().map(|s| s.name.clone()).collect();
    let labels = recent_rows(&labels, limit);
    rows.into_iter().take(labels.len()).collect()
}

// --- handle helpers -------------------------------------------------------

fn hwnd_from(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

fn orb() -> Option<HWND> {
    existing(&ORB)
}

fn search_field() -> Option<HWND> {
    existing(&SEARCH)
}

fn list() -> Option<HWND> {
    existing(&LIST)
}

fn places_list() -> Option<HWND> {
    existing(&PLACES)
}

fn recent_list() -> Option<HWND> {
    existing(&RECENT)
}

fn power_list() -> Option<HWND> {
    existing(&POWER)
}

fn existing(slot: &AtomicIsize) -> Option<HWND> {
    let raw = slot.load(Ordering::SeqCst);
    (raw != 0).then(|| hwnd_from(raw))
}

/// Which of the four stacked list boxes `hwnd` is, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Apps,
    Places,
    Recent,
    Power,
}

fn list_kind(hwnd: HWND) -> Option<ListKind> {
    if list() == Some(hwnd) {
        Some(ListKind::Apps)
    } else if places_list() == Some(hwnd) {
        Some(ListKind::Places)
    } else if recent_list() == Some(hwnd) {
        Some(ListKind::Recent)
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
        Some(ListKind::Recent) => ORIG_RECENT_PROC.load(Ordering::SeqCst),
        Some(ListKind::Power) => ORIG_POWER_PROC.load(Ordering::SeqCst),
        None => 0,
    }
}

/// Locks the items cache, recovering from a poisoned mutex (the data is a plain
/// `Vec` with no invariants a panic could break).
fn items_guard() -> std::sync::MutexGuard<'static, Vec<Shortcut>> {
    lock_or_recover(&ITEMS)
}

/// Locks the recents-row cache; see [`items_guard`].
fn recent_guard() -> std::sync::MutexGuard<'static, Vec<Shortcut>> {
    lock_or_recover(&RECENT_ITEMS)
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

        // Search field at the very top: typing filters the app list below.
        let edit = create_edit(hwnd, hinstance, search_rect())?;
        SEARCH.store(edit.0 as isize, Ordering::SeqCst);
        let previous_edit = subclass_edit(edit);
        ORIG_SEARCH_PROC.store(previous_edit, Ordering::SeqCst);

        // Four uniformly styled list boxes: apps (top), places, recents, power.
        let rects = list_layout(ORB_H, place_row_count());
        let ids = [ID_LIST, ID_PLACES, ID_RECENT, ID_POWER];
        let slots = [&LIST, &PLACES, &RECENT, &POWER];
        let orig_slots = [
            &ORIG_LIST_PROC,
            &ORIG_PLACES_PROC,
            &ORIG_RECENT_PROC,
            &ORIG_POWER_PROC,
        ];
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

/// Creates the search `EDIT` child at `rect` (single-line, scrolling).
///
/// # Safety
/// `parent`/`hinstance` must be live objects; the returned handle is owned by
/// the parent window.
unsafe fn create_edit(
    parent: HWND,
    hinstance: HINSTANCE,
    rect: Rect,
) -> windows::core::Result<HWND> {
    let edit_class = wide("EDIT");
    let no_text = wide("");
    // SAFETY: buffers outlive the call; `parent` is our live popup.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(edit_class.as_ptr()),
            PCWSTR(no_text.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32),
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            Some(parent),
            Some(HMENU(ID_SEARCH as isize as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )
    }
}

/// Installs `search_proc` as `hwnd`'s window procedure, returning the previous
/// one (0 on failure).
///
/// # Safety
/// `hwnd` must be a live window that accepts `GWLP_WNDPROC`.
unsafe fn subclass_edit(hwnd: HWND) -> isize {
    let addr = search_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
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

/// Discovers shortcuts on first use and pushes app/place/recents/power rows
/// into the list boxes. Called once per process (guarded by [`LOADED`]).
fn ensure_items() {
    if LOADED.load(Ordering::SeqCst) {
        return;
    }
    let all = all_items();
    // Show the most-used apps first (empty query ranks by launch frequency).
    let found = app_rows(all, "");
    // Recents are file IO; read them here, once, with no lock held. We re-read
    // them (rather than reusing the ranking above) so we can fall back to bare
    // file names for recently launched apps whose shortcut has since vanished.
    let stats = crate::recents::load();
    let recent = recent_entries(all, &stats, RECENT_ROWS);
    tracing::info!(
        count = found.len(),
        recent = recent.len(),
        places = places().len(),
        pinned = pinned_entries().len(),
        power = power_actions().len(),
        "start menu indexed shortcuts and built fixed sections"
    );
    // SAFETY: all four handles are our live list boxes.
    unsafe {
        if let Some(list) = list() {
            fill_list(list, found.iter().map(|s| s.name.as_str()));
        }
        if let Some(places_box) = places_list() {
            // Fixed places first, then the pinned folders (which expand on
            // click); row indices line up with `activate_place`.
            let labels: Vec<String> = places()
                .iter()
                .map(|p| p.label.to_string())
                .chain(pinned_entries().into_iter().map(|e| e.label))
                .collect();
            fill_list(places_box, labels.iter().map(String::as_str));
        }
        if let Some(recent_box) = recent_list() {
            fill_list(recent_box, recent.iter().map(|s| s.name.as_str()));
        }
        if let Some(power_box) = power_list() {
            fill_list(power_box, power_actions().iter().map(|a| a.label));
        }
    }
    *items_guard() = found;
    *recent_guard() = recent;
    LOADED.store(true, Ordering::SeqCst);
}

/// Reads the current search-field text (empty when unavailable).
fn read_search_text() -> String {
    let Some(edit) = search_field() else {
        return String::new();
    };
    // SAFETY: `edit` is our live control; `buf` is sized from the control.
    unsafe {
        let len = GetWindowTextLengthW(edit);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(edit, &mut buf);
        if copied <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..copied as usize])
    }
}

/// Re-filters the app list for the current search text and repopulates it.
fn refresh_apps() {
    let Some(list) = list() else { return };
    let query = read_search_text();
    // `all_items` is lock-free, so the launcher's file IO (empty-query ranking)
    // never runs while holding a lock.
    let rows = app_rows(all_items(), &query);
    let count = rows.len();
    // SAFETY: `list` is our live list box; strings are copied by the control.
    unsafe {
        fill_list(list, rows.iter().map(|s| s.name.as_str()));
        if count > 0 {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
        }
    }
    *items_guard() = rows;
    tracing::debug!(count, %query, "start menu app list filtered");
}

/// Moves keyboard focus to the app list so its arrow keys scroll the results.
fn focus_apps() {
    let Some(list) = list() else { return };
    // SAFETY: `list` is our live list box.
    unsafe {
        let count = SendMessageW(list, LB_GETCOUNT, None, None).0 as i32;
        if count > 0 && selected_index(list) < 0 {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
        }
        let _ = SetFocus(Some(list));
    }
}

/// Shows the popup, clears the search field, and focuses it so the user can
/// start typing immediately. Row 0 of the app list is preselected.
fn show() {
    let Some(hwnd) = orb() else { return };
    ensure_items();
    // Clearing the field (empty already on the first show) resets the app list.
    if let Some(edit) = search_field() {
        let empty = wide("");
        // SAFETY: `empty` outlives the call; `edit` is our live control.
        unsafe {
            let _ = SetWindowTextW(edit, PCWSTR(empty.as_ptr()));
        }
    }
    // SAFETY: `hwnd`/`list`/`edit` are our live windows.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Some(list) = list() {
            let count = SendMessageW(list, LB_GETCOUNT, None, None).0 as i32;
            if count > 0 {
                SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            }
        }
        if let Some(edit) = search_field() {
            let _ = SetFocus(Some(edit));
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
    // Every place is a folder, i.e. an Explorer job. Going through
    // `shell_launch` guarantees Explorer receives an argument and therefore
    // stays a file browser instead of claiming the shell.
    crate::shell_launch::open(target)
}

/// Activates the `Places` row at `index`: a fixed place opens directly, a
/// pinned folder expands into a popup of its children.
fn activate_place(index: i32) {
    if let Some(place) = place_at(index) {
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
        return;
    }
    if index < 0 {
        tracing::debug!("start menu: no place selected");
        return;
    }
    // Rows past the fixed places are the pinned folders.
    match pinned_entries().get(index as usize - places().len()) {
        Some(entry) => {
            let entry = entry.clone();
            show_stack_menu(&entry);
            hide();
        }
        None => tracing::debug!("start menu: no place selected"),
    }
}

/// Menu flags for the folder popup (kept as a fn so tests can assert them).
fn stack_menu_flags() -> u32 {
    (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN).0
}

/// Expands a pinned folder into a popup listing its children.
///
/// Row 1 opens the folder itself; the following rows open each child. The
/// listing is capped by [`MAX_STACK_ITEMS`] so a huge folder cannot stall the
/// message thread, and every path goes through `folder_stack::open` — i.e. the
/// shell-safe launcher, because a bare `explorer.exe` would steal the shell.
fn show_stack_menu(entry: &crate::folder_stack::StackEntry) {
    let Some(anchor) = orb() else {
        tracing::debug!("stack menu: no anchor window");
        return;
    };
    let children = crate::folder_stack::list_dir(&entry.path, MAX_STACK_ITEMS);
    tracing::debug!(path = %entry.path, children = children.len(), "stack menu shown");
    // SAFETY: the menu and every `wide` buffer live only for this call; the
    // menu is destroyed before returning, and `anchor` is our live popup.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            tracing::warn!(path = %entry.path, "stack menu: CreatePopupMenu failed");
            return;
        };
        let open_label = wide("打开此文件夹");
        let _ = AppendMenuW(menu, MF_STRING, 1, PCWSTR(open_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        for (i, child) in children.iter().enumerate() {
            let label = wide(&child.label);
            // Ids are 1-based with 1 taken by "open", so children start at 2.
            let _ = AppendMenuW(menu, MF_STRING, i + 2, PCWSTR(label.as_ptr()));
        }

        // Anchor at the cursor, matching the overflow menu's behaviour.
        let mut pt = POINT::default();
        let (x, y) = match GetCursorPos(&mut pt) {
            Ok(()) => (pt.x, pt.y),
            Err(_) => (0, 0),
        };
        // Required so the popup dismisses when clicking elsewhere.
        let _ = SetForegroundWindow(anchor);
        let cmd = TrackPopupMenu(
            menu,
            TRACK_POPUP_MENU_FLAGS(stack_menu_flags()),
            x,
            y,
            Some(0),
            anchor,
            None,
        );
        // SAFETY: `menu` is no longer referenced after this point.
        let _ = DestroyMenu(menu);

        let id = cmd.0;
        if id == 1 {
            crate::folder_stack::open(&entry.path);
            tracing::info!(path = %entry.path, "stack menu opened folder");
        } else if id >= 2 && (id as usize) <= children.len() + 1 {
            let child = &children[id as usize - 2];
            crate::folder_stack::open(&child.path);
            tracing::info!(path = %child.path, label = %child.label, "stack menu opened child");
        }
    }
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

/// Launches the `最近使用` row at `index` (via the launcher's shell-safe path,
/// which also records the launch for future ranking) and hides the menu.
fn activate_selected_recent() {
    let Some(list) = recent_list() else { return };
    // SAFETY: `list` is our live recents list box.
    let index = unsafe { selected_index(list) };
    if index < 0 {
        tracing::debug!("start menu: no recent selected");
        return;
    }
    let path = recent_guard().get(index as usize).map(|s| s.path.clone());
    let Some(path) = path else {
        tracing::debug!("start menu: no recent selected");
        return;
    };
    match crate::launcher::launch(&path) {
        Ok(()) => tracing::info!(%path, "start menu launched recent"),
        Err(err) => tracing::warn!(%path, %err, "start menu recent launch failed"),
    }
    hide();
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
            if id == ID_SEARCH && code == EN_CHANGE {
                // The search box changed: refilter the app list.
                refresh_apps();
            } else if code == LBN_DBLCLK {
                match id {
                    ID_LIST => launch_selected(),
                    ID_PLACES => activate_selected_place(),
                    ID_RECENT => activate_selected_recent(),
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
            SEARCH.store(0, Ordering::SeqCst);
            LIST.store(0, Ordering::SeqCst);
            PLACES.store(0, Ordering::SeqCst);
            RECENT.store(0, Ordering::SeqCst);
            POWER.store(0, Ordering::SeqCst);
            ORIG_SEARCH_PROC.store(0, Ordering::SeqCst);
            ORIG_LIST_PROC.store(0, Ordering::SeqCst);
            ORIG_PLACES_PROC.store(0, Ordering::SeqCst);
            ORIG_RECENT_PROC.store(0, Ordering::SeqCst);
            ORIG_POWER_PROC.store(0, Ordering::SeqCst);
            VISIBLE.store(false, Ordering::SeqCst);
            LOADED.store(false, Ordering::SeqCst);
            tracing::info!("start menu window destroyed");
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Subclass procedure for the four list boxes: maps Enter/Esc to the right
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
                    Some(ListKind::Recent) => activate_selected_recent(),
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

/// Subclass procedure for the search field. The `EDIT` swallows key events, so
/// the popup's own `WM_KEYDOWN` never sees them — Enter/Esc/Down/Up have to be
/// intercepted here, on the control that actually owns the keyboard focus.
unsafe extern "system" fn search_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_RETURN.0 {
                launch_selected();
                return LRESULT(0);
            }
            if key == VK_ESCAPE.0 {
                hide();
                return LRESULT(0);
            }
            if key == VK_DOWN.0 || key == VK_UP.0 {
                // Hand the arrow keys to the app list; the native list box then
                // moves its own selection on subsequent presses.
                focus_apps();
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS if visible() && focus_left_popup(wparam.0 as isize) => hide(),
        _ => {}
    }

    let previous = ORIG_SEARCH_PROC.load(Ordering::SeqCst);
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
    fn list_layout_stacks_four_boxes_without_overlap() {
        let [apps, places, recent, power] = list_layout(ORB_H, places().len());
        // Apps starts below the search field; the rest follow with GAP between.
        assert_eq!(apps.x, MARGIN);
        assert_eq!(apps.y, MARGIN + SEARCH_H + GAP);
        assert_eq!(places.y, apps.y + apps.h + GAP);
        assert_eq!(recent.y, places.y + places.h + GAP);
        assert_eq!(power.y, recent.y + recent.h + GAP);
        // Every box sits strictly below the one above it and inside the popup.
        assert!(apps.y + apps.h <= places.y);
        assert!(places.y + places.h <= recent.y);
        assert!(recent.y + recent.h <= power.y);
        assert!(power.y + power.h <= ORB_H - MARGIN);
        // The search field clears the app box and stays inside the popup.
        let search = search_rect();
        assert!(search.y + search.h <= apps.y);
        assert_eq!(search.w, ORB_W - 2 * MARGIN);
        // Widths match and are positive.
        assert_eq!(apps.w, ORB_W - 2 * MARGIN);
        assert!(apps.h > 0 && places.h > 0 && recent.h > 0 && power.h > 0);
        assert_eq!(recent.h, RECENT_ROWS as i32 * ROW_H);
    }

    #[test]
    fn pinned_rows_borrow_height_from_the_app_box() {
        let [apps, ..] = list_layout(ORB_H, places().len());
        let [apps_pinned, places_pinned, recent_pinned, power_pinned] =
            list_layout(ORB_H, places().len() + MAX_STACK_ROWS);
        // The extra folder rows come out of the app box...
        assert!(apps_pinned.h < apps.h);
        // ...but every box still fits (and stays positive) inside the popup.
        assert!(apps_pinned.h > 0 && places_pinned.h > 0 && recent_pinned.h > 0);
        assert!(power_pinned.h > 0);
        assert!(power_pinned.y + power_pinned.h <= ORB_H - MARGIN);
    }

    #[test]
    fn matches_is_case_insensitive_substring() {
        assert!(matches("code", "Visual Studio Code"));
        assert!(matches("CODE", "Visual Studio Code"));
        assert!(matches("studio", "Visual Studio Code"));
        // Substring only: "vsc" is a fuzzy subsequence but not a literal one.
        assert!(!matches("vsc", "Visual Studio Code"));
        assert!(!matches("zzz", "Visual Studio Code"));
    }

    #[test]
    fn matches_empty_query_is_true_and_trims_both_sides() {
        assert!(matches("", "Anything"));
        assert!(matches("   ", "Anything"));
        assert!(matches("  code  ", "  Visual Studio Code "));
        assert!(!matches("   zzz  ", "  Visual Studio Code "));
    }

    #[test]
    fn recent_rows_truncates_and_keeps_order() {
        let entries: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            recent_rows(&entries, 2),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(recent_rows(&entries, 10), entries);
        assert!(recent_rows(&entries, 0).is_empty());
    }

    #[test]
    fn recent_rows_zero_limit_is_empty() {
        let entries = vec!["only".to_string()];
        assert!(recent_rows(&entries, 0).is_empty());
    }

    #[test]
    fn stack_menu_flags_include_return_cmd() {
        assert_ne!(stack_menu_flags() & TPM_RETURNCMD.0, 0);
    }
}
