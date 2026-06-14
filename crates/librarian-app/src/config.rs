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

use std::path::PathBuf;

use librarian_core::{Sort, SortKey, SortOrder};

const FILE_NAME: &str = "librarian.config";
const APP_DIR: &str = "Librarian";

/// The persisted, user-facing view preferences. Defaults match a fresh install:
/// hidden items off, name-ascending sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub show_hidden: bool,
    pub sort: Sort,
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

/// Resolve the config file path: a pre-existing portable file wins, else the
/// per-user roaming location.
fn config_path() -> Option<PathBuf> {
    if let Some(portable) = portable_path()
        && portable.exists()
    {
        return Some(portable);
    }
    roaming_path()
}

/// `librarian.config` beside the executable.
fn portable_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(FILE_NAME))
}

/// `%APPDATA%\Librarian\librarian.config`.
fn roaming_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(APP_DIR).join(FILE_NAME))
}

fn serialize(settings: &Settings) -> String {
    format!(
        "show_hidden={}\nsort_key={}\nsort_order={}\n",
        settings.show_hidden,
        sort_key_str(settings.sort.key),
        sort_order_str(settings.sort.order),
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
}
