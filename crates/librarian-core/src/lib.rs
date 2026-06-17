//! `librarian-core` — OS-agnostic domain logic for the Librarian file explorer.
//!
//! This crate holds the safe, UI-free core: the domain model (entries,
//! locations, navigation history), directory enumeration, and sort/filter
//! logic. All `unsafe` Win32/COM lives in `librarian-win`; all UI lives in
//! `librarian-app`. Keeping this crate free of both makes it reusable (the
//! external installer/launcher project can depend on it) and unit-testable.

pub mod enumerate;
pub mod history;
pub mod matcher;
pub mod model;
pub mod sort;

pub use enumerate::{DEFAULT_BATCH, read_dir_all, read_dir_batched, read_subdirs};
pub use history::History;
pub use matcher::{NameMatcher, find_matching_dirs};
pub use model::{Attributes, Entry, EntryKind, Location, extension_of, is_wsl_host};
pub use sort::{Sort, SortKey, SortOrder, cmp_name_str, is_visible, sort_entries};
