//! Extracts the one executable a release `.zip` is allowed to carry.
//!
//! The archive is read as a stream from a staged file, never fully in memory. Only
//! a single regular entry whose name is exactly the expected executable name is
//! accepted; directories, symbolic links, traversal names, extra entries and
//! oversized or truncated payloads are rejected before anything reaches the
//! installer.
use std::io::{Read, Seek, Write};
use std::path::Path;

/// Largest uncompressed payload accepted, matching the bare-executable limit.
pub(crate) const MAX_EXE: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;

/// Copy the single entry `exe_name` from `archive` into `output`. Returns the number of
/// bytes written. `cancel` is checked between chunks.
pub(crate) fn extract_exe(
    archive: impl Read + Seek,
    exe_name: &str,
    output: &mut impl Write,
    cancel: impl Fn() -> Result<(), String>,
) -> Result<u64, String> {
    if !is_plain_name(exe_name) {
        return Err("expected executable name is not a plain file name".into());
    }
    let mut zip = zip::ZipArchive::new(archive).map_err(|_| "release archive is unreadable")?;
    if zip.len() != 1 {
        return Err("release archive must contain exactly one entry".into());
    }
    let mut entry = zip
        .by_index(0)
        .map_err(|_| "release archive entry is unreadable")?;
    let name = entry
        .name()
        .map_err(|_| "release archive entry name is unreadable")?;
    if name != exe_name {
        return Err("release archive entry name does not match this version".into());
    }
    if entry.is_dir() || entry.is_symlink() || !entry.is_file() {
        return Err("release archive entry is not a regular file".into());
    }
    if entry.enclosed_name().as_deref() != Some(Path::new(exe_name)) {
        return Err("release archive entry path is not allowed".into());
    }
    let declared = entry.size();
    if declared == 0 || declared > MAX_EXE {
        return Err("release archive entry has an unexpected size".into());
    }
    let mut buffer = vec![0u8; CHUNK];
    let mut written: u64 = 0;
    loop {
        cancel()?;
        let read = entry
            .read(&mut buffer)
            .map_err(|_| "release archive entry is corrupt")?;
        if read == 0 {
            break;
        }
        written += read as u64;
        if written > declared {
            return Err("release archive entry is larger than declared".into());
        }
        output
            .write_all(&buffer[..read])
            .map_err(|e| e.to_string())?;
    }
    if written != declared {
        return Err("release archive entry is truncated".into());
    }
    Ok(written)
}

/// True for a single path component with no separators, drive letters or traversal.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    const EXE: &str = "minicon-winmgr-v0.1.24-x86_64-windows.exe";

    fn archive(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes, mode) in entries {
            let mut options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            if let Some(mode) = mode {
                // Unix mode bits are only interpreted when the archive was made on Unix.
                options = options.system(zip::System::Unix).unix_permissions(*mode);
            }
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn extract(bytes: &[u8], name: &str) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        extract_exe(Cursor::new(bytes), name, &mut out, || Ok(()))?;
        Ok(out)
    }

    #[test]
    fn single_expected_entry_is_extracted_byte_for_byte() {
        let payload = vec![7u8; 100_000];
        let zip = archive(&[(EXE, &payload, None)]);
        assert_eq!(extract(&zip, EXE).unwrap(), payload);
    }

    #[test]
    fn wrong_name_extra_entries_and_directories_are_rejected() {
        let zip = archive(&[("other.exe", b"x", None)]);
        assert!(extract(&zip, EXE).is_err());
        let zip = archive(&[(EXE, b"x", None), ("extra.txt", b"y", None)]);
        assert!(extract(&zip, EXE).is_err());
        let zip = archive(&[("nested/", b"", None)]);
        assert!(extract(&zip, "nested/").is_err());
    }

    #[test]
    fn symbolic_links_are_rejected() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(
                EXE,
                "target.exe",
                SimpleFileOptions::default().system(zip::System::Unix),
            )
            .unwrap();
        let zip = writer.finish().unwrap().into_inner();
        assert!(extract(&zip, EXE).is_err());
    }

    #[test]
    fn traversal_and_non_plain_expected_names_are_rejected() {
        let zip = archive(&[("../evil.exe", b"x", None)]);
        assert!(extract(&zip, "../evil.exe").is_err());
        let zip = archive(&[(EXE, b"x", None)]);
        assert!(extract_exe(Cursor::new(&zip), "a/b.exe", &mut Vec::new(), || Ok(())).is_err());
        assert!(extract_exe(Cursor::new(&zip), "..", &mut Vec::new(), || Ok(())).is_err());
        assert!(extract_exe(Cursor::new(&zip), "", &mut Vec::new(), || Ok(())).is_err());
    }

    #[test]
    fn garbage_and_truncated_archives_fail_without_panicking() {
        assert!(extract(b"not a zip at all", EXE).is_err());
        let zip = archive(&[(EXE, &vec![3u8; 4096], None)]);
        assert!(extract(&zip[..zip.len() / 2], EXE).is_err());
    }

    #[test]
    fn cancellation_stops_extraction() {
        let zip = archive(&[(EXE, &vec![1u8; 100_000], None)]);
        let result = extract_exe(Cursor::new(&zip), EXE, &mut Vec::new(), || {
            Err("cancelled".into())
        });
        assert_eq!(result.unwrap_err(), "cancelled");
    }
}
