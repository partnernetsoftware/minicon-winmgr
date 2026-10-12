//! Small shared helpers for Win32 interop.

/// Encodes `s` as a null-terminated UTF-16 buffer for Win32 `PCWSTR` arguments.
///
/// The returned buffer must outlive any `PCWSTR` built from it.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
