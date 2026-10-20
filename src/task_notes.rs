//! Append-only task analysis. Notes never grant process-management authority.
use crate::task_service::{Identity, Process};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 8 * 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub owner: String,
    pub image: Option<String>,
    pub run: Option<Identity>,
}
impl Subject {
    pub fn for_run(owner: String, run: Identity) -> Result<Self, Error> {
        let subject = Self {
            owner,
            image: None,
            run: Some(run),
        };
        subject.validate()?;
        Ok(subject)
    }
    pub fn process(process: &Process) -> Result<Self, Error> {
        let subject = Self {
            owner: process
                .owner
                .clone()
                .ok_or_else(|| Error::invalid("Task owner is unavailable"))?,
            image: process.path.clone(),
            run: process.path.is_none().then_some(process.identity),
        };
        subject.validate()?;
        Ok(subject)
    }
    fn validate(&self) -> Result<(), Error> {
        bounded(&self.owner, 256)?;
        if self.owner.is_empty() || self.image.is_some() == self.run.is_some() {
            return Err(Error::invalid(
                "A subject needs an owner and exactly one image or run identity",
            ));
        }
        if let Some(image) = &self.image {
            bounded(image, 32768)?;
            if image.is_empty() {
                return Err(Error::invalid("Empty image identity"));
            }
        }
        if self.run.is_some_and(|id| id.created_100ns == 0) {
            return Err(Error::invalid("Unverified run identity"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    #[default]
    Unreviewed,
    Normal,
    Watching,
    Suspicious,
    Resolved,
}
impl Flag {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Normal => "normal",
            Self::Watching => "watching",
            Self::Suspicious => "suspicious",
            Self::Resolved => "resolved",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorKind {
    Human,
    Agent,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Author {
    pub kind: AuthorKind,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub revision: u64,
    pub at_unix_ms: u64,
    pub subject: Subject,
    pub run: Option<Identity>,
    pub author: Author,
    pub text: String,
    pub evidence: String,
    pub flag: Option<Flag>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub subject: Subject,
    pub revision: u64,
    pub flag: Flag,
    pub entries: Vec<Entry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub subject: Subject,
    pub revision: u64,
    pub flag: Flag,
}
#[derive(Default, Serialize, Deserialize)]
struct Document {
    schema: u32,
    entries: Vec<Entry>,
}
#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}
impl Error {
    fn invalid(message: &str) -> Self {
        Self {
            code: "invalid",
            message: message.into(),
        }
    }
    fn conflict() -> Self {
        Self {
            code: "conflict",
            message: "Notes changed. Reload and review before saving; keep your draft.".into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self {
            code: "io",
            message: value.to_string(),
        }
    }
}
fn bounded(value: &str, limit: usize) -> Result<(), Error> {
    if value.len() > limit || value.contains('\0') {
        Err(Error::invalid("Note field is too long or contains NUL"))
    } else {
        Ok(())
    }
}
#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
}
impl Store {
    pub fn at(directory: PathBuf) -> Result<Self, Error> {
        if !directory.is_absolute() {
            return Err(Error::invalid("Notes directory must be absolute"));
        }
        Ok(Self { directory })
    }
    #[cfg(windows)]
    pub fn current() -> Result<Self, Error> {
        use windows::Win32::{System::Com::CoTaskMemFree, UI::Shell::*};
        let value = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }
            .map_err(|e| Error {
                code: "io",
                message: e.to_string(),
            })?;
        let path = unsafe { value.to_string() };
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        Self::at(
            PathBuf::from(path.map_err(|e| Error {
                code: "io",
                message: e.to_string(),
            })?)
            .join("minicon-tskmgr")
            .join("analysis"),
        )
    }
    pub fn stamp(&self) -> Option<(u64, SystemTime)> {
        let meta = fs::metadata(self.directory.join("task-notes.json")).ok()?;
        Some((meta.len(), meta.modified().ok()?))
    }
    fn load(&self) -> Result<Document, Error> {
        let file = match fs::File::open(self.directory.join("task-notes.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Document {
                    schema: 1,
                    ..Default::default()
                });
            }
            Err(e) => return Err(e.into()),
        };
        let mut data = Vec::new();
        file.take((LIMIT + 1) as u64).read_to_end(&mut data)?;
        if data.len() > LIMIT {
            return Err(Error::invalid(
                "Notes store exceeds 8 MiB; archive it before adding notes",
            ));
        }
        let document: Document = serde_json::from_slice(&data).map_err(|e| Error {
            code: "corrupt",
            message: format!("Notes could not be read; original file preserved: {e}"),
        })?;
        if document.schema != 1 || document.entries.len() > 10000 {
            return Err(Error::invalid("Unsupported notes schema or history size"));
        }
        let mut last = 0;
        for entry in &document.entries {
            entry.subject.validate()?;
            bounded(&entry.text, 16384)?;
            bounded(&entry.evidence, 8192)?;
            bounded(&entry.author.name, 128)?;
            if entry.revision <= last {
                return Err(Error::invalid("Invalid notes revision sequence"));
            }
            last = entry.revision;
        }
        Ok(document)
    }
    pub fn thread(&self, subject: &Subject) -> Result<Thread, Error> {
        subject.validate()?;
        Ok(Self::select(self.load()?, subject))
    }
    fn select(document: Document, subject: &Subject) -> Thread {
        let entries: Vec<_> = document
            .entries
            .into_iter()
            .filter(|e| e.subject == *subject)
            .collect();
        Thread {
            subject: subject.clone(),
            revision: entries.last().map_or(0, |e| e.revision),
            flag: entries
                .iter()
                .rev()
                .find_map(|e| e.flag)
                .unwrap_or_default(),
            entries,
        }
    }
    pub fn summaries(&self) -> Result<Vec<Summary>, Error> {
        let mut result = HashMap::<Subject, Summary>::new();
        for entry in self.load()?.entries {
            let summary = result.entry(entry.subject.clone()).or_insert(Summary {
                subject: entry.subject,
                revision: 0,
                flag: Flag::Unreviewed,
            });
            summary.revision = entry.revision;
            if let Some(flag) = entry.flag {
                summary.flag = flag;
            }
        }
        let mut result: Vec<_> = result.into_values().collect();
        result.sort_by_key(|s| {
            format!(
                "{}|{:?}|{:?}",
                s.subject.owner, s.subject.image, s.subject.run
            )
        });
        Ok(result)
    }
    pub fn append(&self, request: Append) -> Result<Thread, Error> {
        request.subject.validate()?;
        bounded(&request.text, 16384)?;
        bounded(&request.evidence, 8192)?;
        bounded(&request.author.name, 128)?;
        if request.author.name.trim().is_empty()
            || request.text.trim().is_empty() && request.flag.is_none()
        {
            return Err(Error::invalid(
                "A note or a state change and an author are required",
            ));
        }
        fs::create_dir_all(&self.directory)?;
        let started = Instant::now();
        let _lock = loop {
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(0);
            }
            match options.open(self.directory.join("task-notes.lock")) {
                Ok(file) => break file,
                Err(e)
                    if e.raw_os_error() == Some(32)
                        && started.elapsed() < Duration::from_secs(1) =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => return Err(e.into()),
            }
        };
        let mut document = self.load()?;
        let current = document
            .entries
            .iter()
            .rev()
            .find(|e| e.subject == request.subject)
            .map_or(0, |e| e.revision);
        if current != request.expected_revision {
            return Err(Error::conflict());
        }
        if document.entries.len() >= 10000 {
            return Err(Error::invalid(
                "Notes history is full; archive before adding notes",
            ));
        }
        let revision = document
            .entries
            .last()
            .map_or(0, |e| e.revision)
            .checked_add(1)
            .ok_or_else(|| Error::invalid("Revision exhausted"))?;
        document.entries.push(Entry {
            revision,
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            subject: request.subject.clone(),
            run: request.run,
            author: request.author,
            text: request.text,
            evidence: request.evidence,
            flag: request.flag,
        });
        let data = serde_json::to_vec(&document).map_err(|e| Error::invalid(&e.to_string()))?;
        if data.len() > LIMIT {
            return Err(Error::invalid(
                "Notes store is full; original file preserved",
            ));
        }
        let pending = self.directory.join(format!(
            "notes-pending-{}-{}.json",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)?;
        let write = (|| {
            file.write_all(&data)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&pending, self.directory.join("task-notes.json"))
        })();
        if let Err(error) = write {
            let _ = fs::remove_file(pending);
            return Err(error.into());
        }
        Ok(Self::select(document, &request.subject))
    }
}
pub struct Append {
    pub subject: Subject,
    pub run: Option<Identity>,
    pub expected_revision: u64,
    pub author: Author,
    pub text: String,
    pub evidence: String,
    pub flag: Option<Flag>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Store {
        Store::at(std::env::temp_dir().join(format!(
            "minicon-notes-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )))
        .unwrap()
    }
    fn subject() -> Subject {
        Subject {
            owner: "fixture-user".into(),
            image: Some("example.exe".into()),
            run: None,
        }
    }
    fn append(store: &Store, revision: u64, kind: AuthorKind, flag: Flag) -> Result<Thread, Error> {
        store.append(Append {
            subject: subject(),
            run: Some(Identity {
                pid: 7,
                created_100ns: 123,
            }),
            expected_revision: revision,
            author: Author {
                kind,
                name: "fixture".into(),
            },
            text: "Investigation with evidence".into(),
            evidence: "Owned test sample".into(),
            flag: Some(flag),
        })
    }
    #[test]
    fn human_agent_history_survives_reload_and_conflicts_preserve_it() {
        let store = fixture();
        let first = append(&store, 0, AuthorKind::Human, Flag::Watching).unwrap();
        let second = append(&store, first.revision, AuthorKind::Agent, Flag::Suspicious).unwrap();
        assert_eq!(second.entries.len(), 2);
        assert_eq!(second.flag, Flag::Suspicious);
        assert_eq!(
            append(&store, first.revision, AuthorKind::Human, Flag::Resolved)
                .unwrap_err()
                .code,
            "conflict"
        );
        let reload = Store::at(store.directory.clone())
            .unwrap()
            .thread(&subject())
            .unwrap();
        assert_eq!(reload.revision, second.revision);
        assert!(matches!(reload.entries[1].author.kind, AuthorKind::Agent));
        let other = Subject {
            owner: "another-account".into(),
            ..subject()
        };
        assert_eq!(store.thread(&other).unwrap().revision, 0);
        fs::remove_dir_all(store.directory).unwrap();
    }
    #[test]
    fn concurrent_writers_do_not_erase_each_other_and_corruption_is_preserved() {
        let store = fixture();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..2)
                .map(|_| {
                    let store = store.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        append(&store, 0, AuthorKind::Agent, Flag::Watching)
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|j| j.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(store.thread(&subject()).unwrap().entries.len(), 1);
        fs::write(store.directory.join("task-notes.json"), b"bad json").unwrap();
        assert_eq!(
            append(&store, 0, AuthorKind::Human, Flag::Normal)
                .unwrap_err()
                .code,
            "corrupt"
        );
        assert_eq!(
            fs::read(store.directory.join("task-notes.json")).unwrap(),
            b"bad json"
        );
        fs::remove_dir_all(store.directory).unwrap();
    }
}
