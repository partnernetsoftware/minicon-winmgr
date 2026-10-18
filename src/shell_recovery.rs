//! Durable, account-bound original login-shell record and operation exclusion.
//! No legacy file is adopted implicitly; explicit CLI rollback remains available.
use crate::{
    shell_registry::{self, RawShell},
    single_instance::InstanceGuard,
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_RECORD: u64 = 512 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    sid: String,
    machine: String,
    original: Option<RawShell>,
}
pub(crate) struct Operation {
    _guard: InstanceGuard,
    sid: String,
    machine: String,
    path: PathBuf,
}
fn message(en: &'static str, zh: &'static str) -> Box<dyn Error> {
    if crate::i18n::language() == crate::i18n::Language::En {
        en
    } else {
        zh
    }
    .into()
}
pub(crate) fn begin() -> Result<Operation, Box<dyn Error>> {
    let sid = crate::privilege::sid_text().ok_or("Cannot identify the current account")?;
    let machine = shell_registry::machine_identity()?;
    let guard = InstanceGuard::acquire(&format!(r"Global\minicon-winmgr.shell-operation.{sid}"))
        .map_err(|_| message("Another shell operation is running, or its lock is unavailable. Try again shortly.", "另一个桌面操作正在执行，或无法取得操作锁，请稍后重试。"))?;
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
    let path = PathBuf::from(base)
        .join("minicon-winmgr")
        .join("recovery")
        .join(&sid)
        .join("original-shell.json");
    Ok(Operation {
        _guard: guard,
        sid,
        machine,
        path,
    })
}
fn load(path: &Path, sid: &str, machine: &str) -> Result<Record, Box<dyn Error>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err("Shell recovery record is too large".into());
    }
    let record: Record = serde_json::from_slice(&bytes)?;
    if record.version != 1 || record.sid != sid || record.machine != machine {
        return Err(message(
            "The recovery record belongs to another account or machine, or is unsupported. Login settings were left unchanged.",
            "恢复记录不属于当前账户或本机，或格式不受支持。登录设置未更改。",
        ));
    }
    if let Some(raw) = &record.original {
        raw.text()?;
    }
    Ok(record)
}
fn persist(path: &Path, record: &Record) -> Result<(), Box<dyn Error>> {
    let parent = path.parent().ok_or("Invalid recovery path")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".original-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let result = (|| -> Result<(), Box<dyn Error>> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(record)?)?;
        file.sync_all()?;
        drop(file);
        // Publish complete contents without replacing any existing baseline.
        fs::hard_link(&temp, path)?;
        if load(path, &record.sid, &record.machine)? != *record {
            return Err("Shell recovery record verification failed".into());
        }
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}
pub(crate) fn is_product(value: &Option<RawShell>) -> bool {
    value
        .as_ref()
        .and_then(|v| v.text().ok())
        .is_some_and(|s| shell_registry::is_product_command(&s))
}
impl Operation {
    pub(crate) fn ensure_original(&self, current: &Option<RawShell>) -> Result<(), Box<dyn Error>> {
        match load(&self.path, &self.sid, &self.machine) {
            Ok(record) => {
                if current != &record.original && !is_product(current) {
                    return Err(message(
                        "Login settings changed outside Minicon. The original recovery record was preserved; takeover was cancelled.",
                        "登录设置已被外部修改。已保留原始恢复记录，接管已取消。",
                    ));
                }
                Ok(())
            }
            Err(error) if !self.path.try_exists()? => {
                if is_product(current) {
                    return Err(message(
                        "No account-bound recovery record exists. Legacy shell-backup.txt is not trusted automatically. Use an explicitly confirmed --shell-rollback <value|default> to choose login settings before taking over again.",
                        "缺少当前账户的恢复记录，不能自动采用旧 shell-backup.txt。请显式确认 --shell-rollback <原值|default> 选择登录设置后再接管。",
                    ));
                }
                if let Some(raw) = current {
                    raw.text()?;
                }
                let _ = error;
                persist(
                    &self.path,
                    &Record {
                        version: 1,
                        sid: self.sid.clone(),
                        machine: self.machine.clone(),
                        original: current.clone(),
                    },
                )
            }
            Err(error) => Err(error),
        }
    }
    pub(crate) fn original(&self) -> Result<Option<RawShell>, Box<dyn Error>> {
        Ok(load(&self.path, &self.sid, &self.machine)?.original)
    }
    pub(crate) fn write(
        &self,
        expected: &Option<RawShell>,
        target: Option<&RawShell>,
    ) -> Result<(), Box<dyn Error>> {
        conditional_write(
            expected,
            target,
            shell_registry::current_raw,
            shell_registry::write_raw,
        )
    }
}
fn conditional_write(
    expected: &Option<RawShell>,
    target: Option<&RawShell>,
    mut read: impl FnMut() -> Result<Option<RawShell>, Box<dyn Error>>,
    mut write: impl FnMut(Option<&RawShell>) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    if &read()? != expected {
        return Err(message(
            "Login settings changed during this operation. No further changes were made.",
            "操作期间登录设置已被修改，已停止后续操作。",
        ));
    }
    if let Err(error) = write(target) {
        let restored = match read() {
            Ok(current) if &current == expected => true,
            Ok(current) if current.as_ref() == target => write(expected.as_ref()).is_ok(),
            _ => false,
        };
        return Err(format!(
            "Login settings write failed: {error}; previous value verified/restored: {restored}"
        )
        .into());
    }
    Ok(())
}
/// Registry-only CLI writes share exclusion and save a baseline before installing us.
pub(crate) fn set_login(target: Option<RawShell>) -> Result<(), Box<dyn Error>> {
    let operation = begin()?;
    let current = shell_registry::current_raw()?;
    if is_product(&target) {
        operation.ensure_original(&current)?;
    }
    operation.write(&current, target.as_ref())
}
pub(crate) fn original_text() -> Result<Option<String>, Box<dyn Error>> {
    begin()?.original()?.map(|v| v.text()).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "minicon-recovery-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> PathBuf {
            self.0.join("original.json")
        }
        fn operation(&self) -> Operation {
            Operation {
                _guard: InstanceGuard::acquire(&format!(
                    "minicon-recovery-fixture-{}",
                    self.0.file_name().unwrap().to_string_lossy()
                ))
                .unwrap(),
                sid: "fixture-account".into(),
                machine: "fixture-machine".into(),
                path: self.file(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn record(original: Option<RawShell>) -> Record {
        Record {
            version: 1,
            sid: "fixture-account".into(),
            machine: "fixture-machine".into(),
            original,
        }
    }
    #[test]
    fn original_is_complete_identity_bound_and_never_overwritten() {
        let f = Fixture::new();
        let a = record(Some(RawShell::string("  third-party.exe --option  ")));
        persist(&f.file(), &a).unwrap();
        assert_eq!(load(&f.file(), &a.sid, &a.machine).unwrap(), a);
        assert!(load(&f.file(), "different-account", &a.machine).is_err());
        assert!(load(&f.file(), &a.sid, "different-machine").is_err());
        assert!(persist(&f.file(), &record(None)).is_err());
        assert_eq!(load(&f.file(), &a.sid, &a.machine).unwrap(), a);
    }
    #[test]
    fn missing_empty_corrupt_and_absent_registry_value_are_distinct() {
        let f = Fixture::new();
        let a = record(None);
        assert!(load(&f.file(), &a.sid, &a.machine).is_err());
        fs::write(f.file(), b"").unwrap();
        assert!(load(&f.file(), &a.sid, &a.machine).is_err());
        fs::write(f.file(), b"{bad").unwrap();
        assert!(load(&f.file(), &a.sid, &a.machine).is_err());
        fs::remove_file(f.file()).unwrap();
        persist(&f.file(), &a).unwrap();
        assert_eq!(load(&f.file(), &a.sid, &a.machine).unwrap().original, None);
        assert_ne!(record(Some(RawShell::string(""))), a);
    }
    #[test]
    fn expanded_registry_type_and_bytes_survive_exactly() {
        let f = Fixture::new();
        let mut raw = RawShell::string("%SystemRoot%\\explorer.exe ");
        raw.kind = 2;
        let a = record(Some(raw));
        persist(&f.file(), &a).unwrap();
        assert_eq!(load(&f.file(), &a.sid, &a.machine).unwrap(), a);
    }
    #[test]
    fn invalid_types_sizes_and_utf16_are_rejected() {
        for raw in [
            RawShell {
                kind: 4,
                data: vec![0; 4],
            },
            RawShell {
                kind: 1,
                data: vec![0],
            },
            RawShell {
                kind: 1,
                data: vec![0, 216],
            },
            RawShell::string("a\0b"),
        ] {
            assert!(raw.text().is_err());
        }
    }
    #[test]
    fn external_change_is_never_overwritten() {
        let expected = Some(RawShell::string("original.exe"));
        let external = Some(RawShell::string("externally-changed.exe"));
        let writes = Cell::new(0);
        assert!(
            conditional_write(
                &expected,
                None,
                || Ok(external.clone()),
                |_| {
                    writes.set(writes.get() + 1);
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(writes.get(), 0);
    }
    #[test]
    fn failed_write_rolls_back_our_value_but_preserves_an_external_value() {
        for external in [false, true] {
            let original = Some(RawShell::string("original.exe"));
            let target = Some(RawShell::string("target.exe"));
            let other = Some(RawShell::string("other.exe"));
            let current = RefCell::new(original.clone());
            let writes = Cell::new(0);
            let result = conditional_write(
                &original,
                target.as_ref(),
                || Ok(current.borrow().clone()),
                |value| {
                    writes.set(writes.get() + 1);
                    if writes.get() == 1 {
                        *current.borrow_mut() = if external {
                            other.clone()
                        } else {
                            value.cloned()
                        };
                        Err("injected readback failure".into())
                    } else {
                        *current.borrow_mut() = value.cloned();
                        Ok(())
                    }
                },
            );
            assert!(result.is_err());
            assert_eq!(*current.borrow(), if external { other } else { original });
            assert_eq!(writes.get(), if external { 1 } else { 2 });
        }
    }
    #[test]
    fn unavailable_record_destination_cannot_publish_a_baseline() {
        let f = Fixture::new();
        let blocked = f.0.join("not-a-directory");
        fs::write(&blocked, b"fixture").unwrap();
        assert!(persist(&blocked.join("original.json"), &record(None)).is_err());
        assert_eq!(fs::read(blocked).unwrap(), b"fixture");
    }
    #[test]
    fn legacy_or_missing_record_is_not_adopted_for_an_installed_product() {
        let f = Fixture::new();
        let operation = f.operation();
        fs::write(f.0.join("shell-backup.txt"), "explorer.exe").unwrap();
        assert!(
            operation
                .ensure_original(&Some(RawShell::string(r"C:\Fixture\minicon-winmgr.exe")))
                .is_err()
        );
        assert!(!f.file().exists());
        assert!(operation.original().is_err());
    }
    #[test]
    fn repeated_takeover_keeps_original_but_external_baseline_changes_block_it() {
        let f = Fixture::new();
        let operation = f.operation();
        operation.ensure_original(&None).unwrap();
        let product = Some(RawShell::string(r#""C:\Fixture\minicon-winmgr.exe""#));
        operation.ensure_original(&product).unwrap();
        assert_eq!(operation.original().unwrap(), None);
        assert!(
            operation
                .ensure_original(&Some(RawShell::string("third-party.exe")))
                .is_err()
        );
        assert_eq!(operation.original().unwrap(), None);
    }
    #[test]
    fn corrupt_existing_record_is_not_overwritten_or_treated_as_absent() {
        let f = Fixture::new();
        let operation = f.operation();
        fs::write(f.file(), b"not-json").unwrap();
        assert!(operation.ensure_original(&None).is_err());
        assert_eq!(fs::read(f.file()).unwrap(), b"not-json");
    }
}
