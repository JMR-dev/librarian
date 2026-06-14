//! Sorting and filtering of directory listings.
//!
//! Name comparison is case-insensitive lexical for now. Explorer uses
//! `StrCmpLogicalW` (natural numeric ordering, e.g. `file2 < file10`); that's a
//! Win32 call, so it can be injected later from `librarian-win` as a custom
//! comparator without changing callers.

use std::cmp::Ordering;

use crate::model::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Modified,
    Type,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub order: SortOrder,
    /// Keep directories grouped above files regardless of the sort key.
    pub folders_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            order: SortOrder::Ascending,
            folders_first: true,
        }
    }
}

/// Sort `entries` in place according to `sort`.
pub fn sort_entries(entries: &mut [Entry], sort: &Sort) {
    entries.sort_by(|a, b| {
        if sort.folders_first {
            match (a.is_dir(), b.is_dir()) {
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }
        }

        let ordering = match sort.key {
            SortKey::Name => cmp_name(a, b),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Size => a.size.cmp(&b.size),
            // Group by type, then break ties by name for a stable, readable list.
            SortKey::Type => a
                .extension()
                .cmp(&b.extension())
                .then_with(|| cmp_name(a, b)),
        };

        match sort.order {
            SortOrder::Ascending => ordering,
            SortOrder::Descending => ordering.reverse(),
        }
    });
}

fn cmp_name(a: &Entry, b: &Entry) -> Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase())
}

/// Decide whether an entry should be visible given the current view options.
///
/// `name_filter` is the type-to-filter / search box text; an empty filter
/// matches everything. Matching is case-insensitive substring.
pub fn is_visible(entry: &Entry, show_hidden: bool, name_filter: &str) -> bool {
    if !show_hidden && (entry.attrs.hidden || entry.attrs.system) {
        return false;
    }
    if !name_filter.is_empty()
        && !entry
            .name
            .to_lowercase()
            .contains(&name_filter.to_lowercase())
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attributes, EntryKind};
    use std::path::PathBuf;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from(name),
            kind,
            size,
            modified: None,
            created: None,
            attrs: Attributes::default(),
        }
    }

    #[test]
    fn folders_sort_before_files() {
        let mut v = vec![
            entry("zeta.txt", EntryKind::File, 10),
            entry("alpha", EntryKind::Directory, 0),
        ];
        sort_entries(&mut v, &Sort::default());
        assert_eq!(v[0].name, "alpha");
        assert_eq!(v[1].name, "zeta.txt");
    }

    #[test]
    fn name_sort_is_case_insensitive() {
        let mut v = vec![
            entry("banana.txt", EntryKind::File, 1),
            entry("Apple.txt", EntryKind::File, 1),
        ];
        sort_entries(&mut v, &Sort::default());
        assert_eq!(v[0].name, "Apple.txt");
    }

    #[test]
    fn descending_size_orders_largest_first() {
        let sort = Sort {
            key: SortKey::Size,
            order: SortOrder::Descending,
            folders_first: false,
        };
        let mut v = vec![
            entry("small", EntryKind::File, 1),
            entry("big", EntryKind::File, 100),
        ];
        sort_entries(&mut v, &sort);
        assert_eq!(v[0].name, "big");
    }

    #[test]
    fn hidden_and_system_filtered_unless_shown() {
        let mut e = entry("secret", EntryKind::File, 0);
        e.attrs.hidden = true;
        assert!(!is_visible(&e, false, ""));
        assert!(is_visible(&e, true, ""));
    }

    #[test]
    fn name_filter_matches_substring_case_insensitively() {
        let e = entry("Report.PDF", EntryKind::File, 0);
        assert!(is_visible(&e, true, "report"));
        assert!(is_visible(&e, true, "pdf"));
        assert!(!is_visible(&e, true, "xls"));
    }
}
