//! Core domain types: filesystem entries, locations, and their attributes.
//!
//! These are deliberately plain data — no UI, no `unsafe`, no Win32 handles.
//! Windows file attributes are read from the standard library's cached
//! `MetadataExt` so enumeration needs no extra syscalls per entry.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

// Raw Windows file-attribute bits (see `MetadataExt::file_attributes`). Defined
// locally so this crate stays free of the `windows` dependency.
const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// Whether an entry is a directory or a regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
}

/// Display-relevant Windows file attributes, decoded from the raw bitmask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attributes {
    pub hidden: bool,
    pub system: bool,
    pub readonly: bool,
    /// Reparse point — a symlink, junction, or cloud placeholder.
    pub reparse: bool,
}

impl Attributes {
    /// Decode the raw attribute bitmask from `MetadataExt::file_attributes`.
    pub fn from_raw(bits: u32) -> Self {
        Self {
            hidden: bits & FILE_ATTRIBUTE_HIDDEN != 0,
            system: bits & FILE_ATTRIBUTE_SYSTEM != 0,
            readonly: bits & FILE_ATTRIBUTE_READONLY != 0,
            reparse: bits & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        }
    }
}

/// A single item in a directory listing.
#[derive(Debug, Clone)]
pub struct Entry {
    /// File name only (no parent path).
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Size in bytes; `0` for directories.
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub attrs: Attributes,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory)
    }

    /// Lowercase extension without the dot, or `""` for none/dotfiles.
    pub fn extension(&self) -> String {
        if self.is_dir() {
            return String::new();
        }
        Path::new(&self.name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default()
    }
}

/// A browsable location. `ThisPc` is the virtual root (drives + known folders);
/// everything reachable below a real folder is a plain `Path`. More virtual
/// roots (Recycle Bin, Network) can be added here later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Path(PathBuf),
    ThisPc,
}

impl Location {
    /// Short label for the address bar / window title.
    pub fn label(&self) -> String {
        match self {
            Location::ThisPc => "This PC".to_string(),
            Location::Path(p) => p
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                // Drive roots (`C:\`) have no file_name; show the whole path.
                .unwrap_or_else(|| p.display().to_string()),
        }
    }

    /// The real filesystem path, if this location maps to one.
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            Location::Path(p) => Some(p),
            Location::ThisPc => None,
        }
    }

    /// Parse user input from the address bar into a location.
    pub fn parse(input: &str) -> Location {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("This PC") {
            Location::ThisPc
        } else {
            Location::Path(PathBuf::from(trimmed))
        }
    }

    /// The parent location, used by the "Up" command. The parent of a drive
    /// root (or any path with no parent) is `ThisPc`.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::ThisPc => None,
            Location::Path(p) => match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => {
                    Some(Location::Path(parent.to_path_buf()))
                }
                _ => Some(Location::ThisPc),
            },
        }
    }
}
