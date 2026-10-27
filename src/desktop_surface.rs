//! Replacement-mode desktop surface: a solid colour, optionally with a photo over it.
//!
//! Replacement mode closes Explorer's desktop host, so nothing paints the
//! background. One borderless, non-activating window spans the virtual screen
//! at the bottom of the z-order. It owns no bitmaps: the colour is one brush, and
//! a photo is decoded band by band during painting (see `wallpaper_image`), so
//! nothing stays resident between paints. Companion mode leaves the desktop to
//! Explorer and never creates this window.
use crate::win_util::{guard_abi, wide};
use std::cell::RefCell;
use std::path::Path;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetStockObject,
    HBRUSH, InvalidateRect, PAINTSTRUCT,
};
use windows::Win32::UI::Controls::Dialogs::{
    CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW, GetOpenFileNameW, OFN_FILEMUSTEXIST,
    OFN_HIDEREADONLY, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, PWSTR};

const CLASS: &str = "minicon_winmgr_desktop";

/// An `(r, g, b)` triple as stored in `desktop.color`.
pub type Rgb = (u8, u8, u8);

struct Surface {
    instance: HINSTANCE,
    hwnd: HWND,
    brush: HBRUSH,
    color: Option<Rgb>,
    wallpaper: Option<String>,
}

thread_local! { static SURFACE: RefCell<Option<Surface>> = const { RefCell::new(None) }; }

fn colorref((r, g, b): Rgb) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn rgb_of(value: COLORREF) -> Rgb {
    (
        (value.0 & 0xff) as u8,
        (value.0 >> 8 & 0xff) as u8,
        (value.0 >> 16 & 0xff) as u8,
    )
}

/// Registers the window class and remembers the instance. Creates nothing visible.
pub(crate) fn install(instance: HINSTANCE) -> Result<(), Box<dyn std::error::Error>> {
    let name = wide(CLASS);
    let class = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: instance,
        lpszClassName: PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);
    }
    SURFACE.with(|s| {
        *s.borrow_mut() = Some(Surface {
            instance,
            hwnd: HWND::default(),
            brush: HBRUSH::default(),
            color: None,
            wallpaper: None,
        })
    });
    Ok(())
}

/// Shows the surface when we are the shell and a background is configured.
pub(crate) fn start() {
    if !crate::shell_registry::is_self_shell() {
        return;
    }
    let config = crate::config::current();
    if config.desktop_color.is_some() || config.desktop_wallpaper.is_some() {
        apply(config.desktop_color, config.desktop_wallpaper);
    }
}

