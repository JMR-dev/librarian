//! Known-folder resolution (Desktop, Documents, Downloads, …) for the "This PC"
//! / quick-access section.
//!
//! `SHGetKnownFolderPath` allocates the returned string with the COM task
//! allocator, so each path must be freed with `CoTaskMemFree`. Prefer calling
//! these on the COM worker thread.

use core::ffi::c_void;
use std::path::PathBuf;

use windows::core::GUID;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    SHGetKnownFolderPath, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
    FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos, KNOWN_FOLDER_FLAG,
};

#[derive(Debug, Clone)]
pub struct KnownFolder {
    pub name: &'static str,
    pub path: PathBuf,
}

/// Resolve the standard user folders that exist on the machine. Folders that
/// fail to resolve are simply omitted.
pub fn known_folders() -> Vec<KnownFolder> {
    let items: [(&'static str, &GUID); 6] = [
        ("Desktop", &FOLDERID_Desktop),
        ("Downloads", &FOLDERID_Downloads),
        ("Documents", &FOLDERID_Documents),
        ("Pictures", &FOLDERID_Pictures),
        ("Music", &FOLDERID_Music),
        ("Videos", &FOLDERID_Videos),
    ];

    let mut folders = Vec::with_capacity(items.len());
    for (name, id) in items {
        if let Some(path) = resolve(id) {
            folders.push(KnownFolder { name, path });
        }
    }
    folders
}

fn resolve(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let pwstr = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let path = pwstr.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(pwstr.0 as *const c_void));
        path.filter(|p| !p.as_os_str().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_at_least_some_known_folders() {
        let folders = known_folders();
        assert!(
            folders.iter().any(|f| f.name == "Desktop"),
            "Desktop should always resolve"
        );
        for f in &folders {
            assert!(f.path.is_absolute(), "{} path should be absolute", f.name);
        }
    }
}
