//! Lightweight configuration: theme tokens, hotkey, taskbar visibility,
//! and the taskbar's window-merge policy.
//!
//! Two files feed the effective configuration, lowest precedence first:
//!
//! 1. `minicon.conf` (hand-written; theme + hotkey). **Never rewritten** — it
//!    has no writer, so the UI can never clobber a user's comments.
//! 2. `minicon-settings.txt` (owned by the settings UI; key-level upsert).
//!
//! Environment variables sit on top of both (`env > settings > conf > default`)
//! so debug/compat entry points such as `MINICON_GROUP_TASKS` keep working.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Config-file keys the settings UI writes.
///
/// Defined once, and used by **both** ends of a setting: the writer (`set_*`,
/// which persists the line) and the reader (`apply`, which parses it back).
/// They used to spell the key out separately, so renaming one side would have
/// silently stopped the setting from surviving a restart — see the
/// `every_settable_key_round_trips` test, which walks each pair through both
/// directions.
pub mod keys {
    pub const LANGUAGE: &str = "ui.language";
    pub const TASKBAR_SHOW: &str = "taskbar.show";
    pub const TASKBAR_TRAY: &str = "taskbar.tray";
    pub const TASKBAR_GROUP: &str = "taskbar.group";
    pub const TASKBAR_GROUP_BY: &str = "taskbar.group_by";
    pub const HOVER_PREVIEW: &str = "behavior.hover_preview";
    pub const THEME_MODE: &str = "appearance.mode";
    pub const TASKBAR_POSITION: &str = "taskbar.position";
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    Auto,
}
impl ThemeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Auto => "auto",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => crate::i18n::text("白天"),
            Self::Dark => crate::i18n::text("黑夜"),
            Self::Auto => crate::i18n::text("自动"),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}
impl Position {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bottom" => Some(Self::Bottom),
            "top" => Some(Self::Top),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Bottom => crate::i18n::text("下方"),
            Self::Top => crate::i18n::text("上方"),
            Self::Left => crate::i18n::text("左侧"),
            Self::Right => crate::i18n::text("右侧"),
        }
    }
    pub fn vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}

/// The `true`/`false` spelling used on disk for a boolean setting.
const fn bool_value(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

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

/// How task-window buttons are collapsed on the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStrategy {
    /// One button per window (the default; matches Alt+Tab).
    Never,
    /// One button per window until the bar cannot fit them even at the densest
    /// tier, then fall back to per-application grouping.
    WhenFull,
    /// One button per application.
    Always,
}

impl MergeStrategy {
    /// Parses the stored/typed spelling. Returns `None` for unknown values so
    /// callers can fall back to the default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "never" | "off" | "no" | "no_merge" => Some(MergeStrategy::Never),
            "when_full" | "whenfull" | "auto" | "full" => Some(MergeStrategy::WhenFull),
            "always" | "on" | "yes" | "merge" => Some(MergeStrategy::Always),
            _ => None,
        }
    }

    /// Canonical value written to the settings file.
    pub fn as_key(self) -> &'static str {
        match self {
            MergeStrategy::Never => "never",
            MergeStrategy::WhenFull => "when_full",
            MergeStrategy::Always => "always",
        }
    }

    /// User-facing label (also used by the settings radios).
    pub fn label(self) -> &'static str {
        match self {
            MergeStrategy::Never => crate::i18n::text("从不合并"),
            MergeStrategy::WhenFull => crate::i18n::text("任务栏放不下时合并"),
            MergeStrategy::Always => crate::i18n::text("始终合并"),
        }
    }
}

/// What distinguishes two windows that belong to the same application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
    /// The owning executable path (groups Chrome/Electron's per-window
    /// processes together). Falls back to the pid when the path is unreadable.
    Exe,
    /// The owning process id (keeps same-process windows together only).
    Pid,
}

impl GroupBy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "exe" | "path" | "app" => Some(GroupBy::Exe),
            "pid" | "process" | "proc" => Some(GroupBy::Pid),
            _ => None,
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            GroupBy::Exe => "exe",
            GroupBy::Pid => "pid",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GroupBy::Exe => crate::i18n::text("按应用（可执行文件）"),
            GroupBy::Pid => crate::i18n::text("按进程（pid）"),
        }
    }
}

