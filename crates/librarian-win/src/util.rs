//! Small helpers shared across the Windows wrappers.

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for `PCWSTR` args.
pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decode a (possibly NUL-terminated) UTF-16 buffer into a `String`, stopping
/// at the first NUL.
pub(crate) fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
