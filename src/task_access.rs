//! Windows task-role authorization and a startup-only authenticated caller reference.
//! No command transport, process management, UI, or persistent privileged endpoint.
use crate::task_service::{Actor, Identity, Scope, actor_for_process, elevated};
use crate::win_util::wide;
use windows::{
    Win32::{
        Foundation::*,
        Security::{Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, *},
        Storage::FileSystem::*,
        System::{Com::CoCreateGuid, Pipes::*, Threading::GetCurrentProcessId},
    },
    core::PCWSTR,
};

const PREFIX: &str = r"\\.\pipe\minicon-tskmgr-origin-";
#[cfg(test)]
mod cross_account_tests;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The fields are private: UI arguments and data records cannot grant AllTasks.
#[derive(Clone, Debug)]
pub struct Access {
    scope: Scope,
    origin: Actor,
    execution: Actor,
    debug_warning: Option<String>,
}
impl Access {
    pub fn current() -> Result<Self, String> {
        let execution = actor_for_process(unsafe { GetCurrentProcessId() })?;
        Ok(Self {
            scope: Scope::OwnTasks,
            origin: execution.clone(),
            execution,
            debug_warning: None,
        })
    }

    /// Called before instance forwarding or UI creation in the administrator role.
    pub fn administrator_from_origin(pipe: &str, expected: Identity) -> Result<Self, String> {
        if !elevated()? {
            return Err("All tasks requires explicit Windows administrator authorization.".into());
        }
        let execution = actor_for_process(unsafe { GetCurrentProcessId() })?;
        if !local_name(pipe) {
            return Err("Invalid local caller reference".into());
        }
        let debug_warning = enable_debug().err();
        let origin = peer(pipe, expected)?;
        if origin.session != execution.session {
            return Err("Caller and administrator tool are in different Windows sessions.".into());
        }
        Ok(Self {
            scope: Scope::AllTasks,
            origin,
            execution,
            debug_warning,
        })
    }
    pub fn scope(&self) -> Scope {
        self.scope
    }
    pub fn origin(&self) -> &Actor {
        &self.origin
    }
    pub fn execution(&self) -> &Actor {
        &self.execution
    }
    pub fn debug_warning(&self) -> Option<&str> {
        self.debug_warning.as_deref()
    }

    /// Role, both accounts, and both sessions are part of instance isolation.
    pub fn instance_key(&self) -> Result<String, String> {
        use windows::Win32::Security::Cryptography::*;
        let identity = format!(
            "{:?}.{}.{}.{}.{}",
            self.scope,
            self.origin.owner,
            self.origin.session,
            self.execution.owner,
            self.execution.session
        );
        struct Algorithm(BCRYPT_ALG_HANDLE);
        impl Drop for Algorithm {
            fn drop(&mut self) {
                let _ = unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
            }
        }
        let mut handle = BCRYPT_ALG_HANDLE::default();
        unsafe {
            BCryptOpenAlgorithmProvider(
                &mut handle,
                BCRYPT_SHA256_ALGORITHM,
                None,
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
            )
        }
        .ok()
        .map_err(|e| e.to_string())?;
        let handle = Algorithm(handle);
        let mut hash = [0u8; 32];
        unsafe { BCryptHash(handle.0, None, identity.as_bytes(), &mut hash) }
            .ok()
            .map_err(|e| e.to_string())?;
        Ok(hash.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

fn enable_debug() -> Result<(), String> {
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .map_err(|e| e.to_string())?;
    let token = Handle(token);
    let mut luid = LUID::default();
    unsafe { LookupPrivilegeValueW(None, SE_DEBUG_NAME, &mut luid) }.map_err(|e| e.to_string())?;
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    unsafe {
        SetLastError(ERROR_SUCCESS);
        AdjustTokenPrivileges(token.0, false, Some(&privileges), 0, None, None)
            .map_err(|e| e.to_string())?;
        if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
            return Err(
                "Windows did not grant SeDebugPrivilege; some tasks remain inaccessible".into(),
            );
        }
    }
    Ok(())
}

/// Keep this alive across ShellExecute/UAC until the child completes bootstrap.
/// The server is never used to accept commands or read/write a payload.
pub struct OriginServer {
    _handle: Handle,
    name: String,
    actor: Actor,
}
impl OriginServer {
    pub fn new() -> Result<Self, String> {
        let actor = actor_for_process(unsafe { GetCurrentProcessId() })?;
        let name = format!(
            "{PREFIX}{:?}",
            unsafe { CoCreateGuid() }.map_err(|e| e.to_string())?
        );
        let sddl = wide(&format!("D:P(A;;GA;;;{})(A;;GA;;;BA)", actor.owner));
        let mut security = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut security,
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        struct Descriptor(PSECURITY_DESCRIPTOR);
        impl Drop for Descriptor {
            fn drop(&mut self) {
                let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
            }
        }
        let security = Descriptor(security);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0.0,
            bInheritHandle: false.into(),
        };
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide(&name).as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                256,
                256,
                10000,
                Some(&attributes),
            )
        };
        if handle.is_invalid() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            _handle: Handle(handle),
            name,
            actor,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn identity(&self) -> Identity {
        self.actor.identity
    }
    pub fn origin(&self) -> &Actor {
        &self.actor
    }
}

