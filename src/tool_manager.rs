//! Explicit, bounded companion acquisition. The shell never waits for disk or HTTP.
use crate::{update, update_cancel::Token, update_http as http, update_stream};
use minicon_winmgr::task_service::Identity;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};
use windows::{
    Win32::{Foundation::HWND, UI::WindowsAndMessaging::*},
    core::PCWSTR,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const COMPANION_SHA256: Option<&str> = option_env!("MINICON_TSKMGR_SHA256");
const SUM_LIMIT: usize = 64 * 1024;
static BUSY: AtomicBool = AtomicBool::new(false);
static SERIAL: AtomicU64 = AtomicU64::new(0);
static PHASE: AtomicU8 = AtomicU8::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn status() -> String {
    match PHASE.load(Ordering::Acquire) {
        1 => crate::i18n::text("正在校验任务工具…").into(),
        2 => crate::i18n::text("等待确认下载任务工具…").into(),
        3 => crate::ui_format!(
            "正在下载任务工具… {:.1} MiB",
            "Downloading task tool… {:.1} MiB",
            BYTES.load(Ordering::Relaxed) as f64 / 1048576.0
        ),
        4 => crate::i18n::text("已启动任务工具。").into(),
        5 => crate::i18n::text("任务工具未能打开，可重试。").into(),
        6 => crate::i18n::text("已取消下载任务工具。").into(),
        7 => crate::i18n::text("等待 Windows 管理员授权…").into(),
        8 => crate::i18n::text("已取消管理员授权，当前任务未改变。").into(),
        9 => crate::i18n::text("已打开管理员任务窗口。").into(),
        10 => crate::ui_format!("正在获取任务工具校验表…", "Fetching task tool checksums…"),
        _ => crate::i18n::text("独立管理自己的进程和网络端点。").into(),
    }
}

fn name() -> String {
    format!("minicon-tskmgr-v{VERSION}-x86_64-windows.exe")
}
fn release_url(asset: &str) -> String {
    format!(
        "https://github.com/partnernetsoftware/minicon-winmgr/releases/download/v{VERSION}/{asset}"
    )
}
fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take((SUM_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > SUM_LIMIT {
        return Err("Tool checksum table exceeds its size limit".into());
    }
    Ok(bytes)
}
fn verified(directory: &Path, sums: &[u8]) -> Result<PathBuf, String> {
    let exe = directory.join(name());
    update_stream::verify_file(&exe, &update::checksum(sums, &name())?)?;
    Ok(exe)
}
fn cache() -> Result<PathBuf, String> {
    let root = std::env::var_os("LOCALAPPDATA").ok_or("Local application directory unavailable")?;
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Err("Local application directory must be absolute".into());
    }
    Ok(root.join("minicon-winmgr").join("tools").join(VERSION))
}
fn installed(current: &Path, cache: &Path) -> Result<Option<PathBuf>, String> {
    installed_with_digest(current, cache, COMPANION_SHA256)
}
fn installed_with_digest(
    current: &Path,
    cache: &Path,
    pinned: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    // A sibling checksum table must bind this exact shell as well as the tool.
    // This avoids silently pairing a development build with unrelated binaries.
    let mut problems = Vec::new();
    for (directory, sibling) in current
        .parent()
        .map(|p| (p, true))
        .into_iter()
        .chain(std::iter::once((cache, false)))
    {
        let file = directory.join(name());
        match file.try_exists() {
            Ok(false) => continue,
            Err(error) => {
                problems.push(format!("{}: {error}", file.display()));
                continue;
            }
            Ok(true) => {}
        }
        let result = (|| {
            if let Some(digest) = pinned {
                update_stream::verify_file(&file, digest)?;
                return Ok(file.clone());
            }
            let table = directory.join("SHA256SUMS.txt");
            let sums =
                bounded_read(&table).map_err(|error| format!("{}: {error}", table.display()))?;
            if sibling {
                let expected = update::checksum(
                    &sums,
                    &format!("minicon-winmgr-v{VERSION}-x86_64-windows.exe"),
                )?;
                if update_stream::hash_file(current)? != expected {
                    return Err("SHA256SUMS.txt does not match this running Winmgr".into());
                }
            }
            verified(directory, &sums)
        })();
        match result {
            Ok(path) => return Ok(Some(path)),
            Err(error) => problems.push(format!("{}: {error}", file.display())),
        }
    }
    if problems.is_empty() {
        Ok(None)
    } else {
        Err(problems.join("\n"))
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn temporary(directory: &Path) -> Result<(Temporary, fs::File), String> {
    let path = directory.join(format!(
        ".download-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    Ok((Temporary(path), file))
}
fn publish(source: &Path, target: &Path, expected: &str) -> Result<(), String> {
    match fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(_) if target.exists() => {
            if update_stream::hash_file(target)? == expected {
                Ok(())
            } else {
                Err(format!("Tool cache conflict: {}", target.display()))
            }
        }
        Err(error) => Err(error.to_string()),
    }
}
fn download(directory: &Path, current: &Path) -> Result<PathBuf, String> {
    PHASE.store(10, Ordering::Release);
    BYTES.store(0, Ordering::Relaxed);
    let cancel = Token::default();
    let sums = http::get_cancellable(
        &release_url("SHA256SUMS.txt"),
        SUM_LIMIT,
        Duration::from_secs(25),
        |_| {},
        &cancel,
    ).map_err(|error| crate::ui_format!(
        "获取 Tskmgr {VERSION} 校验表失败（github.com；系统/当前用户代理）：{error}\n可手动下载同版 Winmgr 和 Tskmgr 到同一目录后重试。",
        "Unable to fetch Tskmgr {VERSION} checksums (github.com; system/current-user proxy): {error}\nYou can manually place the matching Winmgr and Tskmgr in one folder, then retry."
    ))?;
    let expected = checksum(&sums, COMPANION_SHA256)?;
    if let Some(path) = reuse_local(directory, &sums, current, &expected)? {
        return Ok(path);
    }
    PHASE.store(3, Ordering::Release);
    install(directory, &sums, |file| {
        http::get_into_cancellable(
            &release_url(&name()),
            update::MAX_EXE,
            Duration::from_secs(300),
            |count| {
                BYTES.store(count, Ordering::Relaxed);
            },
            |chunk| file.write_all(chunk).map_err(|e| e.to_string()),
            &cancel,
        ).map_err(|error| crate::ui_format!(
            "下载或写入 Tskmgr {VERSION} 失败（GitHub 发布附件；系统/当前用户代理）：{error}",
            "Unable to download or write Tskmgr {VERSION} (GitHub release asset; system/current-user proxy): {error}"
        ))?;
        Ok(())
    })
}
fn reuse_local(
    directory: &Path,
    sums: &[u8],
    current: &Path,
    expected: &str,
) -> Result<Option<PathBuf>, String> {
    // A missing receipt must not cause a full re-download of correct local bytes.
    for source in current
        .parent()
        .into_iter()
        .chain(std::iter::once(directory))
    {
        let source = source.join(name());
        if update_stream::verify_file(&source, expected).is_ok() {
            PHASE.store(1, Ordering::Release);
            return install(directory, sums, |destination| {
                let source = fs::File::open(&source).map_err(|e| e.to_string())?;
                let copied =
                    std::io::copy(&mut source.take((update::MAX_EXE + 1) as u64), destination)
                        .map_err(|e| e.to_string())?;
                if copied > update::MAX_EXE as u64 {
                    return Err("Local tool exceeded its size limit".into());
                }
                Ok(())
            })
            .map(Some);
        }
    }
    Ok(None)
}
fn checksum(sums: &[u8], pinned: Option<&str>) -> Result<String, String> {
    let expected = update::checksum(sums, &name())?;
    if pinned.is_some_and(|bound| bound != expected) {
        return Err("Release checksum does not match the companion bound to this Winmgr".into());
    }
    Ok(expected)
}
fn install(
    directory: &Path,
    sums: &[u8],
    fetch: impl FnOnce(&mut fs::File) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let expected = checksum(sums, COMPANION_SHA256)?;
    fs::create_dir_all(directory)
        .map_err(|e| format!("Unable to create tool cache {}: {e}", directory.display()))?;
    let (staged, mut file) =
        temporary(directory).map_err(|e| format!("Unable to create tool staging file: {e}"))?;
    fetch(&mut file)?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    update_stream::verify_file(&staged.0, &expected)
        .map_err(|e| format!("Tool content verification failed: {e}"))?;
    let target = directory.join(name());
    publish(&staged.0, &target, &expected)?;
    let (receipt, mut file) = temporary(directory)?;
    file.write_all(sums).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    // The receipt is published last; interrupted acquisition is never trusted.
    publish(
        &receipt.0,
        &directory.join("SHA256SUMS.txt"),
        &update_stream::hash_reader(sums, SUM_LIMIT)?,
    )?;
    verified(directory, sums)
}
fn dialog(owner: isize, message: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    unsafe {
        MessageBoxW(
            Some(HWND(owner as *mut _)),
            PCWSTR(crate::win_util::wide(message).as_ptr()),
            PCWSTR(crate::win_util::wide("Minicon Tskmgr").as_ptr()),
            flags,
        )
    }
}
fn launch(path: &Path, focus: Option<Identity>, language: &str, light: bool) -> Result<(), String> {
    let mut command = std::process::Command::new(path);
    command.args([
        "--language",
        language,
        "--theme",
        if light { "light" } else { "dark" },
    ]);
    if let Some(key) = focus {
        command.args([
            "--focus-pid",
            &key.pid.to_string(),
            "--focus-created",
            &key.created_100ns.to_string(),
        ]);
    }
    command
        .spawn()
        .map_err(|e| format!("Unable to start Tskmgr: {e}"))?;
    Ok(())
}

/// One finite acquisition/launch at a time; no persistent worker or startup I/O.
pub(crate) fn open(owner: HWND, focus: Option<Identity>) {
    open_requested(owner, focus, false);
}
pub(crate) fn administrator(owner: HWND) {
    open_requested(owner, None, true);
}
fn open_requested(owner: HWND, focus: Option<Identity>, administrator: bool) {
    if BUSY.swap(true, Ordering::AcqRel) {
        dialog(
            owner.0 as isize,
            crate::i18n::text("任务工具正在准备，请稍候再试。"),
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    let owner = owner.0 as isize;
    PHASE.store(1, Ordering::Release);
    let config = crate::config::current();
    let language = config.language.as_key();
    let light = config.theme_mode == crate::config::ThemeMode::Light
        || config.theme_mode == crate::config::ThemeMode::Auto && crate::config::system_light();
    let result = std::thread::Builder::new().name("task-tool".into()).spawn(move || {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) { BUSY.store(false, Ordering::Release); }
        }
        let _reset = Reset;
        let result: Result<(), String> = (|| {
            let cache = cache()?;
            let current = std::env::current_exe().map_err(|e| e.to_string())?;
            let path = match installed(&current, &cache) {
                Ok(Some(path)) => path,
                lookup => {
                    PHASE.store(2, Ordering::Release);
                    let offline = if COMPANION_SHA256.is_some() { crate::ui_format!(
                        "离线使用：将同版官方 Winmgr 和 Tskmgr 两份 exe 放在同一目录，然后重试。",
                        "For offline use, put the matching official Winmgr and Tskmgr executables in one folder, then retry."
                    ) } else { crate::ui_format!(
                        "此开发构建未内置工具摘要；离线使用需同版两份 exe 和 SHA256SUMS.txt。",
                        "This development build has no companion digest; offline use needs matching executables and SHA256SUMS.txt."
                    ) };
                    let question = if let Err(reason) = lookup { crate::ui_format!(
                        "发现了 Tskmgr 文件，但无法通过本版本校验：\n{reason}\n\n{offline}\n是否改为从此版本的 GitHub 公开发布下载？",
                        "A Tskmgr file was found but could not be verified for this version:\n{reason}\n\n{offline}\nDownload from this version's public GitHub release instead?"
                    ) } else { crate::ui_format!(
                        "未找到已校验的 Tskmgr {VERSION}。\n从此版本的 GitHub 公开发布下载并打开？\n下载期间可继续使用 Winmgr。",
                        "Verified Tskmgr {VERSION} was not found.\nDownload and open it from this version's public GitHub release?\nYou can keep using Winmgr during the download."
                    ) };
                    if dialog(owner, &question, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != IDYES {
                        PHASE.store(6, Ordering::Release);
                        return Ok(());
                    }
                    download(&cache, &current)?
                }
            };
            if administrator {
                PHASE.store(7, Ordering::Release);
                let outcome = minicon_winmgr::task_launch::administrator(&path, owner,
                    minicon_winmgr::task_launch::Request {
                        focus, chinese: language == "zh", light, ..Default::default()
                    })?;
                PHASE.store(match outcome { minicon_winmgr::task_launch::Outcome::Opened => 9,
                    minicon_winmgr::task_launch::Outcome::Cancelled => 8 }, Ordering::Release);
                return Ok(());
            }
            launch(&path, focus, language, light)?;
            PHASE.store(4, Ordering::Release);
            Ok(())
        })();
        if let Err(error) = result {
            PHASE.store(5, Ordering::Release);
            let message = crate::ui_format!(
                "无法打开任务工具：{error}\n\n可从同一入口重试，或打开 Windows 任务管理器。",
                "Unable to open task tool: {error}\n\nRetry from the same entry, or open Windows Task Manager."
            );
            dialog(owner, &message, MB_OK | MB_ICONERROR);
        }
    });
    if result.is_err() {
        BUSY.store(false, Ordering::Release);
        PHASE.store(5, Ordering::Release);
        dialog(
            owner,
            crate::i18n::text("无法启动任务工具后台工作。"),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub(crate) fn locate(owner: HWND, target: HWND) {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(target, Some(&mut pid));
    }
    match minicon_winmgr::task_service::identify(pid) {
        Ok(key) => open(owner, Some(key)),
        Err(error) => {
            dialog(owner.0 as isize, &error, MB_OK | MB_ICONWARNING);
        }
    }
}

pub(crate) fn system_task_manager(owner: HWND) {
    let mut buffer = [0u16; 32768];
    let count = unsafe {
        windows::Win32::System::SystemInformation::GetSystemDirectoryW(Some(&mut buffer))
    } as usize;
    let result: Result<(), String> = if count > 0 && count < buffer.len() {
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..count])).join("Taskmgr.exe");
        if crate::shell_launch::open(&path.to_string_lossy()) {
            Ok(())
        } else {
            Err("Unable to open Windows Task Manager".into())
        }
    } else {
        Err("Windows system directory unavailable".into())
    };
    if let Err(error) = result {
        dialog(owner.0 as isize, &error, MB_OK | MB_ICONERROR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "minicon-tool-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn payload() -> Vec<u8> {
        let mut bytes = vec![0; 256];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&[0x64, 0x86]);
        bytes[88..90].copy_from_slice(&[0x0b, 0x02]);
        bytes
    }
    fn sums(bytes: &[u8]) -> Vec<u8> {
        format!(
            "{}  {}\n",
            update_stream::hash_reader(bytes, update::MAX_EXE).unwrap(),
            name()
        )
        .into_bytes()
    }
    #[test]
    fn local_tool_rejection_explains_missing_table_and_preserves_cache_fallback() {
        let sibling = Directory::new();
        let cache = Directory::new();
        let current = sibling.0.join("renamed-shell.exe");
        assert!(installed(&current, &cache.0).unwrap().is_none());
        let bytes = payload();
        let tool = sibling.0.join(name());
        fs::write(&current, &bytes).unwrap();
        fs::write(&tool, &bytes).unwrap();
        assert!(
            installed(&current, &cache.0)
                .unwrap_err()
                .contains("SHA256SUMS.txt")
        );
        let pair = format!(
            "{}  minicon-winmgr-v{VERSION}-x86_64-windows.exe\n{}",
            update_stream::hash_reader(&bytes[..], update::MAX_EXE).unwrap(),
            String::from_utf8(sums(&bytes)).unwrap()
        );
        fs::write(sibling.0.join("SHA256SUMS.txt"), pair).unwrap();
        assert_eq!(installed(&current, &cache.0).unwrap(), Some(tool.clone()));
        fs::write(&current, b"different shell").unwrap();
        assert!(
            installed(&current, &cache.0)
                .unwrap_err()
                .contains("running Winmgr")
        );
        fs::write(&current, &bytes).unwrap();
        fs::write(&tool, b"corrupted sibling").unwrap();
        assert!(installed(&current, &cache.0).is_err());
        let cached = install(&cache.0, &sums(&bytes), |file| {
            file.write_all(&bytes).map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(installed(&current, &cache.0).unwrap(), Some(cached));
        assert_eq!(fs::read(tool).unwrap(), b"corrupted sibling");
    }
    #[test]
    fn bound_tool_opens_without_receipt_and_rejects_corruption_and_wrong_release() {
        let sibling = Directory::new();
        let cache = Directory::new();
        let current = sibling.0.join("shell.exe");
        let bytes = payload();
        let digest = update_stream::hash_reader(&bytes[..], update::MAX_EXE).unwrap();
        let tool = sibling.0.join(name());
        fs::write(&tool, &bytes).unwrap();
        assert_eq!(
            installed_with_digest(&current, &cache.0, Some(&digest)).unwrap(),
            Some(tool.clone())
        );
        assert!(!sibling.0.join("SHA256SUMS.txt").exists());
        fs::write(&tool, b"changed tool").unwrap();
        assert!(installed_with_digest(&current, &cache.0, Some(&digest)).is_err());
        let cached = cache.0.join(name());
        fs::write(&cached, &bytes).unwrap();
        assert_eq!(
            installed_with_digest(&current, &cache.0, Some(&digest)).unwrap(),
            Some(cached)
        );
        assert!(checksum(&sums(&bytes), Some(&"0".repeat(64))).is_err());
        assert_eq!(checksum(&sums(&bytes), Some(&digest)).unwrap(), digest);
        assert_eq!(fs::read(tool).unwrap(), b"changed tool");
    }
    #[test]
    fn fetched_receipt_reuses_existing_bytes_and_keeps_bad_local_file() {
        let sibling = Directory::new();
        let cache = Directory::new();
        let current = sibling.0.join("shell.exe");
        let tool = sibling.0.join(name());
        let bytes = payload();
        let sums = sums(&bytes);
        let digest = checksum(&sums, None).unwrap();
        fs::write(&tool, &bytes).unwrap();
        let installed = reuse_local(&cache.0, &sums, &current, &digest)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(installed).unwrap(), bytes);
        assert_eq!(fs::read(cache.0.join("SHA256SUMS.txt")).unwrap(), sums);
        let empty = Directory::new();
        fs::write(&tool, b"bad local file").unwrap();
        assert!(
            reuse_local(&empty.0, &sums, &current, &digest)
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(tool).unwrap(), b"bad local file");
        assert_eq!(fs::read_dir(empty.0.clone()).unwrap().count(), 0);
    }
    #[test]
    fn interrupted_or_corrupt_acquisition_is_not_installed_and_retry_succeeds() {
        let root = Directory::new();
        let bytes = payload();
        let sums = sums(&bytes);
        assert!(
            install(&root.0, &sums, |file| {
                file.write_all(&bytes[..70]).unwrap();
                Err("interrupted".into())
            })
            .is_err()
        );
        let mut corrupt = bytes.clone();
        corrupt[200] = 1;
        assert!(
            install(&root.0, &sums, |file| file
                .write_all(&corrupt)
                .map_err(|e| e.to_string()))
            .is_err()
        );
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
        let exe = install(&root.0, &sums, |file| {
            file.write_all(&bytes).map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(
            installed(Path::new("missing.exe"), &root.0).unwrap(),
            Some(exe.clone())
        );
        fs::write(&exe, corrupt).unwrap();
        assert!(installed(Path::new("missing.exe"), &root.0).is_err());
    }
    #[test]
    fn conflicting_target_is_preserved_and_wrong_version_is_rejected() {
        let root = Directory::new();
        let bytes = payload();
        fs::write(root.0.join(name()), b"unrelated").unwrap();
        assert!(
            install(&root.0, &sums(&bytes), |file| file
                .write_all(&bytes)
                .map_err(|e| e.to_string()))
            .is_err()
        );
        assert_eq!(fs::read(root.0.join(name())).unwrap(), b"unrelated");
        assert!(!root.0.join("SHA256SUMS.txt").exists());
        let other = format!(
            "{}  minicon-tskmgr-v0.1.15-x86_64-windows.exe\n",
            "a".repeat(64)
        );
        assert!(
            install(&root.0, other.as_bytes(), |_| panic!(
                "must reject before downloading"
            ))
            .is_err()
        );
    }
}
