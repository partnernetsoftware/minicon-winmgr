//! Fixed-size log files, written only by the existing non-blocking log worker.
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(crate) const RECORD_BYTES: usize = 16 * 1024;
const FILE_BYTES: u64 = 8 * 1024 * 1024;
const BACKUPS: usize = 3;

pub(crate) struct Store {
    directory: PathBuf,
    limit: u64,
}
impl Store {
    /// No disk access on the caller's UI/startup thread.
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            limit: FILE_BYTES,
        }
    }

    fn path(&self, index: usize) -> PathBuf {
        self.directory.join(if index == 0 {
            "minicon-winmgr.current.log".into()
        } else {
            format!("minicon-winmgr.{index}.log")
        })
    }

    fn length(path: &Path) -> io::Result<u64> {
        match fs::symlink_metadata(path) {
            Ok(info) if info.is_file() && !info.file_type().is_symlink() => Ok(info.len()),
            Ok(_) => Err(io::Error::other("log slot is not a regular file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    fn rotate(&self) -> io::Result<()> {
        // Validate every fixed slot before changing any names. Never enumerate
        // unrelated files or delete historical daily logs.
        for index in 0..=BACKUPS {
            Self::length(&self.path(index))?;
        }
        for index in (0..BACKUPS).rev() {
            match fs::rename(self.path(index), self.path(index + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
impl Write for Store {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if bytes.len() > RECORD_BYTES || bytes.len() as u64 > self.limit {
            return Err(io::Error::other("log record exceeds size budget"));
        }
        fs::create_dir_all(&self.directory)?;
        // Exclusive open is scoped to one worker write/rotation. A second
        // process fails this write instead of appending to an archived handle.
        let _lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(self.directory.join("minicon-winmgr.logs.lock"))?;
        let path = self.path(0);
        if Self::length(&path)?.saturating_add(bytes.len() as u64) > self.limit {
            self.rotate()?;
        }
        // Open current after rotation, while the lock is still held. There is
        // no persistent file handle that could drift into a backup filename.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        file.write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                    "winmgr-logs-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )))
        }
        fn store(&self) -> Store {
            Store {
                directory: self.0.clone(),
                limit: 32,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // The test creates this exact private directory, with fixed files only.
            let store = self.store();
            for index in 0..=BACKUPS {
                let _ = fs::remove_file(store.path(index));
            }
            let _ = fs::remove_file(self.0.join("minicon-winmgr.logs.lock"));
            let _ = fs::remove_file(self.0.join("unrelated.txt"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn concurrent_writers_share_rotation_budget_without_touching_other_files() {
        let fixture = Fixture::new();
        fs::create_dir_all(&fixture.0).unwrap();
        fs::write(fixture.0.join("unrelated.txt"), b"keep").unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let mut store = fixture.store();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    (0..200)
                        .filter(|_| store.write_all(b"record\n").is_ok())
                        .count()
                })
            })
            .collect();
        start.wait();
        let successes: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert!(successes > 0);
        let store = fixture.store();
        let total: u64 = (0..=BACKUPS)
            .map(|index| {
                let size = Store::length(&store.path(index)).unwrap();
                assert!(size <= 32);
                size
            })
            .sum();
        assert!(total <= 128);
        assert_eq!(fs::read(fixture.0.join("unrelated.txt")).unwrap(), b"keep");
    }

    #[test]
    fn rotating_and_reopening_keep_a_fixed_disk_budget_and_preserve_other_files() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        store.write_all(&[b'a'; 16]).unwrap();
        fs::write(fixture.0.join("unrelated.txt"), b"keep").unwrap();
        for value in b'b'..=b'z' {
            // Each iteration reopens the writer, as after a process restart.
            fixture.store().write_all(&[value; 16]).unwrap();
        }
        let mut total = 0;
        for index in 0..=BACKUPS {
            let length = fs::metadata(store.path(index)).unwrap().len();
            assert!(length <= 32);
            total += length;
        }
        assert!(total <= 32 * (BACKUPS as u64 + 1));
        assert_eq!(fs::read(fixture.0.join("unrelated.txt")).unwrap(), b"keep");
        assert!(fs::read(store.path(0)).unwrap().ends_with(&[b'z'; 16]));
    }
    #[test]
    fn contention_and_locked_rotation_fail_without_growing_files() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        store.write_all(&[b'a'; 32]).unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(fixture.0.join("minicon-winmgr.logs.lock"))
            .unwrap();
        assert!(store.write_all(b"busy").is_err());
        drop(lock);
        let occupied = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.path(0))
            .unwrap();
        assert!(store.write_all(b"cannot rotate").is_err());
        assert_eq!(fs::read(store.path(0)).unwrap(), [b'a'; 32]);
        drop(occupied);
        store.write_all(b"retry").unwrap();
        assert_eq!(fs::read(store.path(0)).unwrap(), b"retry");
    }
    #[test]
    fn invalid_directory_or_large_record_does_not_panic_or_create_more_files() {
        let fixture = Fixture::new();
        let mut store = fixture.store();
        assert!(store.write_all(&[0; 33]).is_err());
        assert!(!fixture.0.exists());
        fs::write(&fixture.0, b"not a directory").unwrap();
        assert!(store.write_all(b"record").is_err());
        assert_eq!(fs::read(&fixture.0).unwrap(), b"not a directory");
        fs::remove_file(&fixture.0).unwrap();
    }
}