fn local_name(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|suffix| {
        (32..=40).contains(&suffix.len())
            && suffix
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || matches!(b, b'-' | b'{' | b'}'))
    })
}
fn peer(name: &str, expected: Identity) -> Result<Actor, String> {
    if !local_name(name) {
        return Err("Invalid local caller reference".into());
    }
    let handle = Handle(
        unsafe {
            CreateFileW(
                PCWSTR(wide(name).as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(|e| e.to_string())?,
    );
    let mut pid = 0;
    unsafe { GetNamedPipeServerProcessId(handle.0, &mut pid) }.map_err(|e| e.to_string())?;
    if pid != expected.pid {
        return Err("Caller reference belongs to a different process".into());
    }
    let actor = actor_for_process(pid)?;
    if actor.identity != expected {
        return Err("Caller exited or its identity changed".into());
    }
    Ok(actor)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the isolated CI administrator token; does not initiate UAC"]
    fn administrator_context_reads_all_tasks_without_widening_own_scope() {
        assert!(
            elevated().unwrap(),
            "This component check requires an existing administrator token"
        );
        let origin = OriginServer::new().unwrap();
        let access = Access::administrator_from_origin(origin.name(), origin.identity()).unwrap();
        assert_eq!(access.scope(), Scope::AllTasks);
        assert_eq!(access.origin().identity, origin.identity());
        let snapshot = crate::task_service::Collector::with_access(access)
            .sample()
            .unwrap();
        assert_eq!(snapshot.scope, Scope::AllTasks);
        assert!(
            snapshot
                .processes
                .iter()
                .any(|p| p.identity.pid == origin.identity().pid)
        );
        assert!(
            snapshot.unavailable.iter().any(|p| p.pid == 0),
            "Idle must remain an explicit metadata-only row"
        );
        let own = Access::current().unwrap();
        let own_snapshot = crate::task_service::Collector::with_access(own.clone())
            .sample()
            .unwrap();
        assert!(own_snapshot.unavailable.is_empty());
        assert!(
            own_snapshot
                .processes
                .iter()
                .all(|p| p.owner.as_deref() == Some(own.origin().owner.as_str()))
        );
        let others = snapshot
            .processes
            .iter()
            .filter(|p| {
                p.owner
                    .as_ref()
                    .is_some_and(|owner| owner != &own.origin().owner)
            })
            .count();
        if let Some(other) = snapshot.processes.iter().find(|p| {
            p.owner
                .as_ref()
                .is_some_and(|owner| owner != &own.origin().owner)
        }) {
            assert!(own.identify(other.identity.pid).is_err());
        }
        println!(
            "Existing-admin component: {} verified rows, {} metadata-only, {} other-account rows; no UAC or management actions",
            snapshot.processes.len(),
            snapshot.unavailable.len(),
            others
        );
    }
    #[test]
    fn startup_reference_uses_kernel_peer_and_rejects_recycled_identity() {
        let origin = OriginServer::new().unwrap();
        assert!(local_name(origin.name()));
        assert!(!origin._handle.0.is_invalid());
        assert_eq!(
            peer(origin.name(), origin.identity()).unwrap(),
            *origin.origin()
        );
        let separate = OriginServer::new().unwrap();
        let wrong = Identity {
            created_100ns: separate.identity().created_100ns + 1,
            ..separate.identity()
        };
        assert!(peer(separate.name(), wrong).is_err());
        for bad in [r"\\server\pipe\minicon-tskmgr-origin-x", r"C:\origin", ""] {
            assert!(peer(bad, origin.identity()).is_err());
        }
    }
    #[test]
    fn ordinary_context_cannot_obtain_administrator_role() {
        let access = Access::current().unwrap();
        assert_eq!(access.scope(), Scope::OwnTasks);
        assert_eq!(access.origin(), access.execution());
        let key = access.instance_key().unwrap();
        assert_eq!(key.len(), 64);
        assert_eq!(Access::current().unwrap().instance_key().unwrap(), key);
        let mut other_context = access.clone();
        other_context.origin.owner = "different verified caller".into();
        assert_ne!(other_context.instance_key().unwrap(), key);
        if !elevated().unwrap() {
            let origin = OriginServer::new().unwrap();
            assert!(Access::administrator_from_origin(origin.name(), origin.identity()).is_err());
        }
    }
}
