//! One explicit UAC launch, with a finite startup-only origin reference.
use crate::{task_access::OriginServer, task_service::Identity, win_util::wide};
use std::{
    fs::OpenOptions,
    os::windows::ffi::OsStrExt,
    os::windows::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        Storage::FileSystem::FILE_SHARE_READ,
        System::{Com::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::PCWSTR,
};

#[derive(Clone, Default)]
pub struct Request {
    pub focus: Option<Identity>,
    pub search: String,
    pub network: bool,
    pub chinese: bool,
    pub light: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Opened,
    Cancelled,
}

fn quote(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            result.push_str(&"\\".repeat(slashes));
        }
        result.push(character);
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}
struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct WindowProbe {
    pid: u32,
    ready: bool,
    prefix: String,
}
unsafe extern "system" fn find_window(hwnd: HWND, parameter: LPARAM) -> windows::core::BOOL {
    crate::win_util::guard_abi(
        "administrator startup window",
        || unsafe {
            let probe = &mut *(parameter.0 as *mut WindowProbe);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == probe.pid && IsWindowVisible(hwnd).as_bool() {
                let mut class = [0u16; 160];
                let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
                if String::from_utf16_lossy(&class[..length]).starts_with(&probe.prefix) {
                    probe.ready = true;
                }
            }
            windows::core::BOOL(1)
        },
        || windows::core::BOOL(0),
    )
}

/// Call on a background worker. The only executable arguments request the task UI.
pub fn administrator(
    executable: &Path,
    owner_window: isize,
    request: Request,
) -> Result<Outcome, String> {
    if !executable.is_absolute()
        || request.search.encode_utf16().count() > 512
        || request.search.contains('\0')
    {
        return Err("Invalid administrator tool path or search length".into());
    }
    // Retain a read-only lock across UAC and image creation; no writer or rename
    // can replace this executable during the handoff.
    let _image = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(executable)
        .map_err(|e| e.to_string())?;
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|e| e.to_string())?;
    let _apartment = Apartment;
    let origin = OriginServer::new()?;
    let identity = origin.identity();
    let mut arguments = vec![
        "--all-users".into(),
        "--origin-pipe".into(),
        origin.name().into(),
        "--origin-pid".into(),
        identity.pid.to_string(),
        "--origin-created".into(),
        identity.created_100ns.to_string(),
        "--language".into(),
        if request.chinese { "zh" } else { "en" }.into(),
        "--theme".into(),
        if request.light { "light" } else { "dark" }.into(),
        "--search".into(),
        request.search,
    ];
    if request.network {
        arguments.push("--network".into());
    }
    if let Some(focus) = request.focus {
        arguments.extend([
            "--focus-pid".into(),
            focus.pid.to_string(),
            "--focus-created".into(),
            focus.created_100ns.to_string(),
        ]);
    }
    let parameters = wide(
        &arguments
            .iter()
            .map(|argument| quote(argument))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let file: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        hwnd: HWND(owner_window as *mut _),
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        return if error.code().0 as u32 == 0x800704c7 {
            Ok(Outcome::Cancelled)
        } else {
            Err(format!("Windows administrator launch failed: {error}"))
        };
    }
    if info.hProcess.is_invalid() {
        return Err("Windows did not return the administrator process".into());
    }
    let process = Process(info.hProcess);
    let mut probe = WindowProbe {
        pid: unsafe { GetProcessId(process.0) },
        ready: false,
        prefix: format!(
            "minicon_tskmgr_{}_",
            env!("CARGO_PKG_VERSION").replace('.', "_")
        ),
    };
    let started = Instant::now();
    loop {
        let _ = unsafe {
            EnumWindows(
                Some(find_window),
                LPARAM((&mut probe as *mut WindowProbe) as isize),
            )
        };
        if probe.ready {
            return Ok(Outcome::Opened);
        }
        if unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0 {
            let mut code = 1;
            unsafe { GetExitCodeProcess(process.0, &mut code) }.map_err(|e| e.to_string())?;
            return if code == 0 {
                Ok(Outcome::Opened)
            } else {
                Err(format!(
                    "Administrator tool exited before readiness ({code})"
                ))
            };
        }
        if started.elapsed() > Duration::from_secs(15) {
            return Err("The administrator tasks window did not become ready".into());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_is_one_windows_argument_even_with_quotes_or_trailing_slashes() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("alpha beta"), "\"alpha beta\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\"), "\"a\\\\\"");
    }
}
