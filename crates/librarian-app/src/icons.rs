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
    IconImage, ShellWorker, computer_icon, folder_icon, icon_for_extension, icon_for_path,
};

/// What an icon represents. `Path` is used for things with a per-item icon
/// (drives, and later custom-icon files); `Folder`/`Ext` are generic and shared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IconKey {
    Folder,
    /// The shell's "This PC" / Computer icon (the drives container node).
    Computer,
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
