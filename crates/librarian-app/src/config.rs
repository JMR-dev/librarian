//! Persistent view preferences, stored in a predictable, optionally-portable
//! location.
//!
//! Resolution order for the config file:
//! 1. **Portable** — `librarian.config` next to the executable, *if it already
//!    exists*. An installer (or the user) can drop the file there to opt the
//!    install into portable mode; we then read and write it in place.
//! 2. **Roaming** — `%APPDATA%\Librarian\librarian.config` otherwise.
//!
//! The format is a tiny `key=value` text file: no dependency on a serializer,
//! and trivial to inspect or hand-edit. Unknown keys and parse failures are
//! ignored so a malformed or future-version file degrades to defaults rather
//! than erroring.

use std::collections::HashMap;
use std::path::PathBuf;

use librarian_core::{Sort, SortKey, SortOrder};

use crate::ViewMode;
use crate::columns::{ColumnLayout, decode_layout, encode_layout};

const FILE_NAME: &str = "librarian.config";
/// Per-folder details-column widths live in their own file (a variable-length
/// map, unlike the fixed `Settings` keys), in the same portable/roaming location.
const COLUMNS_FILE_NAME: &str = "librarian-columns.config";
const APP_DIR: &str = "Librarian";

/// The persisted, user-facing view preferences. Defaults match a fresh install:
/// hidden items off, name-ascending sort, details view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub show_hidden: bool,
    pub sort: Sort,
    pub view_mode: ViewMode,
}

/// Load saved settings, falling back to defaults if none are stored or the file
/// can't be read or parsed.
pub fn load() -> Settings {
    let Some(path) = config_path() else {
        return Settings::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(_) => Settings::default(),
    }
}

/// Persist `settings`, creating the parent directory if needed. Best-effort:
/// failures (e.g. a read-only location) are silently ignored.
pub fn save(settings: &Settings) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serialize(settings));
}

/// Resolve a config file path by name: a pre-existing portable file (beside the
/// executable) wins, else the per-user roaming location. Both config files
/// (settings and columns) share this resolution so they stay together.
fn config_path() -> Option<PathBuf> {
    config_path_for(FILE_NAME)
}

fn config_path_for(file_name: &str) -> Option<PathBuf> {
    if let Some(portable) = portable_path(file_name)
        && portable.exists()
    {
        return Some(portable);
    }
    roaming_path(file_name)
}

/// `<file_name>` beside the executable.
fn portable_path(file_name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(file_name))
}

/// `%APPDATA%\Librarian\<file_name>`.
fn roaming_path(file_name: &str) -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(APP_DIR).join(file_name))
}

/// Load the per-folder column layouts, keyed by folder path. Missing/unreadable
/// file or malformed lines degrade to an empty map (defaults everywhere).
pub fn load_columns() -> HashMap<PathBuf, ColumnLayout> {
    let Some(path) = config_path_for(COLUMNS_FILE_NAME) else {
        return HashMap::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_columns(&text),
        Err(_) => HashMap::new(),
    }
}

/// Persist the per-folder column layouts. Best-effort, like [`save`].
pub fn save_columns(columns: &HashMap<PathBuf, ColumnLayout>) {
    let Some(path) = config_path_for(COLUMNS_FILE_NAME) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serialize_columns(columns));
}

/// One folder per line: `<encoded-layout>|<path>`. A Windows path can't contain
/// `|`, so the first `|` cleanly separates the (delimiter-free) layout fields
/// from the path, which may contain any other character.
fn serialize_columns(columns: &HashMap<PathBuf, ColumnLayout>) -> String {
    let mut out = String::new();
    for (path, layout) in columns {
        // An all-default layout carries no information; never write it.
        if layout.is_default() {
            continue;
        }
        out.push_str(&encode_layout(layout));
        out.push('|');
        out.push_str(&path.to_string_lossy());
        out.push('\n');
    }
    out
}

fn parse_columns(text: &str) -> HashMap<PathBuf, ColumnLayout> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let Some((encoded, path)) = line.split_once('|') else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if let Some(layout) = decode_layout(encoded.trim()) {
            map.insert(PathBuf::from(path), layout);
        }
    }
    map
}

