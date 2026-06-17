# Librarian Refactor — Ticket Log (self-implemented)

## Context

`crates/librarian-app/src/main.rs` has grown to **4308 lines** — nearly half the
whole workspace in one file. Everything else is already reasonably modularized
(next-largest source file is 646 lines, and most of *that* is tests). The goal is
to break `main.rs` into ~9–10 focused modules following SRP, then a final DRY
pass to remove the repeated patterns the analysis found — **without changing any
behavior**.

You (not Claude) are implementing this to learn Rust, ticket by ticket, asking
questions as you go. The tickets are ordered **safest-first**: pure mechanical
moves you can verify with `cargo build` before anything that could change runtime
behavior. DRY consolidation is deliberately saved for the end (Phase 5) so every
earlier ticket is a "no behavior change" move.

Scope: `main.rs` + `tree.rs` (test relocation only). `librarian-win/icon.rs` is
out of scope.

---

## The 3 Rust mechanics you need (read this first)

Everything below uses one of three moves. Learn these and the tickets are
mechanical.

**1. Leaf module (for *pure functions* — no `self`).**
The styling functions, keyboard predicates, and column-math functions don't touch
the app struct. Move them to a new file, add `mod foo;` to `main.rs`, and call
them as `foo::bar(...)`. This is exactly how `metrics.rs` / `rows.rs` already
work. *Zero behavioral risk.*

**2. Split `impl` block (for *methods* — `&self` / `&mut self`).**
`view()`, `update()`, and all the `view_*` / handler methods take `self`. Rust
lets an `impl Librarian { … }` block live in **any file in the same crate**. So
you create `view.rs`, write `use crate::*;` then `impl Librarian { pub(crate) fn
view_toolbar(&self) -> … { … } }`, and the call site `self.view_toolbar()` in
`main.rs` is **unchanged**. You are physically moving the method, not rewriting
the call.

**3. The one visibility rule that will bite you.**
A child module (e.g. `view`) can freely *see* private items of `main.rs` —
descendants can read their ancestors' privates. But `main.rs` **cannot** see a
private item of its child. So:
- Any method/type you move *out* of `main.rs` must become **`pub(crate)`** (so
  `main.rs` and sibling modules can still call it).
- Anything that *stays* in `main.rs` (e.g. `recompute_rows`, the `Librarian`
  struct, the `Message` enum, constants) needs **no change** — the new child
  modules can already see it.

Rule of thumb: **the thing you move gets `pub(crate)`; the things it calls back
into don't.**

> Shrinking `update()` (a 600-line `match`): you can't split one `match` across
> files. Instead, move each complex arm's *body* into a `pub(crate)` handler
> method in the right module, and replace the arm with a one-liner, e.g.
> `Message::Delete => return self.delete_selected(),`. The `match` stays in
> `main.rs` but collapses to a dispatch table.

---

## Target module layout (`crates/librarian-app/src/`)

Existing (leave as-is): `columns config ellipsis icons metrics rows search
selection theme thumbs tree`.

New files this refactor adds:

| New file | Responsibility | Mechanism |
|---|---|---|
| `styles.rs` | All `*_style()` fns + tiny pure element helpers (dividers, tooltip, `loading_overlay`, `view_header`, `gap`) | Leaf |
| `keyboard.rs` | `key_to_message`, `is_input_shortcut`, `is_text_edit`, `clear_focus` | Leaf |
| `columns.rs` (extend) | `compute_col_px`, `measure_columns`, `sample_widest`, `width_bounds`, `sort_key_of` | Leaf (into existing) |
| `view.rs` | Window chrome: `view`, tabs, toolbar, status, command bar, tree view, context menu, empty-state | impl-split |
| `view_details.rs` | Details list: `view_details`, `view_list`, `view_row` | impl-split |
| `view_grid.rs` | Icon grid: `view_body`, `view_grid`, `view_tile` | impl-split |
| `grid_session.rs` | Thumbnail session coordination + `ThumbsCached/ThumbsLoaded` arms | impl-split |
| `tabs.rs` | `TabState` + snapshot/restore/switch/new/close + tab arms | impl-split + struct |
| `fileops.rs` | `dispatch_op`, file-op arms, selection-path helpers, `unique_folder_name` | impl-split + free fn |
| `navload.rs` | `load_current`/`navigate`, folder-load arms, dir-watch, tree building/reveal | impl-split + free fns |

