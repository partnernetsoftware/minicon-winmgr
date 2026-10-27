//! Photo wallpaper painter for the replacement-mode desktop surface.
//!
//! Low-memory by construction: nothing is cached between calls. Each `paint`
//! opens the file with WIC, scales it (cover-fit) through a WIC scaler so codecs
//! that support it decode at reduced size, converts to 32bpp premultiplied BGRA,
//! and copies the visible rectangle out in bands of `BAND_ROWS` rows into one
//! small reusable buffer that is blitted with `SetDIBitsToDevice`. Peak pixel
//! memory owned here is about `BAND_ROWS * target_w * 4` bytes.
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC, SetDIBitsToDevice,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameDecode,
    IWICFormatConverter, IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    WICRect,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::core::PCWSTR;

/// Largest source image accepted, in pixels (width * height). Larger files are
/// rejected before any scaling so a pathological photo cannot balloon decoder
/// memory; 60 MP covers typical camera output.
pub(crate) const MAX_PIXELS: u64 = 60_000_000;

/// Rows per painted band; the only pixel buffer this module allocates.
const BAND_ROWS: u32 = 32;

/// Cover-fit result: the image is scaled to `scaled_w x scaled_h` (both at
/// least the target) and the target rectangle starts at `(crop_x, crop_y)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoverGeometry {
    pub scaled_w: u32,
    pub scaled_h: u32,
    pub crop_x: u32,
    pub crop_y: u32,
}

/// Scale `source` so it covers `target` while keeping aspect ratio, then
/// centre-crop. Returns `None` if any dimension is zero or the result overflows.
pub(crate) fn cover_geometry(
    source_w: u32,
    source_h: u32,
    target_w: u32,
    target_h: u32,
) -> Option<CoverGeometry> {
    if source_w == 0 || source_h == 0 || target_w == 0 || target_h == 0 {
        return None;
    }
    let (sw, sh, tw, th) = (
        source_w as u64,
        source_h as u64,
        target_w as u64,
        target_h as u64,
    );
    // Target relatively wider than source (tw/th >= sw/sh): width is binding.
    let (w, h) = if tw * sh >= th * sw {
        (tw, (sh * tw).div_ceil(sw).max(th))
    } else {
        ((sw * th).div_ceil(sh).max(tw), th)
    };
    let scaled_w = u32::try_from(w).ok()?;
    let scaled_h = u32::try_from(h).ok()?;
    Some(CoverGeometry {
        scaled_w,
        scaled_h,
        crop_x: (scaled_w - target_w) / 2,
        crop_y: (scaled_h - target_h) / 2,
    })
}

/// Split `height` rows into `(start_row, row_count)` bands of at most
/// `band_rows` rows; every row is covered exactly once, the last band may be short.
pub(crate) fn band_ranges(height: u32, band_rows: u32) -> impl Iterator<Item = (u32, u32)> {
    let step = band_rows.max(1);
    (0..height)
        .step_by(step as usize)
        .map(move |start| (start, step.min(height - start)))
}

/// Balances a successful `CoInitializeEx` (including `S_FALSE`) on drop.
struct Apartment(bool);

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn enter_apartment() -> Result<Apartment, String> {
    // SAFETY: plain COM initialisation of the current thread.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_ok() {
        Ok(Apartment(true))
    } else if hr == RPC_E_CHANGED_MODE {
        // COM is already usable in another mode; do not uninitialise it.
        Ok(Apartment(false))
    } else {
        Err("Wallpaper COM initialisation failed".into())
    }
}

/// Chain frame -> scaler -> 32bppPBGRA converter. Nothing is decoded until
/// `CopyPixels`; the scaler lets codecs with IWICBitmapSourceTransform (e.g.
/// JPEG) decode at reduced size instead of full resolution.
fn scaled_source(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    geometry: CoverGeometry,
) -> windows::core::Result<IWICFormatConverter> {
    // SAFETY: all interfaces are valid and used on their creating thread.
    unsafe {
        let scaler = factory.CreateBitmapScaler()?;
        scaler.Initialize(
            frame,
            geometry.scaled_w,
            geometry.scaled_h,
            WICBitmapInterpolationModeFant,
        )?;
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        Ok(converter)
    }
}

