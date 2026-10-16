//! Manual update state machine. No startup I/O, networking, or resident worker.
use crate::{update_http as http, update_install as install};
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

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
#[cfg(test)]
pub(crate) fn verify(bytes: &[u8], expected: &str) -> Result<(), String> {
    crate::update_stream::verify_reader(std::io::Cursor::new(bytes), expected)
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
    Quit,
    Failed(String),
}
struct State {
    view: View,
    release: Option<Release>,
    staged: Option<PathBuf>,
    receiver: Option<mpsc::Receiver<Event>>,
    cancel: Option<crate::update_cancel::Token>,
    progress: Option<Arc<AtomicUsize>>,
    closing: bool,
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
            cancel: None,
            progress: None,
            closing: false,
        }
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
pub(crate) fn view() -> View {
    STATE.with(|s| s.borrow().view.clone())
}
fn begin(
    phase: Phase,
    work: impl FnOnce(Arc<AtomicUsize>, crate::update_cancel::Token) -> Result<Event, String>
    + Send
    + 'static,
) {
    if STATE.with(|state| {
        let state = state.borrow();
        state.closing || state.receiver.is_some()
    }) {
        return;
    }
    let cancel = crate::update_cancel::Token::default();
    let progress = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::sync_channel(1);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.view.phase = phase;
        s.view.error = None;
        s.view.bytes = 0;
        s.receiver = Some(receiver);
        s.cancel = Some(cancel.clone());
        s.progress = Some(progress.clone());
    });
    let result = std::thread::Builder::new()
        .name("manual-update".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(progress, cancel.clone())
            }))
            .unwrap_or_else(|_| Err("update worker panicked".into()));
            if !cancel.cancelled() {
                let _ = sender.try_send(result.unwrap_or_else(Event::Failed));
            }
            // Progress is a coalesced counter, so the single terminal result
            // has its own bounded slot and never waits for a hidden Settings UI.
            drop(cancel);
        });
    if let Err(error) = result {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.receiver = None;
            s.cancel = None;
            s.progress = None;
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
    begin(Phase::Checking, |_, cancel| {
        let endpoint = match std::env::var("MINICON_WINMGR_UPDATE_API") {
            Ok(v) if http::loopback(&v) => v,
            Ok(_) => return Err("only a loopback test API override is supported".into()),
            Err(_) => API.into(),
        };
        let body = http::get_cancellable(
            &endpoint,
            512 * 1024,
            Duration::from_secs(25),
            |_| {},
            &cancel,
        )?;
        cancel.check()?;
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
    begin(Phase::Downloading, move |progress, cancel| {
        let sums = http::get_cancellable(
            &release.sums,
            64 * 1024,
            Duration::from_secs(25),
            |_| {},
            &cancel,
        )?;
        let hash = checksum(&sums, &release.name)?;
        Ok(Event::Staged(install::prepare_cancellable(
            &hash,
            &release.version,
            |file| {
                use std::io::Write;
                http::get_into_cancellable(
                    &release.url,
                    MAX_EXE,
                    Duration::from_secs(300),
                    |count| {
                        progress.store(count, Ordering::Relaxed);
                    },
                    |chunk| file.write_all(chunk).map_err(|e| e.to_string()),
                    &cancel,
                )?;
                Ok(())
            },
            || cancel.check(),
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
    begin(Phase::Restarting, move |_, _| {
        install::start_helper(&plan)?;
        Ok(Event::Quit)
    });
}
/// Application exit only: hiding Settings does not cancel an explicit download.
/// No UI join or file I/O here. Dropping the receiver releases the result slot.
pub(crate) fn shutdown() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.closing = true;
        if let Some(cancel) = state.cancel.take() {
            cancel.cancel();
        }
        state.receiver = None;
        state.progress = None;
    });
}

/// Called by the existing settings timer. It only drains in-memory messages.
pub(crate) fn poll() {
    let mut quit = false;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(progress) = &s.progress {
            s.view.bytes = progress.load(Ordering::Relaxed);
        }
        if let Some(receiver) = s.receiver.as_ref() {
            match receiver.try_recv() {
                Ok(event) => {
                    if let Some(progress) = s.progress.take() {
                        s.view.bytes = progress.load(Ordering::Relaxed);
                    }
                    s.receiver = None;
                    s.cancel = None;
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
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    s.receiver = None;
                    s.cancel = None;
                    s.progress = None;
                    s.view.phase = Phase::Failed;
                    s.view.error = Some("update worker disconnected".into());
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
    fn coalesced_progress_and_terminal_result_release_worker_before_hidden_ui_polls() {
        STATE.with(|state| *state.borrow_mut() = State::default());
        let (entered, started) = mpsc::sync_channel(1);
        begin(Phase::Checking, move |progress, cancel| {
            for bytes in 0..100000 {
                progress.store(bytes, Ordering::Relaxed);
            }
            entered.send(()).unwrap();
            cancel.check()?;
            Err("synthetic completed operation".into())
        });
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let lifetime = STATE.with(|state| state.borrow().cancel.as_ref().unwrap().lifetime());
        let start = std::time::Instant::now();
        // One reference belongs to State. The worker's reference must disappear
        // even though nobody has drained progress or the terminal result.
        while lifetime.strong_count() > 1 && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            lifetime.strong_count(),
            1,
            "worker waited for the hidden UI"
        );
        poll();
        assert_eq!(view().bytes, 99999);
        assert_eq!(
            view().error.as_deref(),
            Some("synthetic completed operation")
        );
        let start = std::time::Instant::now();
        shutdown();
        shutdown();
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(lifetime.upgrade().is_none());
        begin(Phase::Checking, |_, _| {
            panic!("must not start after shutdown")
        });
        assert!(STATE.with(|state| state.borrow().cancel.is_none()));
        STATE.with(|state| *state.borrow_mut() = State::default());
    }

    #[test]
    fn shutdown_interrupts_the_actual_network_worker_without_waiting_for_the_server() {
        use std::io::Read;
        STATE.with(|state| *state.borrow_mut() = State::default());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/shutdown", listener.local_addr().unwrap());
        let (connected, started) = mpsc::sync_channel(1);
        let (release, stop) = mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = [0; 8192];
            assert!(stream.read(&mut bytes).unwrap() > 0);
            connected.send(()).unwrap();
            let _ = stop.recv_timeout(Duration::from_secs(4));
        });
        begin(Phase::Checking, move |_, cancel| {
            http::get_cancellable(&url, 100, Duration::from_secs(30), |_| {}, &cancel)?;
            Err("silent server should not complete".into())
        });
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let lifetime = STATE.with(|state| state.borrow().cancel.as_ref().unwrap().lifetime());
        let started = std::time::Instant::now();
        shutdown();
        let ui_elapsed = started.elapsed();
        while lifetime.upgrade().is_some() && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let exited_before_peer = lifetime.upgrade().is_none();
        let _ = release.send(());
        server.join().unwrap();
        assert!(ui_elapsed < Duration::from_millis(100));
        assert!(
            exited_before_peer,
            "network worker survived application shutdown cancellation"
        );
        assert!(STATE.with(|state| state.borrow().receiver.is_none()));
        STATE.with(|state| *state.borrow_mut() = State::default());
    }

    #[test]
    fn worker_panic_and_coalesced_progress_fail_then_allow_a_new_check() {
        STATE.with(|state| *state.borrow_mut() = State::default());
        let (entered, started) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel(1);
        begin(Phase::Checking, move |progress, _| {
            for bytes in 0..1000 {
                progress.store(bytes, Ordering::Relaxed);
            }
            entered.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
            panic!("synthetic update worker panic");
        });
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let tick = std::time::Instant::now();
        for _ in 0..1000 {
            assert_eq!(view().phase, Phase::Checking);
        }
        assert!(tick.elapsed() < Duration::from_millis(500));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now();
        while view().phase == Phase::Checking && deadline.elapsed() < Duration::from_secs(3) {
            poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(view().phase, Phase::Failed);
        assert_eq!(view().error.as_deref(), Some("update worker panicked"));
        begin(Phase::Checking, |_, _| {
            Ok(Event::Checked(Release {
                version: env!("CARGO_PKG_VERSION").into(),
                url: String::new(),
                sums: String::new(),
                name: String::new(),
            }))
        });
        let deadline = std::time::Instant::now();
        while view().phase == Phase::Checking && deadline.elapsed() < Duration::from_secs(3) {
            poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(view().phase, Phase::Current);
        assert!(view().error.is_none());
        STATE.with(|state| *state.borrow_mut() = State::default());
    }

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