/// Full configuration, with defaults matching `docs/prd_4_1.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub theme: Theme,
    pub language: crate::i18n::Language,
    pub theme_mode: ThemeMode,
    pub position: Position,
    theme_overrides: u8,
    /// Hotkey spec, e.g. `"alt+`"`.
    pub hotkey: String,
    pub show_taskbar: bool,
    /// Whether the notification strip is drawn in the right zone
    /// (`taskbar.tray`), default on.
    ///
    /// What the strip *is* depends on who owns the notification area (0.1.3 plan
    /// §2.1): when we are the shell it shows the icons apps registered with us —
    /// real icons, and clicks are forwarded back to the owning app; when another
    /// shell owns the tray we can only mirror it, so the cells are inert, faded
    /// placeholders that never forward anything. Turning it off removes the strip
    /// (and its reserved width) in either case.
    pub show_tray: bool,
    /// Taskbar window-merge policy (`taskbar.group`).
    pub merge: MergeStrategy,
    /// Grouping key when merging (`taskbar.group_by`).
    pub group_by: GroupBy,
    /// Hover thumbnail preview (`behavior.hover_preview`); off by default.
    pub hover_preview: bool,
    /// Whether the start menu (its taskbar entry dot) is available
    /// (`taskbar.menu`). **Off by default, and no longer offered in the UI**:
    /// the entry was retired in v0.1.2, in the same change that retired the
    /// window-list button — the owner judged both redundant (indication,
    /// 2026-10-14). **Neither** has a settings row now, so nothing here should
    /// be read as "the other control still covers it". Retained as a
    /// config-file / env escape hatch so the feature (Places, folder stacks,
    /// search, recents) stays reachable without shipping a menu row.
    pub start_menu: bool,
    /// Pinned folders rendered as start-menu rows (`folders = a;b`); each row
    /// expands to its children on click. Empty by default.
    pub folders: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: crate::i18n::Language::En,
            theme: Theme::default(),
            theme_mode: ThemeMode::Auto,
            position: Position::Bottom,
            theme_overrides: 0,
            hotkey: "alt+`".to_string(),
            show_taskbar: true,
            show_tray: true,
            merge: MergeStrategy::Never,
            group_by: GroupBy::Exe,
            hover_preview: false,
            start_menu: false,
            folders: Vec::new(),
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
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Applies one trimmed `key = value` line, ignoring unknown keys and bad values.
fn apply(cfg: &mut Config, key: &str, value: &str) {
    match key.trim() {
        "theme.bg" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.bg = c;
                cfg.theme_overrides |= 1;
            }
        }
        "theme.surface" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.surface = c;
                cfg.theme_overrides |= 2;
            }
        }
        "theme.hover" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.hover = c;
                cfg.theme_overrides |= 4;
            }
        }
        "theme.accent" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.accent = c;
                cfg.theme_overrides |= 8;
            }
        }
        "theme.text" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.text = c;
                cfg.theme_overrides |= 16;
            }
        }
        "theme.muted" => {
            if let Some(c) = parse_color(value) {
                cfg.theme.muted = c;
                cfg.theme_overrides |= 32;
            }
        }
        keys::LANGUAGE => {
            if let Some(language) = crate::i18n::Language::parse(value) {
                cfg.language = language;
            }
        }
        keys::THEME_MODE => {
            if let Some(mode) = ThemeMode::parse(value) {
                cfg.theme_mode = mode;
            }
        }
        keys::TASKBAR_POSITION => {
            if let Some(position) = Position::parse(value) {
                cfg.position = position;
            }
        }
        "hotkey" => {
            cfg.hotkey = value.trim().to_string();
        }
        "show_taskbar" | keys::TASKBAR_SHOW => {
            if let Some(b) = parse_bool(value) {
                cfg.show_taskbar = b;
            }
        }
        keys::TASKBAR_TRAY => {
            if let Some(b) = parse_bool(value) {
                cfg.show_tray = b;
            }
        }
        keys::TASKBAR_GROUP => {
            if let Some(m) = MergeStrategy::parse(value) {
                cfg.merge = m;
            }
        }
        keys::TASKBAR_GROUP_BY => {
            if let Some(g) = GroupBy::parse(value) {
                cfg.group_by = g;
            }
        }
        keys::HOVER_PREVIEW => {
            if let Some(b) = parse_bool(value) {
                cfg.hover_preview = b;
            }
        }
        "taskbar.menu" | "start_menu" => {
            if let Some(b) = parse_bool(value) {
                cfg.start_menu = b;
            }
        }
        // Semicolon-separated folder list; shared with the start menu.
        "folders" => {
            cfg.folders = crate::folder_stack::parse_folders(value);
        }
        _ => {}
    }
}

