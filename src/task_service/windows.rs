//! Windows read-only sampling. No shell state, elevation or UI dependencies.
use super::{Endpoint, Identity, Process, Snapshot, Window, cpu_percent};
use ::windows::{
    Win32::{
        Foundation::*,
        NetworkManagement::IpHelper::*,
        Security::{Authorization::ConvertSidToStringSidW, *},
        System::{
            Diagnostics::ToolHelp::*, ProcessStatus::*, RemoteDesktop::ProcessIdToSessionId,
            Threading::*,
        },
        UI::WindowsAndMessaging::*,
    },
    core::{HRESULT, PWSTR},
};
use std::{
    collections::HashMap,
    ffi::c_void,
    net::{Ipv4Addr, Ipv6Addr, SocketAddrV6},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn sid(process: HANDLE) -> Result<String, String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).map_err(|e| e.to_string())?;
        let token = Owned(token);
        let mut size = 0;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut size);
        if size == 0 || size > 65536 {
            return Err("Invalid token information size".into());
        }
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
        .map_err(|e| e.to_string())?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut string = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut string).map_err(|e| e.to_string())?;
        let result = string.to_string().map_err(|e| e.to_string());
        let _ = LocalFree(Some(HLOCAL(string.0.cast())));
        result
    }
}

fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

fn times(process: HANDLE, pid: u32) -> Option<(Identity, u64)> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user).ok()?;
    }
    Some((
        Identity {
            pid,
            created_100ns: filetime(created),
        },
        filetime(kernel).checked_add(filetime(user))?,
    ))
}

fn path(process: HANDLE) -> Option<String> {
    let mut text = vec![0u16; 32768];
    let mut length = text.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(text.as_mut_ptr()),
            &mut length,
        )
        .ok()?;
    }
    Some(String::from_utf16_lossy(&text[..length as usize]))
}

fn memory(pid: u32) -> Option<(u64, u64)> {
    unsafe {
        let process =
            Owned(OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?);
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        GetProcessMemoryInfo(
            process.0,
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
        .ok()?;
        Some((counters.WorkingSetSize as u64, counters.PrivateUsage as u64))
    }
}

fn enumeration(result: ::windows::core::Result<()>) -> Result<bool, String> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

pub struct Collector {
    owner: String,
    session: u32,
    previous: HashMap<Identity, u64>,
    sampled: Option<Instant>,
}

impl Collector {
    pub fn new() -> Result<Self, String> {
        let owner = sid(unsafe { GetCurrentProcess() })?;
        let mut session = 0;
        unsafe {
            ProcessIdToSessionId(GetCurrentProcessId(), &mut session).map_err(|e| e.to_string())?;
        }
        Ok(Self {
            owner,
            session,
            previous: HashMap::new(),
            sampled: None,
        })
    }

    pub fn sample(&mut self) -> Result<Snapshot, String> {
        let now = Instant::now();
        let elapsed = self
            .sampled
            .map(|prior| now.duration_since(prior).as_secs_f64());
        // The denominator is the whole system, not this tool's CPU affinity.
        let cores = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize;
        let snapshot = Owned(unsafe {
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| e.to_string())?
        });
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut rows = Vec::new();
        let mut unknown = 0;
        let mut next = HashMap::new();
        let mut available = enumeration(unsafe { Process32FirstW(snapshot.0, &mut entry) })?;
        while available {
            let pid = entry.th32ProcessID;
            if let Ok(handle) =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            {
                let process = Owned(handle);
                match sid(process.0) {
                    Ok(owner) if owner == self.owner => {
                        if let Some((identity, total)) = times(process.0, pid) {
                            let cpu = elapsed.and_then(|seconds| {
                                cpu_percent(
                                    self.previous.get(&identity).map(|total| (identity, *total)),
                                    identity,
                                    total,
                                    seconds,
                                    cores,
                                )
                            });
                            next.insert(identity, total);
                            let mut session = 0;
                            let session = unsafe { ProcessIdToSessionId(pid, &mut session) }
                                .ok()
                                .map(|_| session);
                            let memory = memory(pid);
                            let end = entry
                                .szExeFile
                                .iter()
                                .position(|c| *c == 0)
                                .unwrap_or(entry.szExeFile.len());
                            rows.push(Process {
                                identity,
                                parent_pid: entry.th32ParentProcessID,
                                name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                                owner,
                                session,
                                path: path(process.0),
                                cpu_percent: cpu,
                                working_bytes: memory.map(|m| m.0),
                                private_bytes: memory.map(|m| m.1),
                                windows: Vec::new(),
                            });
                        } else {
                            unknown += 1;
                        }
                    }
                    Ok(_) => (),
                    Err(_) => unknown += 1,
                }
            } else {
                unknown += 1;
            }
            available = enumeration(unsafe { Process32NextW(snapshot.0, &mut entry) })?;
        }
        rows.sort_by_key(|process| (process.session != Some(self.session), process.identity.pid));
        let mut by_pid: HashMap<u32, Vec<Window>> = HashMap::new();
        unsafe {
            EnumWindows(
                Some(visible_window),
                LPARAM((&mut by_pid as *mut HashMap<u32, Vec<Window>>) as isize),
            )
            .map_err(|e| e.to_string())?;
        }
        for process in &mut rows {
            process.windows = by_pid.remove(&process.identity.pid).unwrap_or_default();
        }
        let mut result = Snapshot {
            schema: 1,
            platform: "windows".into(),
            sampled_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis(),
            owner: self.owner.clone(),
            session: self.session,
            processes: rows,
            endpoints: Vec::new(),
            ownership_unavailable: unknown,
            limitations: Vec::new(),
        };
        let owners: HashMap<u32, Identity> = result
            .processes
            .iter()
            .map(|p| (p.identity.pid, p.identity))
            .collect();
        if cores == 0 {
            result
                .limitations
                .push("CPU percentages unavailable: cannot query active processor count".into());
        }
        for (family, tcp) in [(2, true), (23, true), (2, false), (23, false)] {
            match endpoints(family, tcp, &owners) {
                Ok(endpoints) => result.endpoints.extend(endpoints),
                Err(error) => result.limitations.push(format!(
                    "{} {}: {error}",
                    if family == 2 { "IPv4" } else { "IPv6" },
                    if tcp { "TCP" } else { "UDP" }
                )),
            }
        }
        self.previous = next;
        self.sampled = Some(now);
        Ok(result)
    }
}