/// Applies a colour and an optional photo path, or hides the surface when both are absent.
pub(crate) fn apply(color: Option<Rgb>, wallpaper: Option<String>) {
    let Some(instance) = SURFACE.with(|s| s.borrow().as_ref().map(|s| s.instance)) else {
        return;
    };
    if color.is_none() && wallpaper.is_none() {
        hide();
        return;
    }
    let hwnd = ensure_window(instance);
    if hwnd.is_invalid() {
        return;
    }
    SURFACE.with(|s| {
        let mut guard = s.borrow_mut();
        let Some(surface) = guard.as_mut() else {
            return;
        };
        if surface.color != color {
            unsafe {
                if !surface.brush.is_invalid() {
                    let _ = DeleteObject(surface.brush.into());
                }
                surface.brush = match color {
                    Some(color) => CreateSolidBrush(colorref(color)),
                    None => HBRUSH::default(),
                };
            }
            surface.color = color;
        }
        surface.wallpaper = wallpaper;
    });
    fit_and_show(hwnd);
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn ensure_window(instance: HINSTANCE) -> HWND {
    let existing = SURFACE.with(|s| s.borrow().as_ref().map(|s| s.hwnd).unwrap_or_default());
    if !existing.is_invalid() {
        return existing;
    }
    let name = wide(CLASS);
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(name.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(error) => {
            tracing::error!(%error, "desktop surface window failed; continuing without it");
            return HWND::default();
        }
    };
    SURFACE.with(|s| {
        if let Some(surface) = s.borrow_mut().as_mut() {
            surface.hwnd = hwnd;
        }
    });
    hwnd
}

fn fit_and_show(hwnd: HWND) {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Hides the surface and releases its brush; the window is kept for reuse.
fn hide() {
    let hwnd = SURFACE.with(|s| s.borrow().as_ref().map(|s| s.hwnd).unwrap_or_default());
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    SURFACE.with(|s| {
        if let Some(surface) = s.borrow_mut().as_mut() {
            if !surface.brush.is_invalid() {
                unsafe {
                    let _ = DeleteObject(surface.brush.into());
                }
            }
            surface.brush = HBRUSH::default();
            surface.color = None;
            surface.wallpaper = None;
        }
    });
}

pub(crate) fn shutdown() {
    hide();
    let hwnd = SURFACE.with(|s| s.borrow().as_ref().map(|s| s.hwnd).unwrap_or_default());
    if !hwnd.is_invalid() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    SURFACE.with(|s| *s.borrow_mut() = None);
}

/// Asks whether to choose a photo or a colour, then runs that chooser. Used only in
/// replacement mode, where the system personalisation page could not show the change.
pub(crate) fn choose_background() {
    let question = crate::i18n::text("是：选择图片    否：选择纯色\n取消：不更改");
    let answer = unsafe {
        MessageBoxW(
            None,
            PCWSTR(wide(question).as_ptr()),
            PCWSTR(wide(crate::i18n::text("桌面设置")).as_ptr()),
            MB_YESNOCANCEL | MB_ICONQUESTION,
        )
    };
    if answer == IDYES {
        choose_picture();
    } else if answer == IDNO {
        choose_colour();
    }
}

/// Opens the system colour chooser and applies the result.
fn choose_colour() {
    let config = crate::config::current();
    let current = config.desktop_color.unwrap_or((0, 0, 0));
    let mut custom = [COLORREF(0); 16];
    let mut chooser = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        rgbResult: colorref(current),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    if !unsafe { ChooseColorW(&mut chooser) }.as_bool() {
        return;
    }
    let color = rgb_of(chooser.rgbResult);
    crate::config::set_desktop_color(Some(color));
    apply(Some(color), config.desktop_wallpaper);
}

/// Opens the system file chooser for a photo and applies it over the current colour.
fn choose_picture() {
    let filter: Vec<u16> = "Images (*.jpg;*.jpeg;*.png;*.bmp)\0*.jpg;*.jpeg;*.png;*.bmp\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 1024];
    let title = wide(crate::i18n::text("选择桌面图片"));
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
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
    crate::config::set_desktop_wallpaper(Some(&path));
    let config = crate::config::current();
    apply(config.desktop_color, config.desktop_wallpaper);
}

/// Paints the background: colour (or black when only a photo is set), then the photo
/// band by band. A photo that cannot be drawn is logged and the colour remains.
fn paint(
    hwnd: HWND,
    hdc: windows::Win32::Graphics::Gdi::HDC,
    area: &windows::Win32::Foundation::RECT,
) {
    let (brush, wallpaper) = SURFACE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|s| (s.brush, s.wallpaper.clone()))
            .unwrap_or_default()
    });
    unsafe {
        let fill = if brush.is_invalid() {
            HBRUSH(GetStockObject(BLACK_BRUSH).0)
        } else {
            brush
        };
        let _ = FillRect(hdc, area, fill);
    }
    let Some(path) = wallpaper else {
        return;
    };
    let mut client = windows::Win32::Foundation::RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    if let Err(error) = crate::wallpaper_image::paint(hdc, width, height, Path::new(&path)) {
        tracing::warn!(%error, "desktop photo could not be drawn; keeping the colour background");
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    guard_abi(
        "desktop_surface::proc",
        || unsafe {
            match msg {
                WM_ERASEBKGND => return LRESULT(1),
                WM_PAINT => {
                    let mut paint_struct = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut paint_struct);
                    paint(hwnd, hdc, &paint_struct.rcPaint);
                    let _ = EndPaint(hwnd, &paint_struct);
                    return LRESULT(0);
                }
                WM_DISPLAYCHANGE => {
                    fit_and_show(hwnd);
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
    fn colour_order_round_trips_through_colorref() {
        let color = (0x12, 0x34, 0x56);
        assert_eq!(colorref(color), COLORREF(0x0056_3412));
        assert_eq!(rgb_of(colorref(color)), color);
        assert_eq!(rgb_of(COLORREF(0x00ff_ffff)), (255, 255, 255));
    }

    #[test]
    #[ignore = "private desktop; use isolated_tray_probe.py --test surface"]
    fn native_private_desktop_surface() {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        assert_eq!(std::env::var("MINICON_PRIVATE_DESKTOP").as_deref(), Ok("1"));
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.unwrap().0);
        install(instance).unwrap();
        // The chooser path persists first; the surface must follow from the saved value.
        assert!(crate::config::set_desktop_color(Some((0x1e, 0x3a, 0x5f))));
        let settings = crate::config::settings_path().unwrap();
        let saved = (0..60).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::read_to_string(&settings)
                .map(|text| text.contains("desktop.color") && text.contains("#1E3A5F"))
                .unwrap_or(false)
        });
        assert!(
            saved,
            "desktop.color was not persisted to {}",
            settings.display()
        );
        let hwnd = || SURFACE.with(|s| s.borrow().as_ref().unwrap().hwnd);
        let pump = || unsafe {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            let mut msg = MSG::default();
            while std::time::Instant::now() < deadline {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let pixel_at_centre = |hwnd: HWND| unsafe {
            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect).unwrap();
            let (x, y) = ((rect.right - rect.left) / 2, (rect.bottom - rect.top) / 2);
            let window = GetDC(Some(hwnd));
            let value = GetPixel(window, x, y);
            ReleaseDC(Some(hwnd), window);
            let (cx, cy) = (rect.left + x, rect.top + y);
            (
                value,
                WindowFromPoint(windows::Win32::Foundation::POINT { x: cx, y: cy }),
            )
        };

        apply(Some((0x1e, 0x3a, 0x5f)), None);
        let shown = hwnd();
        assert!(unsafe { IsWindowVisible(shown) }.as_bool());
        let mut rect = RECT::default();
        unsafe { GetWindowRect(shown, &mut rect) }.unwrap();
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        });
        pump();
        let (value, hit) = pixel_at_centre(shown);
        assert_eq!(value, colorref((0x1e, 0x3a, 0x5f)));
        assert_eq!(hit, shown);

        apply(Some((0xf0, 0xe0, 0x20)), None);
        pump();
        let (value, _) = pixel_at_centre(shown);
        assert_eq!(value, colorref((0xf0, 0xe0, 0x20)));

        // A photo over the colour must replace the centre pixel; the built-in
        // wallpaper is present on every supported Windows install.
        let photo = std::path::Path::new(r"C:\Windows\Web\Wallpaper\Windows\img0.jpg");
        if photo.is_file() {
            apply(Some((0xf0, 0xe0, 0x20)), Some(photo.display().to_string()));
            pump();
            let (value, _) = pixel_at_centre(shown);
            assert_ne!(value, colorref((0xf0, 0xe0, 0x20)));
            assert_ne!(value, COLORREF(u32::MAX));
        }

        apply(None, None);
        assert!(!unsafe { IsWindowVisible(shown) }.as_bool());
        shutdown();
    }
}