/// Applies every `key = value` line of `text` onto `cfg` (unknown keys ignored).
fn apply_text(cfg: &mut Config, text: &str) {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            apply(cfg, key, value);
        }
    }
}

/// Parses a `key=value` config text over the defaults. Unknown keys are ignored;
/// color values may be `0xRRGGBB`.
pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    apply_text(&mut cfg, text);
    cfg
}

/// Applies environment overrides on top of the file-derived configuration.
fn apply_env(cfg: &mut Config) {
    if let Some(language) = std::env::var("MINICON_LANGUAGE")
        .ok()
        .and_then(|v| crate::i18n::Language::parse(&v))
    {
        cfg.language = language;
    }
    if let Some(mode) = std::env::var("MINICON_THEME_MODE")
        .ok()
        .as_deref()
        .and_then(ThemeMode::parse)
    {
        cfg.theme_mode = mode;
    }
    if let Some(position) = std::env::var("MINICON_TASKBAR_POSITION")
        .ok()
        .as_deref()
        .and_then(Position::parse)
    {
        cfg.position = position;
    }
    if std::env::var_os("MINICON_NO_TASKBAR").is_some() {
        cfg.show_taskbar = false;
    }
    if let Ok(v) = std::env::var("MINICON_TASKBAR")
        && let Some(b) = parse_bool(&v)
    {
        cfg.show_taskbar = b;
    }
    // Compat/debug entry point: `MINICON_GROUP_TASKS=1` == `taskbar.group=always`.
    if let Ok(v) = std::env::var("MINICON_GROUP_TASKS")
        && let Some(b) = parse_bool(&v)
    {
        cfg.merge = if b {
            MergeStrategy::Always
        } else {
            MergeStrategy::Never
        };
    }
    if let Ok(v) = std::env::var("MINICON_GROUP_BY")
        && let Some(g) = GroupBy::parse(&v)
    {
        cfg.group_by = g;
    }
    if let Ok(v) = std::env::var("MINICON_HOVER_PREVIEW")
        && let Some(b) = parse_bool(&v)
    {
        cfg.hover_preview = b;
    }
    if let Ok(v) = std::env::var("MINICON_TRAY")
        && let Some(b) = parse_bool(&v)
    {
        cfg.show_tray = b;
    }
    if let Ok(v) = std::env::var("MINICON_START_MENU")
        && let Some(b) = parse_bool(&v)
    {
        cfg.start_menu = b;
    }
    if let Ok(v) = std::env::var("MINICON_FOLDERS") {
        cfg.folders = crate::folder_stack::parse_folders(&v);
    }
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Hand-written config path: `MINICON_CONFIG` or `<exe_dir>/minicon.conf`.
fn conf_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MINICON_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("minicon.conf"))
}

/// Path of the UI-owned settings file: `MINICON_SETTINGS` or
/// `<exe_dir>/minicon-settings.txt`.
pub fn settings_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MINICON_SETTINGS") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("minicon-settings.txt"))
}

/// Loads the effective configuration from the files, then the environment.
///
/// Precedence (lowest first): defaults → `minicon.conf` → `minicon-settings.txt`
/// → environment variables.
pub fn load() -> Config {
    let mut cfg = match conf_path().as_deref().and_then(read_text) {
        Some(text) => parse(&text),
        None => Config::default(),
    };
    if let Some(text) = settings_path().as_deref().and_then(read_text) {
        apply_text(&mut cfg, &text);
    }
    apply_env(&mut cfg);
    cfg
}

// --- live configuration ----------------------------------------------------
// The message thread is the sole writer (settings UI); other modules take a
// snapshot with [`current`]. This mirrors how the other UI statics are used.

fn runtime() -> &'static Mutex<Config> {
    static R: OnceLock<Mutex<Config>> = OnceLock::new();
    R.get_or_init(|| {
        let cfg = load();
        crate::i18n::set(cfg.language);
        Mutex::new(cfg)
    })
}

/// Snapshot of the live configuration (settings UI changes applied).
pub fn current() -> Config {
    crate::win_util::lock_or_recover(runtime()).clone()
}

fn taskbar_preference() -> &'static Mutex<bool> {
    static PREFERENCE: OnceLock<Mutex<bool>> = OnceLock::new();
    PREFERENCE.get_or_init(|| Mutex::new(current().show_taskbar))
}

