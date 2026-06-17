//! The display-row model: a uniform view over directory entries, drives, and
//! known folders, plus human-friendly size/time formatting.

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local, Utc};
use librarian_core::{Entry, Location, extension_of};
use librarian_win::{DriveInfo, DriveKind, KnownFolder, WslDistro, distro_unc_path};

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
    /// Rendered width of `label` at the list text size, in logical pixels;
    /// cached so the details view's per-row "does the name overflow its column?"
    /// check is a float compare instead of re-shaping text every frame. `0.0`
    /// until measured — only the details view fills it.
    pub name_px: f32,
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
        name_px: 0.0,
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
        name_px: 0.0,
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
        name_px: 0.0,
    }
}

/// A row for one WSL distribution in the "Linux" landing list. Activating it
/// navigates into the distro's `\\wsl.localhost\<name>` root; it shares the
/// Linux/penguin icon with the WSL group node.
pub fn row_from_distro(distro: &WslDistro) -> Row {
    Row {
        label: distro.name.clone(),
        icon: IconKey::Wsl,
        is_container: true,
        target: Location::Path(distro_unc_path(&distro.name)),
        size: None,
        modified: None,
        type_label: "Linux distribution".to_string(),
        name_px: 0.0,
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
            name_px: 0.0,
        };
    }

    let ext = extension_of(&hit.path);
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
        name_px: 0.0,
    }
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
    ext_type_label(&entry.extension())
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
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use librarian_core::{Attributes, EntryKind};

    fn file_entry(name: &str, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from(format!(r"C:\dir\{name}")),
            kind: EntryKind::File,
            size,
            modified: None,
            created: None,
            attrs: Attributes::default(),
        }
    }

    fn dir_entry(name: &str) -> Entry {
        Entry {
            kind: EntryKind::Directory,
            ..file_entry(name, 0)
        }
    }

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1_500_000), "1.4 MB");
    }

    #[test]
    fn type_label_is_folder_or_uppercase_extension() {
        assert_eq!(type_label(&dir_entry("Photos")), "File folder");
        assert_eq!(type_label(&file_entry("notes.txt", 0)), "TXT File");
        // No extension falls back to the bare "File".
        assert_eq!(type_label(&file_entry("README", 0)), "File");
        // A leading-dot name (no real extension) is also just "File".
        assert_eq!(type_label(&file_entry(".gitignore", 0)), "File");
    }

    #[test]
    fn file_row_carries_size_type_and_extension_icon() {
        let row = row_from_entry(&file_entry("notes.txt", 42));
        assert_eq!(row.label, "notes.txt");
        assert!(!row.is_container);
        assert_eq!(row.size, Some(42));
        assert_eq!(row.type_label, "TXT File");
        assert_eq!(row.icon, IconKey::Ext("txt".to_string()));
        assert_eq!(
            row.target,
            Location::Path(PathBuf::from(r"C:\dir\notes.txt"))
        );
    }

    #[test]
    fn dir_row_is_a_container_with_no_size() {
        let row = row_from_entry(&dir_entry("Photos"));
        assert!(row.is_container);
        assert_eq!(row.size, None);
        assert_eq!(row.icon, IconKey::Folder);
        assert_eq!(row.type_label, "File folder");
    }

    #[test]
    fn drive_row_formats_label_and_falls_back_to_kind() {
        let drive = DriveInfo {
            letter: 'C',
            root: PathBuf::from(r"C:\"),
            label: "Windows".to_string(),
            kind: DriveKind::Fixed,
            total_bytes: 0,
            free_bytes: 0,
        };
        let row = row_from_drive(&drive);
        assert_eq!(row.label, "Windows (C:)");
        assert_eq!(row.type_label, "Local Disk");
        assert!(row.is_container);
        assert_eq!(row.size, None);
        assert_eq!(row.icon, IconKey::Path(PathBuf::from(r"C:\")));

        // An unlabeled drive shows its kind in place of a name.
        let unlabeled = DriveInfo {
            label: String::new(),
            kind: DriveKind::Removable,
            ..drive
        };
        let row = row_from_drive(&unlabeled);
        assert_eq!(row.label, "Removable Disk (C:)");
        assert_eq!(row.type_label, "Removable Disk");
    }

    #[test]
    fn drive_kind_names_cover_every_variant() {
        let cases = [
            (DriveKind::Fixed, "Local Disk"),
            (DriveKind::Removable, "Removable Disk"),
            (DriveKind::Network, "Network Drive"),
            (DriveKind::CdRom, "CD Drive"),
            (DriveKind::RamDisk, "RAM Disk"),
            (DriveKind::Unknown, "Disk"),
        ];
        for (kind, expected) in cases {
            assert_eq!(drive_kind_name(kind), expected);
        }
    }

    #[test]
    fn known_folder_row_uses_its_own_shell_icon() {
        let folder = KnownFolder {
            name: "Downloads",
            path: PathBuf::from(r"C:\Users\j\Downloads"),
        };
        let row = row_from_known(&folder);
        assert_eq!(row.label, "Downloads");
        assert_eq!(row.type_label, "File folder");
        assert!(row.is_container);
        assert_eq!(
            row.icon,
            IconKey::Path(PathBuf::from(r"C:\Users\j\Downloads"))
        );
    }

    #[test]
    fn distro_row_targets_the_unc_root_with_the_wsl_icon() {
        let distro = WslDistro {
            name: "Ubuntu".to_string(),
        };
        let row = row_from_distro(&distro);
        assert_eq!(row.label, "Ubuntu");
        assert_eq!(row.icon, IconKey::Wsl);
        assert_eq!(row.type_label, "Linux distribution");
        assert_eq!(row.target, Location::Path(distro_unc_path("Ubuntu")));
    }

    #[test]
    fn search_hit_label_is_relative_to_the_search_root() {
        let root = Path::new(r"C:\proj");
        let hit = SearchHit {
            path: PathBuf::from(r"C:\proj\src\main.rs"),
            matches: None,
            is_dir: false,
        };
        let row = row_from_hit(&hit, root);
        // The path is shown relative to the root, like an editor's search panel.
        assert_eq!(row.label, r"src\main.rs");
        assert!(!row.is_container);
        assert_eq!(row.type_label, "RS File");
        assert_eq!(row.icon, IconKey::Ext("rs".to_string()));
    }

    #[test]
    fn search_hit_directory_navigates() {
        let root = Path::new(r"C:\proj");
        let hit = SearchHit {
            path: PathBuf::from(r"C:\proj\src"),
            matches: None,
            is_dir: true,
        };
        let row = row_from_hit(&hit, root);
        assert!(row.is_container);
        assert_eq!(row.type_label, "File folder");
        assert_eq!(row.icon, IconKey::Folder);
    }

    #[test]
    fn contents_hit_reports_match_count_with_pluralization() {
        let root = Path::new(r"C:\proj");
        let hit = |n: u64| SearchHit {
            path: PathBuf::from(r"C:\proj\notes.txt"),
            matches: Some(n),
            is_dir: false,
        };
        assert_eq!(row_from_hit(&hit(1), root).type_label, "1 match");
        assert_eq!(row_from_hit(&hit(3), root).type_label, "3 matches");
    }

    #[test]
    fn format_time_renders_minute_precision() {
        // Local time varies by host, so assert the shape, not an exact instant:
        // `YYYY-MM-DD HH:MM`, exactly 16 chars, all fields zero-padded.
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let s = format_time(t);
        assert_eq!(s.len(), 16, "expected `YYYY-MM-DD HH:MM`, got {s:?}");
        let bytes = s.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            match i {
                4 | 7 => assert_eq!(*b, b'-'),
                10 => assert_eq!(*b, b' '),
                13 => assert_eq!(*b, b':'),
                _ => assert!(b.is_ascii_digit(), "non-digit at {i} in {s:?}"),
            }
        }
    }
}
