//! Desktop settings dialog for replacement mode: one colour and one background photo.
//!
//! Modal and single-window. The photo list holds names and paths only (no
//! thumbnails) and is capped, so the dialog adds nothing resident once it closes.
//! Controls are standard BUTTON, COMBOBOX and STATIC children; IsDialogMessage
//! provides Tab, Enter (default button) and Escape (cancel button).
use crate::desktop_surface::Rgb;
use crate::win_util::{guard_abi, wide};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, FillRect, GetStockObject, HDC, HFONT, NULL_BRUSH, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, PWSTR};

const CLASS: &str = "minicon_winmgr_desktop_dialog";

#[cfg(test)]
/// Writes the window's rendered client as a 32-bit BMP for visual review.
pub(crate) unsafe fn save_bitmap(hwnd: HWND, path: &std::path::Path) {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
    };
    use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    // PrintWindow renders the whole window, frame included, so size by the window.
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let screen = unsafe { GetDC(None) };
    let memory = unsafe { CreateCompatibleDC(Some(screen)) };
    let bitmap = unsafe { CreateCompatibleBitmap(screen, w, h) };
    let old = unsafe { SelectObject(memory, bitmap.into()) };
    // PW_RENDERFULLCONTENT: include content that is not in the window's own surface.
    let _ = unsafe { PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(2)) };
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    unsafe {
        GetDIBits(
            memory,
            bitmap,
            0,
            h as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(memory, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
    }
    let mut file = Vec::with_capacity(54 + pixels.len());
    let size = 54 + pixels.len() as u32;
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&size.to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&w.to_le_bytes());
    file.extend_from_slice(&(-h).to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    file.extend_from_slice(&pixels);
    std::fs::write(path, file).unwrap();
}

const ID_COLOUR: usize = 3;
const ID_COMBO: usize = 4;
const ID_BROWSE: usize = 5;
const ID_COLOUR_TEXT: usize = 6;
const ID_CLEAR: usize = 7;
/// Largest number of system wallpapers listed.
const MAX_SYSTEM: usize = 300;
const MAX_DEPTH: usize = 2;

/// What the dialog returns on OK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub color: Option<Rgb>,
    pub wallpaper: Option<String>,
}

#[derive(Default)]
struct State {
    hwnd: HWND,
    colour: Option<Rgb>,
    colour_text: HWND,
    combo: HWND,
    /// Index 0 is "no photo"; the rest are paths shown in the combo box.
    photos: Vec<Option<String>>,
    outcome: Option<Option<Choice>>,
    font: HFONT,
}

thread_local! { static DIALOG: RefCell<Option<State>> = const { RefCell::new(None) }; }

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    DIALOG.with(|s| s.borrow_mut().as_mut().map(f))
}

fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: a window handle query; a zero result falls back to the system DPI.
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    if dpi == 0 {
        unsafe { GetDpiForSystem() }.max(96)
    } else {
        dpi.max(96)
    }
}

/// The settings panel's message font: Microsoft YaHei UI at 14 DIP, scaled to `dpi`.
fn message_font(dpi: u32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure is sized correctly and lives for the call.
    let ok = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some((&mut metrics as *mut NONCLIENTMETRICSW).cast()),
            0,
            dpi,
        )
    }
    .is_ok();
    if !ok {
        return HFONT::default();
    }
    metrics.lfMessageFont.lfHeight = -(14 * dpi as i32 / 96);
    let face = wide("Microsoft YaHei UI");
    metrics.lfMessageFont.lfFaceName = [0; 32];
    metrics.lfMessageFont.lfFaceName[..face.len()].copy_from_slice(&face);
    // SAFETY: the LOGFONT is fully initialised above.
    unsafe { CreateFontIndirectW(&metrics.lfMessageFont) }
}

