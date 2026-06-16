//! Case-insensitive substring matching via a hand-built finite automaton, plus a
//! directory-only tree walk that uses it.
//!
//! ripgrep (Librarian's file-search engine) lists *files* only — it can't report
//! matching directories. To cover folders we run our own matcher: a
//! Knuth–Morris–Pratt automaton over the (lowercased) query that scans each
//! directory name in a single pass with no backtracking, giving the same
//! case-insensitive substring semantics as ripgrep's name search. The result set
//! is appended to ripgrep's file hits by the app layer.

use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

// Reparse-point bit (junctions / symlinks); declared locally to keep this crate
// free of the `windows` dependency, matching `model.rs`.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// Upper bound on directory-walk worker threads. The walk is I/O-bound, so a
/// handful of threads saturates the disk; more just adds lock contention.
const MAX_WALK_THREADS: usize = 8;

/// A compiled case-insensitive substring matcher.
///
/// Built as a KMP automaton: the query is lowercased and turned into a failure
/// table, after which [`is_match`](Self::is_match) tests a candidate in `O(n)`
/// over its characters (no re-scanning), regardless of how many partial matches
/// occur. Folding is done per-character with [`char::to_lowercase`], so it works
/// for non-ASCII names too.
pub struct NameMatcher {
    /// The lowercased query, as `char`s (case folding can change length, and
    /// indexing chars keeps multi-byte handling correct).
    needle: Vec<char>,
    /// KMP failure function: `fail[i]` is the length of the longest proper
    /// prefix of `needle[..=i]` that is also a suffix of it.
    fail: Vec<usize>,
}

impl NameMatcher {
    /// Compile `query` into a matcher. An empty (or all-whitespace-trimmed-away)
    /// query matches nothing.
    pub fn new(query: &str) -> Self {
        let needle: Vec<char> = query.to_lowercase().chars().collect();
        let fail = build_failure(&needle);
        Self { needle, fail }
    }

    /// Whether `haystack` contains the query as a case-insensitive substring.
    pub fn is_match(&self, haystack: &str) -> bool {
        let m = self.needle.len();
        if m == 0 {
            return false;
        }
        let mut state = 0usize; // count of leading needle chars matched so far
        for ch in haystack.chars().flat_map(char::to_lowercase) {
            while state > 0 && self.needle[state] != ch {
                state = self.fail[state - 1];
            }
            if self.needle[state] == ch {
                state += 1;
                if state == m {
                    return true;
                }
            }
        }
        false
    }
}

/// Build the KMP failure function for `needle`.
fn build_failure(needle: &[char]) -> Vec<usize> {
    let mut fail = vec![0usize; needle.len()];
    let mut k = 0usize;
    for i in 1..needle.len() {
        while k > 0 && needle[k] != needle[i] {
            k = fail[k - 1];
        }
        if needle[k] == needle[i] {
            k += 1;
        }
        fail[i] = k;
    }
    fail
}

