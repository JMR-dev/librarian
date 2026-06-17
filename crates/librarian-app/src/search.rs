//! Recursive search, powered by an external [ripgrep] (`rg`) process.
//!
//! ripgrep is a fast, parallel directory walker and content matcher, so rather
//! than reinventing one we shell out to it and stream its stdout. Two modes:
//!
//! * **Name** — list files whose *name* contains the query, via
//!   `rg --files --iglob "*query*"`. ripgrep's walker does the recursion (and
//!   hidden/ignore handling); each output line is a matching file path.
//! * **Contents** — files that *contain* the query, via `rg --count-matches`,
//!   so each output line is `path:count` for one file with at least one hit.
//!
//! The search is exposed as a [`Stream`] of [`SearchEvent`]s built with
//! [`iced::stream::channel`], so the app can drive it as a subscription keyed by
//! [`SearchSpec`]: changing any field tears the stream down, and because the
//! child is spawned with `kill_on_drop`, dropping the stream kills `rg` — giving
//! free cancellation when the user navigates away or starts a new search.
//!
//! `rg --files` lists *files* only. In **Name** mode, once ripgrep finishes we
//! append matching **directories** found by our own [`librarian_core::NameMatcher`]
//! automaton (same query, same case-insensitive substring semantics) — see
//! [`librarian_core::find_matching_dirs`]. **Contents** mode stays files-only: a
//! directory has no contents to match.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::futures::channel::mpsc::Sender;
use iced::futures::{SinkExt, Stream};
use librarian_core::{NameMatcher, find_matching_dirs};

/// Stop after this many results and report the search as capped, so a query
/// matching an entire drive can't flood the UI or run unbounded.
const RESULT_CAP: usize = 5000;
/// Results accumulated before a batch is pushed to the UI.
const BATCH: usize = 128;

/// What the query matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SearchMode {
    /// Match against file names (default), like Explorer's search box.
    #[default]
    Name,
    /// Match against file contents.
    Contents,
}

impl SearchMode {
    /// Every mode, in the order the picker lists them.
    pub const ALL: [SearchMode; 2] = [SearchMode::Name, SearchMode::Contents];
}

impl std::fmt::Display for SearchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SearchMode::Name => "Name",
            SearchMode::Contents => "Contents",
        })
    }
}

/// One search result: a file from ripgrep, or a directory from our own name
/// walk, plus (for a [`SearchMode::Contents`] file) how many lines matched.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    /// Matching-line count for a contents search; `None` otherwise.
    pub matches: Option<u64>,
    /// True for a directory hit (appended after ripgrep), false for a file.
    pub is_dir: bool,
}

/// The full identity of a search. Used as a subscription key, so any change
/// supersedes the running search (and kills its `rg` child).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SearchSpec {
    /// Monotonic id, so even an identical query re-run is treated as new.
    pub token: u64,
    pub root: PathBuf,
    pub query: String,
    pub mode: SearchMode,
}

/// Streamed output of a running search.
#[derive(Debug, Clone)]
pub enum SearchEvent {
    /// A chunk of newly found results.
    Batch(Vec<SearchHit>),
    /// The search finished; `capped` if it stopped early at [`RESULT_CAP`].
    Done { capped: bool },
    /// The search could not run (rg missing, failed to launch, …).
    Failed(String),
}

/// Locate the ripgrep executable. Prefers a copy bundled beside our own
/// executable (portable installs), then `PATH`, then the common per-user and
/// machine install locations. `None` means rg isn't installed.
pub fn ripgrep_path() -> Option<PathBuf> {
    const EXE: &str = "rg.exe";

    // 1. Beside our own executable — a bundled/portable copy.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let cand = dir.join(EXE);
        if cand.is_file() {
            return Some(cand);
        }
    }

    // 2. Anywhere on PATH.
    if let Some(found) = find_on_path(EXE) {
        return Some(found);
    }

    // 3. Common install locations (winget, scoop, chocolatey, and the dir our
    //    install-ripgrep.ps1 uses).
    install_candidates()
        .into_iter()
        .map(|dir| dir.join(EXE))
        .find(|cand| cand.is_file())
}

fn find_on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file())
}

