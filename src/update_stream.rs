//! Bounded streaming executable validation and copying. No executable-sized Vec.
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use windows::Win32::Security::Cryptography::*;

const CHUNK: usize = 16 * 1024;
struct Algorithm(BCRYPT_ALG_HANDLE);
impl Drop for Algorithm {
    fn drop(&mut self) {
        let _ = unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
    }
}
struct Hash(BCRYPT_HASH_HANDLE);
impl Drop for Hash {
    fn drop(&mut self) {
        let _ = unsafe { BCryptDestroyHash(self.0) };
    }
}

fn transfer(
    mut reader: impl Read,
    mut writer: Option<&mut dyn Write>,
    limit: usize,
) -> Result<String, String> {
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            None,
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        )
    }
    .ok()
    .map_err(|e| e.to_string())?;
    let algorithm = Algorithm(algorithm);
    let mut hash = BCRYPT_HASH_HANDLE::default();
    unsafe { BCryptCreateHash(algorithm.0, &mut hash, None, None, 0) }
        .ok()
        .map_err(|e| e.to_string())?;
    // Drop order: hash (including CNG-owned object memory), then algorithm.
    let hash = Hash(hash);
    let mut chunk = [0u8; CHUNK];
    let mut total = 0usize;
    loop {
        let request = CHUNK.min(limit.saturating_sub(total).saturating_add(1));
        let count = reader
            .read(&mut chunk[..request])
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count).ok_or("update size overflow")?;
        if total > limit {
            return Err("update file exceeds size limit".into());
        }
        unsafe { BCryptHashData(hash.0, &chunk[..count], 0) }
            .ok()
            .map_err(|e| e.to_string())?;
        if let Some(writer) = writer.as_mut() {
            writer
                .write_all(&chunk[..count])
                .map_err(|e| e.to_string())?;
        }
    }
    let mut digest = [0u8; 32];
    unsafe { BCryptFinishHash(hash.0, &mut digest, 0) }
        .ok()
        .map_err(|e| e.to_string())?;
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn hash_reader(reader: impl Read, limit: usize) -> Result<String, String> {
    transfer(reader, None, limit)
}
pub(crate) fn hash_file(path: &Path) -> Result<String, String> {
    hash_reader(
        fs::File::open(path).map_err(|e| e.to_string())?,
        crate::update::MAX_EXE,
    )
}
pub(crate) fn copy_hashed(source: &Path, target: &Path) -> Result<String, String> {
    let source = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| e.to_string())?;
    let hash = transfer(source, Some(&mut target), crate::update::MAX_EXE)?;
    target.sync_all().map_err(|e| e.to_string())?;
    Ok(hash)
}

pub(crate) fn verify_reader(mut reader: impl Read + Seek, expected: &str) -> Result<(), String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if length < 256 || length > crate::update::MAX_EXE as u64 {
        return Err("not a Windows executable".into());
    }
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut dos = [0u8; 64];
    reader.read_exact(&mut dos).map_err(|e| e.to_string())?;
    if &dos[..2] != b"MZ" {
        return Err("not a Windows executable".into());
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;
    if offset.saturating_add(26) > length {
        return Err("invalid PE offset".into());
    }
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut header = [0u8; 26];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..4] != b"PE\0\0" || header[4..6] != [0x64, 0x86] || header[24..26] != [0x0b, 0x02] {
        return Err("not an x64 PE executable".into());
    }
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    if hash_reader(reader, crate::update::MAX_EXE)? != expected {
        return Err("SHA-256 mismatch".into());
    }
    Ok(())
}
pub(crate) fn verify_file(path: &Path, expected: &str) -> Result<(), String> {
    verify_reader(fs::File::open(path).map_err(|e| e.to_string())?, expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_hash_matches_known_vectors_and_probes_only_one_byte_over_limit() {
        assert_eq!(
            hash_reader(std::io::empty(), 0).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut reader = std::io::repeat(b'a').take(1_000_000);
        assert_eq!(
            hash_reader(&mut reader, 1_000_000).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        let mut reader = std::io::repeat(0).take(1_000_000);
        assert!(hash_reader(&mut reader, 1024).is_err());
        assert_eq!(reader.limit(), 1_000_000 - 1025);
    }
    #[test]
    fn source_and_destination_failures_propagate_without_unbounded_buffering() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("source failure"))
            }
        }
        assert!(
            hash_reader(Broken, 100)
                .unwrap_err()
                .contains("source failure")
        );
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(
            transfer(std::io::repeat(1).take(100_000), Some(&mut Full), 100_000)
                .unwrap_err()
                .contains("disk full")
        );
    }
}
