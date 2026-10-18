//! Per-user login shell access. All explicit writes share the recovery guard.
use crate::win_util::wide;
use serde::{Deserialize, Serialize};
use std::error::Error;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::PCWSTR;
const SUBKEY: &str = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
const VALUE_NAME: &str = "Shell";
const MAX_VALUE: usize = 65536;

/// Preserve absence, an empty value, exact UTF-16 data and registry type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawShell {
    pub(crate) kind: u32,
    pub(crate) data: Vec<u8>,
}
impl RawShell {
    pub(crate) fn string(value: &str) -> Self {
        Self {
            kind: REG_SZ.0,
            data: sz_bytes(value),
        }
    }
    pub(crate) fn text(&self) -> Result<String, Box<dyn Error>> {
        if !matches!(self.kind, n if n == REG_SZ.0 || n == REG_EXPAND_SZ.0)
            || self.data.len() > MAX_VALUE
            || !self.data.len().is_multiple_of(2)
        {
            return Err("Shell value has an unsupported type or invalid size".into());
        }
        let units: Vec<_> = self
            .data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let value = String::from_utf16(&units)?
            .trim_end_matches('\0')
            .to_string();
        if value.contains('\0') {
            return Err("Shell value contains an embedded NUL".into());
        }
        Ok(value)
    }
}
pub fn current_shell() -> Result<Option<String>, Box<dyn Error>> {
    current_raw()?.map(|v| v.text()).transpose()
}
pub(crate) fn current_raw() -> Result<Option<RawShell>, Box<dyn Error>> {
    read_value(HKEY_CURRENT_USER, SUBKEY, VALUE_NAME)
}
pub(crate) fn machine_identity() -> Result<String, Box<dyn Error>> {
    read_value(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Cryptography",
        "MachineGuid",
    )?
    .ok_or_else(|| "Machine identity is unavailable".into())
    .and_then(|v| v.text())
}
fn read_value(root: HKEY, subkey: &str, name: &str) -> Result<Option<RawShell>, Box<dyn Error>> {
    unsafe {
        let mut key = HKEY::default();
        let rc = RegOpenKeyExW(
            root,
            PCWSTR(wide(subkey).as_ptr()),
            Some(0),
            KEY_READ,
            &mut key,
        );
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(format!("Shell read/open failed (Win32 {})", rc.0).into());
        }
        let result = query_raw(key, name);
        let _ = RegCloseKey(key);
        result
    }
}
fn query_raw(key: HKEY, name: &str) -> Result<Option<RawShell>, Box<dyn Error>> {
    let name = wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut length = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut length),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if rc != ERROR_SUCCESS || length as usize > MAX_VALUE {
        return Err(format!("Shell read/size failed (Win32 {})", rc.0).into());
    }
    let mut data = vec![0; length as usize];
    let rc = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(data.as_mut_ptr()),
            Some(&mut length),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!("Shell read/data failed (Win32 {})", rc.0).into());
    }
    data.truncate(length as usize);
    Ok(Some(RawShell { kind: kind.0, data }))
}
pub fn replace(value: &str) -> Result<(), Box<dyn Error>> {
    crate::shell_recovery::set_login(Some(RawShell::string(value)))
}
pub fn restore(value: Option<&str>) -> Result<(), Box<dyn Error>> {
    crate::shell_recovery::set_login(value.map(RawShell::string))
}
fn sz_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}
/// Internal primitive. Callers hold the account-wide operation guard.
pub(crate) fn write_raw(value: Option<&RawShell>) -> Result<(), Box<dyn Error>> {
    if let Some(v) = value {
        v.text()?;
    }
    unsafe {
        let mut key = HKEY::default();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wide(SUBKEY).as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if rc != ERROR_SUCCESS {
            return Err(format!("Shell write/open failed (Win32 {})", rc.0).into());
        }
        let name = wide(VALUE_NAME);
        let rc = write_value(key, &name, value);
        let _ = RegCloseKey(key);
        if rc != ERROR_SUCCESS {
            return Err(format!("Shell write failed (Win32 {})", rc.0).into());
        }
    }
    if current_raw()?.as_ref() != value {
        return Err("Shell write verification failed".into());
    }
    Ok(())
}

fn write_value(
    key: HKEY,
    name: &[u16],
    value: Option<&RawShell>,
) -> windows::Win32::Foundation::WIN32_ERROR {
    unsafe {
        match value {
            Some(v) => RegSetValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                REG_VALUE_TYPE(v.kind),
                Some(&v.data),
            ),
            None => {
                let rc = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
                if rc == ERROR_FILE_NOT_FOUND {
                    ERROR_SUCCESS
                } else {
                    rc
                }
            }
        }
    }
}