fn table(family: u32, tcp: bool) -> Result<(Vec<u64>, usize), String> {
    let mut bytes = 0u32;
    for _ in 0..4 {
        if bytes > 16 * 1024 * 1024 {
            return Err("Endpoint table exceeds capacity".into());
        }
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        let pointer = (!buffer.is_empty()).then(|| buffer.as_mut_ptr().cast::<c_void>());
        let code = unsafe {
            if tcp {
                GetExtendedTcpTable(
                    pointer,
                    &mut bytes,
                    false,
                    family,
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            } else {
                GetExtendedUdpTable(pointer, &mut bytes, false, family, UDP_TABLE_OWNER_PID, 0)
            }
        };
        if code == 0 {
            if bytes < 4 || bytes as usize > buffer.len() * 8 {
                return Err("Truncated endpoint table".into());
            }
            return Ok((buffer, bytes as usize));
        }
        if code != ERROR_INSUFFICIENT_BUFFER.0 {
            return Err(format!("Windows error {code}"));
        }
    }
    Err("Endpoint table changed too quickly; retry".into())
}

fn state(value: u32) -> String {
    match value {
        1 => "CLOSED",
        2 => "LISTEN",
        3 => "SYN_SENT",
        4 => "SYN_RECEIVED",
        5 => "ESTABLISHED",
        6 => "FIN_WAIT_1",
        7 => "FIN_WAIT_2",
        8 => "CLOSE_WAIT",
        9 => "CLOSING",
        10 => "LAST_ACK",
        11 => "TIME_WAIT",
        12 => "DELETE_TCB",
        _ => "UNKNOWN",
    }
    .into()
}
fn v4(address: u32, port: u32) -> String {
    format!(
        "{}:{}",
        Ipv4Addr::from(address.to_ne_bytes()),
        u16::from_be(port as u16)
    )
}
fn v6(address: [u8; 16], port: u32, scope: u32) -> String {
    SocketAddrV6::new(Ipv6Addr::from(address), u16::from_be(port as u16), 0, scope).to_string()
}

fn endpoints(
    family: u32,
    tcp: bool,
    owners: &HashMap<u32, Identity>,
) -> Result<Vec<Endpoint>, String> {
    let (buffer, length) = table(family, tcp)?;
    let pointer = buffer.as_ptr().cast::<u8>();
    let count = unsafe { pointer.cast::<u32>().read_unaligned() } as usize;
    let row_size = match (family, tcp) {
        (2, true) => size_of::<MIB_TCPROW_OWNER_PID>(),
        (23, true) => size_of::<MIB_TCP6ROW_OWNER_PID>(),
        (2, false) => size_of::<MIB_UDPROW_OWNER_PID>(),
        _ => size_of::<MIB_UDP6ROW_OWNER_PID>(),
    };
    if count > (length - 4) / row_size {
        return Err("Invalid endpoint row count".into());
    }
    let mut output = Vec::new();
    let mut verified = HashMap::new();
    for index in 0..count {
        let row = unsafe { pointer.add(4 + index * row_size) };
        let (pid, local, remote, status) = unsafe {
            match (family, tcp) {
                (2, true) => {
                    let r = row.cast::<MIB_TCPROW_OWNER_PID>().read_unaligned();
                    (
                        r.dwOwningPid,
                        v4(r.dwLocalAddr, r.dwLocalPort),
                        (r.dwState != 2).then(|| v4(r.dwRemoteAddr, r.dwRemotePort)),
                        Some(state(r.dwState)),
                    )
                }
                (23, true) => {
                    let r = row.cast::<MIB_TCP6ROW_OWNER_PID>().read_unaligned();
                    (
                        r.dwOwningPid,
                        v6(r.ucLocalAddr, r.dwLocalPort, r.dwLocalScopeId),
                        (r.dwState != 2)
                            .then(|| v6(r.ucRemoteAddr, r.dwRemotePort, r.dwRemoteScopeId)),
                        Some(state(r.dwState)),
                    )
                }
                (2, false) => {
                    let r = row.cast::<MIB_UDPROW_OWNER_PID>().read_unaligned();
                    (r.dwOwningPid, v4(r.dwLocalAddr, r.dwLocalPort), None, None)
                }
                _ => {
                    let r = row.cast::<MIB_UDP6ROW_OWNER_PID>().read_unaligned();
                    (
                        r.dwOwningPid,
                        v6(r.ucLocalAddr, r.dwLocalPort, r.dwLocalScopeId),
                        None,
                        None,
                    )
                }
            }
        };
        let Some(&identity) = owners.get(&pid) else {
            continue;
        };
        // Endpoint APIs contain only PID. Recheck lifetime after reading the table.
        let live = verified.entry(identity).or_insert_with(|| unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .ok()
                .and_then(|h| times(Owned(h).0, pid))
                .is_some_and(|(current, _)| current == identity)
        });
        if *live {
            output.push(Endpoint {
                process: identity,
                protocol: if tcp { "TCP" } else { "UDP" }.into(),
                local,
                remote,
                state: status,
            });
        }
    }
    Ok(output)
}

