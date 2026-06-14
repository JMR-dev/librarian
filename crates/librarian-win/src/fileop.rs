//! File operations via the shell's `IFileOperation`.
//!
//! Going through the shell (rather than `std::fs`) buys Explorer-identical
//! behavior for free: native progress UI, conflict/overwrite prompts, undo
//! records, and — the reason it's non-negotiable — routing deletes to the
//! Recycle Bin instead of unlinking. Every function here creates and drives COM
//! objects, so all of them MUST run on the COM STA worker
//! ([`crate::com::ShellWorker`]); calling them off that thread is unsound.
//!
//! Each call builds a fresh `IFileOperation`, queues its items, and calls
//! `PerformOperations`. A user cancelling the native dialog is reported as
//! success (the operation simply did less work), not an error.

use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, FILEOPERATION_FLAGS,
    FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR, FOFX_RECYCLEONDELETE,
};

use crate::util::to_wide;

/// Send the given paths to the Recycle Bin, showing native progress and
/// recording an undo entry. No-op for an empty slice.
pub fn delete_to_recycle(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    with_operation(FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE, |op| {
        for path in paths {
            let item = shell_item(path)?;
            unsafe { op.DeleteItem(&item, None) }.map_err(err)?;
        }
        Ok(())
    })
}

/// Rename a single item in place. `new_name` is a bare name (no path).
pub fn rename(path: &Path, new_name: &str) -> Result<(), String> {
    let new_wide = to_wide(new_name);
    with_operation(FOF_ALLOWUNDO, |op| {
        let item = shell_item(path)?;
        unsafe { op.RenameItem(&item, PCWSTR(new_wide.as_ptr()), None) }.map_err(err)
    })
}

/// Create a new folder named `name` inside `parent`.
pub fn create_folder(parent: &Path, name: &str) -> Result<(), String> {
    let name_wide = to_wide(name);
    with_operation(FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR, |op| {
        let dest = shell_item(parent)?;
        unsafe {
            op.NewItem(
                &dest,
                FILE_ATTRIBUTE_DIRECTORY.0,
                PCWSTR(name_wide.as_ptr()),
                PCWSTR::null(),
                None,
            )
        }
        .map_err(err)
    })
}

/// Copy the given paths into `dest_dir`, with native conflict handling.
pub fn copy_items(paths: &[PathBuf], dest_dir: &Path) -> Result<(), String> {
    transfer(paths, dest_dir, Transfer::Copy)
}

/// Move the given paths into `dest_dir`, with native conflict handling.
pub fn move_items(paths: &[PathBuf], dest_dir: &Path) -> Result<(), String> {
    transfer(paths, dest_dir, Transfer::Move)
}

enum Transfer {
    Copy,
    Move,
}

fn transfer(paths: &[PathBuf], dest_dir: &Path, kind: Transfer) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    with_operation(FOF_ALLOWUNDO, |op| {
        let dest = shell_item(dest_dir)?;
        for path in paths {
            let item = shell_item(path)?;
            let result = match kind {
                Transfer::Copy => unsafe { op.CopyItem(&item, &dest, PCWSTR::null(), None) },
                Transfer::Move => unsafe { op.MoveItem(&item, &dest, PCWSTR::null(), None) },
            };
            result.map_err(err)?;
        }
        Ok(())
    })
}

/// Create an `IFileOperation`, apply `flags`, let `queue` enqueue items, then
/// perform them. A user-cancelled operation returns `Ok(())`.
fn with_operation<F>(flags: FILEOPERATION_FLAGS, queue: F) -> Result<(), String>
where
    F: FnOnce(&IFileOperation) -> Result<(), String>,
{
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(err)?;
        op.SetOperationFlags(flags).map_err(err)?;
        queue(&op)?;
        op.PerformOperations().map_err(err)?;
    }
    Ok(())
}

/// Resolve a filesystem path to an `IShellItem`.
fn shell_item(path: &Path) -> Result<IShellItem, String> {
    let wide = to_wide(&path.to_string_lossy());
    unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }.map_err(err)
}

fn err(e: windows::core::Error) -> String {
    e.message()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::ShellWorker;
    use std::sync::{Mutex, OnceLock};

    // Share one STA worker across tests, mirroring production (all shell calls
    // serialize through a single apartment thread).
    fn worker() -> ShellWorker {
        static WORKER: OnceLock<Mutex<ShellWorker>> = OnceLock::new();
        WORKER
            .get_or_init(|| Mutex::new(ShellWorker::spawn()))
            .lock()
            .unwrap()
            .clone()
    }

    /// A unique temp subdirectory for one test, removed best-effort after.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut dir = std::env::temp_dir();
            let pid = std::process::id();
            dir.push(format!("librarian-fileop-{tag}-{pid}"));
            _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_renames_and_recycles_a_folder() {
        let tmp = TempDir::new("crud");
        let root = tmp.0.clone();

        // Create.
        let r = root.clone();
        worker()
            .run(move || create_folder(&r, "alpha"))
            .expect("create_folder");
        assert!(root.join("alpha").is_dir());

        // Rename.
        let target = root.join("alpha");
        worker()
            .run(move || rename(&target, "beta"))
            .expect("rename");
        assert!(!root.join("alpha").exists());
        assert!(root.join("beta").is_dir());

        // Recycle.
        let beta = root.join("beta");
        worker()
            .run(move || delete_to_recycle(&[beta]))
            .expect("delete_to_recycle");
        assert!(!root.join("beta").exists());
    }

    #[test]
    fn copies_and_moves_a_file() {
        let tmp = TempDir::new("copymove");
        let root = tmp.0.clone();
        let src_dir = root.join("src");
        let dst_dir = root.join("dst");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        std::fs::write(src_dir.join("file.txt"), b"hello").unwrap();

        // Copy: original stays, copy appears.
        let (s, d) = (src_dir.join("file.txt"), dst_dir.clone());
        worker()
            .run(move || copy_items(&[s], &d))
            .expect("copy_items");
        assert!(src_dir.join("file.txt").exists());
        assert!(dst_dir.join("file.txt").exists());

        // Move it back into a third folder: original gone.
        let moved_dir = root.join("moved");
        std::fs::create_dir_all(&moved_dir).unwrap();
        let (s, d) = (dst_dir.join("file.txt"), moved_dir.clone());
        worker()
            .run(move || move_items(&[s], &d))
            .expect("move_items");
        assert!(!dst_dir.join("file.txt").exists());
        assert!(moved_dir.join("file.txt").exists());
    }
}
