//! Coalescing, bounded settings writer. The worker owns only file IO and data;
//! window code polls snapshots on its message-thread timer.
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;

#[derive(Clone, Debug)]
struct Entry {
    value: String,
    generation: u64,
    queued: bool,
    error: Option<String>,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<&'static str, Entry>,
    generation: u64,
    available: bool,
    saved: bool,
    stop: bool,
}
struct Writer {
    state: Arc<(Mutex<State>, Condvar)>,
}
static WRITER: OnceLock<Writer> = OnceLock::new();
#[derive(Clone, Debug, Default)]
pub(crate) struct Status {
    pub pending: usize,
    pub failed: usize,
    pub error: Option<String>,
    pub saved: bool,
}
impl Status {
    pub fn dirty(&self) -> bool {
        self.pending + self.failed > 0
    }
}

fn writer() -> &'static Writer {
    WRITER.get_or_init(|| {
        let state = Arc::new((
            Mutex::new(State {
                available: true,
                ..State::default()
            }),
            Condvar::new(),
        ));
        start_worker(&state);
        Writer { state }
    })
}

#[cfg(test)]
pub(crate) fn install_delayed_test_writer() -> std::sync::mpsc::Receiver<bool> {
    let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
    let (sender, receiver) = std::sync::mpsc::channel();
    spawn_worker(&state, move |_, _| {
        sender.send(true).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        sender.send(false).unwrap();
        Ok(())
    });
    assert!(WRITER.set(Writer { state }).is_ok());
    receiver
}

fn start_worker(state: &Arc<(Mutex<State>, Condvar)>) {
    let path = crate::config::settings_path();
    let legacy = crate::config::legacy_settings_path();
    spawn_worker(state, move |key, value| {
        let path = path
            .as_deref()
            .ok_or_else(|| crate::i18n::text("无法确定设置文件路径").to_string())?;
        persist_seeded(path, legacy.as_deref(), key, value, |_| Ok(()))
    });
}
fn spawn_worker(
    state: &Arc<(Mutex<State>, Condvar)>,
    write: impl Fn(&str, &str) -> Result<(), String> + Send + 'static,
) {
    let shared = state.clone();
    crate::win_util::lock_or_recover(&state.0).available = true;
    if std::thread::Builder::new()
        .name("settings-save".into())
        .spawn(move || {
            loop {
                let (lock, signal) = &*shared;
                let (key, request) = {
                    let mut state = crate::win_util::lock_or_recover(lock);
                    while !state.stop && !state.entries.values().any(|e| e.queued) {
                        state = signal.wait(state).unwrap_or_else(|p| p.into_inner());
                    }
                    if state.stop {
                        return;
                    }
                    let (&key, entry) = state.entries.iter_mut().find(|(_, e)| e.queued).unwrap();
                    entry.queued = false;
                    (key, entry.clone())
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    write(key, &request.value)
                }))
                .unwrap_or_else(|_| Err(crate::i18n::text("保存线程处理失败，请重试").into()));
                let mut state = crate::win_util::lock_or_recover(lock);
                finish(&mut state, key, request.generation, result);
            }
        })
        .is_err()
    {
        crate::win_util::lock_or_recover(&state.0).available = false;
    }
}

fn finish(state: &mut State, key: &'static str, generation: u64, result: Result<(), String>) {
    if state
        .entries
        .get(key)
        .is_none_or(|e| e.generation != generation)
    {
        return;
    }
    match result {
        Ok(()) => {
            state.entries.remove(key);
            state.saved = true;
        }
        Err(error) => {
            state.entries.get_mut(key).unwrap().error = Some(error);
        }
    }
}

pub(crate) fn enqueue(key: &'static str, value: &str) {
    // Callers can only submit the supported UI-owned keys: memory stays bounded even
    // while IO is blocked or the user repeatedly changes the same setting.
    assert!(
        [
            crate::config::keys::TASKBAR_SHOW,
            crate::config::keys::TASKBAR_TRAY,
            crate::config::keys::TASKBAR_GROUP,
            crate::config::keys::TASKBAR_GROUP_BY,
            crate::config::keys::HOVER_PREVIEW,
            crate::config::keys::LANGUAGE,
            crate::config::keys::THEME_MODE,
            crate::config::keys::SIDE_WIDTH,
            crate::config::keys::TASKBAR_POSITION
        ]
        .contains(&key)
    );
    let writer = writer();
    let mut state = crate::win_util::lock_or_recover(&writer.state.0);
    state.generation += 1;
    let entry = Entry {
        value: value.into(),
        generation: state.generation,
        queued: state.available,
        error: (!state.available).then(|| crate::i18n::text("保存线程不可用").into()),
    };
    state.entries.insert(key, entry);
    writer.state.1.notify_one();
}