/// Owner-drawn combo: the theme surface, the theme text colour, and the hover colour for
/// the highlighted row. The face (`itemID` -1) shows the current selection.
fn draw_combo_item(draw: &DRAWITEMSTRUCT) {
    let theme = crate::config::theme();
    let index = if draw.itemID == u32::MAX {
        with(|s| unsafe { SendMessageW(s.combo, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))) }.0)
            .unwrap_or(-1)
    } else {
        draw.itemID as isize
    };
    let highlighted = draw.itemState.0 & ODS_SELECTED.0 != 0 && draw.itemID != u32::MAX;
    let background = if highlighted {
        theme.hover
    } else {
        theme.surface
    };
    // SAFETY: the brush is created for this call and deleted before returning.
    unsafe {
        let brush = CreateSolidBrush(COLORREF(background));
        FillRect(draw.hDC, &draw.rcItem, brush);
        let _ = DeleteObject(brush.into());
    }
    let text = with(|s| {
        if index < 0 {
            return String::new();
        }
        let length = unsafe {
            SendMessageW(
                s.combo,
                CB_GETLBTEXTLEN,
                Some(WPARAM(index as usize)),
                Some(LPARAM(0)),
            )
        }
        .0;
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        unsafe {
            SendMessageW(
                s.combo,
                CB_GETLBTEXT,
                Some(WPARAM(index as usize)),
                Some(LPARAM(buffer.as_mut_ptr() as isize)),
            );
        }
        String::from_utf16_lossy(&buffer[..length as usize])
    })
    .unwrap_or_default();
    let mut text_rect = draw.rcItem;
    text_rect.left += scale(8);
    text_rect.right -= scale(8);
    let mut wide_text = wide(&text);
    wide_text.pop();
    // SAFETY: the text buffer and the rectangle live for the call.
    unsafe {
        SetTextColor(draw.hDC, COLORREF(theme.text));
        SetBkMode(draw.hDC, TRANSPARENT);
        DrawTextW(
            draw.hDC,
            &mut wide_text,
            &mut text_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
    }
}

fn scale(value: i32) -> i32 {
    // SAFETY: system DPI query with no arguments.
    let dpi = unsafe { GetDpiForSystem() };
    value * dpi.max(96) as i32 / 96
}