pub(crate) fn preferred_taskbar() -> bool {
    *crate::win_util::lock_or_recover(taskbar_preference())
}

pub(crate) fn env_source_with(
    key: &str,
    get: impl Fn(&str) -> Option<String>,
) -> Option<&'static str> {
    match key {
        keys::LANGUAGE => get("MINICON_LANGUAGE")
            .and_then(|v| crate::i18n::Language::parse(&v))
            .map(|_| "MINICON_LANGUAGE"),
        keys::THEME_MODE => get("MINICON_THEME_MODE")
            .as_deref()
            .and_then(ThemeMode::parse)
            .map(|_| "MINICON_THEME_MODE"),
        keys::TASKBAR_POSITION => get("MINICON_TASKBAR_POSITION")
            .as_deref()
            .and_then(Position::parse)
            .map(|_| "MINICON_TASKBAR_POSITION"),
        keys::TASKBAR_SHOW => {
            if get("MINICON_TASKBAR")
                .as_deref()
                .and_then(parse_bool)
                .is_some()
            {
                Some("MINICON_TASKBAR")
            } else {
                get("MINICON_NO_TASKBAR").map(|_| "MINICON_NO_TASKBAR")
            }
        }
        keys::TASKBAR_GROUP => get("MINICON_GROUP_TASKS")
            .as_deref()
            .and_then(parse_bool)
            .map(|_| "MINICON_GROUP_TASKS"),
        keys::TASKBAR_GROUP_BY => get("MINICON_GROUP_BY")
            .as_deref()
            .and_then(GroupBy::parse)
            .map(|_| "MINICON_GROUP_BY"),
        keys::TASKBAR_TRAY => get("MINICON_TRAY")
            .as_deref()
            .and_then(parse_bool)
            .map(|_| "MINICON_TRAY"),
        keys::HOVER_PREVIEW => get("MINICON_HOVER_PREVIEW")
            .as_deref()
            .and_then(parse_bool)
            .map(|_| "MINICON_HOVER_PREVIEW"),
        _ => None,
    }
}

pub(crate) fn env_source(key: &str) -> Option<&'static str> {
    env_source_with(key, |name| {
        std::env::var_os(name).map(|s| s.to_string_lossy().into_owned())
    })
}

pub(crate) fn source(key: &'static str) -> String {
    if let Some(name) = env_source(key) {
        return crate::ui_format!(
            "由环境变量 {name} 控制",
            "Controlled by environment variable {name}"
        );
    }
    static SOURCES: OnceLock<std::collections::BTreeMap<&'static str, &'static str>> =
        OnceLock::new();
    let sources = SOURCES.get_or_init(|| {
        let mut sources = std::collections::BTreeMap::new();
        for (path, label) in [
            (conf_path(), "minicon.conf"),
            (settings_path(), crate::i18n::text("设置文件")),
        ] {
            if let Some(text) = path.as_deref().and_then(read_text) {
                for raw in text.lines() {
                    let Some((k, value)) = raw.split_once('=') else {
                        continue;
                    };
                    let k = k.trim();
                    let canonical = match k {
                        keys::LANGUAGE if crate::i18n::Language::parse(value).is_some() => {
                            keys::LANGUAGE
                        }
                        keys::THEME_MODE if ThemeMode::parse(value).is_some() => keys::THEME_MODE,
                        keys::TASKBAR_POSITION if Position::parse(value).is_some() => {
                            keys::TASKBAR_POSITION
                        }
                        "show_taskbar" | keys::TASKBAR_SHOW if parse_bool(value).is_some() => {
                            keys::TASKBAR_SHOW
                        }
                        keys::TASKBAR_TRAY if parse_bool(value).is_some() => keys::TASKBAR_TRAY,
                        keys::HOVER_PREVIEW if parse_bool(value).is_some() => keys::HOVER_PREVIEW,
                        keys::TASKBAR_GROUP if MergeStrategy::parse(value).is_some() => {
                            keys::TASKBAR_GROUP
                        }
                        keys::TASKBAR_GROUP_BY if GroupBy::parse(value).is_some() => {
                            keys::TASKBAR_GROUP_BY
                        }
                        _ => continue,
                    };
                    sources.insert(canonical, label);
                }
            }
        }
        sources
    });
    crate::ui_format!(
        "启动来源：{}",
        "Startup source: {}",
        crate::i18n::text(sources.get(key).copied().unwrap_or("内置默认"))
    )
}

