//! Windows read-only sampling. No shell state, elevation or UI dependencies.
use super::{Endpoint, Identity, Process, Snapshot, cpu_percent};
use ::windows::{
    Win32::{
        Foundation::*,
        NetworkManagement::IpHelper::*,
        Security::{Authorization::ConvertSidToStringSidW, *},
        System::{
            Diagnostics::ToolHelp::*, ProcessStatus::*, RemoteDesktop::ProcessIdToSessionId,
            Threading::*,
        },
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
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
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
