# Debug Log

A running log of notable bugs fixed in Librarian, **newest first**. Add new entries
at the top. Each entry records the symptom, the root cause, how it was fixed, and the
files/lines touched, with the commit that carries the fix for context.

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