/// Draw the image at `path` covering `(0,0)-(target_w,target_h)` on `hdc`.
/// Decodes, paints in bands, and frees everything before returning.
pub(crate) fn paint(hdc: HDC, target_w: i32, target_h: i32, path: &Path) -> Result<(), String> {
    if target_w <= 0 || target_h <= 0 {
        return Err("Wallpaper target is empty".into());
    }
    if !path.is_file() {
        return Err("Wallpaper file not found".into());
    }
    let (tw, th) = (target_w as u32, target_h as u32);
    let _apartment = enter_apartment()?;
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

    // SAFETY: COM is initialised on this thread for the lifetime of `_apartment`,
    // which outlives every interface created below (declared earlier, dropped later).
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|_| "Wallpaper imaging factory unavailable".to_string())?;
    // SAFETY: `wide_path` is NUL-terminated and alive for the call.
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR(wide_path.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(|_| "Wallpaper file could not be decoded".to_string())?;
    // SAFETY: valid decoder; frame 0 exists for every supported still image.
    let frame =
        unsafe { decoder.GetFrame(0) }.map_err(|_| "Wallpaper image has no frame".to_string())?;
    let (mut sw, mut sh) = (0u32, 0u32);
    // SAFETY: out-pointers reference live locals.
    unsafe { frame.GetSize(&mut sw, &mut sh) }
        .map_err(|_| "Wallpaper image size unreadable".to_string())?;
    if sw as u64 * sh as u64 > MAX_PIXELS {
        return Err("Wallpaper image is too large".into());
    }
    let geometry =
        cover_geometry(sw, sh, tw, th).ok_or_else(|| "Wallpaper image is empty".to_string())?;

    let converter = scaled_source(&factory, &frame, geometry)
        .map_err(|_| "Wallpaper image could not be scaled".to_string())?;

    let stride = tw
        .checked_mul(4)
        .ok_or_else(|| "Wallpaper target too wide".to_string())?;
    let mut band = vec![0u8; stride as usize * BAND_ROWS.min(th) as usize];
    for (start, rows) in band_ranges(th, BAND_ROWS) {
        let rect = WICRect {
            X: geometry.crop_x as i32,
            Y: (geometry.crop_y + start) as i32,
            Width: target_w,
            Height: rows as i32,
        };
        let bytes = &mut band[..(stride * rows) as usize];
        // SAFETY: `bytes` holds exactly `rows` rows of `stride` bytes.
        unsafe { converter.CopyPixels(&rect, stride, bytes) }
            .map_err(|_| "Wallpaper image decode failed".to_string())?;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: target_w,
                biHeight: -(rows as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: `bytes` matches `info` (32bpp, top-down, `rows` x `target_w`).
        let copied = unsafe {
            SetDIBitsToDevice(
                hdc,
                0,
                start as i32,
                tw,
                rows,
                0,
                0,
                0,
                rows,
                bytes.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            )
        };
        if copied == 0 {
            return Err("Wallpaper band could not be drawn".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(scaled_w: u32, scaled_h: u32, crop_x: u32, crop_y: u32) -> Option<CoverGeometry> {
        Some(CoverGeometry {
            scaled_w,
            scaled_h,
            crop_x,
            crop_y,
        })
    }

    #[test]
    fn cover_landscape_source_on_narrower_target_crops_sides() {
        // 4000x2000 (2:1) onto 1920x1080 (16:9): height binds.
        assert_eq!(
            cover_geometry(4000, 2000, 1920, 1080),
            geo(2160, 1080, 120, 0)
        );
    }

    #[test]
    fn cover_portrait_source_crops_top_and_bottom() {
        // 1000x2000 onto 1920x1080: width binds, height 3840.
        assert_eq!(
            cover_geometry(1000, 2000, 1920, 1080),
            geo(1920, 3840, 0, 1380)
        );
    }

    #[test]
    fn cover_exact_match_and_same_aspect_need_no_crop() {
        assert_eq!(
            cover_geometry(1920, 1080, 1920, 1080),
            geo(1920, 1080, 0, 0)
        );
        assert_eq!(
            cover_geometry(3840, 2160, 1920, 1080),
            geo(1920, 1080, 0, 0)
        );
    }

    #[test]
    fn cover_zero_input_is_none() {
        assert_eq!(cover_geometry(0, 10, 10, 10), None);
        assert_eq!(cover_geometry(10, 0, 10, 10), None);
        assert_eq!(cover_geometry(10, 10, 0, 10), None);
        assert_eq!(cover_geometry(10, 10, 10, 0), None);
    }

    #[test]
    fn cover_always_covers_target() {
        for &(sw, sh, tw, th) in &[
            (3, 7, 1920, 1080),
            (7, 3, 1080, 1920),
            (1, 60_000, 3840, 2160),
        ] {
            let g = cover_geometry(sw, sh, tw, th).unwrap();
            assert!(g.scaled_w >= tw && g.scaled_h >= th);
            assert!(g.crop_x + tw <= g.scaled_w && g.crop_y + th <= g.scaled_h);
        }
    }

    #[test]
    fn bands_cover_every_row_once_with_short_last_band() {
        let bands: Vec<_> = band_ranges(70, 32).collect();
        assert_eq!(bands, vec![(0, 32), (32, 32), (64, 6)]);
        let mut next = 0;
        for (start, rows) in band_ranges(1081, BAND_ROWS) {
            assert_eq!(start, next);
            assert!((1..=BAND_ROWS).contains(&rows));
            next += rows;
        }
        assert_eq!(next, 1081);
        assert_eq!(
            band_ranges(64, 32).collect::<Vec<_>>(),
            vec![(0, 32), (32, 32)]
        );
        assert_eq!(band_ranges(0, 32).count(), 0);
    }

    #[test]
    #[ignore = "measurement; run with explicit path"]
    fn measure_paint_memory_and_time() {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC,
            SelectObject,
        };
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;

        fn memory() -> (usize, usize) {
            let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
            // SAFETY: EX begins with the base struct; cb states the full size.
            unsafe {
                let _ = GetProcessMemoryInfo(
                    GetCurrentProcess(),
                    (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                    size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                );
            }
            (c.WorkingSetSize, c.PrivateUsage)
        }

        let path =
            std::env::var_os("MINICON_WALLPAPER_PROBE").expect("set MINICON_WALLPAPER_PROBE");
        let path = std::path::PathBuf::from(path);
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            // SAFETY: standard memory-DC setup and teardown on this thread.
            unsafe {
                let screen = GetDC(None);
                let dc = CreateCompatibleDC(Some(screen));
                let bitmap = CreateCompatibleBitmap(screen, w, h);
                let old = SelectObject(dc, bitmap.into());
                let (ws0, pb0) = memory();
                let started = std::time::Instant::now();
                let result = paint(dc, w, h, &path);
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                let (ws1, pb1) = memory();
                println!(
                    "{w}x{h}: {result:?} {ms:.1} ms; working set {} -> {} KiB; private {} -> {} KiB",
                    ws0 / 1024,
                    ws1 / 1024,
                    pb0 / 1024,
                    pb1 / 1024
                );
                SelectObject(dc, old);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                ReleaseDC(None, screen);
            }
        }
    }
}