unsafe extern "system" fn visible_window(hwnd: HWND, data: LPARAM) -> ::windows::core::BOOL {
    crate::win_util::guard_abi(
        "task window enumeration",
        || {
            unsafe {
                if !IsWindowVisible(hwnd).as_bool()
                    || GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid())
                    || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
                {
                    return ::windows::core::BOOL(1);
                }
                let mut title = [0u16; 1024];
                let length = GetWindowTextW(hwnd, &mut title);
                if length > 0 {
                    let mut pid = 0;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    let rows = &mut *(data.0 as *mut HashMap<u32, Vec<Window>>);
                    rows.entry(pid).or_default().push(Window {
                        id: format!("{:x}", hwnd.0 as usize),
                        title: String::from_utf16_lossy(&title[..length as usize]),
                    });
                }
            }
            ::windows::core::BOOL(1)
        },
        || ::windows::core::BOOL(0),
    )
}

fn verified(identity: Identity, access: PROCESS_ACCESS_RIGHTS) -> Result<Owned, String> {
    let process = Owned(
        unsafe {
            OpenProcess(
                access | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                identity.pid,
            )
        }
        .map_err(|e| e.to_string())?,
    );
    if times(process.0, identity.pid).is_none_or(|(current, _)| current != identity) {
        return Err("The process exited or its identity changed. Refresh the list.".into());
    }
    if sid(process.0)? != sid(unsafe { GetCurrentProcess() })? {
        return Err("This entry manages only the current user's processes.".into());
    }
    Ok(process)
}

