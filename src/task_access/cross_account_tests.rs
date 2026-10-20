//! Hosted-CI-only real-account fixture. No UAC, GUI, or management of user tasks.
use super::*;
use crate::task_service::Collector;
use std::{
    io::{Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{Win32::System::Threading::*, core::PWSTR};

#[derive(serde::Serialize, serde::Deserialize)]
struct Report {
    pipe: String,
    actor: Actor,
    own_scope_only: bool,
}
fn hosted() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    assert!(
        std::env::var("MINICON_CI_USER_SID").is_ok(),
        "Use the CI fixture driver"
    );
}

#[test]
#[ignore = "owned standard-account fixture; created only by the hosted CI driver"]
fn origin_fixture() {
    let result = std::panic::catch_unwind(origin_fixture_run);
    if let Err(error) = result {
        if let Some(destination) = std::env::var_os("MINICON_CI_ORIGIN_REPORT") {
            let text = error
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    error
                        .downcast_ref::<&str>()
                        .map(|message| (*message).into())
                })
                .unwrap_or_else(|| "Non-string fixture panic".into());
            let _ = std::fs::write(PathBuf::from(destination).with_extension("error"), text);
        }
        std::panic::resume_unwind(error);
    }
}

fn origin_fixture_run() {
    hosted();
    assert!(!elevated().unwrap());
    let own = Access::current().unwrap();
    let snapshot = Collector::with_access(own.clone()).sample().unwrap();
    let reference = OriginServer::new().unwrap();
    let report = Report {
        pipe: reference.name().into(),
        actor: reference.origin().clone(),
        own_scope_only: snapshot.unavailable.is_empty()
            && snapshot
                .processes
                .iter()
                .any(|p| p.identity.pid == own.origin().identity.pid)
            && snapshot
                .processes
                .iter()
                .all(|p| p.owner.as_deref() == Some(own.origin().owner.as_str())),
    };
    let destination = PathBuf::from(std::env::var_os("MINICON_CI_ORIGIN_REPORT").unwrap());
    let staged = destination.with_extension("pending");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .unwrap();
    file.write_all(&serde_json::to_vec(&report).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    drop(file);
    std::fs::rename(staged, destination).unwrap();
    // The parent retains its own process handle and ends only this fixture.
    std::thread::sleep(Duration::from_secs(20));
}

struct Child(PROCESS_INFORMATION);
impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            if WaitForSingleObject(self.0.hProcess, 0) == WAIT_TIMEOUT {
                let _ = TerminateProcess(self.0.hProcess, 0);
                let _ = WaitForSingleObject(self.0.hProcess, 3000);
            }
            let _ = CloseHandle(self.0.hThread);
            let _ = CloseHandle(self.0.hProcess);
        }
    }
}

#[test]
#[ignore = "real differing Windows tokens in an ephemeral hosted-CI account; never prompts for UAC"]
fn parent_retains_original_user_with_different_credentials() {
    hosted();
    assert!(elevated().unwrap());
    let user = wide(&std::env::var("MINICON_CI_USER_NAME").unwrap());
    let mut password = wide(&std::env::var("MINICON_CI_USER_PASSWORD").unwrap());
    let expected_sid = std::env::var("MINICON_CI_USER_SID").unwrap();
    let destination = PathBuf::from(std::env::var_os("MINICON_CI_ORIGIN_REPORT").unwrap());
    let executable = std::env::current_exe().unwrap();
    let file = wide(&executable.to_string_lossy());
    let mut arguments = wide(&format!(
        "\"{}\" --ignored --exact task_access::cross_account_tests::origin_fixture --nocapture",
        executable.display()
    ));
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // Unlike CreateProcessW, a null environment here creates the target user's
    // profile environment. Supply only fixture references, never credentials or
    // a claimed account identity as authority; the real token is still checked.
    let mut variables: Vec<_> = [
        "SystemRoot",
        "WINDIR",
        "GITHUB_ACTIONS",
        "RUNNER_ENVIRONMENT",
        "MINICON_CI_USER_SID",
        "MINICON_CI_ORIGIN_REPORT",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
    .collect();
    variables.sort_by_key(|(key, _)| key.to_ascii_lowercase());
    use std::os::windows::ffi::OsStrExt;
    let mut environment = Vec::<u16>::new();
    for (key, value) in variables {
        environment.extend(key.encode_utf16());
        environment.push(b'=' as u16);
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    environment.push(0);
    let launched = unsafe {
        CreateProcessWithLogonW(
            PCWSTR(user.as_ptr()),
            PCWSTR(wide(".").as_ptr()),
            PCWSTR(password.as_ptr()),
            CREATE_PROCESS_LOGON_FLAGS(0),
            PCWSTR(file.as_ptr()),
            Some(PWSTR(arguments.as_mut_ptr())),
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            None,
            &startup,
            &mut info,
        )
    };
    password.fill(0);
    launched.unwrap();
    let child = Child(info);
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user_time = FILETIME::default();
    unsafe {
        GetProcessTimes(
            child.0.hProcess,
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user_time,
        )
    }
    .unwrap();
    let expected = Identity {
        pid: child.0.dwProcessId,
        created_100ns: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
    };
    let started = Instant::now();
    while !destination.exists() {
        let mut code = 0;
        unsafe { GetExitCodeProcess(child.0.hProcess, &mut code) }.unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(child.0.hProcess, 0) },
            WAIT_TIMEOUT,
            "Owned standard fixture exited before identity handoff: {code:#x}; {}",
            std::fs::read_to_string(destination.with_extension("error"))
                .unwrap_or_else(|_| "No fixture panic report".into())
        );
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "Owned identity handoff timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut data = Vec::new();
    std::fs::File::open(destination)
        .unwrap()
        .take(4097)
        .read_to_end(&mut data)
        .unwrap();
    assert!(data.len() <= 4096);
    let report: Report = serde_json::from_slice(&data).unwrap();
    assert_eq!(report.actor.identity, expected);
    assert_eq!(report.actor.owner, expected_sid);
    assert!(report.own_scope_only);
    let administrator = Access::administrator_from_origin(&report.pipe, expected).unwrap();
    let own = Access::current().unwrap();
    assert_eq!(administrator.scope(), Scope::AllTasks);
    assert_eq!(administrator.origin().owner, expected_sid);
    assert_eq!(administrator.execution().owner, own.origin().owner);
    assert_ne!(
        administrator.origin().owner,
        administrator.execution().owner
    );
    assert_eq!(
        administrator.origin().session,
        administrator.execution().session
    );
    assert_eq!(administrator.identify(expected.pid).unwrap(), expected);
    let all = Collector::with_access(administrator.clone())
        .sample()
        .unwrap();
    assert_eq!(all.owner, expected_sid);
    assert!(
        all.processes
            .iter()
            .any(|p| p.identity == expected && p.owner.as_deref() == Some(expected_sid.as_str()))
    );
    assert!(own.identify(expected.pid).is_err());
    let local_reference = OriginServer::new().unwrap();
    let local_admin =
        Access::administrator_from_origin(local_reference.name(), local_reference.identity())
            .unwrap();
    assert_ne!(
        local_admin.instance_key().unwrap(),
        administrator.instance_key().unwrap()
    );
    println!(
        "Cross-account Windows tokens verified: original standard user retained, different administrator execution, own-scope containment and instance separation; no UAC GUI claim"
    );
}