`main.rs` keeps: the `Librarian` struct, the `Message` enum, shared small enums
(`Content`, `ViewMode`, `PaneKind`, `Nav`, …), constants, `new()`/`title()`/
`subscription()`, the `update()` dispatch `match`, and a handful of cross-cutting
orchestration methods (`recompute_rows`, `request_icons`, `settings`,
`apply_sort`, `scroll_to`, `ensure_visible`, `move_selection`, `on_click`,
`activate`). Target: **~600–800 lines**.

> Line numbers below are anchors from the current file and *will drift* as you
> edit. Navigate by **function name** (search), not by line number.

---

## Conventions to follow (match the existing code)

- Files `snake_case`; types `PascalCase`; free fns `snake_case`.
- One `mod foo;` per new file, alphabetically grouped with the others at the top
  of `main.rs`. Add `use crate::foo::{…}` imports next to the existing block.
- Modules don't define `Message` and don't take `&mut Librarian` as a *parameter*
  — instead they're either leaf free-fns or `impl Librarian` blocks (methods).
- Keep each module's doc-comment header (`//! …`) describing its one job, like
  `tree.rs` and `thumbs.rs` do.
- Colocate any moved `#[cfg(test)]` with its code.

---

## Phase 0 — Baseline (do once, before Ticket 1)

**T0. Establish a green baseline & checkpoint discipline.**
- Run `cargo build -p librarian-app`, `cargo clippy --workspace`,
  `cargo test --workspace`, and launch `cargo run -p librarian-app` once. Note the
  current warning count.
- Commit the clean state. **After every ticket: `cargo build` + `cargo clippy` +
  launch the app + commit.** A ticket isn't done until it builds clean and the app
  still looks/behaves identically.

---

## Phase 1 — Pure leaf extractions (lowest risk, do first)

These move *only* free functions. If it compiles, it's correct.

**T1. Extract `styles.rs`.**
Move the styling block (~`3029–3432`): every `*_style()` fn plus the small pure
element/builder helpers in that range — `gap`, `full_path_text`, `path_tooltip`,
`header_rule`, `tree_section_divider`, `column_divider`, `view_header`,
`loading_overlay`. Add `mod styles;`, mark each moved fn `pub(crate)`, add
`use crate::styles::*;` (or explicit names) in `main.rs`. Some take `&Theme` /
`spinner_frame: usize` — pass-through, no `self`.
*Verify:* compiles; app visuals identical (selection highlight, dividers, tabs,
loading overlay).

**T2. Extract `keyboard.rs`.**
Move `is_input_shortcut`, `is_text_edit`, `clear_focus`, `key_to_message`
(~`3556–3650`). All free fns over `&Message` / `Key`. Mark `pub(crate)`; fix the
call sites in `subscription()` and at the top of `update()`.
*Verify:* compiles; keyboard nav, Ctrl-shortcuts, and typing in the address/search
/rename fields all still work (the overlay-suppression check at `update` line
~1065 still calls `is_input_shortcut`/`is_text_edit`).

**T3. Consolidate column math into `columns.rs`.**
Move `compute_col_px`, `width_bounds`, `sort_key_of` (~`3815–3870`),
`measure_columns` (~`2167–2220`), and `sample_widest` out of `main.rs` into the
existing `columns.rs` (their natural home — they already operate on
`ColumnLayout`/`Column`). Make them `pub` in `columns.rs`. If any need
`measure_width`/`LIST_TEXT_SIZE`, import from `metrics` there.
*Verify:* compiles; column auto-fit, drag-resize, and header sort-arrow widths
behave identically.

---

## Phase 2 — View impl-split (low risk: moving `&self` methods)

Do **T1 first** — these call the now-`pub(crate)` style fns. Pure render methods,
no state mutation, so behavior can't change if it compiles; spot-check visuals.

**T4. Extract `view.rs` (chrome).**
Move methods: `view`, `view_tabs`, `view_tab`, `view_toolbar`, `view_status`,
`view_command_bar`, `view_tree`, `view_tree_row`, `view_context_menu`,
`empty_list_message`. Create `view.rs` with `use crate::*;` and `impl Librarian {
… }`; mark each `pub(crate)`. (`view` itself is required by the Iced `update`/`view`
wiring — confirm it's still found; if the framework needs it on the original impl,
keep just `view`'s signature delegating, or move it too and ensure the trait/inherent
lookup still resolves.)
*Verify:* compiles; toolbar, tabs, status bar, command bar, tree sidebar, and
right-click menu all render and respond.