pub fn identify(pid: u32) -> Result<Identity, String> {
    let process = Owned(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|e| e.to_string())?,
    );
    let (identity, _) = times(process.0, pid).ok_or("The process is unavailable")?;
    verified(identity, PROCESS_QUERY_LIMITED_INFORMATION)?;
    Ok(identity)
}

pub fn user_context() -> Result<(String, u32), String> {
    let owner = sid(unsafe { GetCurrentProcess() })?;
    let mut session = 0;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session).map_err(|e| e.to_string())?;
    }
    Ok((owner, session))
}

pub fn elevated() -> Result<bool, String> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| e.to_string())?;
    }
    let token = Owned(token);
    let mut value = TOKEN_ELEVATION::default();
    let mut length = 0;
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut value as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(value.TokenIsElevated != 0)
}

fn checked_window(identity: Identity, window: &str) -> Result<(Owned, HWND), String> {
    let process = verified(identity, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let value = usize::from_str_radix(window, 16).map_err(|_| "Invalid window identity")?;
    let hwnd = HWND(value as *mut _);
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid != identity.pid || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err("The window is no longer available.".into());
    }
    Ok((process, hwnd))
}

pub fn activate(identity: Identity, window: &str) -> Result<(), String> {
    let (_process, hwnd) = checked_window(identity, window)?;
    let mut response = 0;
    if unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            150,
            Some(&mut response),
        )
    }
    .0 == 0
    {
        return Err("The window is not responding. Close or end it explicitly if needed.".into());
    }
    // Give a non-UI action worker its own message queue before foreground work.
    let mut message = MSG::default();
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    if unsafe { crate::win_util::force_foreground(hwnd) } {
        Ok(())
    } else {
        Err("Windows refused to activate this window.".into())
    }
}

pub fn close_window(identity: Identity, window: &str) -> Result<(), String> {
    let (_process, hwnd) = checked_window(identity, window)?;
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.map_err(|e| e.to_string())
}

pub fn terminate(identity: Identity) -> Result<(), String> {
    if identity.pid == unsafe { GetCurrentProcessId() } {
        return Err("Use Close to exit this task manager.".into());
    }
    let process = verified(identity, PROCESS_TERMINATE | PROCESS_SYNCHRONIZE)?;
    unsafe {
        let mut critical = ::windows::core::BOOL(0);
        IsProcessCritical(process.0, &mut critical).map_err(|e| e.to_string())?;
        if critical.as_bool() {
            return Err("Windows marks this process as critical; it cannot be ended here.".into());
        }
        TerminateProcess(process.0, 1).map_err(|e| e.to_string())?;
        if WaitForSingleObject(process.0, 2000) != WAIT_OBJECT_0 {
            return Err("Termination was requested, but the process has not exited yet.".into());
        }
    }
    Ok(())
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            ::windows::Win32::System::Com::CoUninitialize();
        }
    }
}
fn com(mode: ::windows::Win32::System::Com::COINIT) -> Result<Com, String> {
    unsafe {
        ::windows::Win32::System::Com::CoInitializeEx(None, mode)
            .ok()
            .map_err(|e| e.to_string())?;
    }
    Ok(Com)
}