fn serialize(settings: &Settings) -> String {
    format!(
        "show_hidden={}\nsort_key={}\nsort_order={}\nview_mode={}\n",
        settings.show_hidden,
        sort_key_str(settings.sort.key),
        sort_order_str(settings.sort.order),
        view_mode_str(settings.view_mode),
    )
}

fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "show_hidden" => settings.show_hidden = value == "true",
            "sort_key" => {
                if let Some(key) = sort_key_from(value) {
                    settings.sort.key = key;
                }
            }
            "sort_order" => {
                if let Some(order) = sort_order_from(value) {
                    settings.sort.order = order;
                }
            }
            "view_mode" => {
                if let Some(mode) = view_mode_from(value) {
                    settings.view_mode = mode;
                }
            }
            _ => {}
        }
    }
    settings
}

fn sort_key_str(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Modified => "modified",
        SortKey::Type => "type",
        SortKey::Size => "size",
    }
}

fn sort_key_from(value: &str) -> Option<SortKey> {
    match value {
        "name" => Some(SortKey::Name),
        "modified" => Some(SortKey::Modified),
        "type" => Some(SortKey::Type),
        "size" => Some(SortKey::Size),
        _ => None,
    }
}

fn sort_order_str(order: SortOrder) -> &'static str {
    match order {
        SortOrder::Ascending => "asc",
        SortOrder::Descending => "desc",
    }
}

fn sort_order_from(value: &str) -> Option<SortOrder> {
    match value {
        "asc" => Some(SortOrder::Ascending),
        "desc" => Some(SortOrder::Descending),
        _ => None,
    }
}

fn view_mode_str(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::Details => "details",
        ViewMode::Tiny => "tiny",
        ViewMode::Small => "small",
        ViewMode::Medium => "medium",
        ViewMode::Large => "large",
        ViewMode::ExtraLarge => "xlarge",
    }
}

fn view_mode_from(value: &str) -> Option<ViewMode> {
    match value {
        "details" => Some(ViewMode::Details),
        "tiny" => Some(ViewMode::Tiny),
        "small" => Some(ViewMode::Small),
        "medium" => Some(ViewMode::Medium),
        "large" => Some(ViewMode::Large),
        "xlarge" => Some(ViewMode::ExtraLarge),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_non_default_settings() {
        let settings = Settings {
            show_hidden: true,
            sort: Sort {
                key: SortKey::Size,
                order: SortOrder::Descending,
                ..Sort::default()
            },
            view_mode: ViewMode::ExtraLarge,
        };
        assert_eq!(parse(&serialize(&settings)), settings);
    }

    #[test]
    fn unknown_keys_and_garbage_fall_back_to_defaults() {
        let text = "show_hidden=true\nfuture_key=whatever\nsort_key=bogus\n# comment\n";
        let parsed = parse(text);
        assert!(parsed.show_hidden); // recognized key still applied
        assert_eq!(parsed.sort.key, Sort::default().key); // bad value ignored
    }

    #[test]
    fn empty_input_is_all_defaults() {
        assert_eq!(parse(""), Settings::default());
    }

    #[test]
    fn columns_round_trip_keeping_paths_with_separators() {
        use crate::columns::ColRule;
        let mut map = HashMap::new();
        // A path with a space and a semicolon (both legal on Windows) survives,
        // since only the first `|` splits layout from path.
        map.insert(
            PathBuf::from(r"C:\Users\jay\My ; Docs"),
            ColumnLayout {
                name: ColRule::Fixed(240.0),
                modified: ColRule::Auto,
                type_: ColRule::Fixed(120.0),
                size: ColRule::Auto,
            },
        );
        assert_eq!(parse_columns(&serialize_columns(&map)), map);
    }

    #[test]
    fn default_layouts_are_not_written() {
        let mut map = HashMap::new();
        map.insert(PathBuf::from(r"C:\plain"), ColumnLayout::default());
        assert!(serialize_columns(&map).is_empty());
    }
}
