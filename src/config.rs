//! Lightweight configuration: theme tokens, hotkey, taskbar visibility.
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.

/// Color tokens as `COLORREF`-style `0x00BBGGRR` values.
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
        Theme {
            bg: 0x001F_1B1B,
            surface: 0x002C_2626,
            hover: 0x003B_3333,
            accent: 0x00F5_8B4C,
            text: 0x00E6_E6E6,
            muted: 0x00A2_9A9A,
        }
    }
}

/// Full configuration, with defaults matching `docs/prd_设计语言.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub theme: Theme,
    /// Hotkey spec, e.g. `"alt+`"`.
    pub hotkey: String,
    pub show_taskbar: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: Theme::default(),
            hotkey: "alt+`".to_string(),
            show_taskbar: true,
        }
    }
}

/// Parses `0xRRGGBB` / `#RRGGBB` (input order: RGB) into a `0x00BBGGRR` COLORREF.
fn parse_color(s: &str) -> Option<u32> {
    let s = s.trim();
    let hex = if let Some(rest) = s.strip_prefix('#') {
        rest
    } else {
        s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?
    };
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let r = (v >> 16) & 0xFF;
    let g = (v >> 8) & 0xFF;
    let b = v & 0xFF;
    Some((b << 16) | (g << 8) | r)
}

/// Parses a boolean value accepting `true/false/1/0/yes/no` (case-insensitive).
fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Applies one trimmed `key = value` line, ignoring unknown keys and bad values.
fn apply(cfg: &mut Config, key: &str, value: &str) {
    match key.trim() {
        "theme.bg" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.bg = c;
            }
        }
        "theme.surface" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.surface = c;
            }
        }
        "theme.hover" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.hover = c;
            }
        }
        "theme.accent" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.accent = c;
            }
        }
        "theme.text" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.text = c;
            }
        }
        "theme.muted" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.muted = c;
            }
        }
        "hotkey" => {
            cfg.hotkey = value.trim().to_string();
        }
        "show_taskbar" => {
            if let Some(b) = parse_bool(value) {
                cfg.show_taskbar = b;
            }
        }
        _ => {}
    }
}

/// Parses a `key=value` config text over the defaults. Unknown keys are ignored;
/// color values may be `0xRRGGBB`.
pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            apply(&mut cfg, key, value);
        }
    }
    cfg
}

/// Loads configuration, falling back to defaults when absent or invalid.
pub fn load() -> Config {
    let path = if let Some(p) = std::env::var_os("MINICON_CONFIG") {
        std::path::PathBuf::from(p)
    } else {
        match std::env::current_exe() {
            Ok(exe) => match exe.parent() {
                Some(dir) => dir.join("minicon.conf"),
                None => return Config::default(),
            },
            Err(_) => return Config::default(),
        }
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(_) => Config::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_default() {
        assert_eq!(parse(""), Config::default());
        assert_eq!(parse("   \n\t\n"), Config::default());
    }

    #[test]
    fn single_key_override() {
        let cfg = parse("hotkey = ctrl+space\n");
        assert_eq!(cfg.hotkey, "ctrl+space");
        // Other fields remain default.
        assert_eq!(cfg.theme, Theme::default());
        assert!(cfg.show_taskbar);
    }

    #[test]
    fn color_hash_rgb_to_colorref() {
        // #RRGGBB 0x112233 -> COLORREF 0x00BBGGRR = 0x00332211
        let cfg = parse("theme.bg = #112233\n");
        assert_eq!(cfg.theme.bg, 0x0033_2211);
    }

    #[test]
    fn color_0x_prefix_rgb_to_colorref() {
        let cfg = parse("theme.accent = 0xF58B4C\n");
        // 0xF58B4C -> R=F5 G=8B B=4C -> 0x004C8BF5
        assert_eq!(cfg.theme.accent, 0x004C_8BF5);
    }

    #[test]
    fn color_uppercase_0x_prefix() {
        let cfg = parse("theme.text = 0XE6E6E6\n");
        assert_eq!(cfg.theme.text, 0x00E6_E6E6);
    }

    #[test]
    fn comments_and_blank_lines() {
        let text = "\
# a comment
; another comment
hotkey = alt+`

theme.surface = #2C2626
";
        let cfg = parse(text);
        assert_eq!(cfg.hotkey, "alt+`");
        // #2C2626 -> R=2C G=26 B=26 -> COLORREF 0x0026262C
        assert_eq!(cfg.theme.surface, 0x0026_262C);
    }

    #[test]
    fn whitespace_around_key_and_value() {
        let cfg = parse("   theme.hover   =   #3B3333   \n");
        // #3B3333 -> R=3B G=33 B=33 -> 0x0033333B
        assert_eq!(cfg.theme.hover, 0x0033_333B);
    }

    #[test]
    fn illegal_values_are_ignored() {
        let cfg = parse("theme.bg = notacolor\nshow_taskbar = maybe\nhotkey_is_wrong_but_ok = x\n");
        assert_eq!(cfg.theme.bg, Theme::default().bg);
        assert!(cfg.show_taskbar);
        // bad color lengths
        assert_eq!(parse("theme.bg = #12345").theme.bg, Theme::default().bg);
        assert_eq!(parse("theme.bg = 0xGGGGGG").theme.bg, Theme::default().bg);
        assert_eq!(parse("theme.bg = 123456").theme.bg, Theme::default().bg);
    }

    #[test]
    fn bool_variants() {
        for t in ["true", "1", "yes", "TRUE", "Yes"] {
            let cfg = parse(&format!("show_taskbar = {t}\n"));
            assert!(cfg.show_taskbar, "expected true for {t}");
        }
        for f in ["false", "0", "no", "FALSE", "No"] {
            let cfg = parse(&format!("show_taskbar = {f}\n"));
            assert!(!cfg.show_taskbar, "expected false for {f}");
        }
    }

    #[test]
    fn unknown_keys_ignored() {
        let cfg = parse("nope = 1\n#; mixed\nhotkey = x\n");
        assert_eq!(cfg.hotkey, "x");
    }

    #[test]
    fn line_without_equals_is_ignored() {
        assert_eq!(parse("garbage line\n"), Config::default());
    }
}
