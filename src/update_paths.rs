//! Versioned downloads must not keep an obsolete version in their installed name.
use crate::shell_registry::RawShell;
use std::path::{Path, PathBuf};

fn version(value: &str) -> bool {
    value.len() <= 32
        && value.split('.').count() == 3
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

pub(crate) fn destination(target: &Path, release: &str) -> Result<PathBuf, String> {
    if !version(release) {
        return Err("invalid installation version".into());
    }
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid installation filename")?
        .to_ascii_lowercase();
    let versioned = name
        .strip_prefix("minicon-winmgr-v")
        .and_then(|name| name.strip_suffix("-x86_64-windows.exe"))
        .is_some_and(version);
    Ok(if versioned {
        target.with_file_name(format!("minicon-winmgr-v{release}-x86_64-windows.exe"))
    } else {
        // A stable or explicitly customized name has no obsolete version to fix.
        target.to_path_buf()
    })
}

fn comparable(path: &str) -> String {
    path.strip_prefix(r"\\?\").unwrap_or(path).to_lowercase()
}

fn replacement(raw: &RawShell, old: &Path, new: &Path) -> Option<RawShell> {
    let text = raw.text().ok()?;
    let command = text.trim();
    let executable = if let Some(quoted) = command.strip_prefix('"') {
        let end = quoted.find('"')?;
        if !quoted[end + 1..].trim().is_empty() {
            return None;
        }
        &quoted[..end]
    } else {
        command
    };
    if comparable(executable) != comparable(&old.to_string_lossy()) {
        return None;
    }
    let mut result = RawShell::string(&format!("\"{}\"", new.display()));
    result.kind = raw.kind;
    Some(result)
}

pub(crate) struct LoginRedirect {
    before: RawShell,
    after: RawShell,
}

impl LoginRedirect {
    pub(crate) fn apply(old: &Path, new: &Path) -> Result<Option<Self>, String> {
        if old == new {
            return Ok(None);
        }
        let operation = crate::shell_recovery::begin().map_err(|error| error.to_string())?;
        let current = crate::shell_registry::current_raw().map_err(|error| error.to_string())?;
        let Some(before) = current.as_ref() else {
            return Ok(None);
        };
        let Some(after) = replacement(before, old, new) else {
            return Ok(None);
        };
        operation
            .ensure_original(&current)
            .map_err(|error| error.to_string())?;
        operation
            .write(&current, Some(&after))
            .map_err(|error| error.to_string())?;
        Ok(Some(Self {
            before: before.clone(),
            after,
        }))
    }

    pub(crate) fn rollback(&self) -> Result<(), String> {
        let operation = crate::shell_recovery::begin().map_err(|error| error.to_string())?;
        operation
            .write(&Some(self.after.clone()), Some(&self.before))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_names_follow_the_release_but_custom_names_are_not_reinterpreted() {
        let root = Path::new(r"C:\Apps With Spaces\张三");
        for name in [
            "minicon-winmgr-v0.1.12-x86_64-windows.exe",
            "MINICON-WINMGR-V0.1.14-X86_64-WINDOWS.EXE",
        ] {
            assert_eq!(
                destination(&root.join(name), "0.1.15").unwrap(),
                root.join("minicon-winmgr-v0.1.15-x86_64-windows.exe")
            );
        }
        for name in [
            "minicon-winmgr.exe",
            "custom.exe",
            "minicon-winmgr-vnotes-x86_64-windows.exe",
        ] {
            assert_eq!(
                destination(&root.join(name), "0.1.15").unwrap(),
                root.join(name)
            );
        }
        for invalid in ["../other", "0.1.15/evil", "0.1", "0.1.15-beta"] {
            assert!(destination(&root.join("minicon-winmgr.exe"), invalid).is_err());
        }
    }

    #[test]
    fn login_redirect_matches_only_the_exact_old_executable_and_preserves_type() {
        let old = Path::new(r"\\?\C:\Apps\minicon-winmgr-v0.1.12-x86_64-windows.exe");
        let new = old.with_file_name("minicon-winmgr-v0.1.15-x86_64-windows.exe");
        let mut original =
            RawShell::string(r#"  "c:\apps\minicon-winmgr-v0.1.12-x86_64-windows.exe"  "#);
        original.kind = 2;
        let changed = replacement(&original, old, &new).unwrap();
        assert_eq!(changed.kind, 2);
        assert_eq!(changed.text().unwrap(), format!("\"{}\"", new.display()));
        for other in [
            r"C:\Other\minicon-winmgr-v0.1.12-x86_64-windows.exe",
            "explorer.exe",
            r#""c:\apps\minicon-winmgr-v0.1.12-x86_64-windows.exe" --custom"#,
        ] {
            assert!(replacement(&RawShell::string(other), old, &new).is_none());
        }
    }
}
