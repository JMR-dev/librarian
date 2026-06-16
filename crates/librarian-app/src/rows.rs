//! The display-row model: a uniform view over directory entries, drives, and
//! known folders, plus human-friendly size/time formatting.

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local, Utc};
use librarian_core::{Entry, Location};
use librarian_win::{DriveInfo, DriveKind, KnownFolder};

use crate::icons::IconKey;
use crate::search::SearchHit;

/// One row in the file list, independent of where it came from.
#[derive(Debug, Clone)]
pub struct Row {
    pub label: String,
    pub icon: IconKey,
    /// True if activating navigates into it; false if activating opens it.
    pub is_container: bool,
    /// Navigation target (containers) or the path to open (files).
    pub target: Location,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub type_label: String,
}

pub fn row_from_entry(entry: &Entry) -> Row {
    let is_dir = entry.is_dir();
    Row {
        label: entry.name.clone(),
        icon: if is_dir {
            IconKey::Folder
        } else {
            IconKey::Ext(entry.extension())
        },
        is_container: is_dir,
        target: Location::Path(entry.path.clone()),
        size: (!is_dir).then_some(entry.size),
        modified: entry.modified,
        type_label: type_label(entry),
    }
}

pub fn row_from_drive(drive: &DriveInfo) -> Row {
    let kind = drive_kind_name(drive.kind);
    let name = if drive.label.is_empty() {
        kind.to_string()
    } else {
        drive.label.clone()
    };
    Row {
        label: format!("{name} ({}:)", drive.letter),
        icon: IconKey::Path(drive.root.clone()),
        is_container: true,
        target: Location::Path(drive.root.clone()),
        size: None,
        modified: None,
        type_label: kind.to_string(),
    }
}

pub fn row_from_known(folder: &KnownFolder) -> Row {
    Row {
        label: folder.name.to_string(),
        // Resolve the folder's real shell icon (Desktop/Documents/Downloads/…
        // each have a distinct one in Explorer), like drives do, instead of the
        // generic folder glyph.
        icon: IconKey::Path(folder.path.clone()),
        is_container: true,
        target: Location::Path(folder.path.clone()),
        size: None,
        modified: None,
        type_label: "File folder".to_string(),
    }
}

/// A row for one search result. The label is the path *relative to the search
/// root* (so the result's location is visible at a glance, like an editor's
/// search panel). A directory hit navigates when activated; a file hit opens.
/// For contents searches `matches` is the hit count, surfaced in the Type column.
pub fn row_from_hit(hit: &SearchHit, root: &Path) -> Row {
    let label = hit
        .path
        .strip_prefix(root)
        .unwrap_or(&hit.path)
        .to_string_lossy()
        .into_owned();

    if hit.is_dir {
        return Row {
            label,
            icon: IconKey::Folder,
            is_container: true,
            target: Location::Path(hit.path.clone()),
            size: None,
            modified: None,
            type_label: "File folder".to_string(),
        };
    }

    let ext = file_extension(&hit.path);
    let type_label = match hit.matches {
        Some(n) => format!("{} match{}", n, if n == 1 { "" } else { "es" }),
        None => ext_type_label(&ext),
    };
    Row {
        label,
        icon: IconKey::Ext(ext),
        is_container: false,
        target: Location::Path(hit.path.clone()),
        size: None,
        modified: None,
        type_label,
    }
}

/// Lowercase extension without the dot, or `""` for none.
fn file_extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Human "type" for a file with the given (lowercase, dotless) extension.
fn ext_type_label(ext: &str) -> String {
    if ext.is_empty() {
        "File".to_string()
    } else {
        format!("{} File", ext.to_uppercase())
    }
}

fn type_label(entry: &Entry) -> String {
    if entry.is_dir() {
        return "File folder".to_string();
    }
    let ext = entry.extension();
    if ext.is_empty() {
        "File".to_string()
    } else {
        format!("{} File", ext.to_uppercase())
    }
}

fn drive_kind_name(kind: DriveKind) -> &'static str {
    match kind {
        DriveKind::Fixed => "Local Disk",
        DriveKind::Removable => "Removable Disk",
        DriveKind::Network => "Network Drive",
        DriveKind::CdRom => "CD Drive",
        DriveKind::RamDisk => "RAM Disk",
        DriveKind::Unknown => "Disk",
    }
}

/// Format a byte count like Explorer's Size column (e.g. `1.4 MB`).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// Format a timestamp in local time (e.g. `2026-06-13 09:41`).
pub fn format_time(time: SystemTime) -> String {
    let utc: DateTime<Utc> = time.into();
    utc.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1_500_000), "1.4 MB");
    }
}
