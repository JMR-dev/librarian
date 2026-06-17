# Librarian

A from-scratch file explorer for Windows 11, written in Rust with the
[Iced](https://iced.rs) GUI toolkit. The goal is native-Explorer feature parity
with better performance and none of the legacy bloat.

![Librarian](screenshots/Screenshot%202026-06-16%20220426.png)

## Features

- **Tabbed browsing** — multiple folder tabs, each with its own navigation
  history, view mode, scroll position, column layout, and search state.
- **Recursive search** — fast file search powered by an external
  [ripgrep](https://github.com/BurntSushi/ripgrep) process, with **Name** and
  **Contents** modes and results that stream in as they are found.
- **Details view with smart columns** — resizable, drag-to-size columns that
  auto-fit to their content via exact text measurement, with widths remembered
  per folder.
- **Icons & thumbnails** — a grid view backed by shell-extracted icons and
  thumbnails, loaded off the UI thread so scrolling stays smooth.
- **Folder tree** — an expandable navigation tree in the left pane.
- **WSL browsing** — a "Linux" group that lists your installed WSL distros and
  browses their filesystems over `\\wsl.localhost\<distro>`.
- **Standard navigation** — This PC / drives, known folders, back / forward / up,
  an editable address bar, and live refresh on disk changes.
- **Built-in file operations** — copy, move, rename, delete-to-Recycle-Bin, and
  open-with-default-app, routed through the Windows shell.

## Architecture

Librarian is a Cargo workspace of three crates, layered so that all `unsafe`
Win32/COM code is isolated and the domain logic stays portable and testable:

| Crate | Responsibility |
| --- | --- |
| [`librarian-core`](crates/librarian-core) | OS-agnostic domain logic: the model (entries, locations, history), directory enumeration, sorting, and search matching. No `unsafe`, no UI. |
| [`librarian-win`](crates/librarian-win) | All Windows shell integration — icons, thumbnails, `IFileOperation`, drives, known folders, WSL discovery — behind safe wrappers. Owns a dedicated COM single-threaded-apartment (STA) worker thread. UI-free (returns plain RGBA/data). |
| [`librarian-app`](crates/librarian-app) | The Iced `librarian` binary: the GUI, view model, and message loop. |

Recursive search shells out to `rg.exe` rather than re-implementing traversal;
the process is spawned with an argv array (never a shell string) so search
queries can't be interpreted as command flags.

Packaging is intentionally **out of scope** for this repo: it ships a
relocatable `librarian.exe` plus a reusable core library, and is meant to be
deployed by a separate installer project.

## Building

Requires a recent stable Rust toolchain (edition 2024, Rust ≥ 1.94) on Windows.

```powershell
# Build everything
cargo build --workspace

# Run the app
cargo run -p librarian-app

# Optimized build
cargo build --release -p librarian-app
```

The release binary lands at `target/release/librarian.exe` and is relocatable.

### Search setup (ripgrep)

Search requires `rg.exe`. At runtime Librarian looks for it next to its own
executable, then on `PATH`, then in the common winget/scoop/chocolatey
locations. If you don't already have ripgrep, the helper script provisions a
per-user copy:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-ripgrep.ps1
```

See [`scripts/README.md`](scripts/README.md) for options (pinning a version,
installing alongside the app, skipping the PATH update).

## Development

```powershell
cargo test --workspace --all-targets   # unit tests
cargo clippy --workspace --all-targets # lints
cargo fmt --all --check                # formatting
```

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs the test suite
on `windows-2025` for every push and pull request to `main`. Releases are built
on demand via [`release.yml`](.github/workflows/release.yml), which produces
`librarian.exe` and a SHA256 checksum.

Notable engineering notes and reviews live in [`docs/`](docs):

- [`debug_log.md`](docs/debug_log.md) — running log of notable bug fixes.
- [`phase2_simplify_review.md`](docs/phase2_simplify_review.md) — code-quality pass.
- [`crash_class_review_filepilot.md`](docs/crash_class_review_filepilot.md) —
  defensive review against a known file-explorer crash class.

## License

Licensed under the [GNU Affero General Public License v3.0](LICENSE).
