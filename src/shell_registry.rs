//! M5 — optional, reversible Explorer-shell replacement.
//!
//! SAFETY CONTRACT (see `docs/prd.md` §8 risk "替换破坏性" and
//! `docs/prd_3_1.md`):
//! - This module *reads* the current per-user shell value freely.
//! - It only ever *writes* when [`replace`] / [`restore`] are called explicitly,
//!   and the CLI layer additionally requires a second human confirmation token.
//! - The default companion run never calls the write paths, so the registry is
//!   left untouched.
//!
//! The relevant value is `HKCU\Software\Microsoft\Windows NT\CurrentVersion\Winlogon\Shell`
//! (per-user, so it is reversible without machine-wide impact).

use std::error::Error;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::PCWSTR;

use crate::win_util::wide;

const SUBKEY: &str = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
const VALUE_NAME: &str = "Shell";

/// Machine-wide Winlogon key, home of `AutoRestartShell`.
const MACHINE_SUBKEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
const AUTO_RESTART_VALUE: &str = "AutoRestartShell";

/// Reads the current per-user `Shell` value. `Ok(None)` means the value is not
/// set (Windows falls back to `explorer.exe`).
pub fn current_shell() -> Result<Option<String>, Box<dyn Error>> {
    let subkey = wide(SUBKEY);
    let value = wide(VALUE_NAME);

    // SAFETY: buffers outlive their calls; handles are closed before returning.
    unsafe {
        let mut hkey = HKEY::default();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        );
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(format!("RegOpenKeyExW failed (Win32 error {})", rc.0).into());
        }

        let result = query_string(hkey, &value);
        let _ = RegCloseKey(hkey);
        result
    }
}

/// Sets the per-user `Shell` value to `shell_path`.
///
/// Callers MUST have obtained explicit human confirmation; the CLI gate in
/// `main` enforces this before reaching here.
pub fn replace(shell_path: &str) -> Result<(), Box<dyn Error>> {
    write_string(Some(shell_path))
}

/// Restores the `Shell` value: `Some(v)` to set a specific value, or `None` to
/// delete the value (reverting to the Windows default of `explorer.exe`).
pub fn restore(previous: Option<&str>) -> Result<(), Box<dyn Error>> {
    match previous {
        Some(v) => write_string(Some(v)),
        None => write_string(None),
    }
}

/// Encodes a UTF-16, null-terminated REG_SZ payload.
fn sz_bytes(s: &str) -> Vec<u8> {
    let mut units: Vec<u16> = s.encode_utf16().collect();
    units.push(0);
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

unsafe fn query_string(hkey: HKEY, value: &[u16]) -> Result<Option<String>, Box<dyn Error>> {
    let mut ty = REG_VALUE_TYPE::default();
    let mut len = 0u32;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(value.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut len),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if rc != ERROR_SUCCESS {
        return Err(format!("RegQueryValueExW(size) failed (Win32 error {})", rc.0).into());
    }

    let mut buf = vec![0u8; len as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(value.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!("RegQueryValueExW(data) failed (Win32 error {})", rc.0).into());
    }

    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string();
    Ok(Some(s))
}

fn write_string(value: Option<&str>) -> Result<(), Box<dyn Error>> {
    let subkey = wide(SUBKEY);
    let name = wide(VALUE_NAME);

    // SAFETY: buffers outlive their calls; the key is closed before returning.
    unsafe {
        let mut hkey = HKEY::default();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        );
        if rc != ERROR_SUCCESS {
            return Err(format!("RegOpenKeyExW failed (Win32 error {})", rc.0).into());
        }

        let rc = match value {
            Some(v) => {
                let data = sz_bytes(v);
                RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data))
            }
            // `None` means "restore the Windows default", so the value must be
            // *deleted* — writing an empty REG_SZ would leave a broken value.
            None => {
                let deleted = RegDeleteValueW(hkey, PCWSTR(name.as_ptr()));
                if deleted == ERROR_FILE_NOT_FOUND {
                    ERROR_SUCCESS
                } else {
                    deleted
                }
            }
        };
        let _ = RegCloseKey(hkey);

        if rc != ERROR_SUCCESS {
            return Err(format!("RegSetValueExW failed (Win32 error {})", rc.0).into());
        }
        Ok(())
    }
}

/// Enables or disables the machine-wide shell auto-restart
/// (`HKLM\...\Winlogon\AutoRestartShell`).
///
/// With auto-restart left on, Windows re-launches a shell shortly after the
/// running one is stopped, which would immediately undo a takeover. The value
/// lives under `HKLM`, so this needs elevation; callers treat failure as a
/// warning rather than a hard error.
pub fn set_auto_restart(enabled: bool) -> Result<(), Box<dyn Error>> {
    let subkey = wide(MACHINE_SUBKEY);
    let name = wide(AUTO_RESTART_VALUE);
    let data = u32::from(enabled).to_le_bytes();

    // SAFETY: buffers outlive their calls; the key is closed before returning.
    unsafe {
        let mut hkey = HKEY::default();
        let rc = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            KEY_SET_VALUE,
            &mut hkey,
        );
        if rc != ERROR_SUCCESS {
            return Err(format!("RegOpenKeyExW(HKLM) failed (Win32 error {})", rc.0).into());
        }
        let rc = RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&data));
        let _ = RegCloseKey(hkey);
        if rc != ERROR_SUCCESS {
            return Err(format!("RegSetValueExW failed (Win32 error {})", rc.0).into());
        }
        Ok(())
    }
}

/// True when the per-user `Shell` value points at this executable — i.e. we are
/// running as the login shell (replacement mode), not as a companion.
pub fn is_self_shell() -> bool {
    match current_shell() {
        Ok(Some(configured)) => std::env::current_exe()
            .map(|exe| configured.eq_ignore_ascii_case(&exe.display().to_string()))
            .unwrap_or(false),
        _ => false,
    }
}
