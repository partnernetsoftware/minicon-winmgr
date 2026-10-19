//! Shell-safe launching — the one place that starts things for the user.
//!
//! ## Why this module exists
//!
//! `explorer.exe` has two personalities: it is the file browser *and* the
//! shell. Which one it becomes depends on its command line. Started **without
//! arguments** it finds no `Shell_TrayWnd`, assumes the shell is missing, and
//! creates the desktop plus its own taskbar — which silently pushes our bar
//! out and hands the session back to Explorer. Started **with an argument**
//! (a path or a folder moniker) it just opens a browsing window.
//!
//! So a shell replacement must never let a bare `explorer.exe` escape. Every
//! "open" we originate — quick-launch pins, start-menu places, the run dialog,
//! the launcher — goes through [`open`], which rewrites the dangerous cases
//! into safe ones (see [`plan`]).
//!
//! The mapping itself is a pure function so the rule can be unit-tested
//! without a display.

use windows::Win32::Foundation::{CO_E_APPNOTFOUND, GetLastError, HWND};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use crate::win_util::wide;

/// The "This PC" folder, expressed as a CLSID Explorer can open.
///
/// Used as the argument whenever something asked for Explorer with nothing to
/// open: an argument is what keeps Explorer in file-browser mode.
pub const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// What to actually run for a requested target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// File/document handed to `ShellExecuteW` (`lpFile`).
    pub file: String,
    /// Arguments (`lpParameters`); empty means "none".
    pub params: String,
}

/// A run-dialog request. Parsing and execution may touch disk and belong on a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub file: String,
    pub params: String,
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchError {
    pub code: Option<i32>,
    pub message: String,
}

impl LaunchError {
    fn input(message: &str) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }
}

/// Expand known environment variables once. Unknown names remain visible.
pub(crate) fn expand_target(target: &str, lookup: &impl Fn(&str) -> Option<String>) -> String {
    let mut result = String::new();
    let mut rest = target;
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('%') else {
            result.push_str(&rest[start..]);
            return result;
        };
        let name = &tail[..end];
        if let Some(value) = lookup(name) {
            result.push_str(&value);
        } else {
            result.push_str(&rest[start..start + end + 2]);
        }
        rest = &tail[end + 1..];
    }
    result.push_str(rest);
    result
}

fn whole_target(input: &str) -> Option<&str> {
    if let Some(tail) = input.strip_prefix('"') {
        let end = tail.find('"')?;
        (end + 2 == input.len()).then_some(&tail[..end])
    } else {
        Some(input)
    }
}

