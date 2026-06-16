//! Thumbnail cache and background extraction for the icon-grid views.
//!
//! Distinct from the file list's 16px [`crate::icons`] cache: thumbnails are
//! larger, per-file (not shared by type), and keyed by size + modification time
//! so a resized view or a changed file fetches fresh pixels. Extraction runs on
//! the COM worker (see `librarian-win`); we hand back plain [`IconImage`] data
//! and build the Iced [`Handle`] on the UI thread.
//!
//! The cache is bounded with FIFO eviction. Because we only ever request the
//! visible window (plus a little overscan), the working set stays small; the
//! bound just caps memory when scrolling through a huge directory.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::SystemTime;

use iced::widget::image::Handle;
use librarian_win::{IconImage, ShellWorker, thumbnail};

/// How many decoded thumbnails to keep. Bounds memory (a 256px tile is ~256 KB,
/// so this caps the cache near ~130 MB worst-case) while comfortably covering
/// several screenfuls across the supported sizes.
const CAPACITY: usize = 512;

/// Identifies a specific thumbnail: a file at a pixel size, invalidated when the
/// file's modification time changes (so an edited file re-thumbnails on refresh).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub path: PathBuf,
    pub size: u16,
    pub mtime: Option<SystemTime>,
}

#[derive(Default)]
pub struct ThumbCache {
    handles: HashMap<ThumbKey, Handle>,
    requested: HashSet<ThumbKey>,
    /// Materialized keys in insertion order, for FIFO eviction past `CAPACITY`.
    /// In-flight (requested-but-not-yet-inserted) keys are absent, so they're
    /// never evicted out from under a pending extraction.
    order: VecDeque<ThumbKey>,
}

impl ThumbCache {
    pub fn get(&self, key: &ThumbKey) -> Option<&Handle> {
        self.handles.get(key)
    }

    pub fn insert(&mut self, key: ThumbKey, image: IconImage) {
        let handle = Handle::from_rgba(image.width, image.height, image.rgba);
        if self.handles.insert(key.clone(), handle).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.handles.remove(&evicted);
                // Allow a re-request if it scrolls back into view later.
                self.requested.remove(&evicted);
            }
        }
    }

    /// From `keys`, return those not already cached or in-flight, marking them
    /// in-flight so each is extracted only once.
    pub fn take_unrequested(&mut self, keys: impl IntoIterator<Item = ThumbKey>) -> Vec<ThumbKey> {
        let mut out = Vec::new();
        for key in keys {
            if !self.requested.contains(&key) && !self.handles.contains_key(&key) {
                self.requested.insert(key.clone());
                out.push(key);
            }
        }
        out
    }

    /// Clear the in-flight mark for `keys` we chose not to extract (e.g. tiles
    /// that scrolled off before their slow pass ran), so they're eligible to be
    /// requested again if they return to view.
    pub fn release(&mut self, keys: impl IntoIterator<Item = ThumbKey>) {
        for key in keys {
            self.requested.remove(&key);
        }
    }
}

/// The outcome of a cache-only sweep: thumbnails already in the OS cache
/// (`hits`, ready to display now) and the keys that weren't (`misses`, which need
/// a full extraction).
#[derive(Debug, Clone, Default)]
pub struct CacheSweep {
    pub hits: Vec<(ThumbKey, IconImage)>,
    pub misses: Vec<ThumbKey>,
}

/// Fast pass: ask the shell for each thumbnail *only if already cached*, doing no
/// slow extraction (see [`thumbnail`]'s `cache_only`). Partitions into ready hits
/// and the misses a later [`extract_full`] must extract.
pub fn extract_cached(worker: &ShellWorker, keys: Vec<ThumbKey>) -> CacheSweep {
    let mut sweep = CacheSweep::default();
    for key in keys {
        let path = key.path.clone();
        let size = key.size as u32;
        match worker.run(move |apt| thumbnail(apt, &path, size, true)) {
            Some(image) => sweep.hits.push((key, image)),
            None => sweep.misses.push(key),
        }
    }
    sweep
}

/// Slow pass: fully extract each thumbnail, generating it if needed. Run on the
/// dedicated thumbnail worker so a slow decode can't stall interactive shell ops.
pub fn extract_full(worker: &ShellWorker, keys: Vec<ThumbKey>) -> Vec<(ThumbKey, IconImage)> {
    keys.into_iter()
        .filter_map(|key| {
            let path = key.path.clone();
            let size = key.size as u32;
            worker
                .run(move |apt| thumbnail(apt, &path, size, false))
                .map(|img| (key, img))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, size: u16) -> ThumbKey {
        ThumbKey {
            path: PathBuf::from(name),
            size,
            mtime: None,
        }
    }

    fn img() -> IconImage {
        IconImage {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        }
    }

    #[test]
    fn take_unrequested_dedups_against_in_flight_and_cached() {
        let mut cache = ThumbCache::default();
        let a = key("a", 48);
        let b = key("b", 48);

        // First pass: both are new and become in-flight.
        let out = cache.take_unrequested([a.clone(), b.clone()]);
        assert_eq!(out, vec![a.clone(), b.clone()]);
        // Second pass: in-flight, so nothing re-requested.
        assert!(cache.take_unrequested([a.clone(), b.clone()]).is_empty());

        // Once one resolves, it's cached and still not re-requested.
        cache.insert(a.clone(), img());
        assert!(cache.get(&a).is_some());
        assert!(cache.take_unrequested([a]).is_empty());
    }

    #[test]
    fn release_allows_a_skipped_key_to_be_requested_again() {
        let mut cache = ThumbCache::default();
        let a = key("a", 48);

        assert_eq!(cache.take_unrequested([a.clone()]), vec![a.clone()]);
        // While in-flight it isn't re-requested...
        assert!(cache.take_unrequested([a.clone()]).is_empty());
        // ...but once released (we decided not to extract it), it's eligible again.
        cache.release([a.clone()]);
        assert_eq!(cache.take_unrequested([a.clone()]), vec![a]);
    }

    #[test]
    fn fifo_eviction_caps_the_cache_and_allows_re_request() {
        let mut cache = ThumbCache::default();
        let first = key("file-0", 48);

        for i in 0..CAPACITY + 1 {
            cache.insert(key(&format!("file-{i}"), 48), img());
        }
        // The oldest entry was evicted to stay within the bound.
        assert!(cache.get(&first).is_none());
        // And an evicted key is eligible to be requested again.
        assert_eq!(cache.take_unrequested([first.clone()]), vec![first]);
    }
}