/// Shows the dialog modally and returns the chosen colour and photo, or `None` on cancel.
pub(crate) fn run(instance: HINSTANCE, initial: Choice, parent: HWND) -> Option<Choice> {
    let mut photos = vec![None];
    photos.extend(
        system_wallpapers()
            .into_iter()
            .map(|p| Some(p.display().to_string())),
    );
    if let Some(path) = &initial.wallpaper
        && !photos.iter().any(|p| p.as_deref() == Some(path.as_str()))
    {
        photos.push(Some(path.clone()));
    }
    let selected = photos
        .iter()
        .position(|p| p.as_deref() == initial.wallpaper.as_deref())
        .unwrap_or(0);
    DIALOG.with(|s| {
        *s.borrow_mut() = Some(State {
            colour: initial.color,
            photos,
            ..Default::default()
        })
    });
    let hwnd = create(instance, parent, selected);
    if hwnd.is_invalid() {
        DIALOG.with(|s| *s.borrow_mut() = None);
        return None;
    }
    let mut msg = MSG::default();
    loop {
        let decided = with(|s| s.outcome.is_some()).unwrap_or(true);
        if decided {
            break;
        }
        // SAFETY: the loop owns the thread's message queue while the dialog is open.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            // WM_QUIT: treat as cancel and re-post so the caller's loop still exits.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break;
        }
        // SAFETY: `msg` came from GetMessageW on this thread.
        if !unsafe { IsDialogMessageW(hwnd, &msg) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    let outcome = with(|s| s.outcome.take()).flatten().flatten();
    let font = with(|s| s.font).unwrap_or_default();
    unsafe {
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        if !font.is_invalid() {
            let _ = DeleteObject(font.into());
        }
    }
    DIALOG.with(|s| *s.borrow_mut() = None);
    outcome
}

fn create(instance: HINSTANCE, parent: HWND, selected: usize) -> HWND {
    let name = wide(CLASS);
    let class = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: instance,
        lpszClassName: PCWSTR(name.as_ptr()),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
            (windows::Win32::Graphics::Gdi::COLOR_BTNFACE.0 + 1) as *mut _,
        ),
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);
    }
    // The content size is what the layout measures; the frame is added around it.
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let ex_style = WS_EX_DLGMODALFRAME;
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: scale(380),
        bottom: scale(206),
    };
    // SAFETY: the rectangle is a valid out/in parameter for the call.
    let _ = unsafe { AdjustWindowRectEx(&mut outer, style, false, ex_style) };
    let width = outer.right - outer.left;
    let height = outer.bottom - outer.top;
    let title = wide(crate::i18n::text("桌面设置"));
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            PCWSTR(name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            (!parent.is_invalid()).then_some(parent),
            None,
            Some(instance),
            None,
        )
    }
    .unwrap_or_default();
    if hwnd.is_invalid() {
        return hwnd;
    }
    let font = message_font(dpi_of(hwnd));
    with(|s| s.font = font);
    let child = |class: &str,
                 text: &str,
                 style: WINDOW_STYLE,
                 x: i32,
                 y: i32,
                 w: i32,
                 h: i32,
                 id: usize|
     -> HWND {
        let control = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(wide(class).as_ptr()),
                PCWSTR(wide(text).as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                scale(x),
                scale(y),
                scale(w),
                scale(h),
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )
        }
        .unwrap_or_default();
        unsafe {
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        control
    };
    let tab = WS_TABSTOP;
    child(
        "STATIC",
        crate::i18n::text("颜色"),
        WINDOW_STYLE::default(),
        12,
        14,
        120,
        18,
        0,
    );
    child(
        "BUTTON",
        crate::i18n::text("选择颜色…"),
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab,
        12,
        34,
        140,
        26,
        ID_COLOUR,
    );
    let colour_text = child(
        "STATIC",
        "",
        WINDOW_STYLE::default(),
        162,
        40,
        120,
        18,
        ID_COLOUR_TEXT,
    );
    child(
        "BUTTON",
        crate::i18n::text("清除"),
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab,
        290,
        34,
        72,
        26,
        ID_CLEAR,
    );
    child(
        "STATIC",
        crate::i18n::text("背景图片"),
        WINDOW_STYLE::default(),
        12,
        74,
        120,
        18,
        0,
    );
    let combo = child(
        "COMBOBOX",
        "",
        WINDOW_STYLE((CBS_DROPDOWNLIST | CBS_OWNERDRAWFIXED | CBS_HASSTRINGS) as u32)
            | WS_VSCROLL
            | tab,
        12,
        94,
        350,
        220,
        ID_COMBO,
    );
    child(
        "BUTTON",
        crate::i18n::text("浏览…"),
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab,
        12,
        124,
        120,
        26,
        ID_BROWSE,
    );
    let ok = child(
        "BUTTON",
        crate::i18n::text("确定"),
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | tab,
        186,
        162,
        86,
        28,
        IDOK.0 as usize,
    );
    child(
        "BUTTON",
        crate::i18n::text("取消"),
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | tab,
        276,
        162,
        86,
        28,
        IDCANCEL.0 as usize,
    );
    let _ = ok;
    with(|s| {
        s.hwnd = hwnd;
        s.colour_text = colour_text;
        s.combo = combo;
    });
    populate(combo, selected);
    refresh_colour_text();
    crate::appearance::install(hwnd);
    unsafe {
        let _ = SetFocus(Some(GetDlgItem(Some(hwnd), IDOK.0).unwrap_or_default()));
        let _ = crate::win_util::force_foreground(hwnd);
    }
    hwnd
}

fn populate(combo: HWND, selected: usize) {
    let labels: Vec<String> = with(|s| {
        s.photos
            .iter()
            .map(|p| match p {
                None => crate::i18n::text("无图片").to_string(),
                Some(path) => photo_label(path),
            })
            .collect()
    })
    .unwrap_or_default();
    for label in &labels {
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(wide(label).as_ptr() as isize)),
            );
        }
    }
    unsafe {
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(selected)), Some(LPARAM(0)));
    }
}

/// Human label: the file name without extension, with its folder for system photos.
fn photo_label(path: &str) -> String {
    let path = Path::new(path);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match path.parent().and_then(|p| p.file_name()) {
        Some(folder) => format!("{}\\{stem}", folder.to_string_lossy()),
        None => stem,
    }
}

