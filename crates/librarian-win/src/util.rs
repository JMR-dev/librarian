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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_encodes_and_nul_terminates() {
        // ASCII encodes one u16 per char, with a trailing NUL the FFI needs.
        assert_eq!(to_wide("Hi"), vec![0x48, 0x69, 0x00]);
        // The empty string is just the terminator.
        assert_eq!(to_wide(""), vec![0x00]);
    }

    #[test]
    fn wide_to_string_stops_at_first_nul() {
        // Bytes past the NUL (here a stray 'X') are ignored — buffers come back
        // from Win32 padded with garbage after the terminator.
        assert_eq!(wide_to_string(&[0x48, 0x69, 0x00, 0x58]), "Hi");
        // No NUL at all: the whole buffer is decoded.
        assert_eq!(wide_to_string(&[0x48, 0x69]), "Hi");
        // A leading NUL is the empty string.
        assert_eq!(wide_to_string(&[0x00, 0x69]), "");
    }

    #[test]
    fn round_trips_through_utf16_including_non_ascii() {
        for s in ["", "C:\\Windows", "Ubuntu", "café — résumé", "日本語"] {
            assert_eq!(wide_to_string(&to_wide(s)), s);
        }
    }
}