**T5. Extract `view_details.rs`.**
Move `view_details`, `view_list`, `view_row` (~`2547–2587`, `2729–2844`). These
call `compute_col_px`/`measure_columns` (now in `columns.rs` from T3) and styles
(T1) — import them.
*Verify:* details list renders; virtualization (scroll), inline rename field, and
per-column ellipsis all intact.

**T6. Extract `view_grid.rs`.**
Move `view_body`, `view_grid`, `view_tile` (~`2594–2597`, `2850–2953`).
*Verify:* all icon-grid sizes (Tiny→ExtraLarge) render; thumbnails appear; tile
rename field works.

---

## Phase 3 — Stateful impl-split (moves `&mut self` methods + update arms)

Higher care: these mutate state and own update-arm logic. Move the methods, then
collapse each related `update` arm to a one-line delegating call.

**T7. Extract `grid_session.rs` (thumbnail coordination).**
Move `begin_grid_session`, `prefetch_thumbs`, `retain_visible_misses`,
`extract_full_task`, `seed_background`, `next_background_chunk`,
`visible_thumb_keys` (~`2243–2366`) and the `thumb_key` free fn. Add a
`pub(crate) fn handle_thumbs_cached/loaded(...)` and move the `ThumbsCached` /
`ThumbsLoaded` arm bodies (~`1335–1397`) into them; replace arms with delegating
calls. Keep `ThumbKind` where it's referenced (move to this module as `pub(crate)`
if only used here).
*Verify:* switch to a grid view in an image folder — loading overlay raises/clears,
viewport thumbs load first, background pre-cache fills in on scroll.

**T8. Extract `tabs.rs`.**
Move `TabState` (~`745–819`) and `snapshot_flat`/`restore_flat`/`reset_flat`/
`show_active`/`switch_tab`/`new_tab`/`close_tab`. Move the tab arm bodies
(`NewTab`/`SelectTab`/`CloseTab`/`CloseActiveTab`/`NextTab`/`PrevTab`/
`OpenInNewTab`, ~`1401–1430`) into `pub(crate)` handler methods. `TabState` and its
fields used by `main.rs` become `pub(crate)`.
*Note:* the active tab's state lives directly on `Librarian`; only parked tabs are
`Some(TabState)`. Preserve that exactly — `snapshot_flat`/`restore_flat` are the
only places that convert between the two.
*Verify:* open/close/switch/cycle tabs; each tab restores its own folder, scroll,
selection, columns.

**T9. Extract `fileops.rs`.**
Move `dispatch_op` and the file-op arm bodies (`OpenSelected`, `NewFolder`,
`DeleteSelected`, `Copy`, `Cut`, `Paste`, `RenameStart/Changed/Commit`,
`OpFinished`, ~`1560–1657`) into `pub(crate)` handler methods; move
`unique_folder_name` (~`3544–3554`), `selected_paths`, `current_dir`, `lead_path`,
`restore_selection`, `begin_pending_rename`. Keep calling `librarian-win`
(`copy_items`, `move_items`, `delete_to_recycle`, `rename`, `create_folder`) from
here — that's the right layering.
*Verify:* create folder (+ auto-rename), rename, delete-to-recycle, cut/copy/paste,
open — and the listing auto-refreshes after each (`OpFinished`).

**T10. Extract `navload.rs` (navigation + async loading + dir watch + tree build).**
Move `load_current`/`navigate`/reset helpers (~`1754–1831`), the load arm bodies
(`ThisPcLoaded`, `WslLoaded`, `Loaded`, `Reloaded`, `DirChanged`, ~`1192–1313`),
`watch_stream` (~`3684–3725`), and the tree-building free fns + factories
(`fetch_tree_roots`, `fetch_tree_children`, `home/this_pc/wsl_tree_child`,
`tree_child_from_*`, ~`3444–3542`) plus tree arm bodies (`TreeToggle`,
`TreeNavigate`, `TreeChildrenLoaded`, `PaneResized`, ~`1432–1473`) and
`load_tree_roots`/`load_tree_children`/`drive_reveal`.
*This is the biggest stateful ticket — consider splitting into T10a (folder load +
watch) and T10b (tree build/reveal) if it feels too large.*
*Verify:* navigate via address bar, tree clicks, back/forward/up; This PC + WSL
listings; external file changes auto-refresh; revealing a deep path expands the
tree.