fn refresh_colour_text() {
    let (text, edit) = with(|s| {
        let text = match s.colour {
            Some((r, g, b)) => format!("#{r:02X}{g:02X}{b:02X}"),
            None => crate::i18n::text("未设置").to_string(),
        };
        (text, s.colour_text)
    })
    .unwrap_or_default();
    if !edit.is_invalid() {
        unsafe {
            let _ = SetWindowTextW(edit, PCWSTR(wide(&text).as_ptr()));
        }
    }
}

fn choose_colour(hwnd: HWND) {
    let current = with(|s| s.colour).flatten().unwrap_or((0, 0, 0));
    let initial = u32::from(current.0) | u32::from(current.1) << 8 | u32::from(current.2) << 16;
    let Some(value) = crate::colour_picker::pick(hwnd, initial) else {
        return;
    };
    let picked = (
        (value & 0xff) as u8,
        (value >> 8 & 0xff) as u8,
        (value >> 16 & 0xff) as u8,
    );
    with(|s| s.colour = Some(picked));
    refresh_colour_text();
}

fn browse(hwnd: HWND) {
    let filter: Vec<u16> = "Images (*.jpg;*.jpeg;*.png;*.bmp)\0*.jpg;*.jpeg;*.png;*.bmp\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 1024];
    let title = wide(crate::i18n::text("选择桌面图片"));
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        return;
    }
    let end = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    let path = String::from_utf16_lossy(&file[..end]);
    if path.trim().is_empty() {
        return;
    }
    let (combo, index) = with(|s| {
        let index = match s
            .photos
            .iter()
            .position(|p| p.as_deref() == Some(path.as_str()))
        {
            Some(index) => index,
            None => {
                s.photos.push(Some(path.clone()));
                s.photos.len() - 1
            }
        };
        (s.combo, index)
    })
    .unwrap_or_default();
    if index + 1 == with(|s| s.photos.len()).unwrap_or(0) {
        unsafe {
            SendMessageW(
                combo,
                CB_ADDSTRING,
                Some(WPARAM(0)),
                Some(LPARAM(wide(&photo_label(&path)).as_ptr() as isize)),
            );
        }
    }
    unsafe {
        SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(index)), Some(LPARAM(0)));
    }
}

fn finish(apply: bool) {
    let choice = with(|s| {
        if !apply {
            return None;
        }
        let selected =
            unsafe { SendMessageW(s.combo, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))) }.0;
        let wallpaper = usize::try_from(selected)
            .ok()
            .and_then(|i| s.photos.get(i).cloned())
            .flatten();
        Some(Choice {
            color: s.colour,
            wallpaper,
        })
    })
    .flatten();
    with(|s| s.outcome = Some(choice));
}

/// `%SystemRoot%\Web\Wallpaper` photos, shallow and capped, names only.
fn system_wallpapers() -> Vec<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Web")
        .join("Wallpaper");
    let mut found = Vec::new();
    collect(&root, 0, &mut found);
    found.sort();
    found.truncate(MAX_SYSTEM);
    found
}

