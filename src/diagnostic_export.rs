//! One lazily-created export worker; slow destinations cannot multiply workers.
use crate::{diagnostics::Snapshot, win_util::lock_or_recover};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
};
use windows::{
    Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    core::PCWSTR,
};

struct Job {
    path: PathBuf,
    snapshot: Snapshot,
}
#[derive(Default)]
struct State {
    pending: Option<Job>,
    busy: bool,
    result: Option<bool>,
    stop: bool,
}
struct Exporter {
    shared: Arc<(Mutex<State>, Condvar)>,
}
impl Drop for Exporter {
    fn drop(&mut self) {
        self.stop();
    }
}
static EXPORTER: OnceLock<Option<Exporter>> = OnceLock::new();
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub busy: bool,
    pub result: Option<bool>,
}
impl Exporter {
    fn start(
        write: impl Fn(&Path, &Snapshot, &dyn Fn() -> bool) -> bool + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let state = shared.clone();
        std::thread::Builder::new()
            .name("diagnostic-export".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut guard = lock_or_recover(&state.0);
                        while !guard.stop && guard.pending.is_none() {
                            guard = state.1.wait(guard).unwrap_or_else(|p| p.into_inner());
                        }
                        if guard.stop {
                            return;
                        }
                        guard.pending.take().unwrap()
                    };
                    let cancel = || lock_or_recover(&state.0).stop;
                    let success = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        write(&job.path, &job.snapshot, &cancel)
                    }))
                    .unwrap_or(false);
                    let mut guard = lock_or_recover(&state.0);
                    guard.busy = false;
                    guard.result = Some(success);
                }
            })?;
        Ok(Self { shared })
    }
    fn submit(&self, path: PathBuf, snapshot: Snapshot) -> bool {
        let mut guard = lock_or_recover(&self.shared.0);
        if guard.busy || guard.stop {
            return false;
        }
        guard.busy = true;
        guard.result = None;
        guard.pending = Some(Job { path, snapshot });
        self.shared.1.notify_one();
        true
    }
    fn status(&self) -> Status {
        let guard = lock_or_recover(&self.shared.0);
        Status {
            busy: guard.busy,
            result: guard.result,
        }
    }
    fn stop(&self) {
        let mut guard = lock_or_recover(&self.shared.0);
        guard.stop = true;
        guard.pending = None;
        self.shared.1.notify_one();
    }
}
fn save(path: &Path, snapshot: &Snapshot, cancel: &dyn Fn() -> bool) -> bool {
    save_with(path, &snapshot.markdown(), cancel, || Ok(())).is_ok()
}
fn save_with(
    path: &Path,
    text: &str,
    cancel: &dyn Fn() -> bool,
    before_replace: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    if text.len() > 16 * 1024 || cancel() {
        return Err(std::io::Error::other("export cancelled or too large"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing export directory"))?;
    let temporary = parent.join(format!(
        ".winmgr-diagnostic-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        before_replace()?;
        if cancel() {
            return Err(std::io::Error::other("export cancelled"));
        }
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(std::io::Error::other)
    })();
    let _ = fs::remove_file(temporary);
    result
}
pub(crate) fn submit(path: PathBuf, snapshot: Snapshot) -> bool {
    EXPORTER
        .get_or_init(|| Exporter::start(save).ok())
        .as_ref()
        .is_some_and(|worker| worker.submit(path, snapshot))
}
pub(crate) fn status() -> Status {
    EXPORTER
        .get()
        .and_then(Option::as_ref)
        .map_or_else(Status::default, Exporter::status)
}
pub(crate) fn shutdown() {
    if let Some(Some(worker)) = EXPORTER.get() {
        worker.stop();
    }
}
#[cfg(test)]
pub(crate) fn initialized() -> bool {
    EXPORTER.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        crate::diagnostics::Recording::new(
            crate::diagnostics::Scenario::Idle,
            crate::diagnostics::Timestamp::now(),
            crate::metrics::Observation::default,
        )
        .snapshot()
    }
    #[test]
    fn slow_export_refuses_more_jobs_and_never_holds_the_ui_status_lock() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = Exporter::start(move |_, _, _| {
            tx.send(()).unwrap();
            release_rx.recv().unwrap();
            true
        })
        .unwrap();
        assert!(worker.submit(PathBuf::from("unused"), snapshot()));
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        for _ in 0..1000 {
            assert!(!worker.submit(PathBuf::from("unused"), snapshot()));
            assert!(worker.status().busy);
        }
        worker.stop();
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while worker.status().busy {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!worker.submit(PathBuf::from("unused"), snapshot()));
    }
    #[test]
    fn replace_and_cancellation_failures_preserve_the_original_and_clean_owned_temporary() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = std::env::temp_dir().join(format!(
            "winmgr-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("summary.md");
        fs::write(&path, "keep").unwrap();
        assert!(
            save_with(&path, "new", &|| false, || Err(std::io::Error::other(
                "injected"
            )))
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(save_with(&path, "new", &|| false, || Ok(())).is_err());
        drop(lock);
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
        assert!(save_with(&path, "new", &|| true, || Ok(())).is_err());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        save_with(&path, &snapshot().markdown(), &|| false, || Ok(())).unwrap();
        let data = fs::read_to_string(&path).unwrap();
        assert!(data.contains("diagnostic summary"));
        assert!(!data.contains(&directory.to_string_lossy().to_string()));
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