pub(crate) fn retry() {
    let writer = writer();
    let mut state = crate::win_util::lock_or_recover(&writer.state.0);
    if !state.available {
        drop(state);
        start_worker(&writer.state);
        state = crate::win_util::lock_or_recover(&writer.state.0);
        if !state.available {
            return;
        }
    }
    for entry in state.entries.values_mut() {
        entry.error = None;
        entry.queued = true;
    }
    writer.state.1.notify_one();
}

pub(crate) fn status() -> Status {
    let state = crate::win_util::lock_or_recover(&writer().state.0);
    Status {
        pending: state.entries.values().filter(|e| e.error.is_none()).count(),
        failed: state.entries.values().filter(|e| e.error.is_some()).count(),
        error: state.entries.values().find_map(|e| e.error.clone()),
        saved: state.saved,
    }
}

pub(crate) fn read_existing(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    const LIMIT: u64 = 1024 * 1024;
    match std::fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(LIMIT + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Settings file exceeds 1 MiB",
                ));
            }
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
pub(crate) fn persist(path: &Path, key: &str, value: &str) -> Result<(), String> {
    persist_with(path, key, value, |_| Ok(()))
}

#[cfg(test)]
fn persist_with(
    path: &Path,
    key: &str,
    value: &str,
    checkpoint: impl Fn(&str) -> io::Result<()>,
) -> Result<(), String> {
    persist_seeded(path, None, key, value, checkpoint)
}