fn is_uri(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Injected filesystem/environment access makes boundary rules independently testable.
pub fn parse_command_with(
    input: &str,
    home: Option<&str>,
    system_root: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
    exists: impl Fn(&str) -> bool,
) -> Result<LaunchRequest, LaunchError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(LaunchError::input(crate::i18n::text(
            "请输入程序、路径或网址。",
        )));
    }
    if input.contains('\0') {
        return Err(LaunchError::input(crate::i18n::text(
            "输入包含无效字符，请修改后重试。",
        )));
    }
    let home = home.filter(|h| !h.is_empty());
    let resolve = |target: &str, check_whole: bool| -> Result<String, LaunchError> {
        let path = std::path::Path::new(target);
        // C:foo normally uses Windows' hidden per-drive working directory.
        // Use our declared profile base on the same drive; never inherit that state.
        let drive_relative = target.as_bytes().get(1) == Some(&b':')
            && target.as_bytes()[0].is_ascii_alphabetic()
            && !target.get(2..).is_some_and(|t| t.starts_with(['\\', '/']));
        if !is_uri(target)
            && !path.is_absolute()
            && (check_whole
                || drive_relative
                || target.contains(['\\', '/'])
                || target.starts_with('.'))
        {
            let base = home.ok_or_else(|| {
                LaunchError::input(crate::i18n::text(
                    "用户目录不可用，无法打开相对路径。请使用完整路径。",
                ))
            })?;
            let target = if drive_relative {
                if !base
                    .get(..2)
                    .is_some_and(|drive| drive.eq_ignore_ascii_case(&target[..2]))
                {
                    return Err(LaunchError::input(crate::i18n::text(
                        "盘符相对路径没有用户目录基准，请使用完整路径（例如 C:\\目录\\文件）。",
                    )));
                }
                &target[2..]
            } else {
                target
            };
            Ok(std::path::Path::new(base)
                .join(target)
                .to_string_lossy()
                .into_owned())
        } else {
            Ok(target.into())
        }
    };
    // Check the entire path first, including unquoted paths containing spaces.
    let whole = whole_target(input).map(|t| expand_target(t, &lookup));
    let existing = whole
        .as_deref()
        .filter(|t| !is_uri(t))
        .and_then(|t| resolve(t, true).ok())
        .filter(|t| exists(t));
    let (target, params) = if let Some(target) = existing {
        (target, String::new())
    } else if is_uri(input) {
        (expand_target(input, &lookup), String::new())
    } else {
        let (target, params) = if let Some(tail) = input.strip_prefix('"') {
            let end = tail.find('"').ok_or_else(|| {
                LaunchError::input(crate::i18n::text("引号未闭合，请补全目标路径的引号。"))
            })?;
            let remainder = &tail[end + 1..];
            if !remainder.is_empty() && !remainder.starts_with(char::is_whitespace) {
                return Err(LaunchError::input(crate::i18n::text(
                    "目标与参数之间需要空格。",
                )));
            }
            (&tail[..end], remainder)
        } else {
            let end = input.find(char::is_whitespace).unwrap_or(input.len());
            (&input[..end], &input[end..])
        };
        (
            resolve(&expand_target(target, &lookup), false)?,
            expand_target(params, &lookup),
        )
    };
    if target.is_empty() {
        return Err(LaunchError::input(crate::i18n::text(
            "请输入程序、路径或网址。",
        )));
    }
    let launch = if params.trim().is_empty() {
        let name = target.rsplit(['\\', '/']).next().unwrap_or(&target);
        if name.eq_ignore_ascii_case("explorer") || name.eq_ignore_ascii_case("explorer.exe") {
            Launch {
                file: target,
                params: THIS_PC.into(),
            }
        } else {
            let mut launch = plan(&target, home, system_root);
            // Explorer receives this generated path as one argument even when
            // the profile directory contains spaces. User arguments stay verbatim.
            if target
                .get(..6)
                .is_some_and(|p| p.eq_ignore_ascii_case("shell:"))
                && launch.params.contains(char::is_whitespace)
            {
                launch.params = format!("\"{}\"", launch.params);
            }
            launch
        }
    } else {
        Launch {
            file: target,
            params,
        }
    };
    Ok(LaunchRequest {
        file: launch.file,
        params: launch.params,
        directory: home.map(str::to_owned),
    })
}

pub fn parse_command(input: &str) -> Result<LaunchRequest, LaunchError> {
    parse_command_with(
        input,
        std::env::var("USERPROFILE").ok().as_deref(),
        std::env::var("SystemRoot").ok().as_deref(),
        |name| std::env::var(name).ok(),
        |path| std::path::Path::new(path).exists(),
    )
}

