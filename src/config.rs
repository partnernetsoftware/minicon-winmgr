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
//!
//! INTERFACE FROZEN — implement the signatures below; edit only this file.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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
            MergeStrategy::Never => "从不合并",
            MergeStrategy::WhenFull => "任务栏放不下时合并",
            MergeStrategy::Always => "始终合并",
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
            GroupBy::Exe => "按应用（可执行文件）",
            GroupBy::Pid => "按进程（pid）",
        }
    }
}

/// Full configuration, with defaults matching `docs/prd_4_1.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub theme: Theme,
    /// Hotkey spec, e.g. `"alt+`"`.
    pub hotkey: String,
    pub show_taskbar: bool,
    /// Whether the mirrored notification-icon strip is drawn in the right zone
    /// (`taskbar.tray`). Experimental/read-only — the strip currently shows only
    /// per-process placeholder squares, so it is toggleable but off-meaning.
    pub show_tray: bool,
    /// Taskbar window-merge policy (`taskbar.group`).
    pub merge: MergeStrategy,
    /// Grouping key when merging (`taskbar.group_by`).
    pub group_by: GroupBy,
    /// Hover thumbnail preview (`behavior.hover_preview`); off by default.
    pub hover_preview: bool,
    /// Whether the start menu (its taskbar entry dot) is available
    /// (`taskbar.menu`). **Off by default, and no longer offered in the UI**:
    /// the entry was retired because the window-list button already covers
    /// "find any window" (product decision, 2026-10-14). Retained as a
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
            theme: Theme::default(),
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
        "show_taskbar" | "taskbar.show" => {
            if let Some(b) = parse_bool(value) {
                cfg.show_taskbar = b;
            }
        }
        "taskbar.tray" => {
            if let Some(b) = parse_bool(value) {
                cfg.show_tray = b;
            }
        }
        "taskbar.group" => {
            if let Some(m) = MergeStrategy::parse(value) {
                cfg.merge = m;
            }
        }
        "taskbar.group_by" => {
            if let Some(g) = GroupBy::parse(value) {
                cfg.group_by = g;
            }
        }
        "behavior.hover_preview" => {
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
    R.get_or_init(|| Mutex::new(load()))
}

/// Snapshot of the live configuration (settings UI changes applied).
pub fn current() -> Config {
    runtime().lock().unwrap().clone()
}

/// Replaces the live config and persists the single changed key.
///
/// `key`/`value` describe the one field that changed, so persistence is a
/// key-level upsert that never touches the user's other lines.
fn update(f: impl FnOnce(&mut Config), key: &str, value: &str) -> bool {
    let mut guard = runtime().lock().unwrap();
    let before = guard.clone();
    f(&mut guard);
    if *guard == before {
        return false;
    }
    drop(guard);
    persist(key, value);
    true
}

/// Sets the merge policy. Returns whether anything changed.
pub fn set_merge(m: MergeStrategy) -> bool {
    update(|c| c.merge = m, "taskbar.group", m.as_key())
}

/// Sets the grouping key. Returns whether anything changed.
pub fn set_group_by(g: GroupBy) -> bool {
    update(|c| c.group_by = g, "taskbar.group_by", g.as_key())
}

/// Sets taskbar visibility (restart-effective). Returns whether it changed.
pub fn set_show_taskbar(b: bool) -> bool {
    update(
        |c| c.show_taskbar = b,
        "taskbar.show",
        if b { "true" } else { "false" },
    )
}

/// Sets whether the notification-icon strip is shown (effective immediately).
/// Returns whether anything changed.
pub fn set_show_tray(b: bool) -> bool {
    update(
        |c| c.show_tray = b,
        "taskbar.tray",
        if b { "true" } else { "false" },
    )
}

/// Sets the hover-preview preference (effective immediately). Returns whether
/// anything changed.
pub fn set_hover_preview(b: bool) -> bool {
    update(
        |c| c.hover_preview = b,
        "behavior.hover_preview",
        if b { "true" } else { "false" },
    )
}

/// Key-level upsert of `key = value` into `text`, preserving every other line
/// (comments included). Pure, so the round-trip is unit-tested.
pub fn upsert(text: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for raw in text.lines() {
        let is_target = raw
            .split_once('=')
            .map(|(k, _)| k.trim() == key)
            .unwrap_or(false);
        if is_target {
            out.push_str(key);
            out.push_str(" = ");
            out.push_str(value);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(raw);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// Writes one setting to the settings file, backfilling a `.bak` of the prior
/// contents first. Best effort: failures are logged, never fatal.
fn persist(key: &str, value: &str) {
    let Some(path) = settings_path() else {
        return;
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = upsert(&existing, key, value);
    if !existing.is_empty() {
        let mut bak = path.clone().into_os_string();
        bak.push(".bak");
        let bak = PathBuf::from(bak);
        if let Err(err) = std::fs::write(&bak, &existing) {
            tracing::warn!(path = %bak.display(), %err, "settings backup write failed");
        }
    }
    match std::fs::write(&path, &updated) {
        Ok(()) => tracing::info!(path = %path.display(), key, value, "setting persisted"),
        Err(err) => tracing::warn!(path = %path.display(), %err, "setting write failed"),
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
}