fn persist_seeded(
    path: &Path,
    legacy: Option<&Path>,
    key: &str,
    value: &str,
    checkpoint: impl Fn(&str) -> io::Result<()>,
) -> Result<(), String> {
    let mut stage = "读取";
    let mut action = || -> io::Result<()> {
        let original = read_existing(path)?;
        let seed = if original.is_none() {
            legacy.map_or(Ok(None), read_existing)?
        } else {
            None
        };
        let text = std::str::from_utf8(original.as_deref().or(seed.as_deref()).unwrap_or_default())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let updated = crate::config::upsert(text, key, value);
        if updated.len() > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Settings file exceeds 1 MiB",
            ));
        }
        checkpoint("read")?;
        stage = "备份";
        if let Some(bytes) = &original {
            let backup = path.as_os_str().to_os_string();
            let mut backup = backup;
            backup.push(".bak");
            std::fs::write(backup, bytes)?;
        }
        checkpoint("backup")?;
        stage = "写入临时文件";
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temporary = path.as_os_str().to_os_string();
        temporary.push(format!(".{}.{}.tmp", std::process::id(), serial));
        let temporary = std::path::PathBuf::from(temporary);
        // The operation owns this unique temporary path; cleanup on every exit.
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let result = (|| {
            checkpoint("write")?;
            file.write_all(updated.as_bytes())?;
            file.sync_all()?;
            drop(file);
            checkpoint("replace")?;
            stage = "检查外部修改并替换";
            if read_existing(path)? != original {
                return Err(io::Error::other(crate::i18n::text(
                    "文件已被外部修改，请重试",
                )));
            }
            let from = crate::win_util::wide(&temporary.to_string_lossy());
            let to = crate::win_util::wide(&path.to_string_lossy());
            let flags = MOVEFILE_WRITE_THROUGH
                | if original.is_some() {
                    MOVEFILE_REPLACE_EXISTING
                } else {
                    Default::default()
                };
            unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }
                .map_err(io::Error::other)
        })();
        let _ = std::fs::remove_file(&temporary);
        result
    };
    action().map_err(|e| {
        crate::ui_format!(
            "设置{}失败：{e}\n{}",
            "Settings {} failed: {e}\n{}",
            crate::i18n::text(stage),
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_home_save_migrates_all_old_preferences_without_modifying_legacy() {
        let dir = Temp::new();
        let path = dir.0.join("winmgr.cnf");
        let legacy = dir.0.join("legacy.txt");
        let old = "# retained\nui.language=zh\nappearance.mode=dark\ntaskbar.side_width=248\n";
        std::fs::write(&legacy, old).unwrap();
        persist_seeded(
            &path,
            Some(&legacy),
            "taskbar.position",
            "right",
            |_| Ok(()),
        )
        .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let restored = crate::config::parse(&saved);
        assert_eq!(restored.language, crate::i18n::Language::Zh);
        assert_eq!(restored.theme_mode, crate::config::ThemeMode::Dark);
        assert_eq!(restored.side_width, 248);
        assert_eq!(restored.position, crate::config::Position::Right);
        assert!(saved.contains("# retained"));
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), old);
        // Once the home file exists, old preferences no longer seed it.
        std::fs::write(&legacy, [0xff]).unwrap();
        persist_seeded(
            &path,
            Some(&legacy),
            "taskbar.side_width",
            "320",
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            crate::config::parse(&std::fs::read_to_string(path).unwrap()).side_width,
            320
        );
    }

    #[test]
    fn migration_failure_and_oversize_input_preserve_existing_files() {
        let dir = Temp::new();
        let path = dir.0.join("winmgr.cnf");
        let legacy = dir.0.join("legacy.txt");
        std::fs::write(&legacy, "ui.language=zh\n").unwrap();
        assert!(
            persist_seeded(&path, Some(&legacy), "taskbar.position", "left", |stage| {
                if stage == "replace" {
                    Err(io::Error::other("injected"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "ui.language=zh\n"
        );
        std::fs::write(&legacy, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert!(persist_seeded(&path, Some(&legacy), "key", "value", |_| Ok(())).is_err());
        assert!(!path.exists());
        // A corrupt migration source is also preserved rather than replaced.
        std::fs::write(&legacy, [0xff]).unwrap();
        assert!(persist_seeded(&path, Some(&legacy), "key", "value", |_| Ok(())).is_err());
        assert_eq!(std::fs::read(&legacy).unwrap(), [0xff]);
        assert!(!path.exists());
    }

    #[test]
    fn slow_serial_worker_coalesces_last_choice_without_holding_state_lock() {
        let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        spawn_worker(&state, move |_, value| {
            started_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
            result_tx.send(value.to_string()).unwrap();
            Ok(())
        });
        let submit = |value: &str, generation| {
            let mut s = crate::win_util::lock_or_recover(&state.0);
            s.entries.insert(
                "test",
                Entry {
                    value: value.into(),
                    generation,
                    queued: true,
                    error: None,
                },
            );
            state.1.notify_one();
        };
        submit("first", 1);
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let start = std::time::Instant::now();
        for generation in 2..102 {
            submit(&generation.to_string(), generation);
        }
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        assert_eq!(crate::win_util::lock_or_recover(&state.0).entries.len(), 1);
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap(),
            "first"
        );
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap(),
            "101"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !crate::win_util::lock_or_recover(&state.0)
            .entries
            .is_empty()
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        crate::win_util::lock_or_recover(&state.0).stop = true;
        state.1.notify_one();
    }

    #[test]
    fn real_backup_and_destination_failures_leave_original_intact() {
        let dir = Temp::new();
        let path = dir.0.join("s.txt");
        std::fs::write(&path, "keep").unwrap();
        std::fs::create_dir(dir.0.join("s.txt.bak")).unwrap();
        assert!(
            persist(&path, "key", "value")
                .unwrap_err()
                .contains(crate::i18n::text("备份"))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
        std::fs::remove_dir(dir.0.join("s.txt.bak")).unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        let _locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(persist(&path, "key", "value").is_err());
    }
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "minicon-settings-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn writes_preserve_other_lines_and_backup_original_bytes() {
        let dir = Temp::new();
        let path = dir.0.join("settings.txt");
        let original = b"# user comment\r\nunknown = keep\r\ntaskbar.show = true\r\n";
        std::fs::write(&path, original).unwrap();
        persist(&path, "taskbar.show", "false").unwrap();
        assert_eq!(
            std::fs::read(dir.0.join("settings.txt.bak")).unwrap(),
            original
        );
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("unknown = keep"));
        assert!(result.contains("taskbar.show = false"));
        persist(&dir.0.join("new.txt"), "key", "value").unwrap();
    }
    #[test]
    fn every_failure_preserves_original_and_cleans_temporary_files() {
        let dir = Temp::new();
        let path = dir.0.join("settings.txt");
        for phase in ["read", "backup", "write", "replace"] {
            std::fs::write(&path, "original = keep\n").unwrap();
            assert!(
                persist_with(&path, "key", "value", |stage| if stage == phase {
                    Err(io::Error::other("injected"))
                } else {
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "original = keep\n");
            assert!(
                !std::fs::read_dir(&dir.0).unwrap().any(|e| e
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|e| e == "tmp"))
            );
        }
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(persist(&path, "key", "value").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe]);
        assert!(persist(&dir.0, "key", "value").is_err());
    }
    #[test]
    fn external_edit_conflicts_are_preserved() {
        let dir = Temp::new();
        let path = dir.0.join("s.txt");
        std::fs::write(&path, "old").unwrap();
        assert!(
            persist_with(&path, "key", "value", |stage| {
                if stage == "replace" {
                    std::fs::write(&path, "external")?;
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external");
    }
    #[test]
    fn stale_success_cannot_clear_newer_pending_or_failed_choice() {
        let mut state = State::default();
        state.entries.insert(
            "key",
            Entry {
                value: "new".into(),
                generation: 2,
                queued: false,
                error: Some("new failure".into()),
            },
        );
        finish(&mut state, "key", 1, Ok(()));
        assert_eq!(state.entries["key"].error.as_deref(), Some("new failure"));
        finish(&mut state, "key", 2, Ok(()));
        assert!(state.entries.is_empty());
    }
}
