//! An ordinary-privilege, short-lived copy of this executable owns the handoff.
use crate::update_http::sha256;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
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
fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(crate::update::MAX_EXE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > crate::update::MAX_EXE {
        return Err("update file exceeds size limit".into());
    }
    Ok(bytes)
}
pub(crate) fn prepare(bytes: &[u8], hash: &str, version: &str) -> Result<PathBuf, String> {
    let target = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let parent = target.parent().ok_or("no executable directory")?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let folder = parent.join(format!(".winmgr-update-{}-{id}", std::process::id()));
    fs::create_dir(&folder).map_err(|e| format!("installation directory is not writable: {e}"))?;
    let result = (|| {
        let old = bounded_read(&target)?;
        let plan = Plan {
            target: target.clone(),
            parent: std::process::id(),
            old_hash: sha256(&old)?,
            new_hash: hash.into(),
            version: version.into(),
        };
        write_new(&folder.join("payload.exe"), bytes)?;
        write_new(&folder.join("helper.exe"), &old)?;
        let path = folder.join("plan.json");
        write_new(
            &path,
            &serde_json::to_vec(&plan).map_err(|e| e.to_string())?,
        )?;
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
    let bytes = bounded_read(&canonical)?;
    if bytes.len() > 16 * 1024 {
        return Err("oversized handoff manifest".into());
    }
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
    crate::update::verify(&bounded_read(&folder.join("payload.exe"))?, &plan.new_hash)?;
    if sha256(&bounded_read(&plan.target)?)? != plan.old_hash
        || sha256(&bounded_read(&folder.join("helper.exe"))?)? != plan.old_hash
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
    crate::update::verify(&bounded_read(&folder.join("payload.exe"))?, &plan.new_hash)?;
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
        if bounded_read(&plan.target)
            .ok()
            .and_then(|b| sha256(&b).ok())
            .as_deref()
            == Some(&plan.old_hash)
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
    crate::update::verify(&bounded_read(&folder.join("payload.exe"))?, &plan.new_hash)?;
    if sha256(&bounded_read(&plan.target)?)? != plan.old_hash {
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
