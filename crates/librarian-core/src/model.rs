//! Core domain types: filesystem entries, locations, and their attributes.
//!
//! These are deliberately plain data — no UI, no `unsafe`, no Win32 handles.
//! Windows file attributes are read from the standard library's cached
//! `MetadataExt` so enumeration needs no extra syscalls per entry.

use std::path::{Component, Path, PathBuf, Prefix};
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

/// A browsable location. `ThisPc` and `Wsl` are the virtual roots — `ThisPc`
/// holds the drives (and known folders), `Wsl` the installed Linux distros;
/// everything reachable below a real folder (including inside a distro, via its
/// `\\wsl.localhost\` UNC path) is a plain `Path`. More virtual roots (Recycle
/// Bin, Network) can be added here later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Path(PathBuf),
    ThisPc,
    /// The "Linux" group: the landing list of WSL distributions.
    Wsl,
}

impl Location {
    /// Short label for the address bar / window title.
    pub fn label(&self) -> String {
        match self {
            Location::ThisPc => "This PC".to_string(),
            Location::Wsl => "Linux".to_string(),
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
            Location::ThisPc | Location::Wsl => None,
        }
    }

    /// Parse user input from the address bar into a location.
    pub fn parse(input: &str) -> Location {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("This PC") {
            Location::ThisPc
        } else if trimmed.eq_ignore_ascii_case("Linux") || trimmed.eq_ignore_ascii_case("WSL") {
            Location::Wsl
        } else {
            Location::Path(PathBuf::from(trimmed))
        }
    }

    /// The parent location, used by the "Up" command. The parent of a drive
    /// root (or any path with no parent) is `ThisPc`; the parent of a distro
    /// root (`\\wsl.localhost\<name>`) is the `Wsl` group.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::ThisPc | Location::Wsl => None,
            Location::Path(p) if is_wsl_root_path(p) => Some(Location::Wsl),
            Location::Path(p) => match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => {
                    Some(Location::Path(parent.to_path_buf()))
                }
                _ => Some(Location::ThisPc),
            },
        }
    }
}

/// Whether `p` is the root of a WSL distro — a `\\wsl.localhost\<name>` (or
/// legacy `\\wsl$\<name>`) UNC path with nothing below the distro share. Such a
/// path has no filesystem parent, so "Up" routes to the `Wsl` group instead.
fn is_wsl_root_path(p: &Path) -> bool {
    let mut comps = p.components();
    let Some(Component::Prefix(prefix)) = comps.next() else {
        return false;
    };
    let Prefix::UNC(server, _share) = prefix.kind() else {
        return false;
    };
    let server = server.to_string_lossy();
    if !server.eq_ignore_ascii_case("wsl.localhost") && !server.eq_ignore_ascii_case("wsl$") {
        return false;
    }
    // The UNC prefix already includes the distro (the share); a bare distro root
    // leaves only the root directory, with no further components below it.
    matches!(comps.next(), Some(Component::RootDir)) && comps.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_virtual_roots() {
        assert_eq!(Location::parse(""), Location::ThisPc);
        assert_eq!(Location::parse("This PC"), Location::ThisPc);
        assert_eq!(Location::parse("linux"), Location::Wsl);
        assert_eq!(Location::parse("WSL"), Location::Wsl);
        assert_eq!(
            Location::parse(r"\\wsl.localhost\Ubuntu"),
            Location::Path(PathBuf::from(r"\\wsl.localhost\Ubuntu"))
        );
    }

    #[test]
    fn wsl_group_labels_and_has_no_path_or_parent() {
        assert_eq!(Location::Wsl.label(), "Linux");
        assert_eq!(Location::Wsl.as_path(), None);
        assert_eq!(Location::Wsl.parent(), None);
    }

    #[test]
    fn distro_root_parent_is_the_wsl_group() {
        let root = Location::Path(PathBuf::from(r"\\wsl.localhost\Ubuntu"));
        assert_eq!(root.parent(), Some(Location::Wsl));
        // The legacy alias resolves the same way.
        let legacy = Location::Path(PathBuf::from(r"\\wsl$\Ubuntu"));
        assert_eq!(legacy.parent(), Some(Location::Wsl));
    }

    #[test]
    fn inside_a_distro_walks_up_normally() {
        let sub = Location::Path(PathBuf::from(r"\\wsl.localhost\Ubuntu\home"));
        assert_eq!(
            sub.parent(),
            Some(Location::Path(PathBuf::from(r"\\wsl.localhost\Ubuntu")))
        );
    }

    #[test]
    fn ordinary_unc_share_is_not_a_distro_root() {
        // A normal network share must keep its This PC fallback, not become Wsl.
        let share = Location::Path(PathBuf::from(r"\\server\share"));
        assert_eq!(share.parent(), Some(Location::ThisPc));
    }
}