pub fn error_from_code(code: i32) -> LaunchError {
    let message = match code {
        value if value == CO_E_APPNOTFOUND.0 => {
            crate::i18n::text("关联的应用程序不可用。请指定程序或修复系统默认打开方式。").into()
        }
        2 => crate::i18n::text("找不到目标。请检查名称或路径后重试。").into(),
        3 => crate::i18n::text("路径不可用。请检查目录或设备连接。").into(),
        5 => crate::i18n::text("权限不足。请检查目标的访问权限。").into(),
        27 | 31 | 1155 => crate::i18n::text("没有默认打开方式。请为此文件设置关联应用。").into(),
        1223 => crate::i18n::text("打开操作已取消。可以修改输入后重试。").into(),
        28 | 258 | 1460 => crate::i18n::text("关联应用响应超时。请确认应用可用后重试。").into(),
        29 | 30 | 1156 => crate::i18n::text("无法与关联应用通信。请关闭其阻塞窗口后重试。").into(),
        26 | 32 | 1157 => {
            crate::i18n::text("目标或所需组件不可用。请关闭占用程序或修复应用后重试。").into()
        }
        11 | 193 | 216 => {
            crate::i18n::text("目标不是可运行的程序，或与当前系统不兼容。请检查文件。").into()
        }
        _ => crate::ui_format!(
            "启动失败（错误码 {code}），请修改输入后重试。",
            "Launch failed (error {code}). Edit the input and try again."
        ),
    };
    LaunchError {
        code: Some(code),
        message,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchMode {
    #[default]
    Standard,
    Administrator,
}

pub fn execute(request: &LaunchRequest) -> Result<(), LaunchError> {
    execute_with_mode(request, LaunchMode::Standard, None)
}

pub fn execute_with_mode(
    request: &LaunchRequest,
    mode: LaunchMode,
    owner: Option<HWND>,
) -> Result<(), LaunchError> {
    if request.file.trim().is_empty()
        || request.file.contains('\0')
        || request.params.contains('\0')
        || request
            .directory
            .as_deref()
            .is_some_and(|d| d.contains('\0'))
    {
        return Err(LaunchError::input("启动目标无效，请修改后重试。"));
    }
    let file = wide(&request.file);
    let name = request
        .file
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&request.file);
    let params = wide(
        if request.params.trim().is_empty()
            && (name.eq_ignore_ascii_case("explorer") || name.eq_ignore_ascii_case("explorer.exe"))
        {
            THIS_PC
        } else {
            &request.params
        },
    );
    let directory = request.directory.as_deref().map(wide);
    let verb = wide(if mode == LaunchMode::Administrator {
        "runas"
    } else {
        "open"
    });
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // Return ordinary errors to our inline UI. Wait for DDE/shell work on
        // the COM-initialized worker rather than relying on later message pumping.
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        hwnd: owner.unwrap_or_default(),
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if params.len() == 1 {
            PCWSTR::null()
        } else {
            PCWSTR(params.as_ptr())
        },
        lpDirectory: directory
            .as_ref()
            .map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: buffers outlive the call; no process handle is requested.
    let result = unsafe { ShellExecuteExW(&mut info) };
    let last_error = unsafe { GetLastError() }.0 as i32;
    match result {
        Ok(()) => {
            tracing::info!(file = %request.file, "run request accepted");
            Ok(())
        }
        Err(system_error) => {
            // The binding captures GetLastError as an HRESULT immediately. Prefer
            // that captured Win32 value over thread-local state that COM may alter.
            let captured = system_error.code().0 as u32;
            let code = if captured & 0xffff0000 == 0x80070000 {
                (captured & 0xffff) as i32
            } else if captured & 0x80000000 != 0 {
                captured as i32
            } else {
                last_error
            };
            let mut error = error_from_code(code);
            if mode == LaunchMode::Administrator && code == 1223 {
                error.message = crate::i18n::text("已取消授权。可以修改输入后重试。").into();
            }
            // An existing document can still fail with FILE_NOT_FOUND when its
            // association points at an uninstalled editor. Do not blame its path.
            if (matches!(code, 2 | 3) || code == CO_E_APPNOTFOUND.0)
                && request.params.trim().is_empty()
                && std::path::Path::new(&request.file).is_file()
                && !std::path::Path::new(&request.file)
                    .extension()
                    .is_some_and(|ext| {
                        ["exe", "com", "bat", "cmd"]
                            .iter()
                            .any(|name| ext.eq_ignore_ascii_case(name))
                    })
            {
                error.message = crate::i18n::text("文件存在，但默认打开程序不可用。请在文件路径前指定程序（例如 notepad），或修复系统默认打开方式。").into();
            }
            if request.file.contains('%') {
                error
                    .message
                    .push_str(crate::i18n::text(" 请检查未定义的环境变量。"));
            }
            tracing::warn!(file = %request.file, code, "run request failed");
            Err(error)
        }
    }
}

/// Whether `target` is Explorer started with no argument — the invocation that
/// makes it claim the shell.
///
/// A space anywhere in the target is treated as "arguments already present",
/// which is the conservative reading: we only rewrite what we are sure of.
pub fn is_bare_explorer(target: &str) -> bool {
    let t = target.trim().trim_matches('"');
    if t.is_empty() || t.contains(' ') {
        return false;
    }
    let name = t.rsplit(['\\', '/']).next().unwrap_or(t);
    name.eq_ignore_ascii_case("explorer.exe") || name.eq_ignore_ascii_case("explorer")
}

/// The sub-folder of the user profile behind a `shell:` moniker, if known.
///
/// Known monikers become real paths so Explorer can be given a concrete
/// argument; unknown ones keep the moniker itself (still an argument, still
/// safe — only a *bare* Explorer is dangerous).
fn home_subfolder(moniker: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("personal", "Documents"),
        ("documents", "Documents"),
        ("downloads", "Downloads"),
        ("mypictures", "Pictures"),
        ("pictures", "Pictures"),
        ("mymusic", "Music"),
        ("music", "Music"),
        ("myvideo", "Videos"),
        ("videos", "Videos"),
        ("desktop", "Desktop"),
    ];
    KNOWN
        .iter()
        .find(|(m, _)| m.eq_ignore_ascii_case(moniker))
        .map(|(_, sub)| *sub)
}

