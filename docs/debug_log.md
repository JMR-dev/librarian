# Debug Log

A running log of notable bugs fixed in Librarian, **newest first**. Add new entries
at the top. Each entry records the symptom, the root cause, how it was fixed, and the
files/lines touched, with the commit that carries the fix for context.

---

## 2026-06-17 — Inline rename could rename the WRONG file after a background refresh

**Commit:** `18318c7` — "Code Review fixes" (branch `feat-phase2`)

### Symptom
Start an inline rename (F2) on a file, then — before pressing Enter — let the open
folder change on disk (an external tool adds/removes/renames a sibling, e.g. an
editor's temp/`.lock` churn). Pressing Enter renames a *different* file than the one
whose name is being edited. Silent data corruption.

### Root cause
`Rename` stored only `{ index, value }` — a *positional* row index. The folder watcher
fires `DirChanged` → `Reloaded`, whose `recompute_rows()` re-sorts and rebuilds
`self.rows`. After the re-sort, `renaming.index` points at whatever file now occupies
that row, and `RenameCommit` did `self.rows.get(state.index)` and renamed that row's
target. The same positional-index hazard scrambled the selection during a *streaming
search*: `SearchBatch` re-sorts every batch but — unlike `Reloaded` — did not re-map
the selection, so a click / Delete made mid-stream acted on the wrong hit.

### Fix
Carry **file identity, not row position**, across a recompute:
- `Rename` gained a `path: PathBuf` captured at `RenameStart`; `RenameCommit` renames
  that path (and derives the old name from it), ignoring the possibly-stale index.
- `SearchBatch` now captures `selected_paths()` before `recompute_rows()` and calls
  `restore_selection()` after, exactly as the `Reloaded` arm already did (re-select by
  path).

### Affected files & lines
`crates/librarian-app/src/main.rs`:
- **L192–202** — `Rename`: added the `path` identity field (doc'd why `index` can go stale).
- **L1616–1628** — `RenameStart`: capture the lead path into `Rename.path`.
- **L1635–1655** — `RenameCommit`: rename `state.path`, not `rows[index]`.
- **L2071–2076** — `begin_pending_rename`: set `path` for a programmatic rename.
- **L1137–1160** — `SearchBatch`: capture/restore selection by path across the re-sort.

---

## 2026-06-17 — Background refresh / stale landing list clobbered the visible view

**Commit:** `18318c7` — "Code Review fixes" (branch `feat-phase2`)

### Symptom
Two related glitches:
1. Run a search in a folder; while results show, anything changes the folder on disk →
   the results vanish, replaced by the normal folder listing, and the search silently
   stops streaming.
2. From "This PC", open a drive and immediately navigate into a folder. The slow
   drive-list enumeration finishes *after* you've arrived and overwrites the folder with
   the "This PC" drive grid, dropping the loading overlay early.

### Root cause
Both are "a stale/background producer clobbers the current view":
1. `Reloaded`'s success arm set `self.content = Content::Folder { entries }`
   unconditionally — even with `Content::Search` on screen. The search subscription was
   still alive, so subsequent `SearchBatch`es were then dropped (content no longer
   `Search`).
2. `ThisPcLoaded` / `WslLoaded` carried no load token, so a slow `list_drives` result
   applied even after a newer navigation. (Last session's F2 guarded the *reverse*
   direction — navigating away from a virtual root — by bumping the token in
   `load_current`; this is the other half.)

### Fix
- `Reloaded` (Ok) bails when `Content::Search` is active, and `DirChanged` skips the read
  entirely during a search — the search owns the screen; clearing it re-reads the folder
  fresh.
- `ThisPcLoaded` / `WslLoaded` now carry the load token (captured when `load_current`
  dispatches `list_drives` / `list_wsl_distros`) and drop on mismatch.

### Affected files & lines
`crates/librarian-app/src/main.rs`:
- **L606–607** — `ThisPcLoaded` / `WslLoaded`: added a `u64` load-token field.
- **L1192–1210** — handlers: drop on token mismatch.
- **L1247–1252** — `DirChanged`: skip the refresh while a search is active.
- **L1275–1281** — `Reloaded` (Ok): don't overwrite an active `Content::Search`.
- **L1792–1812** — `load_current` (`ThisPc` / `Wsl`): capture and tag the load token.

---

## 2026-06-17 — Review hardening: overlay lock, tab session reset, config & grid guards

**Commit:** `18318c7` — "Code Review fixes" (branch `feat-phase2`)

### Summary
Lower-severity fixes from the same comprehensive review:

- **Modal-overlay total-lock gaps** — `ViewModeChanged`, `SortBy`, and `SearchDebounced`
  bypassed the loading overlay's input lock (a floating `pick_list` dropdown or a deferred
  search debounce could fire during a cold load and release the lock early). Added them to
  `is_input_shortcut`. → `main.rs` **L3578–3586**.
- **New tab inherited a stale thumbnail session** — `reset_flat` didn't clear
  `overlay_loading` / `thumb_token` / `bg_queue`, so a new tab could show the previous
  tab's spinner and keep pumping its background thumbnail queue. `reset_flat` now supersedes
  the session. → `main.rs` **L843–849**.
- **Non-finite persisted column width** — a corrupt `nan` / `inf` entry in the columns
  config parsed to a valid `f32` and pushed a NaN width through `compute_col_px` into iced
  layout (`f32::clamp` returns NaN unchanged). `decode_rule` now rejects non-finite values
  (+ regression test). → `columns.rs` **L119–131**.
- **Grid window arithmetic** — `visible_thumb_keys` computed `row * cols` before clamping;
  switched to `saturating_*` so an extreme stale `scroll_y` can't overflow a `usize` or
  invert the slice range (matches the `window_for` hardening in `afd23ed`). → `main.rs`
  **L2370–2376**.
- **De-duplicated name comparison** — hoisted the triplicated case-insensitive name compare
  into `librarian_core::cmp_name_str`, now called by the folder, search-results, and
  tree-children sorts (a single point to later switch to `StrCmpLogicalW`). → `sort.rs`
  **L77**, `lib.rs` **L19**, `main.rs` search/tree sorts.

---

## 2026-06-17 — WSL folder load: column "pinch" + inconsistent loading screen

**Commit:** `20347e3` — "WSL loading screen fix" (branch `feat-phase2`)
The fix is confined to `crates/librarian-app/src/main.rs`. That commit also bundles
unrelated in-flight phase-2 work (`rows.rs`, `model.rs`, `sort.rs`, `lib.rs`,
`docs/phase2_simplify_review.md`) — those files are **not** part of this fix.

### Symptom
Navigating into a WSL folder (e.g. `\\wsl.localhost\Ubuntu\...`) showed a different
loading experience than thumbnail loading: the details-view columns visibly *pinched*
to their minimum widths and then snapped back when the listing arrived, and there was
no spinner overlay. On fast local folders this flashed by; over WSL's slow 9P network
reads it sat on screen long enough to be jarring.

### Root cause
Two unrelated loading paths existed:

- **Thumbnail loading** used a modal overlay (`loading_overlay`): a rotating spinner +
  "Loading…" on a semi-transparent scrim drawn *over the existing grid*, so nothing
  reflowed.
- **Folder loading** (`load_current`) instead *blanked* the list immediately —
  set `Content::Folder { loading: true }`, cleared `rows`, and reset `col_measure` to
  default. With no rows to measure, the auto-fit columns collapsed to `MIN_COL_W`
  (the pinch), and no overlay was shown during the directory read at all. The nice
  overlay only ever appeared later, for the thumbnail phase.

### Fix
Unified folder loads onto the same modal overlay, and kept the previous folder on
screen (dimmed, under the overlay) instead of blanking — which removes the pinch:

- `load_current` (the `Path` branch) no longer touches `content`/`rows`/columns. It
  raises `overlay_loading` and a new per-tab `load_pending` flag, then kicks off the
  read. The previous folder stays visible under the spinner.
- `Message::Loaded` clears `load_pending`, *then* adopts the new folder's column
  layout and resets `col_measure` (deferred from navigation), swaps in the entries,
  and hands off to the thumbnail session. In details view the overlay simply releases
  onto correctly-sized columns; in grid views it carries into thumbnail loading.
- `load_pending` replaced the old `Content::Folder { loading }` marker (which only
  existed to blank + later resume a mid-load tab), so that field was removed. The flag
  is parked/restored per tab via `TabState`.
- Edge case covered: a disk-change refresh (`Message::Reloaded`) landing mid-navigation
  now fulfills the pending load (adopts columns, clears the flag) instead of leaving it
  stuck.

Because all navigation (back/forward/up/refresh/tree/address) routes through
`load_current`, the unified overlay + no-pinch behavior applies everywhere.

### Affected files & lines
`crates/librarian-app/src/main.rs`:
- **L299–301** — `Content::Folder`: removed the `loading: bool` field.
- **L408–413** — `Librarian` struct: added the per-tab `load_pending: bool` field.
- **L450–454** — updated the `overlay_loading` doc comment (now covers folder reads).
- **L521** — `TabState`: added `load_pending: bool`.
- **L695** — `new()`: init `load_pending: false`.
- **L771** — `snapshot_flat`: park `load_pending`.
- **L792** — `restore_flat`: restore `load_pending`.
- **L823** — `reset_flat`: reset `load_pending = false`.
- **L898–901** — `show_active`: resume a mid-load tab via `if self.load_pending`.
- **L1169–1205** — `Message::Loaded`: clear `load_pending` (L1173), adopt columns
  (L1178–1179), drop the `loading` field, release the overlay on the error path.
- **L1216–1231** — `Message::Reloaded`: mid-navigation guard (`std::mem::take`, L1226),
  drop the `loading` field.
- **L1677–1735** — `load_current`: removed the eager column reset; landing-page arms
  (`ThisPc`/`Wsl`) reset columns to default; the `Path` branch keeps the previous
  folder and raises `load_pending` (L1729) + `overlay_loading` (L1730).
- **L2492–2493** — `empty_list_message`: key the "Loading…" placeholder off
  `load_pending` (only seen at startup / fresh tabs, where there is no previous folder).