/// Active palette, refreshed on preference changes and Windows theme notifications.
///
/// The taskbar, the bar-tip bubble and the settings window each used to wrap
/// the same config value in their own `OnceLock`. One accessor means one
/// answer, and a repaint still never touches the config file.
fn theme_cache() -> &'static Mutex<Theme> {
    static T: OnceLock<Mutex<Theme>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(effective_theme()))
}
pub fn theme() -> Theme {
    *crate::win_util::lock_or_recover(theme_cache())
}
pub(crate) fn refresh_theme() -> bool {
    let resolved = effective_theme();
    let mut theme = crate::win_util::lock_or_recover(theme_cache());
    let changed = *theme != resolved;
    *theme = resolved;
    changed
}
fn effective_theme() -> Theme {
    use windows::Win32::{
        Graphics::Gdi::*,
        UI::{Accessibility::*, WindowsAndMessaging::*},
    };
    let mut high = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        if SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high.cbSize,
            Some((&mut high as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && high.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
        {
            return Theme {
                bg: GetSysColor(COLOR_BTNFACE),
                surface: GetSysColor(COLOR_WINDOW),
                hover: GetSysColor(COLOR_HIGHLIGHT),
                accent: GetSysColor(COLOR_HIGHLIGHT),
                text: GetSysColor(COLOR_WINDOWTEXT),
                muted: GetSysColor(COLOR_WINDOWTEXT),
            };
        }
    }
    resolve_theme(&current(), system_light())
}
fn system_light() -> bool {
    use windows::{Win32::System::Registry::*, core::PCWSTR};
    let mut light = 1u32;
    let mut size = size_of::<u32>() as u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(
                crate::win_util::wide(
                    "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
                )
                .as_ptr(),
            ),
            PCWSTR(crate::win_util::wide("AppsUseLightTheme").as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut light as *mut u32).cast()),
            Some(&mut size),
        );
    }
    light != 0
}
fn resolve_theme(cfg: &Config, system_light: bool) -> Theme {
    let rgb = |value: u32| (value & 0xff00) | ((value >> 16) & 0xff) | ((value & 0xff) << 16);
    let light =
        cfg.theme_mode == ThemeMode::Light || (cfg.theme_mode == ThemeMode::Auto && system_light);
    let values = if light {
        [0xF5F1E9, 0xFFFCF7, 0xE9E0D3, 0x9A5F34, 0x352A24, 0x827568]
    } else {
        [0x24211E, 0x2F2A25, 0x40372F, 0xC89864, 0xF0E6D8, 0xB9AB9B]
    };
    let custom = [
        cfg.theme.bg,
        cfg.theme.surface,
        cfg.theme.hover,
        cfg.theme.accent,
        cfg.theme.text,
        cfg.theme.muted,
    ];
    let color = |index| {
        if cfg.theme_overrides & (1u8 << index) != 0 {
            custom[index]
        } else {
            rgb(values[index])
        }
    };
    Theme {
        bg: color(0),
        surface: color(1),
        hover: color(2),
        accent: color(3),
        text: color(4),
        muted: color(5),
    }
}
pub fn set_language(language: crate::i18n::Language) -> bool {
    if update(|c| c.language = language, keys::LANGUAGE, language.as_key()) {
        crate::i18n::set(language);
        true
    } else {
        false
    }
}
pub fn set_theme_mode(mode: ThemeMode) -> bool {
    update(|c| c.theme_mode = mode, keys::THEME_MODE, mode.as_key())
}
pub fn set_position(position: Position) -> bool {
    update(
        |c| c.position = position,
        keys::TASKBAR_POSITION,
        position.as_key(),
    )
}

/// Replaces the live config and persists the single changed key.
///
/// `key`/`value` describe the one field that changed, so persistence is a
/// key-level upsert that never touches the user's other lines.
fn update(f: impl FnOnce(&mut Config), key: &'static str, value: &str) -> bool {
    if env_source(key).is_some() {
        return false;
    }
    let mut guard = crate::win_util::lock_or_recover(runtime());
    let before = guard.clone();
    f(&mut guard);
    if *guard == before {
        return false;
    }
    drop(guard);
    crate::settings_store::enqueue(key, value);
    true
}

/// Sets the merge policy. Returns whether anything changed.
pub fn set_merge(m: MergeStrategy) -> bool {
    update(|c| c.merge = m, keys::TASKBAR_GROUP, m.as_key())
}

