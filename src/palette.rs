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