---

## Phase 4 — `tree.rs` (test relocation only)

**T11. Move `tree.rs` tests into `src/tree/tests.rs`.**
`tree.rs` logic (lines 1–371) is already SRP-clean — **leave it alone.** Only the
275-line `#[cfg(test)] mod tests` block (372–646) inflates it. Replace that block
in `tree.rs` with `#[cfg(test)] mod tests;`, create `src/tree/tests.rs`, paste the
test bodies there with `use super::*;` at the top. (A file-module `tree.rs` can own
a child module file at `src/tree/tests.rs` — no `mod.rs` needed.)
*Verify:* `cargo test -p librarian-app` runs the same tree tests, all green.

---

## Phase 5 — DRY consolidation (separate final phase)

Now that concerns live in the right modules, remove the repeated patterns the
analysis found. **These change code paths**, so do them one at a time with a build
+ run between each. Each becomes a small helper on `Librarian` (or a free fn)
placed in the owning module.

**T12. `recompute_rows` + status string (4×).** Sites: load/reload arms
(~`1196`, `1205`, `1221`, `1289`). Extract `fn refresh_listing_status(&mut self)`
that does `recompute_rows()` + `self.status = format!("{n} items")`.

**T13. Selection preserve/restore (2×).** Sites ~`1154`, `1288`. Extract
`fn recompute_preserving_selection(&mut self)` wrapping
`selected_paths` → `recompute_rows` → `restore_selection`.

**T14. Icon+thumbnail warm batches (5×).** Sites ~`1095`, `1158`, `1178`, `1199`,
`1224`. Extract small helpers like `warm_content(lock)` / `warm_after_search()`
returning the `Task::batch([...])` so handlers read intent, not wiring. (Land this
in `grid_session.rs`.)

**T15. File-op status strings (3×).** Sites ~`1582/1590/1598`. Extract
`fn op_label(verb: &str, count: usize) -> String` (uses existing `plural`). Lives
in `fileops.rs`.

**T16. Icon-or-placeholder render (2×).** Sites `view_tree_row` ~`2469`, `view_row`
~`2766`. Extract `fn icon_element(&self, key, size) -> Element<'_, Message>`.

**T17. Inline rename field (2×).** Sites `view_row` ~`2775`, `view_tile` ~`2923`.
Extract `fn rename_field(value) -> Element` (free fn; uses `RENAME_ID`,
`RenameChanged`, `RenameCommit`).

**T18. Tree-child factories (3×).** `tree_child_from_drive/distro/known`
(~`3527–3542`) are identical `row → TreeChild::lazy(label, icon, target)`. Collapse
to one `tree_child_from_row(row: Row) -> TreeChild`. Lives in `navload.rs`.

**T19. (Optional / stretch) Async token-supersession guard (9×).** The
`if token != self.X_token { return Task::none() }` pattern recurs in every async
arm. A macro or `fn is_stale(token, current) -> bool` helper trims it, but each
arm checks a *different* token field — only worth it if it reads cleaner to you.
Skip if it feels forced.

---

## Global verification

After **each** ticket:
1. `cargo build -p librarian-app` — clean.
2. `cargo clippy --workspace` — no new warnings vs. the T0 baseline.
3. `cargo test --workspace` — all green (esp. after T3, T10, T11).
4. `cargo run -p librarian-app` — exercise the area the ticket touched (see each
   ticket's *Verify*).
5. `git commit` — one commit per ticket, so any regression is trivially bisectable.

At the end: `main.rs` should be ~600–800 lines and read as *struct + Message +
dispatch*, with each feature's logic one `mod` away. Confirm the full app still
behaves identically to the T0 baseline (navigation, tabs, search, grid/details,
file ops, rename, tree reveal, WSL, external-change refresh).

---

## Suggested order & dependencies

`T0 → T1 → T2 → T3` (independent leaf moves; T1 before Phase 2) `→ T4 → T5 → T6`
(views; need T1/T3) `→ T7 → T8 → T9 → T10` (stateful; independent of each other,
any order) `→ T11` (tree tests; independent — can be done anytime) `→ Phase 5`
(T12–T19; after their owning module exists).

Lowest-risk first stop if you want a quick early win and a feel for the pattern:
**T1 (styles)** removes ~400 lines with zero behavioral risk.
