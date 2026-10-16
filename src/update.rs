//! Manual update state machine. No startup I/O, networking, or resident worker.
use crate::{update_http as http, update_install as install};
use std::{cell::RefCell, path::PathBuf, sync::mpsc, time::Duration};

const API: &str = "https://api.github.com/repos/partnernetsoftware/minicon-winmgr/releases/latest";
pub(crate) const MAX_EXE: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Version(pub [u64; 3]);
impl Version {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let pieces: Vec<_> = value
            .strip_prefix('v')
            .unwrap_or(value)
            .split('.')
            .collect();
        if pieces.len() != 3 {
            return None;
        }
        let mut result = [0; 3];
        for (index, piece) in pieces.iter().enumerate() {
            if piece.is_empty()
                || piece.len() > 1 && piece.starts_with('0')
                || !piece.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            result[index] = piece.parse().ok()?;
        }
        Some(Self(result))
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Release {
    pub version: String,
    pub url: String,
    pub sums: String,
    pub name: String,
}
pub(crate) fn parse_release(bytes: &[u8], local: bool) -> Result<Release, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value["draft"] != false || value["prerelease"] != false {
        return Err("not a stable public release".into());
    }
    let tag = value["tag_name"].as_str().ok_or("missing release tag")?;
    Version::parse(tag).ok_or("unsupported release version")?;
    if !tag.starts_with('v') {
        return Err("unexpected release tag".into());
    }
    let name = format!("minicon-winmgr-{tag}-x86_64-windows.exe");
    let assets = value["assets"].as_array().ok_or("missing assets")?;
    let find = |wanted: &str| -> Result<String, String> {
        let matched: Vec<_> = assets
            .iter()
            .filter(|v| v["name"].as_str() == Some(wanted))
            .collect();
        if matched.len() != 1 {
            return Err(format!("missing or ambiguous asset {wanted}"));
        }
        let url = matched[0]["browser_download_url"]
            .as_str()
            .ok_or("asset URL missing")?;
        let expected = format!(
            "https://github.com/partnernetsoftware/minicon-winmgr/releases/download/{tag}/{wanted}"
        );
        if !(url == expected || local && http::loopback(url)) {
            return Err("asset URL is outside this repository".into());
        }
        http::Url::parse(url)?;
        Ok(url.into())
    };
    Ok(Release {
        version: tag.trim_start_matches('v').into(),
        url: find(&name)?,
        sums: find("SHA256SUMS.txt")?,
        name,
    })
}
pub(crate) fn checksum(table: &[u8], name: &str) -> Result<String, String> {
    let text = std::str::from_utf8(table).map_err(|e| e.to_string())?;
    let mut result = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2 || fields[1].trim_start_matches('*') != name {
            continue;
        }
        if result.is_some()
            || fields[0].len() != 64
            || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid or duplicate checksum".into());
        }
        result = Some(fields[0].to_ascii_lowercase());
    }
    result.ok_or_else(|| "asset checksum missing".into())
}
pub(crate) fn verify(bytes: &[u8], expected: &str) -> Result<(), String> {
    if bytes.len() > MAX_EXE || bytes.len() < 256 || &bytes[..2] != b"MZ" {
        return Err("not a Windows executable".into());
    }
    let offset = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as usize;
    let header = bytes
        .get(offset..offset.saturating_add(26))
        .ok_or("invalid PE offset")?;
    if &header[..4] != b"PE\0\0" || header[4..6] != [0x64, 0x86] || header[24..26] != [0x0b, 0x02] {
        return Err("not an x64 PE executable".into());
    }
    if http::sha256(bytes)? != expected {
        return Err("SHA-256 mismatch".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Ready,
    Restarting,
    Failed,
    Recovered,
}
#[derive(Clone)]
pub(crate) struct View {
    pub phase: Phase,
    pub version: Option<String>,
    pub bytes: usize,
    pub error: Option<String>,
}
enum Event {
    Checked(Release),
    Staged(PathBuf),
    Progress(usize),
    Quit,
    Failed(String),
}
struct State {
    view: View,
    release: Option<Release>,
    staged: Option<PathBuf>,
    receiver: Option<mpsc::Receiver<Event>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            view: View {
                phase: if std::env::var("MINICON_WINMGR_UPDATE_RECOVERED").as_deref() == Ok("1") {
                    Phase::Recovered
                } else {
                    Phase::Idle
                },
                version: None,
                bytes: 0,
                error: None,
            },
            release: None,
            staged: None,
            receiver: None,
        }
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
pub(crate) fn view() -> View {
    STATE.with(|s| s.borrow().view.clone())
}
fn begin(
    phase: Phase,
    work: impl FnOnce(mpsc::SyncSender<Event>) -> Result<Event, String> + Send + 'static,
) {
    let (sender, receiver) = mpsc::sync_channel(4);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.view.phase = phase;
        s.view.error = None;
        s.view.bytes = 0;
        s.receiver = Some(receiver);
    });
    let result = std::thread::Builder::new()
        .name("manual-update".into())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(sender.clone())))
                    .unwrap_or_else(|_| Err("update worker panicked".into()));
            let _ = sender.send(result.unwrap_or_else(Event::Failed));
        });
    if let Err(error) = result {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.receiver = None;
            s.view.phase = Phase::Failed;
            s.view.error = Some(error.to_string());
        });
    }
}
pub(crate) fn check() {
    if matches!(
        view().phase,
        Phase::Checking | Phase::Downloading | Phase::Restarting | Phase::Ready
    ) {
        return;
    }
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.release = None;
        state.view.version = None;
    });
    begin(Phase::Checking, |_| {
        let endpoint = match std::env::var("MINICON_WINMGR_UPDATE_API") {
            Ok(v) if http::loopback(&v) => v,
            Ok(_) => return Err("only a loopback test API override is supported".into()),
            Err(_) => API.into(),
        };
        let body = http::get(&endpoint, 512 * 1024, Duration::from_secs(25), |_| {})?;
        Ok(Event::Checked(parse_release(
            &body,
            http::loopback(&endpoint),
        )?))
    });
}
pub(crate) fn can_download() -> bool {
    STATE.with(|s| {
        let s = s.borrow();
        s.release.is_some() && matches!(s.view.phase, Phase::Available | Phase::Failed)
    })
}
pub(crate) fn download() {
    if !matches!(view().phase, Phase::Available | Phase::Failed) {
        return;
    }
    let Some(release) = STATE.with(|s| s.borrow().release.clone()) else {
        return;
    };
    begin(Phase::Downloading, move |sender| {
        let sums = http::get(&release.sums, 64 * 1024, Duration::from_secs(25), |_| {})?;
        let hash = checksum(&sums, &release.name)?;
        let bytes = http::get(&release.url, MAX_EXE, Duration::from_secs(300), |count| {
            let _ = sender.try_send(Event::Progress(count));
        })?;
        verify(&bytes, &hash)?;
        Ok(Event::Staged(install::prepare(
            &bytes,
            &hash,
            &release.version,
        )?))
    });
}
pub(crate) fn restart() {
    if view().phase != Phase::Ready || crate::settings_store::status().dirty() {
        return;
    }
    let Some(plan) = STATE.with(|s| s.borrow().staged.clone()) else {
        return;
    };
    begin(Phase::Restarting, move |_| {
        install::start_helper(&plan)?;
        Ok(Event::Quit)
    });
}
/// Called by the existing settings timer. It only drains in-memory messages.
pub(crate) fn poll() {
    let mut quit = false;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        while let Some(receiver) = s.receiver.as_ref() {
            match receiver.try_recv() {
                Ok(Event::Progress(bytes)) => s.view.bytes = bytes,
                Ok(event) => {
                    s.receiver = None;
                    match event {
                        Event::Checked(release) => {
                            let newer = Version::parse(&release.version)
                                > Version::parse(env!("CARGO_PKG_VERSION"));
                            s.view.phase = if newer {
                                Phase::Available
                            } else {
                                Phase::Current
                            };
                            s.view.version = Some(release.version.clone());
                            s.release = newer.then_some(release);
                        }
                        Event::Staged(path) => {
                            s.staged = Some(path);
                            s.view.phase = Phase::Ready;
                        }
                        Event::Quit => quit = true,
                        Event::Failed(error) => {
                            tracing::warn!(%error,"manual update failed");
                            s.view.error = Some(error);
                            s.view.phase = if s.staged.is_some() {
                                Phase::Ready
                            } else {
                                Phase::Failed
                            };
                        }
                        Event::Progress(_) => unreachable!(),
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    s.receiver = None;
                    s.view.phase = Phase::Failed;
                    s.view.error = Some("update worker disconnected".into());
                    break;
                }
            }
        }
    });
    if quit {
        if crate::settings_store::status().dirty() {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if let Some(path) = s.staged.clone() {
                    let _ = std::thread::Builder::new()
                        .name("update-cancel".into())
                        .spawn(move || {
                            let _ = std::fs::write(path.with_file_name("abort"), b"cancel");
                        });
                }
                s.view.phase = Phase::Ready;
                s.view.error = Some("settings changed during handoff".into());
            });
        } else if let Some(hwnd) = crate::hub::shell_hwnd() {
            let _ = unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(hwnd),
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                )
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_numeric_versions_and_exact_checksum() {
        assert!(Version::parse("0.1.10") > Version::parse("v0.1.9"));
        for bad in ["1.2", "1.2.3-beta", "1.2.03", "1.2.3/", "-1.2.3"] {
            assert!(Version::parse(bad).is_none());
        }
        let hash = "a".repeat(64);
        assert_eq!(
            checksum(format!("{hash} *app.exe\n").as_bytes(), "app.exe").unwrap(),
            hash
        );
        assert!(
            checksum(
                format!("{hash} app.exe\n{hash} app.exe").as_bytes(),
                "app.exe"
            )
            .is_err()
        );
        assert!(checksum(format!("{hash} other.exe").as_bytes(), "app.exe").is_err());
    }
    #[test]
    fn release_rejects_previews_missing_assets_and_foreign_urls() {
        let exe = "minicon-winmgr-v0.1.10-x86_64-windows.exe";
        let mut value = serde_json::json!({"tag_name":"v0.1.10","draft":false,"prerelease":false,"assets":[{"name":exe,"browser_download_url":format!("https://github.com/partnernetsoftware/minicon-winmgr/releases/download/v0.1.10/{exe}")},{"name":"SHA256SUMS.txt","browser_download_url":"https://github.com/partnernetsoftware/minicon-winmgr/releases/download/v0.1.10/SHA256SUMS.txt"}]});
        assert!(parse_release(&serde_json::to_vec(&value).unwrap(), false).is_ok());
        value["prerelease"] = true.into();
        assert!(parse_release(&serde_json::to_vec(&value).unwrap(), false).is_err());
        value["prerelease"] = false.into();
        value["assets"][0]["browser_download_url"] = "https://example.com/app.exe".into();
        assert!(parse_release(&serde_json::to_vec(&value).unwrap(), false).is_err());
    }
    #[test]
    fn rejects_hash_corruption_and_wrong_machine() {
        let mut bytes = vec![0; 256];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&[0x64, 0x86]);
        bytes[88..90].copy_from_slice(&[0x0b, 0x02]);
        let hash = http::sha256(&bytes).unwrap();
        assert!(verify(&bytes, &hash).is_ok());
        bytes[200] = 1;
        assert!(verify(&bytes, &hash).is_err());
        bytes[68] = 0;
        let hash = http::sha256(&bytes).unwrap();
        assert!(verify(&bytes, &hash).is_err());
    }
}