/// Recursively walk `root` and return the paths of directories whose name
/// matches `matcher`, up to `cap` results.
///
/// The walk runs in parallel across a small pool of scoped threads: directories
/// live on a shared stack; each worker pops one, reads it *off-lock* (the part
/// that actually parallelizes), and pushes back any real subdirectories. The
/// walk is finished once the stack is empty and no worker is mid-read. It stops
/// early when `cap` is reached or `cancel` is set (checked as each worker claims
/// its next directory, so a torn-down search ends promptly). Reparse points
/// (junctions/symlinks) are matched by name but never descended into, so the
/// walk can't cycle or escape the subtree. Unreadable directories are skipped.
pub fn find_matching_dirs(
    root: &Path,
    matcher: &NameMatcher,
    cap: usize,
    cancel: &AtomicBool,
) -> Vec<PathBuf> {
    if cap == 0 {
        return Vec::new();
    }

    /// Work queue + results shared across the walker threads.
    struct Shared {
        /// Directories discovered but not yet read.
        stack: Vec<PathBuf>,
        /// Workers currently reading a directory (so possibly about to push
        /// more): the walk ends only when this is 0 *and* `stack` is empty.
        active: usize,
        results: Vec<PathBuf>,
        /// Set once `cap` is hit or the walk drains, to release every worker.
        done: bool,
    }

    /// One worker: claim directories and process them until the walk finishes.
    fn run_worker(
        shared: &Mutex<Shared>,
        idle: &Condvar,
        matcher: &NameMatcher,
        cap: usize,
        cancel: &AtomicBool,
    ) {
        loop {
            // Claim the next directory, or exit when the walk is finished.
            let dir = {
                let mut state = shared.lock().unwrap();
                loop {
                    if state.done || cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some(dir) = state.stack.pop() {
                        state.active += 1;
                        break dir;
                    }
                    if state.active == 0 {
                        // Nothing queued and no worker can produce more.
                        state.done = true;
                        idle.notify_all();
                        return;
                    }
                    // Others are still reading; wait for work or for the end.
                    state = idle.wait(state).unwrap();
                }
            };

            // Read the directory off-lock — the actual parallel work.
            let mut subdirs = Vec::new();
            let mut matches = Vec::new();
            if let Ok(entries) = fs::read_dir(&dir) {
                for dirent in entries.flatten() {
                    // Cheap on Windows (no extra syscall); doesn't follow links.
                    let Ok(meta) = dirent.metadata() else {
                        continue;
                    };
                    if !meta.is_dir() {
                        continue;
                    }
                    if matcher.is_match(&dirent.file_name().to_string_lossy()) {
                        matches.push(dirent.path());
                    }
                    // Descend into real directories only — not junctions/symlinks.
                    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                        subdirs.push(dirent.path());
                    }
                }
            }

            // Merge findings back and release this directory.
            let mut state = shared.lock().unwrap();
            if !state.done {
                for path in matches {
                    if state.results.len() >= cap {
                        break;
                    }
                    state.results.push(path);
                }
                if state.results.len() >= cap {
                    state.done = true;
                } else {
                    state.stack.extend(subdirs);
                }
            }
            state.active -= 1;
            idle.notify_all();
        }
    }

    let shared = Mutex::new(Shared {
        stack: vec![root.to_path_buf()],
        active: 0,
        results: Vec::new(),
        done: false,
    });
    let idle = Condvar::new();

    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_WALK_THREADS);

    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| run_worker(&shared, &idle, matcher, cap, cancel));
        }
    });

    shared.into_inner().unwrap().results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn matches_substring_case_insensitively() {
        let m = NameMatcher::new("Report");
        assert!(m.is_match("Quarterly Report 2026"));
        assert!(m.is_match("annual-report.txt"));
        assert!(!m.is_match("summary"));
    }

    #[test]
    fn handles_overlapping_prefixes() {
        // A pattern whose prefixes overlap exercises the failure function: the
        // needle nearly matches, resets, then matches.
        let m = NameMatcher::new("aabaa");
        assert!(m.is_match("xaabaabaay"));
        assert!(!m.is_match("aabab"));
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(!NameMatcher::new("").is_match("anything"));
    }

    #[test]
    fn finds_matching_directories_recursively() {
        let tmp = std::env::temp_dir().join(format!("librarian_match_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("projects").join("report-archive")).unwrap();
        fs::create_dir_all(tmp.join("Reports")).unwrap();
        fs::create_dir_all(tmp.join("misc")).unwrap();
        File::create(tmp.join("report.txt")).unwrap(); // a *file*, must be ignored

        let matcher = NameMatcher::new("report");
        let cancel = AtomicBool::new(false);
        let mut found = find_matching_dirs(&tmp, &matcher, 100, &cancel);
        found.sort();

        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["Reports", "report-archive"]);

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn finds_matches_deep_in_a_wide_tree() {
        // A wide, several-levels-deep tree exercises the parallel workers'
        // push/pop and termination: a match is buried at the bottom of one branch.
        let tmp = std::env::temp_dir().join(format!("librarian_deep_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        for a in 0..6 {
            for b in 0..6 {
                fs::create_dir_all(tmp.join(format!("a{a}")).join(format!("b{b}")).join("leaf"))
                    .unwrap();
            }
        }
        // One uniquely-named directory hidden deep in the tree.
        fs::create_dir_all(tmp.join("a3").join("b4").join("treasure-chest")).unwrap();

        let matcher = NameMatcher::new("treasure");
        let cancel = AtomicBool::new(false);
        let found = find_matching_dirs(&tmp, &matcher, 100, &cancel);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("treasure-chest"));

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn cancelled_walk_returns_promptly() {
        let tmp = std::env::temp_dir().join(format!("librarian_cancel_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("alpha")).unwrap();
        // Pre-cancelled: the walk should bail out without collecting anything.
        let cancel = AtomicBool::new(true);
        let matcher = NameMatcher::new("alpha");
        let found = find_matching_dirs(&tmp, &matcher, 100, &cancel);
        assert!(found.is_empty());

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn respects_the_cap() {
        let tmp = std::env::temp_dir().join(format!("librarian_cap_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        for i in 0..5 {
            fs::create_dir_all(tmp.join(format!("data{i}"))).unwrap();
        }
        let matcher = NameMatcher::new("data");
        let cancel = AtomicBool::new(false);
        let found = find_matching_dirs(&tmp, &matcher, 3, &cancel);
        assert_eq!(found.len(), 3);

        fs::remove_dir_all(&tmp).unwrap();
    }
}
