# Phase 2 — `/simplify` review

**Date:** 2026-06-17
**Branch:** `feat-phase2`
**Scope:** `git diff main...HEAD` — the whole phase-2 feature branch (~5,900 added
lines across 34 files: tabs, search, resizable columns, WSL browsing,
icons/thumbnails, and supporting modules).

This was a **quality** pass (reuse, simplification, efficiency, altitude), not a
correctness-bug hunt. The change was reviewed from four independent angles, the
findings deduped, and the high-confidence ones applied. Everything below is
behavior-preserving.

**Verification:** `cargo check --workspace --all-targets`, `cargo clippy
--workspace --all-targets`, and `cargo fmt --all --check` are all clean; all 86
tests pass (23 `librarian-core`, 13 `librarian-win`, 50 `librarian-app`).

---

## Applied

### Reuse

1. **`tab_title` was a verbatim re-implementation of `Location::label()`.**
   `tab_title` reproduced `label`'s logic for every arm (`ThisPc → "This PC"`,
   `Wsl → "Linux"`, `Path → file_name() else display()`).
   - Deleted `fn tab_title` in `crates/librarian-app/src/main.rs`; `view_tab` now
     calls `self.tab_location(i).label()`.
   - The app-level test `tab_titles_use_the_folder_name` was removed and its
     coverage moved to where the logic now lives:
     `path_label_is_the_file_name_or_full_path_for_roots` in
     `crates/librarian-core/src/model.rs`.
   - `address_text` was checked and left alone — it uses the full `display()` for
     paths, so it is *not* a duplicate of `label`.

2. **`rows::file_extension` duplicated `Entry::extension`.**
   Both computed the lowercase, dotless extension.
   - Added one shared free function `extension_of(&Path) -> String` in
     `crates/librarian-core/src/model.rs` (re-exported from `lib.rs`).
   - `Entry::extension` now delegates to it; `rows::file_extension` was deleted
     and `row_from_hit` calls `extension_of` directly.

3. **`type_label` duplicated `ext_type_label`.**
   In `crates/librarian-app/src/rows.rs`, `type_label` re-derived the
   `"{EXT} File"` / `"File"` strings that `ext_type_label` already produces. It
   now returns early for directories and delegates to
   `ext_type_label(&entry.extension())`.

### Simplification

4. **The `Content::ThisPc { drives: Vec::new() }` placeholder appeared four times.**
   It was used both as the real "This PC" landing page and as a throwaway
   filler. Added `impl Default for Content` (returning that value) in
   `crates/librarian-app/src/main.rs`; the park site in `snapshot_flat` now uses
   `std::mem::take(&mut self.content)`, and the other sites use
   `Content::default()`.

5. **The "allocate a fresh load token" idiom was copy-pasted.**
   `self.next_token += 1; let token = self.next_token; self.load_token = token;`
   appeared in both `DirChanged` and `load_current`. Collapsed into a
   `next_load_token(&mut self) -> u64` helper.

### Efficiency

6. **`view_row` re-shaped every visible row's name text on every frame.**
   The per-row ellipsis-tooltip check called
   `measure_width(&data.label, LIST_TEXT_SIZE)` — a full text-shaping pass — for
   each rendered row, every redraw (scroll, hover, selection, spinner tick…).
   - Added a cached `name_px: f32` to `Row` (`crates/librarian-app/src/rows.rs`).
   - Added `remeasure_details(&mut self)` in `main.rs`, which fills `name_px`
     alongside the existing column measurement. It is called from the two places
     that already measure details widths — `recompute_rows` (row set changed) and
     `ViewModeChanged` (switching into details) — and is a no-op in the grid
     modes. Because the rows are parked per-tab, the cached widths travel with
     them across tab switches.
   - `view_row`'s check is now the float compare `data.name_px > widths.name`.
   The cached value equals the old live measurement (font and `LIST_TEXT_SIZE`
   are constant), so tooltip behavior is unchanged.

### Altitude

7. **`is_visible`'s `name_filter` parameter was dead infrastructure.**
   It was a leftover seam from the old type-to-filter box that the ripgrep-backed
   search replaced; both surviving callers passed `""`. Dropped the parameter and
   its unreachable substring-match branch from
   `crates/librarian-core/src/sort.rs`, updated the two callers in `main.rs`, and
   removed the now-vestigial `name_filter` test.

8. **The WSL host literals (`wsl.localhost` / `wsl$`) were duplicated across crates.**
   The recognition strings lived in both `librarian-core`'s `is_wsl_root_path`
   and the app's `tree.rs::is_wsl_path`. Added `is_wsl_host(&str) -> bool` (backed
   by a single `WSL_HOSTS` constant) in `crates/librarian-core/src/model.rs`
   (re-exported from `lib.rs`); both recognizers now call it for the host test.
   The two predicates keep their deliberately different parsing strategies
   (component/`Prefix`-based "is this a distro *root*" vs. string-based "is this
   *under* a WSL host"); only the host knowledge is now shared.
   `librarian-win::distro_unc_path` was left as-is: it only *constructs* the
   canonical path (it is not a recognizer) and `librarian-win` does not depend on
   `librarian-core`.

---

## Considered but not applied

- **`TabState` / `snapshot_flat` / `restore_flat` / `reset_flat` repeat the same
  ~15 fields four times** (the top simplification finding). The proper fix —
  embedding the per-tab fields in one struct held by `Librarian` and moving it as
  a unit — would require rewriting every `self.<field>` access across the whole
  ~5,000-line `main.rs`, almost all of it outside this diff. Too broad and risky
  for a cleanup pass; better done deliberately as its own focused change. It is
  the most valuable remaining cleanup and also the one most likely to harbor a
  silent "forgot a field in `restore_flat`" bug, so it is worth scheduling.

- **`is_input_shortcut` hand-mirrors the set of messages `key_to_message` can
  produce** (flagged by both the simplification and altitude angles; the comment
  itself says "keep in sync"). The only real dedup is to gate at the source —
  tag keyboard-origin messages, or suppress translation in the subscription while
  the loading overlay is up — which is an architectural change to the message
  flow that risks altering behavior. Left as-is.

- **`view_command_bar` / `view_toolbar` build a row with `let mut bar … if let
  Some(clear) = clear { bar = bar.push(clear) }`.** The suggested `.push_maybe()`
  does **not** exist on `Row` in iced 0.14.2 (only `grid` and `keyed::column`
  expose it), so the current form is already the idiomatic one. No change.

- **The command bar is rendered inside the right `pane_grid` pane rather than at
  the top level.** This is an intentional trick to align its controls with the
  list's columns across the tree/list divider; moving it would change the layout.
  Left as intended behavior.

- **`ColumnLayout` / `ColumnPx` / `ColumnMeasure` encode the four columns as named
  fields with per-column `match` accessors** instead of an array indexed by
  `Column`. The altitude angle flagged it; the simplification angle explicitly
  judged the named fields more readable than the array churn for a fixed,
  rarely-changing set of four. Kept the named fields.

- **Smaller efficiency / cohesion items** — co-locating `ViewMode`'s two
  string-mapping tables, caching the per-frame tab-title and tree-row label
  allocations, and the redundant startup `list_wsl_distros()` call. Each is low
  value (small N, already off the UI thread, or cross-file cohesion only) and
  would add state or coupling that cuts against the simplification goal. Left
  as-is.