/// Sets the grouping key. Returns whether anything changed.
pub fn set_group_by(g: GroupBy) -> bool {
    update(|c| c.group_by = g, keys::TASKBAR_GROUP_BY, g.as_key())
}

/// Sets taskbar visibility (restart-effective). Returns whether it changed.
pub fn set_show_taskbar(b: bool) -> bool {
    if env_source(keys::TASKBAR_SHOW).is_some() {
        return false;
    }
    *crate::win_util::lock_or_recover(taskbar_preference()) = b;
    crate::settings_store::enqueue(keys::TASKBAR_SHOW, bool_value(b));
    true
}

/// Sets whether the notification strip is shown (effective immediately).
/// Returns whether anything changed.
pub fn set_show_tray(b: bool) -> bool {
    update(|c| c.show_tray = b, keys::TASKBAR_TRAY, bool_value(b))
}

/// Sets the hover-preview preference (effective immediately). Returns whether
/// anything changed.
pub fn set_hover_preview(b: bool) -> bool {
    update(|c| c.hover_preview = b, keys::HOVER_PREVIEW, bool_value(b))
}

/// Key-level upsert of `key = value` into `text`, preserving every other line
/// (comments included). Pure, so the round-trip is unit-tested.
pub fn upsert(text: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for raw in text.split_inclusive('\n') {
        let is_target = raw
            .split_once('=')
            .map(|(k, _)| {
                k.trim() == key || (key == keys::TASKBAR_SHOW && k.trim() == "show_taskbar")
            })
            .unwrap_or(false);
        if is_target {
            out.push_str(key);
            out.push_str(" = ");
            out.push_str(value);
            out.push_str(if raw.ends_with("\r\n") { "\r\n" } else { "\n" });
            replaced = true;
        } else {
            out.push_str(raw);
        }
    }
    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(value);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_preference_defaults_to_english_and_round_trips() {
        use crate::i18n::Language;
        assert_eq!(parse("").language, Language::En);
        assert_eq!(parse("ui.language = invalid").language, Language::En);
        for language in [Language::En, Language::Zh] {
            let text = upsert("unknown = keep\n", keys::LANGUAGE, language.as_key());
            assert_eq!(parse(&text).language, language);
            assert!(text.contains("unknown = keep"));
        }
        assert_eq!(
            env_source_with(keys::LANGUAGE, |_| Some("bad".into())),
            None
        );
        assert_eq!(
            env_source_with(keys::LANGUAGE, |_| Some("zh".into())),
            Some("MINICON_LANGUAGE")
        );
    }
    #[test]
    fn appearance_preferences_round_trip_and_ignore_invalid_values() {
        for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Auto] {
            for edge in [
                Position::Bottom,
                Position::Top,
                Position::Left,
                Position::Right,
            ] {
                let text = upsert(
                    &upsert("# keep\nunknown = yes\n", keys::THEME_MODE, mode.as_key()),
                    keys::TASKBAR_POSITION,
                    edge.as_key(),
                );
                let cfg = parse(&text);
                assert_eq!(cfg.theme_mode, mode);
                assert_eq!(cfg.position, edge);
                assert!(text.contains("unknown = yes"));
            }
        }
        let cfg = parse("appearance.mode = bad\ntaskbar.position = bad\n");
        assert_eq!(cfg.theme_mode, ThemeMode::Auto);
        assert_eq!(cfg.position, Position::Bottom);
        assert_eq!(
            env_source_with(keys::THEME_MODE, |_| Some("bad".into())),
            None
        );
        assert_eq!(
            env_source_with(keys::TASKBAR_POSITION, |_| Some("left".into())),
            Some("MINICON_TASKBAR_POSITION")
        );
    }
    #[test]
    fn auto_palette_follows_system_and_explicit_colors_survive_mode_changes() {
        let auto = Config::default();
        let light = parse("appearance.mode = light\n");
        let dark = parse("appearance.mode = dark\n");
        assert_eq!(resolve_theme(&auto, true), resolve_theme(&light, false));
        assert_eq!(resolve_theme(&auto, false), resolve_theme(&dark, true));
        assert_ne!(resolve_theme(&auto, true), resolve_theme(&auto, false));
        let custom = parse("appearance.mode = dark\ntheme.bg = #112233\n");
        assert_eq!(resolve_theme(&custom, true).bg, 0x332211);
        assert_eq!(
            resolve_theme(&custom, true).text,
            resolve_theme(&dark, true).text
        );
    }
    #[test]
    fn effective_environment_sources_follow_parser_precedence() {
        let vars = [
            ("MINICON_NO_TASKBAR", "1"),
            ("MINICON_TASKBAR", "true"),
            ("MINICON_GROUP_TASKS", "false"),
            ("MINICON_GROUP_BY", "nonsense"),
            ("MINICON_TRAY", "invalid"),
            ("MINICON_HOVER_PREVIEW", "yes"),
        ];
        let get = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        };
        assert_eq!(
            env_source_with(keys::TASKBAR_SHOW, get),
            Some("MINICON_TASKBAR")
        );
        assert_eq!(
            env_source_with(keys::TASKBAR_GROUP, get),
            Some("MINICON_GROUP_TASKS")
        );
        assert_eq!(env_source_with(keys::TASKBAR_GROUP_BY, get), None);
        assert_eq!(env_source_with(keys::TASKBAR_TRAY, get), None);
        assert_eq!(
            env_source_with(keys::HOVER_PREVIEW, get),
            Some("MINICON_HOVER_PREVIEW")
        );
        assert_eq!(
            env_source_with(keys::TASKBAR_SHOW, |k| (k == "MINICON_NO_TASKBAR")
                .then(|| "".into())),
            Some("MINICON_NO_TASKBAR")
        );
    }

    #[test]
    fn upsert_preserves_crlf_non_target_bytes_and_replaces_visibility_aliases() {
        let original =
            "# comment\r\nunknown = value\r\ntaskbar.show = true\r\nshow_taskbar = true\r\n# tail";
        let updated = upsert(original, keys::TASKBAR_SHOW, "false");
        assert!(updated.starts_with("# comment\r\nunknown = value\r\n"));
        assert!(updated.ends_with("# tail"));
        assert!(!parse(&updated).show_taskbar);
        assert_eq!(
            upsert("# no final newline", keys::TASKBAR_TRAY, "true"),
            "# no final newline\ntaskbar.tray = true\n"
        );
    }

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

    // --- merge policy ------------------------------------------------------

    #[test]
    fn merge_defaults_to_never() {
        assert_eq!(Config::default().merge, MergeStrategy::Never);
        assert_eq!(Config::default().group_by, GroupBy::Exe);
    }

    #[test]
    fn merge_keys_parse() {
        assert_eq!(parse("taskbar.group = always").merge, MergeStrategy::Always);
        assert_eq!(
            parse("taskbar.group = when_full").merge,
            MergeStrategy::WhenFull
        );
        assert_eq!(parse("taskbar.group = never").merge, MergeStrategy::Never);
        // Unknown value falls back to the default.
        assert_eq!(
            parse("taskbar.group = nonsense").merge,
            MergeStrategy::Never
        );
    }

    #[test]
    fn merge_strategy_round_trips_through_key() {
        for m in [
            MergeStrategy::Never,
            MergeStrategy::WhenFull,
            MergeStrategy::Always,
        ] {
            assert_eq!(MergeStrategy::parse(m.as_key()), Some(m));
            assert!(!m.label().is_empty());
        }
    }

    #[test]
    fn group_by_keys_parse() {
        assert_eq!(parse("taskbar.group_by = pid").group_by, GroupBy::Pid);
        assert_eq!(parse("taskbar.group_by = exe").group_by, GroupBy::Exe);
        assert_eq!(parse("taskbar.group_by = ???").group_by, GroupBy::Exe);
        for g in [GroupBy::Exe, GroupBy::Pid] {
            assert_eq!(GroupBy::parse(g.as_key()), Some(g));
        }
    }

    #[test]
    fn tray_toggle_parses_and_defaults_on() {
        assert!(Config::default().show_tray);
        assert!(!parse("taskbar.tray = false\n").show_tray);
        assert!(parse("taskbar.tray = yes\n").show_tray);
        // Unknown value leaves the default in place.
        assert!(parse("taskbar.tray = maybe\n").show_tray);
    }

    #[test]
    fn start_menu_is_opt_in_and_parses_both_keys() {
        // Off by default: the shell is taskbar-driven, the menu is opt-in.
        assert!(!Config::default().start_menu);
        assert!(parse("taskbar.menu = true\n").start_menu);
        assert!(parse("start_menu = 1\n").start_menu);
        assert!(!parse("taskbar.menu = off\n").start_menu);
        // Unknown value leaves the default in place.
        assert!(!parse("taskbar.menu = maybe\n").start_menu);
    }

    #[test]
    fn folders_parse_into_deduped_paths() {
        // Empty by default.
        assert!(Config::default().folders.is_empty());
        // Semicolon-separated, trimmed, duplicates dropped, order kept.
        let cfg = parse("folders = C:\\a; D:\\b ;; C:\\a\n");
        assert_eq!(cfg.folders, vec!["C:\\a", "D:\\b"]);
        // An empty value clears the list rather than leaving a blank entry.
        assert!(parse("folders =\n").folders.is_empty());
    }

    #[test]
    fn later_file_wins_over_earlier() {
        // Simulate conf then settings: the settings text is applied last.
        let mut cfg = parse("taskbar.group = never\n");
        apply_text(&mut cfg, "taskbar.group = always\n");
        assert_eq!(cfg.merge, MergeStrategy::Always);
    }

    // --- persistence -------------------------------------------------------

    #[test]
    fn upsert_replaces_only_the_target_line() {
        let text = "# my theme\ntheme.accent = #F58B4C\ntaskbar.group = never\n";
        let out = upsert(text, "taskbar.group", "always");
        assert_eq!(
            out,
            "# my theme\ntheme.accent = #F58B4C\ntaskbar.group = always\n"
        );
        // The comment and the untouched key survive verbatim.
        assert!(out.contains("# my theme"));
        assert!(out.contains("theme.accent = #F58B4C"));
    }

    #[test]
    fn upsert_appends_when_absent() {
        let out = upsert("theme.accent = #F58B4C\n", "taskbar.group", "always");
        assert_eq!(out, "theme.accent = #F58B4C\ntaskbar.group = always\n");
    }

    #[test]
    fn upsert_is_idempotent() {
        let once = upsert("", "taskbar.group", "always");
        let twice = upsert(&once, "taskbar.group", "always");
        assert_eq!(once, twice);
    }

    #[test]
    fn upsert_round_trips_through_parse() {
        let text = upsert("# keep me\n", "taskbar.group_by", "pid");
        let cfg = parse(&text);
        assert_eq!(cfg.group_by, GroupBy::Pid);
    }

    #[test]
    fn every_settable_key_round_trips_in_both_directions() {
        // A setting has two ends: `set_*` writes `<key> = <value>` and `apply`
        // has to read that same key back. While the key was spelled out at both
        // ends, renaming one silently broke persistence — the setting would look
        // applied and be gone after a restart. Walk every pair both ways.
        fn round_trip(key: &str, value: &str, holds: impl Fn(&Config) -> bool) {
            let mut direct = Config::default();
            apply(&mut direct, key, value);
            assert!(holds(&direct), "apply({key} = {value}) had no effect");

            let text = upsert("", key, value);
            let mut reparsed = Config::default();
            apply_text(&mut reparsed, &text);
            assert!(
                holds(&reparsed),
                "the persisted form {text:?} does not parse back into {key}"
            );
        }

        round_trip(keys::TASKBAR_SHOW, "false", |c| !c.show_taskbar);
        round_trip(keys::TASKBAR_TRAY, "false", |c| !c.show_tray);
        round_trip(keys::HOVER_PREVIEW, "true", |c| c.hover_preview);
        round_trip(keys::TASKBAR_GROUP, MergeStrategy::Always.as_key(), |c| {
            c.merge == MergeStrategy::Always
        });
        round_trip(keys::TASKBAR_GROUP_BY, GroupBy::Pid.as_key(), |c| {
            c.group_by == GroupBy::Pid
        });
    }

    #[test]
    fn the_setters_write_the_keys_the_parser_reads() {
        // Belt and braces on top of the round trip: no key may be spelled out as a
        // literal in the production half of this file, so a new setter cannot
        // quietly introduce a second copy of it.
        //
        // The test half is deliberately excluded — those literals are the
        // independent spelling a parser test should assert against, not a copy of
        // the constant.
        let source = include_str!("config.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("the file has a production half");
        for key in [
            keys::TASKBAR_SHOW,
            keys::TASKBAR_TRAY,
            keys::TASKBAR_GROUP,
            keys::TASKBAR_GROUP_BY,
            keys::HOVER_PREVIEW,
        ] {
            let literal = format!("\"{key}\"");
            assert_eq!(
                production.matches(&literal).count(),
                1,
                "{key} must be spelled out exactly once in production code (in `keys`); \
                 a second copy is a setting that can silently stop round-tripping"
            );
        }
    }
}