fn install_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut from_env = |var: &str, tail: &[&str]| {
        if let Some(base) = std::env::var_os(var) {
            let mut p = PathBuf::from(base);
            p.extend(tail);
            dirs.push(p);
        }
    };
    // Where install-ripgrep.ps1 places rg.exe.
    from_env("LOCALAPPDATA", &["Programs", "ripgrep"]);
    // winget's shim links.
    from_env("LOCALAPPDATA", &["Microsoft", "WinGet", "Links"]);
    // scoop shims.
    from_env("USERPROFILE", &["scoop", "shims"]);
    // chocolatey.
    dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    dirs
}

/// Build the [`SearchEvent`] stream for `spec`: spawn `rg`, read its stdout, and
/// emit batched [`SearchHit`]s, then [`SearchEvent::Done`]. Spawned with
/// `kill_on_drop`, so dropping the returned stream cancels the search.
///
/// `+ use<>` keeps the stream `'static` (it owns `spec`), as the subscription
/// runtime requires.
pub fn run(spec: SearchSpec) -> impl Stream<Item = SearchEvent> + use<> {
    iced::stream::channel(
        8,
        move |mut output: iced::futures::channel::mpsc::Sender<SearchEvent>| async move {
            let Some(rg) = ripgrep_path() else {
                let _ = output
                    .send(SearchEvent::Failed(
                        "ripgrep (rg.exe) was not found. Install it (e.g. with the \
                     bundled install-ripgrep.ps1) and try again."
                            .to_string(),
                    ))
                    .await;
                return;
            };

            let query = spec.query.trim();
            if query.is_empty() {
                let _ = output.send(SearchEvent::Done { capped: false }).await;
                return;
            }

            let mut command = build_command(&rg, query, spec.mode, &spec.root);
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = output
                        .send(SearchEvent::Failed(format!(
                            "Failed to launch ripgrep: {error}"
                        )))
                        .await;
                    return;
                }
            };
            let Some(stdout) = child.stdout.take() else {
                let _ = output.send(SearchEvent::Done { capped: false }).await;
                return;
            };

            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            let mut batch: Vec<SearchHit> = Vec::with_capacity(BATCH);
            let mut total = 0usize;
            let mut capped = false;

            // Stops on EOF or a read error (`next_line` yields `Ok(None)`/`Err`).
            while let Ok(Some(line)) = lines.next_line().await {
                let Some(hit) = parse_line(&line, spec.mode) else {
                    continue;
                };
                batch.push(hit);
                total += 1;
                if batch.len() >= BATCH
                    && output
                        .send(SearchEvent::Batch(std::mem::take(&mut batch)))
                        .await
                        .is_err()
                {
                    return; // receiver gone; kill_on_drop reaps rg
                }
                if total >= RESULT_CAP {
                    capped = true;
                    break;
                }
            }

            if !batch.is_empty() {
                let _ = output.send(SearchEvent::Batch(batch)).await;
            }
            // Reap rg promptly when we stopped early; harmless if it already exited.
            let _ = child.start_kill();

            // ripgrep covered files. In Name mode, append matching *directories*
            // via our own name automaton (using whatever cap rg's files left).
            // Contents mode is files-only — a directory has nothing to grep.
            if spec.mode == SearchMode::Name {
                let remaining = RESULT_CAP.saturating_sub(total);
                if stream_dir_matches(&mut output, &spec.root, query, remaining).await {
                    capped = true;
                }
            }
            let _ = output.send(SearchEvent::Done { capped }).await;
        },
    )
}

