//! An ordinary-privilege, short-lived copy of this executable owns the handoff.
use crate::update_stream::{copy_hashed, hash_file, verify_file};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::Threading::*,
};
use windows::core::PWSTR;

#[derive(Serialize, Deserialize)]
struct Plan {
    target: PathBuf,
    parent: u32,
    old_hash: String,
    new_hash: String,
    version: String,
}
static RECEIPT: OnceLock<PathBuf> = OnceLock::new();
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
const MAX_PLAN: usize = 16 * 1024;

fn read_limited(reader: impl std::io::Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::io::Read::take(reader, limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("update file exceeds size limit".into());
    }
    Ok(bytes)
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    read_limited(file, limit)
}

pub(crate) fn prepare_cancellable(
    hash: &str,
    version: &str,
    download: impl FnOnce(&mut fs::File) -> Result<(), String>,
    checkpoint: impl Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    checkpoint()?;
    let target = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| e.to_string())?;
    prepare_at_cancellable(target, hash, version, download, checkpoint)
}
#[cfg(test)]
fn prepare_at(
    target: PathBuf,
    hash: &str,
    version: &str,
    download: impl FnOnce(&mut fs::File) -> Result<(), String>,
) -> Result<PathBuf, String> {
    prepare_at_cancellable(target, hash, version, download, || Ok(()))
}
fn prepare_at_cancellable(
    target: PathBuf,
    hash: &str,
    version: &str,
    download: impl FnOnce(&mut fs::File) -> Result<(), String>,
    checkpoint: impl Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    checkpoint()?;
    let parent = target.parent().ok_or("no executable directory")?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let folder = parent.join(format!(".winmgr-update-{}-{id}", std::process::id()));
    fs::create_dir(&folder).map_err(|e| format!("installation directory is not writable: {e}"))?;
    let result = (|| {
        let payload_path = folder.join("payload.exe");
        let mut payload = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&payload_path)
            .map_err(|e| e.to_string())?;
        checkpoint()?;
        download(&mut payload)?;
        checkpoint()?;
        payload.sync_all().map_err(|e| e.to_string())?;
        drop(payload);
        checkpoint()?;
        verify_file(&payload_path, hash)?;
        checkpoint()?;
        let old_hash = copy_hashed(&target, &folder.join("helper.exe"))?;
        checkpoint()?;
        let plan = Plan {
            target: target.clone(),
            parent: std::process::id(),
            old_hash,
            new_hash: hash.into(),
            version: version.into(),
        };
        let path = folder.join("plan.json");
        write_new(
            &path,
            &serde_json::to_vec(&plan).map_err(|e| e.to_string())?,
        )?;
        checkpoint()?;
        Ok(path)
    })();
    if result.is_err() {
        // Only this newly-created directory and its fixed, owned files.
        for name in ["payload.exe", "helper.exe", "plan.json"] {
            let _ = fs::remove_file(folder.join(name));
        }
        let _ = fs::remove_dir(&folder);
    }
    result
}
fn read_plan(path: &Path) -> Result<(Plan, PathBuf), String> {
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    if canonical.file_name().and_then(|v| v.to_str()) != Some("plan.json") {
        return Err("invalid handoff manifest".into());
    }
    let folder = canonical
        .parent()
        .ok_or("missing stage directory")?
        .to_path_buf();
    if !folder
        .file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.starts_with(".winmgr-update-"))
    {
        return Err("invalid stage directory".into());
    }
    let bytes = bounded_read(&canonical, MAX_PLAN)?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if plan.target.parent() != folder.parent() || plan.target.file_name().is_none() {
        return Err("handoff target is outside installation directory".into());
    }
    Ok((plan, folder))
}
fn spawn(path: &Path, args: &[&std::ffi::OsStr]) -> Result<Child, String> {
    let mut command = Command::new(path);
    command
        .args(args)
        .env_remove("MINICON_WINMGR_UPDATE_RECOVERED")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0 | CREATE_BREAKAWAY_FROM_JOB.0);
    match command.spawn() {
        Ok(child) => Ok(child),
        Err(_) => command
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .map_err(|e| e.to_string()),
    }
}
pub(crate) fn start_helper(path: &Path) -> Result<(), String> {
    let (plan, folder) = read_plan(path)?;
    if plan.parent != std::process::id()
        || plan.target
            != std::env::current_exe()
                .and_then(fs::canonicalize)
                .map_err(|e| e.to_string())?
    {
        return Err("handoff belongs to another instance".into());
    }
    verify_file(&folder.join("payload.exe"), &plan.new_hash)?;
    if hash_file(&plan.target)? != plan.old_hash
        || hash_file(&folder.join("helper.exe"))? != plan.old_hash
    {
        return Err("installation changed since download".into());
    }
    if let Ok(previous) = fs::read_to_string(folder.join("ready"))
        && let Ok(pid) = previous.parse::<u32>()
        && let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
    {
        let process = Process(handle);
        let mut image = vec![0u16; 32768];
        let mut count = image.len() as u32;
        if unsafe {
            QueryFullProcessImageNameW(
                process.0,
                PROCESS_NAME_FORMAT(0),
                PWSTR(image.as_mut_ptr()),
                &mut count,
            )
        }
        .is_ok()
            && PathBuf::from(String::from_utf16_lossy(&image[..count as usize]))
                .canonicalize()
                .ok()
                == Some(folder.join("helper.exe"))
        {
            return Err("previous helper is still finishing; retry shortly".into());
        }
    }
    for name in ["abort", "ready"] {
        if folder.join(name).exists() {
            fs::remove_file(folder.join(name)).map_err(|e| e.to_string())?;
        }
    }
    let mut child = spawn(
        &folder.join("helper.exe"),
        &[std::ffi::OsStr::new("--apply-update"), path.as_os_str()],
    )?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if folder.join("ready").exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("update helper exited: {status}"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = write_new(&folder.join("abort"), b"timeout");
    let _ = child.kill();
    let _ = child.wait();
    Err("update helper did not become ready".into())
}
struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
pub(crate) fn helper(path: &Path) -> Result<(), String> {
    let (plan, folder) = read_plan(path)?;
    if std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| e.to_string())?
        != folder.join("helper.exe")
        || !crate::single_instance::same_user_session(plan.parent)
    {
        return Err("invalid update helper or parent identity".into());
    }
    let process = Process(
        unsafe { OpenProcess(PROCESS_ACCESS_RIGHTS(0x100000 | 0x1000), false, plan.parent) }
            .map_err(|e| e.to_string())?,
    );
    let mut image = vec![0u16; 32768];
    let mut count = image.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_FORMAT(0),
            PWSTR(image.as_mut_ptr()),
            &mut count,
        )
    }
    .map_err(|e| e.to_string())?;
    if PathBuf::from(String::from_utf16_lossy(&image[..count as usize]))
        .canonicalize()
        .map_err(|e| e.to_string())?
        != plan.target
    {
        return Err("parent executable does not match handoff".into());
    }
    verify_file(&folder.join("payload.exe"), &plan.new_hash)?;
    write_new(
        &folder.join("ready"),
        std::process::id().to_string().as_bytes(),
    )?;
    let start = Instant::now();
    loop {
        if folder.join("abort").exists() {
            return Err("handoff cancelled".into());
        }
        if unsafe { WaitForSingleObject(process.0, 100) } == WAIT_OBJECT_0 {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err("old process did not exit; installation unchanged".into());
        }
    }
    let outcome = install_and_launch(&plan, &folder);
    if let Err(error) = &outcome {
        let _ = fs::write(folder.join("error.txt"), error);
        // Verification or a locked file may fail before any rename. If the old
        // executable is intact and there is no live shell, restore availability.
        if hash_file(&plan.target).ok().as_deref() == Some(&plan.old_hash)
            && let Ok(guard) =
                crate::single_instance::InstanceGuard::acquire("minicon-winmgr.singleton")
        {
            drop(guard);
            let mut command = Command::new(&plan.target);
            command
                .arg("--settings")
                .env("MINICON_WINMGR_UPDATE_RECOVERED", "1")
                .creation_flags(CREATE_NO_WINDOW.0);
            command
                .spawn()
                .map_err(|e| format!("{error}; old version intact but restart failed: {e}"))?;
        }
    }
    outcome
}
fn install_and_launch(plan: &Plan, folder: &Path) -> Result<(), String> {
    // Recheck after waiting: another instance or installer must not be overwritten.
    let guard = crate::single_instance::InstanceGuard::acquire("minicon-winmgr.singleton")
        .map_err(|e| e.to_string())?;
    verify_file(&folder.join("payload.exe"), &plan.new_hash)?;
    if hash_file(&plan.target)? != plan.old_hash {
        drop(guard);
        return Err("installation changed during handoff".into());
    }
    let backup = folder.join("previous.exe");
    if let Err(error) = fs::rename(&plan.target, &backup) {
        drop(guard);
        return Err(error.to_string());
    }
    if let Err(error) = fs::rename(folder.join("payload.exe"), &plan.target) {
        let rollback = fs::rename(&backup, &plan.target);
        drop(guard);
        return Err(format!("install failed: {error}; rollback: {rollback:?}"));
    }
    drop(guard);
    let receipt = folder.join("started");
    let result = (|| {
        let mut child = spawn(
            &plan.target,
            &[std::ffi::OsStr::new("--after-update"), receipt.as_os_str()],
        )?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if receipt.exists() {
                return Ok(());
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("new version exited before startup: {status}"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Only our newly-created candidate process, before its startup receipt.
        child.kill().map_err(|e| e.to_string())?;
        child.wait().map_err(|e| e.to_string())?;
        Err("new version startup timed out".into())
    })();
    if let Err(error) = result {
        fs::rename(&plan.target, folder.join("failed.exe"))
            .map_err(|e| format!("{error}; cannot retain failed candidate: {e}"))?;
        fs::rename(&backup, &plan.target).map_err(|e| format!("{error}; rollback failed: {e}"))?;
        return Err(error);
    }
    Ok(())
}
pub(crate) fn set_receipt(path: PathBuf) {
    let _ = RECEIPT.set(path);
}
pub(crate) fn confirm_startup() {
    let Some(path) = RECEIPT.get() else {
        return;
    };
    let result = (|| {
        let (plan, folder) = read_plan(&path.with_file_name("plan.json"))?;
        if path != &folder.join("started")
            || plan.target
                != std::env::current_exe()
                    .and_then(fs::canonicalize)
                    .map_err(|e| e.to_string())?
            || plan.version != env!("CARGO_PKG_VERSION")
        {
            return Err("invalid startup receipt".into());
        }
        write_new(path, b"started")
    })();
    if let Err(error) = result {
        tracing::error!(%error,"cannot acknowledge update startup");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Installation(PathBuf);
    impl Installation {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "winmgr-stream-install-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn executable(&self) -> PathBuf {
            self.0.join("winmgr.exe")
        }
    }
    impl Drop for Installation {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn executable_bytes() -> Vec<u8> {
        let mut bytes = vec![0; 80_000];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&[0x64, 0x86]);
        bytes[152..154].copy_from_slice(&[0x0b, 0x02]);
        bytes
    }
    #[test]
    fn cancellation_at_each_staging_boundary_preserves_installation_and_removes_owned_files() {
        let body = executable_bytes();
        let expected = crate::update_http::sha256(&body).unwrap();
        for stop_at in 1..=7 {
            let installation = Installation::new();
            let target = installation.executable();
            let mut old = body.clone();
            old[1000] = 7;
            fs::write(&target, &old).unwrap();
            let cancel = crate::update_cancel::Token::default();
            let calls = std::cell::Cell::new(0);
            let result = prepare_at_cancellable(
                target.clone(),
                &expected,
                "0.1.10",
                |file| file.write_all(&body).map_err(|error| error.to_string()),
                || {
                    calls.set(calls.get() + 1);
                    if calls.get() == stop_at {
                        cancel.cancel();
                    }
                    cancel.check()
                },
            );
            assert_eq!(result.unwrap_err(), "update operation cancelled");
            assert_eq!(calls.get(), stop_at);
            assert_eq!(fs::read(&target).unwrap(), old);
            assert_eq!(
                fs::read_dir(&installation.0).unwrap().count(),
                1,
                "cancellation boundary {stop_at} retained owned staging files"
            );
        }
    }

    #[test]
    #[ignore = "isolated update memory experiment; run update-memory layer"]
    fn streaming_preparation_commit_footprint() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        };
        const LENGTH: usize = 32 * 1024 * 1024;
        let installation = Installation::new();
        let mut header = executable_bytes();
        let target = installation.executable();
        {
            let mut file = fs::File::create(&target).unwrap();
            file.write_all(&header).unwrap();
            file.set_len(LENGTH as u64).unwrap();
            file.sync_all().unwrap();
        }
        let old_hash = hash_file(&target).unwrap();
        header[1000] = 7;
        let body = || {
            std::io::Cursor::new(&header)
                .chain(std::io::repeat(0).take((LENGTH - header.len()) as u64))
        };
        let expected = crate::update_stream::hash_reader(body(), LENGTH).unwrap();
        let mode = std::env::var("WINMGR_TEST_UPDATE_BUFFER").unwrap_or_else(|_| "stream".into());
        assert!(matches!(mode.as_str(), "legacy" | "stream"));
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let invalid = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let observer = {
            let stop = stop.clone();
            let peak = peak.clone();
            let invalid = invalid.clone();
            std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                while !stop.load(Ordering::SeqCst) {
                    let bytes = crate::metrics::sample().private_bytes;
                    if bytes == 0 {
                        invalid.store(true, Ordering::SeqCst);
                    }
                    peak.fetch_max(bytes, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let baseline = crate::metrics::sample().private_bytes;
        assert!(baseline > 0);
        peak.store(baseline, Ordering::SeqCst);
        let result: Result<(), String> = (|| {
            if mode == "legacy" {
                // Previous live-buffer pattern, test only: a full new payload
                // remains allocated while the full old executable is read.
                let mut new = fs::read(&target).map_err(|e| e.to_string())?;
                new[1000] = 7;
                let old = fs::read(&target).map_err(|e| e.to_string())?;
                peak.fetch_max(crate::metrics::sample().private_bytes, Ordering::SeqCst);
                assert_eq!(crate::update_http::sha256(&new)?, expected);
                fs::write(installation.0.join("legacy-payload.bin"), &new)
                    .map_err(|e| e.to_string())?;
                fs::write(installation.0.join("legacy-helper.bin"), &old)
                    .map_err(|e| e.to_string())?;
                std::hint::black_box((&new, &old));
            } else {
                let plan_path = prepare_at(target.clone(), &expected, "0.1.10", |file| {
                    let mut source = body();
                    let mut chunk = [0u8; 16 * 1024];
                    loop {
                        let count = source.read(&mut chunk).map_err(|e| e.to_string())?;
                        if count == 0 {
                            break;
                        }
                        file.write_all(&chunk[..count]).map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })?;
                let (plan, folder) = read_plan(&plan_path)?;
                assert_eq!(plan.old_hash, old_hash);
                verify_file(&folder.join("payload.exe"), &expected)?;
                assert_eq!(hash_file(&folder.join("helper.exe"))?, old_hash);
            }
            assert_eq!(hash_file(&target)?, old_hash);
            Ok(())
        })();
        stop.store(true, Ordering::SeqCst);
        observer.join().unwrap();
        result.unwrap();
        assert!(
            !invalid.load(Ordering::SeqCst),
            "private commit sampling failed"
        );
        let peak_delta = peak.load(Ordering::SeqCst).saturating_sub(baseline);
        if mode == "legacy" {
            assert!(peak_delta >= 60 * 1024 * 1024);
        } else {
            assert!(
                peak_delta < 8 * 1024 * 1024,
                "streaming staging needs memory investigation"
            );
        }
        println!(
            "UPDATE_MEMORY_SAMPLE {{\"mode\":\"{mode}\",\"file_bytes\":{LENGTH},\"private_peak_delta_bytes\":{peak_delta}}}"
        );
    }

    #[test]
    fn streamed_staging_verifies_payload_copies_old_helper_and_preserves_installation() {
        let installation = Installation::new();
        let old = executable_bytes();
        fs::write(installation.executable(), &old).unwrap();
        let mut new = old.clone();
        new[1000] = 7;
        let hash = crate::update_http::sha256(&new).unwrap();
        let path = prepare_at(installation.executable(), &hash, "0.1.10", |file| {
            for chunk in new.chunks(16 * 1024) {
                file.write_all(chunk).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .unwrap();
        let (plan, folder) = read_plan(&path).unwrap();
        assert_eq!(plan.old_hash, crate::update_http::sha256(&old).unwrap());
        assert_eq!(plan.new_hash, hash);
        assert_eq!(
            hash_file(&folder.join("helper.exe")).unwrap(),
            plan.old_hash
        );
        verify_file(&folder.join("payload.exe"), &hash).unwrap();
        assert_eq!(fs::read(installation.executable()).unwrap(), old);
    }
    #[test]
    fn failed_or_invalid_streamed_staging_cleans_owned_files_and_keeps_old_executable() {
        use std::os::windows::fs::OpenOptionsExt;
        let installation = Installation::new();
        let old = executable_bytes();
        let target = installation.executable();
        fs::write(&target, &old).unwrap();
        let hash = crate::update_http::sha256(&old).unwrap();
        let bad_hash = "0".repeat(64);
        for mode in ["download", "hash", "oversize", "locked-old"] {
            let lock = (mode == "locked-old").then(|| {
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&target)
                    .unwrap()
            });
            let result = prepare_at(
                target.clone(),
                if mode == "hash" { &bad_hash } else { &hash },
                "0.1.10",
                |file| {
                    file.write_all(&old).map_err(|e| e.to_string())?;
                    if mode == "download" {
                        return Err("injected download failure".into());
                    }
                    if mode == "oversize" {
                        file.set_len(crate::update::MAX_EXE as u64 + 1)
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(())
                },
            );
            drop(lock);
            assert!(result.is_err(), "{mode} unexpectedly staged");
            assert_eq!(fs::read(&target).unwrap(), old);
            assert_eq!(
                fs::read_dir(&installation.0).unwrap().count(),
                1,
                "{mode} left a staging folder"
            );
        }
    }

    #[test]
    fn manifest_limit_stops_reading_before_executable_sized_allocation() {
        use std::io::Read;
        let length = crate::update::MAX_EXE as u64;
        let mut oversized = std::io::repeat(b' ').take(length);
        assert!(read_limited(&mut oversized, MAX_PLAN).is_err());
        assert_eq!(oversized.limit(), length - MAX_PLAN as u64 - 1);
        let exact = std::io::repeat(b' ').take(MAX_PLAN as u64);
        assert_eq!(read_limited(exact, MAX_PLAN).unwrap().len(), MAX_PLAN);
    }

    #[test]
    fn manifest_is_bounded_and_confined_to_its_install_directory() {
        let root = std::env::temp_dir().join(format!(
            "winmgr-manifest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let folder = root.join(".winmgr-update-test");
        fs::create_dir(&folder).unwrap();
        let path = folder.join("plan.json");
        let mut plan = Plan {
            target: root.join("winmgr.exe"),
            parent: std::process::id(),
            old_hash: "a".repeat(64),
            new_hash: "b".repeat(64),
            version: "0.1.9".into(),
        };
        fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(read_plan(&path).is_ok());
        plan.target = root.parent().unwrap().join("unrelated.exe");
        fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(read_plan(&path).is_err());
        fs::write(&path, vec![b' '; 17 * 1024]).unwrap();
        assert!(read_plan(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&folder).unwrap();
        fs::remove_dir(&root).unwrap();
    }
}
