//! Icon cache and background extraction.
//!
//! Icons are keyed coarsely so they're cached and reused: every folder shares
//! one handle, every `.txt` shares one, etc. Extraction runs on the COM worker
//! (see `librarian-win`); we hand back plain [`IconImage`] data and build the
//! Iced [`Handle`] on the UI thread.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use iced::widget::image::Handle;
use librarian_win::{
    IconImage, ShellWorker, computer_icon, folder_icon, icon_for_extension, icon_for_path, wsl_icon,
};

/// What an icon represents. `Path` is used for things with a per-item icon
/// (drives, and later custom-icon files); `Folder`/`Ext` are generic and shared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IconKey {
    Folder,
    /// The shell's "This PC" / Computer icon (the drives container node).
    Computer,
    /// The shell's "Linux" / WSL icon (the penguin), shared by the WSL group node
    /// and every distro — one local extraction, cached and reused.
    Wsl,
    Ext(String),
    Path(PathBuf),
}

#[derive(Default)]
pub struct IconCache {
    handles: HashMap<IconKey, Handle>,
    requested: HashSet<IconKey>,
}

impl IconCache {
    pub fn get(&self, key: &IconKey) -> Option<&Handle> {
        self.handles.get(key)
    }

    pub fn insert(&mut self, key: IconKey, image: IconImage) {
        let handle = Handle::from_rgba(image.width, image.height, image.rgba);
        self.handles.insert(key, handle);
    }

    /// From `keys`, return those not already cached or in-flight, marking them
    /// in-flight so they're only extracted once.
    pub fn take_unrequested(&mut self, keys: impl IntoIterator<Item = IconKey>) -> Vec<IconKey> {
        let mut out = Vec::new();
        for key in keys {
            if !self.requested.contains(&key) && !self.handles.contains_key(&key) {
                self.requested.insert(key.clone());
                out.push(key);
            }
        }
        out
    }
}

/// Extract icons for `keys` on the COM worker thread. Serialized through the
/// single STA worker; intended to be called from a background task.
pub fn extract_icons(worker: &ShellWorker, keys: Vec<IconKey>) -> Vec<(IconKey, IconImage)> {
    keys.into_iter()
        .filter_map(|key| {
            let image = match &key {
                IconKey::Folder => worker.run(|apt| folder_icon(apt, false)),
                IconKey::Computer => worker.run(|apt| computer_icon(apt, false)),
                IconKey::Wsl => worker.run(|apt| wsl_icon(apt, false)),
                IconKey::Ext(ext) => {
                    let ext = ext.clone();
                    worker.run(move |apt| icon_for_extension(apt, &ext, false))
                }
                IconKey::Path(path) => {
                    let path = path.clone();
                    worker.run(move |apt| icon_for_path(apt, &path, false))
                }
            };
            image.map(|img| (key, img))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(name: &str) -> IconKey {
        IconKey::Ext(name.to_string())
    }

    #[test]
    fn take_unrequested_returns_each_key_once_across_calls() {
        let mut cache = IconCache::default();

        // First sight of these keys: all are handed back for extraction.
        let first = cache.take_unrequested([IconKey::Folder, ext("txt")]);
        assert_eq!(first, vec![IconKey::Folder, ext("txt")]);

        // They're now in-flight, so asking again yields nothing — each icon is
        // only extracted once, even before its handle has come back.
        assert!(
            cache
                .take_unrequested([IconKey::Folder, ext("txt")])
                .is_empty()
        );

        // A genuinely new key still comes through.
        assert_eq!(cache.take_unrequested([ext("rs")]), vec![ext("rs")]);
    }

    #[test]
    fn take_unrequested_dedupes_within_a_single_call() {
        let mut cache = IconCache::default();
        // The same key repeated in one batch is requested only once.
        let out = cache.take_unrequested([ext("txt"), ext("txt"), IconKey::Folder]);
        assert_eq!(out, vec![ext("txt"), IconKey::Folder]);
    }

    #[test]
    fn cached_keys_are_not_requested_again() {
        let mut cache = IconCache::default();
        let image = IconImage {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 0],
        };

        assert!(cache.get(&IconKey::Folder).is_none());
        cache.insert(IconKey::Folder, image);
        // Once a handle is cached, get() resolves it and take_unrequested skips it.
        assert!(cache.get(&IconKey::Folder).is_some());
        assert!(cache.take_unrequested([IconKey::Folder]).is_empty());
    }
}