/// Path of `explorer.exe`, from `%SystemRoot%` when available.
fn explorer_exe(system_root: Option<&str>) -> String {
    let root = system_root.unwrap_or(r"C:\Windows");
    format!(r"{root}\explorer.exe")
}

/// Turns a requested target into a shell-safe [`Launch`].
///
/// `home` is the user profile (`%USERPROFILE%`) and `system_root` is
/// `%SystemRoot%`; both are optional so the mapping is testable.
pub fn plan(target: &str, home: Option<&str>, system_root: Option<&str>) -> Launch {
    let t = target.trim();
    if t.is_empty() {
        return Launch {
            file: String::new(),
            params: String::new(),
        };
    }

    // `shell:<moniker>` — opened by Explorer, so it must carry an argument.
    if let Some(moniker) = t.get(0..6).filter(|p| p.eq_ignore_ascii_case("shell:")) {
        let rest = t[moniker.len()..].trim_matches('/').trim();
        let arg = match home_subfolder(rest) {
            Some(sub) => match home {
                Some(h) => format!(r"{h}\{sub}"),
                None => t.to_string(),
            },
            None if rest.eq_ignore_ascii_case("mycomputerfolder") => THIS_PC.to_string(),
            None => t.to_string(),
        };
        return Launch {
            file: explorer_exe(system_root),
            params: arg,
        };
    }

    // Bare Explorer: give it somewhere to go so it stays a file browser.
    if is_bare_explorer(t) {
        return Launch {
            file: t.trim_matches('"').to_string(),
            params: THIS_PC.to_string(),
        };
    }

    Launch {
        file: t.to_string(),
        params: String::new(),
    }
}

