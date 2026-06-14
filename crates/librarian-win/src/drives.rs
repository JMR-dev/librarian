//! Logical drive enumeration for the "This PC" root.
//!
//! These are plain Win32 calls (no COM), so they're safe to call from any
//! thread; we still typically route them through the worker for a single shell
//! access path. Label and capacity queries are best-effort: an empty CD/card
//! reader legitimately fails them, in which case the drive still appears with a
//! blank label and zero sizes.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};

use crate::util::{to_wide, wide_to_string};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Removable,
    Fixed,
    Network,
    CdRom,
    RamDisk,
    Unknown,
}

impl DriveKind {
    // Values from the Win32 DRIVE_* constants returned by GetDriveType.
    fn from_raw(raw: u32) -> Self {
        match raw {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::CdRom,
            6 => DriveKind::RamDisk,
            _ => DriveKind::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DriveInfo {
    /// Drive letter, e.g. `'C'`.
    pub letter: char,
    /// Root path, e.g. `C:\`.
    pub root: PathBuf,
    /// Volume label, or empty if unavailable.
    pub label: String,
    pub kind: DriveKind,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Enumerate all mounted logical drives.
pub fn list_drives() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();

    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root_str = format!("{letter}:\\");
        let wide = to_wide(&root_str);
        let root = PCWSTR(wide.as_ptr());

        // SAFETY: `wide` outlives every call below that borrows `root`.
        let kind = DriveKind::from_raw(unsafe { GetDriveTypeW(root) });

        let mut label_buf = [0u16; 256];
        let label = unsafe {
            match GetVolumeInformationW(
                root,
                Some(&mut label_buf),
                None,
                None,
                None,
                None,
            ) {
                Ok(()) => wide_to_string(&label_buf),
                Err(_) => String::new(),
            }
        };

        let mut total = 0u64;
        let mut free = 0u64;
        unsafe {
            let _ = GetDiskFreeSpaceExW(root, None, Some(&mut total), Some(&mut free));
        }

        drives.push(DriveInfo {
            letter,
            root: PathBuf::from(&root_str),
            label,
            kind,
            total_bytes: total,
            free_bytes: free,
        });
    }

    drives
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_system_drive() {
        let drives = list_drives();
        assert!(!drives.is_empty(), "expected at least one mounted drive");
        assert!(
            drives.iter().any(|d| d.letter == 'C'),
            "expected a C: drive on a standard Windows install"
        );
    }
}
