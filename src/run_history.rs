//! Run history and bounded, target-only local path suggestions. Call on a worker.
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    Win32::Storage::FileSystem::{
        GetDriveTypeW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    },
    core::PCWSTR,
};

const HISTORY_LIMIT: usize = 30;
const CANDIDATE_LIMIT: usize = 20;
const HISTORY_BYTES_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateSource {
    History,
    Path,
    Application,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub label: Option<String>,
    pub text: String,
    pub source: CandidateSource,
}

pub struct History {
    path: Option<PathBuf>,
    entries: Vec<String>,
}

impl History {
    pub fn load_default() -> Self {
        match std::env::var_os("LOCALAPPDATA") {
            Some(root) => Self::load(PathBuf::from(root).join("minicon-winmgr/run-history.json")),
            None => {
                tracing::warn!("LOCALAPPDATA missing: run history is session-only");
                Self {
                    path: None,
                    entries: Vec::new(),
                }
            }
        }
    }

    pub fn load(path: PathBuf) -> Self {
        let entries = match read_history(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<String>>(&bytes) {
                Ok(entries) => normalize(entries),
                Err(error) => {
                    tracing::warn!(%error, "Damaged run history; starting session history");
                    Vec::new()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "Cannot read run history; starting session history");
                Vec::new()
            }
        };
        Self {
            path: Some(path),
            entries,
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Update session state even if persistence fails. Only call after launch acceptance.
    pub fn record_success(&mut self, text: &str) -> io::Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.entries.retain(|old| old != text);
        self.entries.insert(0, text.to_owned());
        self.entries.truncate(HISTORY_LIMIT);
        self.persist()
    }

    /// Keep this action available after failure: a retry still attempts disk removal.
    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut failure = None;
        for artifact in [path.clone(), path.with_extension("json.tmp")] {
            if let Err(error) = fs::remove_file(&artifact)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%error, path = %artifact.display(), "Disk run history clear failed");
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn persist(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let result = (|| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("json.tmp");
            let bytes = serde_json::to_vec(&self.entries).map_err(io::Error::other)?;
            {
                use io::Write;
                let mut file = fs::File::create(&temporary)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(target.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(io::Error::other)
        })();
        if let Err(error) = &result {
            tracing::warn!(%error, "Cannot persist run history; session history retained");
            // Failed replacement must not leave a second plaintext history artifact.
            if let Err(cleanup) = fs::remove_file(path.with_extension("json.tmp"))
                && cleanup.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%cleanup, "Cannot remove temporary run history; clear history can retry");
            }
        }
        result
    }
}

use std::os::windows::ffi::OsStrExt;