/// Sets a flag on drop, so a directory walk handed to `spawn_blocking` is asked
/// to stop when the search future is torn down (the walk checks the flag once
/// per directory).
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Walk `root` for directories whose name matches `query` and stream them as
/// directory [`SearchHit`]s. Returns whether the walk hit `cap`. The walk runs on
/// a blocking thread (it's synchronous filesystem work) and is cancelled if the
/// caller's future is dropped. A no-op for an empty query or a zero cap.
async fn stream_dir_matches(
    output: &mut Sender<SearchEvent>,
    root: &Path,
    query: &str,
    cap: usize,
) -> bool {
    if cap == 0 || query.is_empty() {
        return false;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    // Dropped when this future is, signalling the blocking walk to stop.
    let _guard = CancelOnDrop(Arc::clone(&cancel));

    let root = root.to_path_buf();
    let query = query.to_string();
    let dirs = tokio::task::spawn_blocking(move || {
        let matcher = NameMatcher::new(&query);
        find_matching_dirs(&root, &matcher, cap, &cancel)
    })
    .await
    .unwrap_or_default();

    let capped = dirs.len() >= cap;
    for chunk in dirs.chunks(BATCH) {
        let hits: Vec<SearchHit> = chunk
            .iter()
            .map(|path| SearchHit {
                path: path.clone(),
                matches: None,
                is_dir: true,
            })
            .collect();
        if output.send(SearchEvent::Batch(hits)).await.is_err() {
            break; // receiver gone
        }
    }
    capped
}

/// Assemble the `rg` invocation for a mode, with a hidden console window and
/// `kill_on_drop` so cancellation reaps the process.
fn build_command(
    rg: &PathBuf,
    query: &str,
    mode: SearchMode,
    root: &PathBuf,
) -> tokio::process::Command {
    let mut std_cmd = std::process::Command::new(rg);
    match mode {
        SearchMode::Name => {
            // `--files` lists every file; `--iglob` filters to those whose name
            // (a path component, since the glob has no `/`) contains the query,
            // case-insensitively.
            std_cmd.args(["--files", "--no-ignore", "--hidden", "--iglob"]);
            std_cmd.arg(format!("*{}*", escape_glob(query)));
        }
        SearchMode::Contents => {
            // One `path:count` line per file containing the (literal) query.
            std_cmd.args([
                "--count-matches",
                "--no-ignore",
                "--hidden",
                "--smart-case",
                "--fixed-strings",
                "-e",
            ]);
            std_cmd.arg(query);
        }
    }
    std_cmd.arg("--").arg(root);
    std_cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: don't flash a console for the child rg process.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std_cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.kill_on_drop(true);
    cmd
}

/// Parse one line of `rg` output into a hit, or `None` to skip it.
fn parse_line(line: &str, mode: SearchMode) -> Option<SearchHit> {
    // tokio's line reader strips the trailing newline (and a preceding CR).
    if line.is_empty() {
        return None;
    }
    match mode {
        SearchMode::Name => Some(SearchHit {
            path: PathBuf::from(line),
            matches: None,
            is_dir: false,
        }),
        SearchMode::Contents => {
            // `path:count`. Split on the *last* colon: a Windows path's drive
            // colon (`C:`) is earlier, and the count field has none.
            let (path, count) = line.rsplit_once(':')?;
            if path.is_empty() {
                return None;
            }
            Some(SearchHit {
                path: PathBuf::from(path),
                matches: count.trim().parse::<u64>().ok(),
                is_dir: false,
            })
        }
    }
}

/// Escape glob metacharacters so a name query is matched literally. Each special
/// character becomes a single-member character class (`*` → `[*]`), which avoids
/// backslash escaping — awkward on Windows, where `\` is the path separator.
fn escape_glob(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 8);
    for ch in query.chars() {
        match ch {
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            ']' => out.push_str("[]]"),
            '{' => out.push_str("[{]"),
            '}' => out.push_str("[}]"),
            // A backslash in the query is almost certainly a path separator;
            // the glob engine wants forward slashes.
            '\\' => out.push('/'),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_name_result() {
        let hit = parse_line(r"C:\Users\me\photo.png", SearchMode::Name).unwrap();
        assert_eq!(hit.path, PathBuf::from(r"C:\Users\me\photo.png"));
        assert_eq!(hit.matches, None);
    }

    #[test]
    fn parses_a_contents_result_keeping_the_drive_colon() {
        let hit = parse_line(r"C:\Users\me\notes.txt:7", SearchMode::Contents).unwrap();
        assert_eq!(hit.path, PathBuf::from(r"C:\Users\me\notes.txt"));
        assert_eq!(hit.matches, Some(7));
    }

    #[test]
    fn blank_lines_are_skipped() {
        assert!(parse_line("", SearchMode::Name).is_none());
        assert!(parse_line("", SearchMode::Contents).is_none());
    }

    #[test]
    fn escapes_glob_metacharacters_literally() {
        // A query of literal metacharacters becomes character classes, never
        // wildcards.
        assert_eq!(escape_glob("a*b?c"), "a[*]b[?]c");
        assert_eq!(escape_glob("x[y]z"), "x[[]y[]]z");
        assert_eq!(escape_glob("plain"), "plain");
        // Backslash (a path separator) normalizes to a forward slash.
        assert_eq!(escape_glob(r"a\b"), "a/b");
    }
}
