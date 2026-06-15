//! `librarian-core` — OS-agnostic domain logic for the Librarian file explorer.
//!
//! This crate holds the safe, UI-free core: the domain model (entries,
//! locations, navigation history), directory enumeration, and sort/filter
//! logic. All `unsafe` Win32/COM lives in `librarian-win`; all UI lives in
//! `librarian-app`. Keeping this crate free of both makes it reusable (the
//! external installer/launcher project can depend on it) and unit-testable.

pub mod enumerate;
pub mod history;
pub mod model;
pub mod sort;

pub use enumerate::{read_dir_all, read_dir_batched, read_subdirs, DEFAULT_BATCH};
pub use history::History;
pub use model::{Attributes, Entry, EntryKind, Location};
pub use sort::{is_visible, sort_entries, Sort, SortKey, SortOrder};