/// Opens `target` the shell-safe way. Returns whether it was launched.
pub fn open(target: &str) -> bool {
    let launch = plan(
        target,
        std::env::var("USERPROFILE").ok().as_deref(),
        std::env::var("SystemRoot").ok().as_deref(),
    );
    if launch.file.is_empty() {
        return false;
    }
    if is_bare_explorer(target) {
        tracing::warn!(
            from = %target.trim(),
            to = %launch.params,
            "explorer started bare would have taken the shell; redirected"
        );
    }

    let file = wide(&launch.file);
    let params = wide(&launch.params);
    // SAFETY: both buffers outlive the call; no owner window.
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(wide("open").as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns <= 32 on failure (no instance / no association).
    if code.0 as i32 > 32 {
        tracing::info!(target, "launched (shell-safe)");
        true
    } else {
        tracing::warn!(target, code = code.0 as i32, "shell-safe launch failed");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{Launch, THIS_PC, is_bare_explorer, plan};

    // Error wording must be deterministic even if another native fixture loads
    // the user's Chinese configuration. A separate process avoids racing the
    // process-wide UI language and never changes the user's preferences.
    fn english_process(test: &str) -> bool {
        const MARKER: &str = "MINICON_TEST_SHELL_LAUNCH_ENGLISH";
        if std::env::var(MARKER).as_deref() == Ok(test) {
            crate::i18n::set(crate::i18n::Language::En);
            return true;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("shell_launch::tests::{test}"),
                "--nocapture",
            ])
            .env(MARKER, test)
            .env("MINICON_LANGUAGE", "en")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    #[test]
    fn missing_associated_application_hresult_has_actionable_feedback() {
        if !english_process("missing_associated_application_hresult_has_actionable_feedback") {
            return;
        }
        let error = super::error_from_code(super::CO_E_APPNOTFOUND.0);
        assert_eq!(error.code, Some(0x800401F5u32 as i32));
        assert!(error.message.contains("file association"));
    }

    fn command(input: &str) -> Result<super::LaunchRequest, super::LaunchError> {
        super::parse_command_with(
            input,
            Some(r"C:\Users\u"),
            Some(r"C:\Windows"),
            |key| (key == "ProgramFiles").then(|| r"C:\Program Files".into()),
            |path| path == r"C:\Program Files",
        )
    }

    #[test]
    fn run_preserves_parameter_text_and_expands_environment() {
        let got = command(r#"%ProgramFiles%\App\app.exe  --name "张 三" %UNKNOWN%"#).unwrap();
        assert_eq!(got.file, r"C:\Program Files\App\app.exe");
        assert_eq!(got.params, r#"  --name "张 三" %UNKNOWN%"#);
        assert_eq!(got.directory.as_deref(), Some(r"C:\Users\u"));
        assert_eq!(
            command(r#""%ProgramFiles%\App\app.exe" --x"#).unwrap().file,
            got.file
        );
    }

    #[test]
    fn userprofile_document_and_notepad_arguments_expand() {
        let parse = |input| {
            super::parse_command_with(
                input,
                Some(r"C:\Users\张 三"),
                Some(r"C:\Windows"),
                |key| {
                    key.eq_ignore_ascii_case("USERPROFILE")
                        .then(|| r"C:\Users\张 三".into())
                },
                |path| path == r"C:\Users\张 三\.workbuddy\models.json",
            )
            .unwrap()
        };
        assert_eq!(
            parse(r"%USERPROFILE%\.workbuddy\models.json").file,
            r"C:\Users\张 三\.workbuddy\models.json"
        );
        let request = parse(r#"notepad  "%USERPROFILE%\.workbuddy\models.json""#);
        assert_eq!(request.file, "notepad");
        assert_eq!(
            request.params,
            r#"  "C:\Users\张 三\.workbuddy\models.json""#
        );
        assert_eq!(
            parse(r"notepad  %USERPROFILE%\.workbuddy\models.json").params,
            r"  C:\Users\张 三\.workbuddy\models.json"
        );
        assert_eq!(
            super::expand_target("%UNKNOWN% %USERPROFILE%", &|key| (key == "USERPROFILE")
                .then(|| "%NESTED%".into())),
            "%UNKNOWN% %NESTED%"
        );
    }

    #[test]
    fn whole_existing_path_precedes_command_split() {
        assert_eq!(
            command(r"C:\Program Files").unwrap().file,
            r"C:\Program Files"
        );
        assert!(command(r#""C:\Program Files""#).unwrap().params.is_empty());
        assert_eq!(
            command("cmd /c \"echo hello\"").unwrap().params,
            " /c \"echo hello\""
        );
        assert_eq!(
            command("control.exe timedate.cpl").unwrap().file,
            "control.exe"
        );
    }

    #[test]
    fn uri_and_unknown_environment_are_not_reinterpreted() {
        let uri = "https://example.com/?q=hello world&x=1";
        assert_eq!(command(uri).unwrap().file, uri);
        assert!(command(uri).unwrap().params.is_empty());
        assert_eq!(command(&format!("\"{uri}\"")).unwrap().file, uri);
        assert!(command(&format!("\"{uri}\"")).unwrap().params.is_empty());
        assert_eq!(command("%UNKNOWN% --x").unwrap().file, "%UNKNOWN%");
        assert_eq!(
            command("shell:Downloads").unwrap().params,
            r"C:\Users\u\Downloads"
        );
    }

    #[test]
    fn complete_relative_filename_is_checked_in_profile_not_process_directory() {
        let checked = std::cell::RefCell::new(Vec::new());
        let got = super::parse_command_with(
            "notes with spaces.txt",
            Some(r"C:\Users\u"),
            None,
            |_| None,
            |path| {
                checked.borrow_mut().push(path.to_string());
                path == r"C:\Users\u\notes with spaces.txt"
            },
        )
        .unwrap();
        assert_eq!(got.file, r"C:\Users\u\notes with spaces.txt");
        assert_eq!(*checked.borrow(), vec![r"C:\Users\u\notes with spaces.txt"]);
        let got = super::parse_command_with(
            "notes.txt",
            Some(r"C:\Users\u"),
            None,
            |_| None,
            |path| path == r"C:\Users\u\notes.txt",
        )
        .unwrap();
        assert_eq!(got.file, r"C:\Users\u\notes.txt");
        assert_eq!(got.directory.as_deref(), Some(r"C:\Users\u"));
    }

    #[test]
    fn drive_and_root_relative_paths_do_not_inherit_hidden_working_directories() {
        if !english_process(
            "drive_and_root_relative_paths_do_not_inherit_hidden_working_directories",
        ) {
            return;
        }
        assert_eq!(
            command(r"C:notes.txt").unwrap().file,
            r"C:\Users\u\notes.txt"
        );
        assert_eq!(
            command(r"c:notes.txt").unwrap().file,
            r"C:\Users\u\notes.txt"
        );
        assert_eq!(command(r"\notes.txt").unwrap().file, r"C:\notes.txt");
        assert!(
            command(r"D:notes.txt")
                .unwrap_err()
                .message
                .contains("absolute path")
        );
        assert!(super::parse_command_with(r"\notes.txt", None, None, |_| None, |_| false).is_err());
    }

    #[test]
    fn invalid_request_directory_fails_before_launching() {
        let request = super::LaunchRequest {
            file: "notepad".into(),
            params: String::new(),
            directory: Some("C:\\Users\\bad\0suffix".into()),
        };
        assert!(super::execute(&request).is_err());
    }

    #[test]
    fn relative_paths_have_an_explicit_base_and_quotes_fail_visibly() {
        if !english_process("relative_paths_have_an_explicit_base_and_quotes_fail_visibly") {
            return;
        }
        assert_eq!(
            command(r".\notes.txt").unwrap().file,
            r"C:\Users\u\.\notes.txt"
        );
        assert!(
            super::parse_command_with(r".\notes.txt", None, None, |_| None, |_| false).is_err()
        );
        assert!(
            command("\"C:\\Program Files\\app.exe")
                .unwrap_err()
                .message
                .contains("quotation marks")
        );
        assert!(command("\"app\"--x").is_err());
        assert!(command(" ").is_err());
    }

    #[test]
    fn parsed_explorer_is_always_given_a_nonempty_argument() {
        for input in [
            "explorer",
            "explorer.exe   ",
            r#""C:\Windows\explorer.exe""#,
        ] {
            assert_eq!(command(input).unwrap().params, THIS_PC);
        }
        assert_eq!(command(r"explorer.exe C:\").unwrap().params, r" C:\");
    }

    #[test]
    fn shell_errors_offer_specific_corrections() {
        if !english_process("shell_errors_offer_specific_corrections") {
            return;
        }
        assert!(super::error_from_code(2).message.contains("not found"));
        assert!(
            super::error_from_code(3)
                .message
                .contains("path is unavailable")
        );
        assert!(super::error_from_code(5).message.contains("Access denied"));
        assert!(
            super::error_from_code(31)
                .message
                .contains("file association")
        );
        assert!(
            super::error_from_code(1155)
                .message
                .contains("file association")
        );
        assert!(super::error_from_code(1223).message.contains("cancelled"));
        assert!(super::error_from_code(1156).message.contains("communicate"));
        assert!(super::error_from_code(1460).message.contains("timed out"));
        assert!(super::error_from_code(8).message.contains('8'));
    }

    fn launch(file: &str, params: &str) -> Launch {
        Launch {
            file: file.to_string(),
            params: params.to_string(),
        }
    }

    #[test]
    fn bare_explorer_is_recognised() {
        assert!(is_bare_explorer("explorer"));
        assert!(is_bare_explorer("explorer.exe"));
        assert!(is_bare_explorer(r"C:\Windows\explorer.exe"));
        assert!(is_bare_explorer(r"c:\windows\EXPLORER.EXE"));
        assert!(is_bare_explorer(r#""C:\Windows\explorer.exe""#));
    }

    #[test]
    fn explorer_with_arguments_is_left_alone() {
        assert!(!is_bare_explorer(r"explorer.exe C:\"));
        assert!(!is_bare_explorer(r"explorer.exe shell:Downloads"));
        assert!(!is_bare_explorer(r"C:\Program Files\Some Explorer\app.exe"));
    }

    #[test]
    fn other_programs_are_not_explorer() {
        assert!(!is_bare_explorer("notepad"));
        assert!(!is_bare_explorer(r"C:\Windows\System32\notepad.exe"));
        assert!(!is_bare_explorer(""));
    }

    #[test]
    fn bare_explorer_gets_a_folder_to_open() {
        let got = plan(r"C:\Windows\explorer.exe", None, Some(r"C:\Windows"));
        assert_eq!(got.file, r"C:\Windows\explorer.exe");
        assert_eq!(got.params, THIS_PC);
    }

    #[test]
    fn explorer_with_its_own_argument_is_untouched() {
        let got = plan(r"explorer.exe D:\dev", None, None);
        assert_eq!(got.file, r"explorer.exe D:\dev");
        assert!(got.params.is_empty());
    }

    #[test]
    fn shell_monikers_become_real_paths() {
        let got = plan("shell:Downloads", Some(r"C:\Users\u"), Some(r"C:\Windows"));
        assert_eq!(got.file, r"C:\Windows\explorer.exe");
        assert_eq!(got.params, r"C:\Users\u\Downloads");
    }

    #[test]
    fn unknown_monikers_are_kept_as_arguments() {
        let got = plan("shell:RecycleBinFolder", None, Some(r"C:\Windows"));
        assert_eq!(got.file, r"C:\Windows\explorer.exe");
        assert_eq!(got.params, "shell:RecycleBinFolder");
    }

    #[test]
    fn this_pc_moniker_maps_to_a_clsid() {
        let got = plan("shell:MyComputerFolder", None, Some(r"C:\Windows"));
        assert_eq!(got.params, THIS_PC);
    }

    #[test]
    fn moniker_without_home_falls_back_to_the_moniker() {
        let got = plan("shell:Personal", None, Some(r"C:\Windows"));
        assert_eq!(got.params, "shell:Personal");
    }

    #[test]
    fn ordinary_targets_pass_through() {
        let got = plan(r"C:\Windows\System32\notepad.exe", None, None);
        assert_eq!(got, launch(r"C:\Windows\System32\notepad.exe", ""));
    }

    #[test]
    fn empty_target_launches_nothing() {
        assert!(plan("   ", None, None).file.is_empty());
    }

    #[test]
    fn explorer_path_defaults_to_the_windows_directory() {
        let got = plan("explorer", None, None);
        assert_eq!(got.file, "explorer");
        assert_eq!(got.params, THIS_PC);
    }
}