fn collect(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if found.len() >= MAX_SYSTEM {
            return;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if depth + 1 < MAX_DEPTH {
                collect(&path, depth + 1, found);
            }
            continue;
        }
        let photo = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "bmp"));
        if photo {
            found.push(path);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "desktop_dialog::proc",
        || unsafe {
            match msg {
                WM_COMMAND => {
                    let id = wp.0 & 0xffff;
                    let notify = (wp.0 >> 16) & 0xffff;
                    if notify == BN_CLICKED as usize {
                        match id {
                            ID_COLOUR => choose_colour(hwnd),
                            ID_CLEAR => {
                                with(|s| s.colour = None);
                                refresh_colour_text();
                            }
                            ID_BROWSE => browse(hwnd),
                            x if x == IDOK.0 as usize => finish(true),
                            x if x == IDCANCEL.0 as usize => finish(false),
                            _ => {}
                        }
                        return LRESULT(0);
                    }
                }
                WM_MEASUREITEM => {
                    if wp.0 == ID_COMBO {
                        // SAFETY: for ownerdraw messages lp points at a MEASUREITEMSTRUCT.
                        let measure = &mut *(lp.0 as *mut MEASUREITEMSTRUCT);
                        measure.itemHeight = scale(24) as u32;
                        return LRESULT(1);
                    }
                }
                WM_DRAWITEM => {
                    if wp.0 == ID_COMBO {
                        // SAFETY: for ownerdraw messages lp points at a DRAWITEMSTRUCT.
                        let draw = &*(lp.0 as *const DRAWITEMSTRUCT);
                        draw_combo_item(draw);
                        return LRESULT(1);
                    }
                }
                WM_ERASEBKGND => {
                    // Paint the theme background so the window matches the settings panel.
                    let theme = crate::config::theme();
                    let brush = CreateSolidBrush(COLORREF(theme.bg));
                    let mut rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rect);
                    let _ = FillRect(HDC(wp.0 as *mut _), &rect, brush);
                    let _ = DeleteObject(brush.into());
                    return LRESULT(1);
                }
                WM_CTLCOLORSTATIC => {
                    // Labels are transparent over the themed background, in the theme text colour.
                    let hdc = HDC(wp.0 as *mut _);
                    SetTextColor(hdc, COLORREF(crate::config::theme().text));
                    SetBkMode(hdc, TRANSPARENT);
                    return LRESULT(GetStockObject(NULL_BRUSH).0 as isize);
                }
                WM_CLOSE => {
                    finish(false);
                    return LRESULT(0);
                }
                _ => {}
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        },
        || unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_labels_show_folder_and_stem_without_extension() {
        assert_eq!(
            photo_label(r"C:\Windows\Web\Wallpaper\Windows\img0.jpg"),
            r"Windows\img0"
        );
        assert_eq!(photo_label("loose.png"), "loose");
    }

    #[test]
    fn system_wallpaper_listing_is_shallow_and_capped() {
        let dir =
            std::env::temp_dir().join(format!("winmgr-wallpaper-test-{}", std::process::id()));
        let deep = dir.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.join("top.jpg"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("a").join("one.PNG"), b"x").unwrap();
        std::fs::write(deep.join("hidden.jpg"), b"x").unwrap();
        let mut found = Vec::new();
        collect(&dir, 0, &mut found);
        std::fs::remove_dir_all(&dir).unwrap();
        found.sort();
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["one.PNG", "top.jpg"]);
    }

    // Desktop attachment lives behind a feature this crate does not otherwise enable.
    #[link(name = "user32")]
    unsafe extern "system" {
        #[link_name = "GetThreadDesktop"]
        fn thread_desktop_raw(thread: u32) -> isize;
        #[link_name = "SetThreadDesktop"]
        fn set_thread_desktop_raw(desktop: isize) -> i32;
    }
    unsafe fn thread_desktop(thread: u32) -> isize {
        unsafe { thread_desktop_raw(thread) }
    }
    unsafe fn set_thread_desktop(desktop: isize) -> i32 {
        unsafe { set_thread_desktop_raw(desktop) }
    }

    #[test]
    #[ignore = "private desktop; use isolated_tray_probe.py --test dialog"]
    fn native_private_desktop_dialog_returns_ok_and_cancel() {
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::Threading::GetCurrentThreadId;
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.unwrap().0);
        // Press from another thread once the dialog is up. A new thread may start on
        // another desktop, so it is attached to this test's private desktop first.
        let desktop = unsafe { thread_desktop(GetCurrentThreadId()) };
        let press = |id: i32| {
            std::thread::spawn(move || {
                let _ = unsafe { set_thread_desktop(desktop) };
                std::thread::sleep(std::time::Duration::from_millis(800));
                let class = wide(CLASS);
                let hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }
                    .unwrap_or_default();
                assert!(!hwnd.is_invalid(), "dialog window not found");
                let _ = unsafe {
                    PostMessageW(
                        Some(hwnd),
                        WM_COMMAND,
                        WPARAM((id as usize) | ((BN_CLICKED as usize) << 16)),
                        LPARAM(0),
                    )
                };
            })
        };
        let initial = Choice {
            color: Some((0x1e, 0x3a, 0x5f)),
            wallpaper: None,
        };
        let handle = press(IDOK.0);
        let ok = run(instance, initial.clone(), HWND::default());
        handle.join().unwrap();
        assert_eq!(ok, Some(initial.clone()));
        let handle = press(IDCANCEL.0);
        let cancelled = run(instance, initial, HWND::default());
        handle.join().unwrap();
        assert_eq!(cancelled, None);
        // Clear, then OK: the colour is unset, which is how the surface is hidden.
        let desktop = unsafe { thread_desktop(GetCurrentThreadId()) };
        let clearing = std::thread::spawn(move || {
            let _ = unsafe { set_thread_desktop(desktop) };
            let class = wide(CLASS);
            let hwnd = {
                let mut found = HWND::default();
                for _ in 0..50 {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                    found = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }
                        .unwrap_or_default();
                    if !found.is_invalid() {
                        break;
                    }
                }
                found
            };
            assert!(!hwnd.is_invalid(), "dialog window not found");
            for id in [ID_CLEAR, IDOK.0 as usize] {
                let _ = unsafe {
                    PostMessageW(
                        Some(hwnd),
                        WM_COMMAND,
                        WPARAM(id | ((BN_CLICKED as usize) << 16)),
                        LPARAM(0),
                    )
                };
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        });
        let cleared = run(
            instance,
            Choice {
                color: Some((0x1e, 0x3a, 0x5f)),
                wallpaper: None,
            },
            HWND::default(),
        );
        clearing.join().unwrap();
        assert_eq!(
            cleared,
            Some(Choice {
                color: None,
                wallpaper: None
            })
        );
    }

    #[test]
    #[ignore = "private desktop screenshot; use isolated_tray_probe.py --test dialog-shots"]
    fn native_private_desktop_dialog_screenshots() {
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::Threading::GetCurrentThreadId;
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.unwrap().0);
        let folder = std::path::PathBuf::from("target/test-reports/dialog-shots");
        std::fs::create_dir_all(&folder).unwrap();
        let desktop = unsafe { thread_desktop(GetCurrentThreadId()) };
        let variants = [
            (
                "zh-light",
                crate::i18n::Language::Zh,
                crate::config::ThemeMode::Light,
            ),
            (
                "zh-dark",
                crate::i18n::Language::Zh,
                crate::config::ThemeMode::Dark,
            ),
            (
                "en-light",
                crate::i18n::Language::En,
                crate::config::ThemeMode::Light,
            ),
            (
                "en-dark",
                crate::i18n::Language::En,
                crate::config::ThemeMode::Dark,
            ),
        ];
        for (name, language, mode) in variants {
            crate::config::set_language(language);
            crate::config::set_theme_mode(mode);
            crate::config::refresh_theme();
            let path = folder.join(format!("{name}.bmp"));
            let shooter = std::thread::spawn(move || {
                let _ = unsafe { set_thread_desktop(desktop) };
                let class = wide(CLASS);
                let mut hwnd = HWND::default();
                for _ in 0..60 {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                    hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }
                        .unwrap_or_default();
                    if !hwnd.is_invalid() {
                        break;
                    }
                }
                assert!(!hwnd.is_invalid(), "dialog window not found");
                std::thread::sleep(std::time::Duration::from_millis(400));
                unsafe { save_bitmap(hwnd, &path) };
                let _ = unsafe {
                    PostMessageW(
                        Some(hwnd),
                        WM_COMMAND,
                        WPARAM(IDCANCEL.0 as usize | ((BN_CLICKED as usize) << 16)),
                        LPARAM(0),
                    )
                };
            });
            let _ = run(
                instance,
                Choice {
                    color: Some((0x1e, 0x3a, 0x5f)),
                    wallpaper: None,
                },
                HWND::default(),
            );
            shooter.join().unwrap();
        }
        crate::config::set_language(crate::i18n::Language::Zh);
        crate::config::set_theme_mode(crate::config::ThemeMode::Auto);
    }

    #[test]
    fn choice_compares_colour_and_photo_together() {
        let a = Choice {
            color: Some((1, 2, 3)),
            wallpaper: None,
        };
        assert_ne!(
            a,
            Choice {
                color: Some((1, 2, 3)),
                wallpaper: Some("x.jpg".into()),
            }
        );
    }
}