fn read_history(path: &Path) -> io::Result<Vec<u8>> {
    use io::Read;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(HISTORY_BYTES_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > HISTORY_BYTES_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "run history exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

fn history_candidates(history: &[String], input: &str) -> Vec<Candidate> {
    let prefix = input.to_lowercase();
    history
        .iter()
        .filter(|entry| entry.to_lowercase().starts_with(&prefix))
        .take(CANDIDATE_LIMIT)
        .map(|text| Candidate {
            label: None,
            text: text.clone(),
            source: CandidateSource::History,
        })
        .collect()
}

fn normalize(entries: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter(|entry| !entry.trim().is_empty() && seen.insert(entry.clone()))
        .take(HISTORY_LIMIT)
        .collect()
}

/// History first, then path suggestions; exact duplicate text is displayed once.
pub fn merge_candidates(history: &[String], input: &str) -> Vec<Candidate> {
    let mut result = history_candidates(history, input);
    let mut seen: HashSet<String> = result.iter().map(|item| item.text.clone()).collect();
    if result.len() < CANDIDATE_LIMIT {
        for item in path_candidates(input) {
            if seen.insert(item.text.clone()) {
                result.push(item);
            }
            if result.len() == CANDIDATE_LIMIT {
                break;
            }
        }
    }
    result
}

/// App discovery is requested only for a bare name, never for paths or arguments.
pub fn application_query(input: &str) -> bool {
    let query = input.trim();
    query.chars().count() >= 2
        && !query
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\\' | '/' | ':' | '"' | '%'))
}

/// Bounded local Start Menu discovery. No recursive symlinks, remote drives or startup scan.
pub fn discover_applications() -> Vec<crate::model::Shortcut> {
    let started = Instant::now();
    let mut pending = Vec::new();
    for key in ["APPDATA", "ProgramData"] {
        if let Some(root) = std::env::var_os(key) {
            let root = Path::new(&root).join("Microsoft/Windows/Start Menu/Programs");
            let value = root.to_string_lossy();
            if value.len() >= 3 && value.as_bytes()[1] == b':' {
                let drive: Vec<u16> = value[..3].encode_utf16().chain(Some(0)).collect();
                if unsafe { GetDriveTypeW(PCWSTR(drive.as_ptr())) } == 3 {
                    pending.push((root, 0));
                }
            }
        }
    }
    let mut found = Vec::new();
    let mut visited = 0;
    while let Some((directory, depth)) = pending.pop() {
        if started.elapsed() > Duration::from_millis(200) || visited >= 4096 {
            break;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited >= 4096 || started.elapsed() > Duration::from_millis(200) {
                break;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() && depth < 12 {
                pending.push((path, depth + 1));
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
                && let (Some(name), Some(path)) =
                    (path.file_stem().and_then(|s| s.to_str()), path.to_str())
            {
                found.push(crate::model::Shortcut {
                    name: name.into(),
                    path: path.into(),
                });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    found.dedup_by(|a, b| a.path == b.path);
    found
}

pub fn with_applications(
    mut result: Vec<Candidate>,
    input: &str,
    apps: &[crate::model::Shortcut],
) -> Vec<Candidate> {
    if !application_query(input) {
        return result;
    }
    for app in crate::launcher::search(apps, input, CANDIDATE_LIMIT) {
        let text = format!("\"{}\"", app.path);
        if result.len() == CANDIDATE_LIMIT {
            break;
        }
        if result.iter().any(|item| item.text == text) {
            continue;
        }
        result.push(Candidate {
            label: Some(app.name.clone()),
            text,
            source: CandidateSource::Application,
        });
    }
    result
}

struct Target<'a> {
    prefix: &'a str,
    text: &'a str,
    suffix: &'a str,
    quoted: bool,
}

fn target_span(input: &str) -> Option<Target<'_>> {
    let start = input.len() - input.trim_start().len();
    let rest = &input[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        let suffix = &quoted[end + 1..];
        if !suffix.is_empty() && !suffix.starts_with(char::is_whitespace) {
            return None;
        }
        Some(Target {
            prefix: &input[..start],
            text: &quoted[..end],
            suffix,
            quoted: true,
        })
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if end < rest.len() && !rest[end..].trim().is_empty() {
            let lowered = rest[..end].to_ascii_lowercase();
            // Without explicit executable syntax, a space may be part of a directory.
            // Suppress ambiguous suggestions rather than inventing command parameters.
            if ![".exe", ".bat", ".cmd", ".com"]
                .iter()
                .any(|extension| lowered.ends_with(extension))
            {
                return None;
            }
        }
        Some(Target {
            prefix: &input[..start],
            text: &rest[..end],
            suffix: &rest[end..],
            quoted: false,
        })
    }
}

fn replace_target(target: &Target<'_>, path: &str) -> String {
    if target.quoted || path.contains(char::is_whitespace) {
        format!("{}\"{}\"{}", target.prefix, path, target.suffix)
    } else {
        format!("{}{}{}", target.prefix, path, target.suffix)
    }
}

/// No UNC, mapped or removable drives. Caps enumeration work as well as output.
pub fn path_candidates(input: &str) -> Vec<Candidate> {
    path_candidates_with(input, &|key| std::env::var(key).ok())
}
fn path_candidates_with(input: &str, lookup: &impl Fn(&str) -> Option<String>) -> Vec<Candidate> {
    let started = Instant::now();
    let Some(target) = target_span(input) else {
        return Vec::new();
    };
    let expanded = crate::shell_launch::expand_target(target.text, lookup);
    let target = Target {
        text: &expanded,
        ..target
    };
    let bytes = target.text.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return Vec::new();
    }
    let root: Vec<u16> = target.text[..3].encode_utf16().chain(Some(0)).collect();
    // DRIVE_FIXED = 3. Remote/removable/CD-ROM/RAM disks are deliberately excluded.
    if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != 3 {
        return Vec::new();
    }
    let Some(split) = target.text.rfind('\\') else {
        return Vec::new();
    };
    let parent = &target.text[..split + 1];
    let prefix = target.text[split + 1..].to_lowercase();
    let Ok(entries) = fs::read_dir(Path::new(parent)) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in entries.take(4096) {
        if started.elapsed() > Duration::from_millis(100) {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_lowercase().starts_with(&prefix) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        matches.push((!kind.is_dir(), name.to_lowercase(), name));
    }
    matches.sort();
    matches
        .into_iter()
        .take(CANDIDATE_LIMIT)
        .map(|(file, _, name)| {
            let path = format!("{parent}{name}{}", if file { "" } else { "\\" });
            Candidate {
                label: Some(name),
                text: replace_target(&target, &path),
                source: CandidateSource::Path,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn application_suggestions_preserve_commands_and_only_scan_bare_names() {
        for input in [
            "",
            "n",
            "notepad file.txt",
            r"C:\Windows\",
            "%USERPROFILE%",
            "https://example.com",
        ] {
            assert!(!application_query(input), "{input}");
        }
        let apps = vec![crate::model::Shortcut {
            name: "Notepad".into(),
            path: r"C:\Program Files\Notepad.lnk".into(),
        }];
        let input = "note";
        let found = with_applications(Vec::new(), input, &apps);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label.as_deref(), Some("Notepad"));
        assert_eq!(found[0].text, r#""C:\Program Files\Notepad.lnk""#);
        assert_eq!(found[0].source, CandidateSource::Application);
        let again = with_applications(found.clone(), input, &apps);
        assert_eq!(again.len(), 1);
    }
    #[test]
    fn exact_dedup_keeps_case_and_parameters() {
        let entries = normalize(vec![
            "cmd /c a".into(),
            "CMD /c a".into(),
            "cmd /c b".into(),
            "cmd /c a".into(),
        ]);
        assert_eq!(entries, ["cmd /c a", "CMD /c a", "cmd /c b"]);
    }
    #[test]
    fn replacement_preserves_parameters_and_whitespace() {
        let span = target_span("  \"C:\\Program Files\\ap\"  --name \"张 三\"  ").unwrap();
        assert_eq!(
            replace_target(&span, "C:\\Program Files\\app.exe"),
            "  \"C:\\Program Files\\app.exe\"  --name \"张 三\"  "
        );
        assert!(target_span("\"C:\\unfinished").is_none());
        assert!(target_span("\"C:\\a\"bad").is_none());
        assert!(target_span("C:\\Program Files").is_none());
        assert!(target_span("C:\\Program Files\\App").is_none());
        let span = target_span("C:\\app.exe  --name \"张 三\"").unwrap();
        assert_eq!(
            replace_target(&span, "C:\\application.exe"),
            "C:\\application.exe  --name \"张 三\""
        );
    }
    #[test]
    fn history_matches_prefix_without_changing_case() {
        let history = History {
            path: None,
            entries: vec!["CMD /c a".into(), "notepad".into()],
        };
        assert_eq!(
            merge_candidates(history.entries(), "cmd")[0].text,
            "CMD /c a"
        );
        assert!(merge_candidates(history.entries(), "missing").is_empty());
    }
    #[test]
    fn success_is_exact_and_bounded() {
        let mut history = History {
            path: None,
            entries: Vec::new(),
        };
        for index in 0..31 {
            history.record_success(&format!("cmd /c {index}")).unwrap();
        }
        history.record_success("cmd /c 12").unwrap();
        assert_eq!(history.entries().len(), 30);
        assert_eq!(history.entries()[0], "cmd /c 12");
        assert!(!history.entries().iter().any(|entry| entry == "cmd /c 0"));
    }

    #[test]
    fn disk_roundtrip_corruption_and_clear_retry() {
        let root = std::env::temp_dir().join(format!(
            "minicon-history-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("run-history.json");
        let mut history = History::load(path.clone());
        history.record_success("  cmd /c \"张 三\"  ").unwrap();
        assert_eq!(History::load(path.clone()).entries(), history.entries());
        fs::write(&path, b"broken JSON").unwrap();
        let mut history = History::load(path.clone());
        assert!(history.entries().is_empty());
        history.record_success("notepad").unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(history.clear().is_err());
        assert!(history.entries().is_empty());
        fs::remove_dir(&path).unwrap();
        history.clear().unwrap();
        assert!(History::load(path).entries().is_empty());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn oversized_disk_history_is_bounded_and_ignored() {
        let path =
            std::env::temp_dir().join(format!("minicon-history-large-{}.json", std::process::id()));
        let file = fs::File::create(&path).unwrap();
        file.set_len(HISTORY_BYTES_LIMIT + 1024).unwrap();
        assert_eq!(
            read_history(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(History::load(path.clone()).entries().is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn clear_removes_temporary_artifact_and_retries_failures() {
        let root =
            std::env::temp_dir().join(format!("minicon-history-artifact-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("run-history.json");
        let temporary = path.with_extension("json.tmp");
        let mut history = History::load(path.clone());
        history.record_success("sensitive --secret value").unwrap();
        fs::create_dir(&temporary).unwrap();
        assert!(history.clear().is_err());
        assert!(!path.exists());
        assert!(history.entries().is_empty());
        fs::remove_dir(&temporary).unwrap();
        fs::write(&temporary, b"sensitive old history").unwrap();
        history.clear().unwrap();
        assert!(!temporary.exists());
        // A blocked final target causes replacement failure after the temp write.
        fs::create_dir(&path).unwrap();
        assert!(history.record_success("another --secret value").is_err());
        assert!(!temporary.exists());
        assert_eq!(history.entries(), ["another --secret value"]);
        fs::remove_dir(&path).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn local_directory_candidates_are_sorted_bounded_and_target_only() {
        let root = std::env::temp_dir().join(format!("minicon-path-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("z directory")).unwrap();
        fs::create_dir(root.join("a directory")).unwrap();
        for number in 0..25 {
            fs::write(root.join(format!("file{number:02}.txt")), []).unwrap();
        }
        let input = format!("\"{}\\\"  --name \"张 三\"  ", root.display());
        let suggestions = path_candidates(&input);
        assert_eq!(suggestions.len(), 20);
        let variable = path_candidates_with(r#""%ROOT%\"  --name "张 三"  "#, &|key| {
            (key == "ROOT").then(|| root.to_string_lossy().into_owned())
        });
        assert_eq!(variable, suggestions);
        assert!(path_candidates_with(r"%UNKNOWN%\", &|_| None).is_empty());
        assert_eq!(
            suggestions[0].text,
            format!("\"{}\\a directory\\\"  --name \"张 三\"  ", root.display())
        );
        assert_eq!(
            suggestions[1].text,
            format!("\"{}\\z directory\\\"  --name \"张 三\"  ", root.display())
        );
        assert!(
            suggestions
                .iter()
                .all(|candidate| candidate.source == CandidateSource::Path)
        );
        assert!(path_candidates("\\\\server\\share\\").is_empty());
        assert!(path_candidates("C:relative").is_empty());
        assert!(path_candidates("https://example.com/").is_empty());
        for number in 0..25 {
            fs::remove_file(root.join(format!("file{number:02}.txt"))).unwrap();
        }
        fs::remove_dir(root.join("z directory")).unwrap();
        fs::remove_dir(root.join("a directory")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
