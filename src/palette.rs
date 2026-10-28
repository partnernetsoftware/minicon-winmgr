//! Shared suite palette, independent of configuration and native window handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub bg: u32,
    pub surface: u32,
    pub hover: u32,
    pub accent: u32,
    pub text: u32,
    pub muted: u32,
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: 0x001F_1B1B,
            surface: 0x002C_2626,
            hover: 0x003B_3333,
            accent: 0x00F5_8B4C,
            text: 0x00E6_E6E6,
            muted: 0x00A2_9A9A,
        }
    }
}
pub fn base(light: bool) -> Theme {
    let rgb = |v: u32| (v & 0xff00) | ((v >> 16) & 255) | ((v & 255) << 16);
    let colors = if light {
        [0xF5F1E9, 0xFFFCF7, 0xE9E0D3, 0x9A5F34, 0x352A24, 0x827568]
    } else {
        [0x24211E, 0x2F2A25, 0x40372F, 0xC89864, 0xF0E6D8, 0xB9AB9B]
    };
    Theme {
        bg: rgb(colors[0]),
        surface: rgb(colors[1]),
        hover: rgb(colors[2]),
        accent: rgb(colors[3]),
        text: rgb(colors[4]),
        muted: rgb(colors[5]),
    }
}

/// Smallest contrast ratio a custom accent must reach against the background and surface
/// of the mode it applies to (WCAG non-text contrast).
pub const ACCENT_MIN_CONTRAST: f64 = 3.0;

/// WCAG relative luminance of a colour stored as `0x00BBGGRR`.
pub fn luminance(value: u32) -> f64 {
    let linear = |c: u32| {
        let c = c as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let r = linear(value & 0xff);
    let g = linear((value >> 8) & 0xff);
    let b = linear((value >> 16) & 0xff);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio between two colours stored as `0x00BBGGRR`, from 1.0 to 21.0.
pub fn contrast_ratio(a: u32, b: u32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a custom accent is legible on the background and surface it will sit on.
pub fn accent_allowed(accent: u32, bg: u32, surface: u32) -> bool {
    contrast_ratio(accent, bg) >= ACCENT_MIN_CONTRAST
        && contrast_ratio(accent, surface) >= ACCENT_MIN_CONTRAST
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    #[test]
    fn black_and_white_reach_the_full_ratio_and_equal_colours_reach_one() {
        assert!((contrast_ratio(0x0000_0000, 0x00FF_FFFF) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(0x0012_3456, 0x0012_3456) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn accent_gate_checks_both_background_and_surface() {
        let dark_bg = 0x0000_0000;
        let dark_surface = 0x0010_1010;
        assert!(accent_allowed(0x00F5_8B4C, dark_bg, dark_surface));
        assert!(!accent_allowed(0x0020_2020, dark_bg, dark_surface));
        // Legible on black but not on a light surface.
        assert!(!accent_allowed(0x00E0_E0E0, 0x00FF_FFFF, 0x00F0_F0F0));
    }
}
