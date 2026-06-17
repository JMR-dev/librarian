//! Directory enumeration.
//!
//! Built on `std::fs::read_dir`, which on Windows is backed by
//! `FindFirstFile`/`FindNextFile`. Crucially we read size/time/attributes from
//! [`std::fs::DirEntry::metadata`], which returns the data already cached by the
//! enumeration — no extra `stat` syscall per file. This is the single most
//! important perf decision for browsing huge directories.
//!
//! Results are delivered in batches through a caller-supplied callback so the
//! UI can populate progressively and stay responsive. The callback returns a
//! [`std::ops::ControlFlow`] so navigation away can cancel an in-flight scan.

use std::fs;
use std::io;
use std::ops::ControlFlow;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

use crate::model::{Attributes, Entry, EntryKind};

/// Number of entries accumulated before a batch is flushed to the callback.
pub const DEFAULT_BATCH: usize = 256;

/// Enumerate `dir`, invoking `on_batch` with chunks of up to `batch` entries.
///
/// Designed to run on a worker thread; the app forwards each batch to the Iced
/// runtime. Returning [`ControlFlow::Break`] from `on_batch` stops enumeration
/// early (e.g. the user navigated elsewhere). Individual entries that fail to
/// stat are skipped rather than aborting the whole listing.
pub fn read_dir_batched(
    dir: &Path,
    batch: usize,
    mut on_batch: impl FnMut(Vec<Entry>) -> ControlFlow<()>,
) -> io::Result<()> {
    let batch = batch.max(1);
    let mut buffer = Vec::with_capacity(batch);

    for dirent in fs::read_dir(dir)? {
        let Ok(dirent) = dirent else { continue };
        // Cheap on Windows: no extra syscall, does not follow symlinks.
        let Ok(meta) = dirent.metadata() else {
            continue;
        };

        let name = dirent.file_name().to_string_lossy().into_owned();
        let kind = if meta.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };

        buffer.push(Entry {
            name,
            path: dirent.path(),
            kind,
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            attrs: Attributes::from_raw(meta.file_attributes()),
        });

        if buffer.len() >= batch {
            let chunk = std::mem::replace(&mut buffer, Vec::with_capacity(batch));
            if on_batch(chunk).is_break() {
                return Ok(());
            }
        }
    }

    if !buffer.is_empty() {
        let _ = on_batch(buffer);
    }
    Ok(())
}

/// Convenience wrapper that collects an entire directory into a `Vec`.
///
/// Prefer [`read_dir_batched`] for directories that may be large; this is handy
/// for tests and small/known listings.
pub fn read_dir_all(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut all = Vec::new();
    read_dir_batched(dir, DEFAULT_BATCH, |chunk| {
        all.extend(chunk);
        ControlFlow::Continue(())
    })?;
    Ok(all)
}

/// Enumerate only the *subdirectories* of `dir`, skipping files.
///
/// This backs the navigation tree, which only ever shows folders. Skipping
/// files during enumeration keeps it light on directories with many files —
/// the whole point of the tree being cheap to expand. Like [`read_dir_all`],
/// metadata is read from the cached enumeration data, so there's no extra
/// `stat` per entry.
pub fn read_subdirs(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut dirs = Vec::new();
    for dirent in fs::read_dir(dir)? {
        let Ok(dirent) = dirent else { continue };
        let Ok(meta) = dirent.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        dirs.push(Entry {
            name: dirent.file_name().to_string_lossy().into_owned(),
            path: dirent.path(),
            kind: EntryKind::Directory,
            size: 0,
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            attrs: Attributes::from_raw(meta.file_attributes()),
        });
    }
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn enumerates_files_and_dirs_with_metadata() {
        let tmp = std::env::temp_dir().join(format!("librarian_enum_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("subdir")).unwrap();
        File::create(tmp.join("hello.txt")).unwrap();

        let mut entries = read_dir_all(&tmp).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "hello.txt");
        assert_eq!(entries[0].kind, EntryKind::File);
        assert_eq!(entries[0].extension(), "txt");
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].is_dir());

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn read_subdirs_returns_only_directories() {
        let tmp = std::env::temp_dir().join(format!("librarian_subdirs_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("alpha")).unwrap();
        fs::create_dir_all(tmp.join("beta")).unwrap();
        File::create(tmp.join("file.txt")).unwrap();

        let mut dirs = read_subdirs(&tmp).unwrap();
        dirs.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(dirs.len(), 2, "files must be excluded");
        assert_eq!(dirs[0].name, "alpha");
        assert_eq!(dirs[1].name, "beta");
        assert!(dirs.iter().all(|d| d.is_dir()));

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn batching_breaks_early_on_request() {
        let tmp = std::env::temp_dir().join(format!("librarian_break_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for i in 0..10 {
            File::create(tmp.join(format!("f{i}.bin"))).unwrap();
        }

        let mut seen = 0usize;
        read_dir_batched(&tmp, 2, |chunk| {
            seen += chunk.len();
            ControlFlow::Break(())
        })
        .unwrap();
        // Stopped after the first flushed batch rather than reading all 10.
        assert_eq!(seen, 2);

        fs::remove_dir_all(&tmp).unwrap();
    }
}