/// True when the per-user `Shell` value points at this executable — i.e. we are
/// running as the login shell (replacement mode), not as a companion.
///
/// An exact path match is the normal case. A *different copy of the same
/// product* also counts: the registered shell is routinely the hot-swapped
/// debug copy while the user runs a downloaded release build, and the intent
/// behind the value is "this product is my shell", not "this one file is my
/// shell". Treating another copy as a companion silently skips the tray host,
/// and the notification strip then has no owner and stays empty.
pub fn is_self_shell() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    match current_shell() {
        Ok(Some(configured)) => {
            configured
                .trim()
                .trim_matches('"')
                .eq_ignore_ascii_case(&exe.display().to_string())
                || same_product_build(&configured, &exe.display().to_string())
        }
        _ => false,
    }
}

/// Every build of this product ships as `minicon-winmgr.exe` (the hot-swapped
/// debug copy) or `minicon-winmgr-vX.Y.Z-....exe` (a release archive).
const PRODUCT_STEM: &str = "minicon-winmgr";

/// Whether both paths name a build of this product.
fn same_product_build(configured: &str, running: &str) -> bool {
    is_product_command(configured) && is_product_command(running)
}

/// The bare stem or the stem plus a `-version` / `.exe` suffix — never an
/// unrelated program that merely starts with the same characters.
pub(crate) fn is_product_command(path: &str) -> bool {
    let path = path.trim();
    let path = if let Some(rest) = path.strip_prefix('"') {
        let Some(end) = rest.find('"') else {
            return false;
        };
        if !rest[end + 1..].trim().is_empty() {
            return false;
        }
        &rest[..end]
    } else {
        path
    };
    std::env::current_exe()
        .ok()
        .is_some_and(|exe| path.eq_ignore_ascii_case(&exe.to_string_lossy()))
        || is_product_file_name(path)
}

fn is_product_file_name(path: &str) -> bool {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match name.strip_prefix(PRODUCT_STEM) {
        Some(rest) => {
            rest == ".exe"
                || (rest.starts_with("-v")
                    && rest.ends_with(".exe")
                    && rest.as_bytes().get(2).is_some_and(u8::is_ascii_digit))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_registry_round_trip_preserves_absence_empty_type_and_exact_bytes() {
        struct Fixture {
            key: HKEY,
            path: String,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = unsafe { RegCloseKey(self.key) };
                assert!(self.path.starts_with(r"Software\minicon-winmgr\test-"));
                let _ = unsafe {
                    windows::Win32::System::Registry::RegDeleteTreeW(
                        HKEY_CURRENT_USER,
                        PCWSTR(wide(&self.path).as_ptr()),
                    )
                };
            }
        }
        let path = format!(
            r"Software\minicon-winmgr\test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut key = HKEY::default();
        assert_eq!(
            unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(wide(&path).as_ptr()),
                    Some(0),
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_READ | KEY_SET_VALUE,
                    None,
                    &mut key,
                    None,
                )
            },
            ERROR_SUCCESS
        );
        let fixture = Fixture { key, path };
        let mut expanded = RawShell::string("  %SystemRoot%\\explorer.exe  ");
        expanded.kind = REG_EXPAND_SZ.0;
        let name = wide("Shell");
        for value in [
            None,
            Some(RawShell::string("")),
            Some(expanded),
            Some(RawShell::string("\"third-party.exe\" --arg  ")),
            None,
        ] {
            assert_eq!(
                write_value(fixture.key, &name, value.as_ref()),
                ERROR_SUCCESS
            );
            assert_eq!(query_raw(fixture.key, "Shell").unwrap(), value);
        }
    }

    const LIVE: &str = r"C:\Fixture\product\target\debug\live\minicon-winmgr.exe";
    const RELEASE: &str = r"D:\minicon-winmgr-v0.1.10-x86_64-windows.exe";

    #[test]
    fn another_copy_of_the_same_product_is_the_same_shell() {
        assert!(same_product_build(LIVE, RELEASE));
        assert!(same_product_build(RELEASE, LIVE));
        assert!(same_product_build("minicon-winmgr.exe", LIVE));
    }

    #[test]
    fn explorer_and_lookalike_names_are_not_this_product() {
        assert!(!same_product_build(r"C:\Windows\explorer.exe", LIVE));
        assert!(!same_product_build("explorer.exe", RELEASE));
        assert!(!same_product_build(LIVE, r"C:\Windows\explorer.exe"));
        // Same prefix characters, different file name: no separator after the
        // stem, so this is another program entirely.
        assert!(!same_product_build(r"C:\minicon-winmgrhelper.exe", LIVE));
        assert!(!same_product_build(LIVE, r"C:\minicon-winmgrhelper.exe"));
        // Empty or directory-only paths never name a build.
        assert!(!same_product_build("", LIVE));
        assert!(!same_product_build(r"D:\dev\", RELEASE));
    }
}
