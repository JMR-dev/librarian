# Crash-class review: is Librarian prone to the File Pilot NULL-write bug?

**Date:** 2026-06-17
**Branch:** `feat-phase2`
**Scope:** A defensive review prompted by an *external* crash. A minidump from
File Pilot (Voidstar's file explorer, written in C) was analyzed separately; this
doc records whether Librarian — a different file explorer, in Rust — is exposed to
the **same class** of bug, what makes it (mostly) immune, and the one latent
instance the review found and fixed.

This was a **crash-class** review, not a general bug hunt. The conclusion: Librarian
is structurally resistant to that class, the worst realistic outcome is a bounded
panic rather than memory corruption, and one latent footgun in the exact class was
hardened (`window_for`) with a regression test added.

---

## The File Pilot bug (the thing we're checking for)

File Pilot crashed with an access violation — a **write through a NULL pointer** —
in its per-directory **statistics roll-up**:

- It maintains a long-lived per-name aggregate keyed by an **open-addressing hash
  map**.
- On a **lookup miss** it sets the destination "aggregate record" pointer to NULL
  as a sentinel.
- The accumulation loop that follows is guarded **only by a cached child-item
  count** (`count == 32`), *not* by whether the destination is valid, so it runs
  and executes `add qword ptr [rsi+30h], rax` with `rsi = 0` → write to `0x30` →
  `0xC0000005`.

**Trigger:** a tab open on a directory containing `.claude.json.lock`, a lock file
an external tool creates and deletes very rapidly. The churn drove a stats update
for an entry whose name was momentarily absent from the aggregation map → miss →
NULL destination → unguarded count-driven loop → NULL write.

### The bug class, abstracted

Two ingredients are required:

1. **A long-lived aggregate that is mutated incrementally** as the directory
   changes (rather than recomputed from scratch).
2. **A loop/index guarded by a separately-tracked count or stale offset**, instead
   of by the validity/bounds of the destination it actually writes to.

When the two drift apart — count says "32 items", destination says "absent" — you
get a write to an invalid location.

---

## Why Librarian is structurally resistant

**1. It recomputes from scratch; it never mutates a persistent aggregate.**
On any external change the watcher emits `Message::DirChanged`, which re-enumerates
the whole directory off-thread (`crates/librarian-app/src/main.rs`, `DirChanged`
handler ~L1211); `Message::Reloaded` then rebuilds *all* derived state via
`recompute_rows()` (~L2003). There is no per-directory stats structure being
incrementally patched as files churn, so there is no aggregate to fall out of sync.
This is the single biggest reason ingredient (1) is absent.

**2. Reconciliation is unconditional.** `recompute_rows()` always calls
`self.selection.retain_below(self.rows.len())` (~L2034), which drops any
selection / lead / anchor index that no longer exists
(`selection.rs::retain_below`). The count and the data are rebuilt together from
the same source in the same step.

**3. Indexing is guarded by the destination, not a stale count.** Every raw index
into `self.rows` is clamped or guarded against the *current* length:
- list view `&self.rows[index]` — `index ∈ start..end` from `visible_window`,
  whose `end` is `.min(count)`;
- grid thumbs `self.rows[start..end]` — both ends `.min(self.rows.len())`;
- grid tiles `&self.rows[i]` — explicit `if i >= total { break; }` first.

Everywhere else lookups use `self.rows.get(i)` → `Option`, and the one `HashMap`
lookup that matters degrades safely on miss:
`self.col_store.get(path).copied().unwrap_or_default()`.

**4. Language backstop.** Even if a stale index slipped through, safe Rust turns it
into a **bounded `index out of bounds` panic**, not a wild write into a struct
field. The data/enumeration/stats paths contain no `unsafe`, no raw-pointer
arithmetic (`model.rs` documents "no `unsafe`"). The C failure mode — `add [rsi+30h]`
with `rsi = 0` — is simply not expressible there. The `windows`-facing crate has
`unsafe`, but it is not on the watcher/enumeration path.

**5. The trigger is blunted, too.** The watcher is **debounced at 200 ms and
non-recursive** (`watch_stream`, ~L3566); each refresh supersedes the previous via
a load token (~L1223). A `.claude.json.lock`-style storm collapses into a single
re-enumeration, and a file that vanishes mid-scan is skipped, not fatal
(`enumerate.rs` — a failed `DirEntry::metadata()` → `continue`).

---

## The closest analog in Librarian

The Rust-shaped version of "count drifted from destination" is a **stale `usize`
index into `self.rows` that survives a listing shrink**: selection / lead / scroll
/ render-window indices computed against a *larger* prior listing, used after an
external change shrank it. Reasons (1)–(3) above keep all of these reconciled, so
there is no live panic on the current call sites.

### Latent instance found and fixed: `window_for`

While building the regression test, an extreme-stale-offset assertion failed and
surfaced a real latent footgun. `window_for` (which backs `visible_window`)
clamped `end` to `count` but **not** `start`:

```rust
// before
let start = first.saturating_sub(overscan);
let end   = first.saturating_add(onscreen).saturating_add(overscan).min(count);
```

When the list shrinks under a **stale scroll offset** (an external change deletes
rows while you're scrolled down), `first` can sit past the new end, leaving `start`
above `count`. That:

- **violates the function's own documented `0..count` contract**, and
- produces an **inverted `start..end`** that panics the instant a caller *slices*
  `rows[start..end]` — which the grid view does.

It was not a live panic *today*: the list view *iterates* `start..end` (harmlessly
empty when inverted), and the grid computes its own separately-clamped range. But
it is exactly the "derived index not reconciled to the current count" shape we were
reviewing for — a panic waiting for the next caller that slices. Fixed by clamping
`start` to `end`:

```rust
// after
let end   = first.saturating_add(onscreen).saturating_add(overscan).min(count);
let start = first.saturating_sub(overscan).min(end); // start <= end <= count
```

Now the window is always a well-formed, in-bounds, possibly-empty range.

---

## Regression guard added

`crates/librarian-app/src/main.rs`, test module:

- **`rapid_churn_never_leaves_a_stale_row_index`** — arms the *real* notify watcher
  (non-recursive, the same settings `watch_stream` uses) on a temp dir, hammers a
  `.churn.lock` create/delete burst from a background thread (the exact File Pilot
  trigger), and on every re-enumeration runs the app's reconciliation
  (`Selection::retain_below` + the `visible_window`/`window_for` clamp), carrying
  selection + scroll from a *larger* prior listing into the new one. It asserts no
  produced index ever falls outside the current rows and nothing panics, however
  the count moves between reads. It also exercises `read_dir_all` tolerating an
  entry vanishing mid-scan.
- **`visible_window_renders_only_the_viewport_plus_overscan`** — extended with a
  pure-arithmetic case proving the `0..count` contract holds for a stale scroll
  offset past the end of a shrunk list (`start <= end <= count`, never inverted).

Both pass; full `librarian-app` suite green (51 tests), `cargo fmt --all --check`
and `cargo clippy -p librarian-app --all-targets` clean.

---

## Verdict

Librarian is **not susceptible** to File Pilot's bug class. It rebuilds derived
state wholesale instead of mutating a long-lived aggregate, reconciles indices
unconditionally, guards every raw index against the current length, and runs in
safe Rust where the worst outcome is a panic, not memory corruption. The one latent
footgun in the class (`window_for`'s un-clamped `start`) has been fixed and pinned
with tests.

### Future-risk watch

The immunity comes from the *recompute-from-scratch* design. The moment a feature
mutates a **persistent per-directory aggregate incrementally from watcher events** —
e.g. live folder-size totals, per-extension counts, or a running selection summary —
ingredient (1) reappears and this bug class is back in play. If that is ever added:
guard the accumulation on the **destination's existence/bounds**, not on a cached
item count, and re-run the churn test against it.