pub fn open_location(identity: Identity) -> Result<(), String> {
    use ::windows::{
        Win32::{System::Com::*, UI::Shell::*},
        core::PCWSTR,
    };
    let process = verified(identity, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let path = path(process.0).ok_or("Executable path is unavailable")?;
    let _com = com(COINIT_APARTMENTTHREADED)?;
    let mut pidl = std::ptr::null_mut();
    unsafe {
        SHParseDisplayName(
            PCWSTR(crate::win_util::wide(&path).as_ptr()),
            None,
            &mut pidl,
            0,
            None,
        )
        .map_err(|e| e.to_string())?;
        let result = SHOpenFolderAndSelectItems(pidl, None, 0).map_err(|e| e.to_string());
        CoTaskMemFree(Some(pidl.cast()));
        result
    }
}

fn wmi_created(value: &str) -> Option<u64> {
    use ::windows::Win32::System::Time::SystemTimeToFileTime;
    if value.len() != 25 || !value.is_ascii() || value.as_bytes()[14] != b'.' {
        return None;
    }
    let part = |a, b| value.get(a..b)?.parse::<u16>().ok();
    let date = SYSTEMTIME {
        wYear: part(0, 4)?,
        wMonth: part(4, 6)?,
        wDay: part(6, 8)?,
        wHour: part(8, 10)?,
        wMinute: part(10, 12)?,
        wSecond: part(12, 14)?,
        ..Default::default()
    };
    let mut time = FILETIME::default();
    unsafe {
        SystemTimeToFileTime(&date, &mut time).ok()?;
    }
    let fraction = value[15..21].parse::<u64>().ok()?.checked_mul(10)?;
    let time = filetime(time).checked_add(fraction)?;
    let offset = value[22..25]
        .parse::<u64>()
        .ok()?
        .checked_mul(600_000_000)?;
    match value.as_bytes()[21] {
        b'+' => time.checked_sub(offset),
        b'-' => time.checked_add(offset),
        _ => None,
    }
}

/// Called only by a dedicated detail worker, never by the UI or sampler.
pub fn command_line(identity: Identity) -> Result<String, String> {
    use ::windows::{
        Win32::System::{Com::*, Variant::*, Wmi::*},
        core::{BSTR, PCWSTR},
    };
    let process = verified(identity, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let _com = com(COINIT_MULTITHREADED)?;
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| e.to_string())?;
        let empty = BSTR::new();
        let service = locator
            .ConnectServer(
                &BSTR::from("ROOT\\CIMV2"),
                &empty,
                &empty,
                &empty,
                0,
                &empty,
                None,
            )
            .map_err(|e| e.to_string())?;
        CoSetProxyBlanket(
            &service,
            10,
            0,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .map_err(|e| e.to_string())?;
        let query = BSTR::from(format!(
            "SELECT CommandLine, CreationDate FROM Win32_Process WHERE ProcessId = {}",
            identity.pid
        ));
        let result = service
            .ExecQuery(
                &BSTR::from("WQL"),
                &query,
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
            .map_err(|e| e.to_string())?;
        let mut values = [None];
        let mut returned = 0;
        result
            .Next(1500, &mut values, &mut returned)
            .ok()
            .map_err(|e| e.to_string())?;
        let object = values[0].as_ref().ok_or("Command line is not available")?;
        let property = |name: &str| -> Result<String, String> {
            let mut variant = VARIANT::default();
            object
                .Get(
                    PCWSTR(crate::win_util::wide(name).as_ptr()),
                    0,
                    &mut variant,
                    None,
                    None,
                )
                .map_err(|e| e.to_string())?;
            let mut buffer = vec![0u16; 32768];
            let converted = VariantToString(&variant, &mut buffer).map_err(|e| e.to_string());
            let _ = VariantClear(&mut variant);
            converted?;
            let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
            Ok(String::from_utf16_lossy(&buffer[..end]))
        };
        let created =
            wmi_created(&property("CreationDate")?).ok_or("Creation time is unavailable")?;
        if created.abs_diff(identity.created_100ns) >= 10
            || times(process.0, identity.pid).is_none_or(|(current, _)| current != identity)
        {
            return Err("The process identity changed while reading its command line.".into());
        }
        let command = property("CommandLine")?;
        if command.is_empty() {
            Err("Command line is not available with current permissions.".into())
        } else {
            Ok(command)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::windows::process::CommandExt,
        process::{Child, Command, Stdio},
    };
    #[test]
    #[ignore = "private owned-process fixture, invoked by the identity guard test"]
    fn owned_target_fixture() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    fn termination_rejects_changed_creation_time_without_touching_owned_target() {
        struct Target(Child);
        impl Drop for Target {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "task_service::windows::tests::owned_target_fixture",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .unwrap();
        let mut target = Target(child);
        let mut wrong = identify(target.0.id()).unwrap();
        wrong.created_100ns += 1;
        let error = terminate(wrong).unwrap_err();
        assert!(
            error.contains("identity"),
            "expected identity refusal, received {error}"
        );
        assert!(
            target.0.try_wait().unwrap().is_none(),
            "the owned target must remain alive"
        );
    }
}
