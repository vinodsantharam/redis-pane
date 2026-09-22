# Review M2: split `update.rs`, and derive the Mode instead of implying it

Status: **approved by the user 2026-09-20**, to land as its own PR. Execution by a Sonnet subagent,
phase by phase, stopping at each checkpoint.

This is finding **M2** of [the 2026-09-13 codebase design review](../reviews/2026-09-13-codebase-design-review.md#m2--updaters-has-poor-locality).
It was deferred out of PR #34 on purpose: landed in the same change as the behaviour fixes, a move
of this size would have buried every one of them. The seams have now settled, so it is cheapest
here.

## Context

`crates/core/src/update.rs` is **6,921 lines** on `main` at `9fd0fa1`:

- ~1,890 lines of production code (everything above the first `#[cfg(test)]` at line 1895).
- ~5,030 lines of tests in **11** `#[cfg(test)]` modules, holding **211** of the core crate's 400
  tests.
- `update()` is a ~390-line `match` on `Msg`. Eleven of its arms carry their body inline rather
  than calling a named function; the largest are `ValueLoaded` (~64 lines), `ValueGone` (~48) and
  `MetadataBatch` (~43).
- `key_press` is ~437 lines (`update.rs:515`).

**Mode precedence** — confirm dialog > editor > filter > keymap — is implemented as three early
returns at `update.rs:523–537`. The rule is right and well commented, but it exists only as control
flow: there is no `Mode` type anywhere in the crate, so nothing can name the active mode, and
`render` re-derives pieces of the same question independently at `render/mod.rs:53`
(`state.confirm`), `render/mod.rs:423` (`open.typing()`) and `render/mod.rs:1139`
(`state.filtering`).

One correction to the review: it also claimed precedence was re-derived in
`keymap::pane_is_on_screen`. It is not. That function (`keymap/mod.rs:108`) answers pane
*visibility* below 70 columns, which is a different question and is fine as it stands. Only
`render` duplicates the mode question.

## Invariants this change must not break

These are the reasons the file is shaped the way it is. Breaking one of them is worse than leaving
`update.rs` at 6,921 lines.

1. **`update()` stays the only public entry.** The seam is `(State, Msg) -> (State, Vec<Command>)`.
   Every submodule function is `pub(super)` or tighter. Nothing outside `crates/core/src/update/`
   may gain a new way in.
2. **Do not adopt ratatui's component architecture.** Trait objects owning their own state and
   handlers would break the single `(State, Msg)` seam that golden frames and the liveness tests
   depend on. The review names this explicitly as a non-goal.
3. **The keyspace cap stays enforced in exactly one place** — `scan_batch` — per CLAUDE.md. It
   moves to `update/scan.rs` and stays the only place that can grow the Loaded set.
4. **No golden frame may change.** All 122 are a behaviour assertion over the whole render path. If
   one changes, the move was not a move — stop and report.
5. **No test may be deleted or weakened** to make the split compile. The counts below are exact and
   are checked at every checkpoint.
6. **This is a refactor, not a fix.** Do not repair anything you find along the way, however
   tempting. Note it at the bottom of this file under "Found while moving" and keep going.

## Target layout

```text
update/mod.rs      update(), key_press() dispatch, Mode + mode(),
                   shared read helpers (issue_read, refetch, read_key),
                   epoch_secs, quit, failed, clear_editing
update/link.rs     Connected, ConnectionLost, ReconnectScheduled, TrackingArmed,
                   Invalidated, ServerState
update/scan.rs     ScanStarted/Batch/Complete/Cancelled/Failed, MetadataBatch,
                   scan_batch (the cap lives here)
update/keys.rs     selection, filter capture, tree fold, sort, parent_row,
                   group_prefix_at, move_selection, after_move, filter_key
update/viewer.rs   ValueLoaded, ValueGone, cursor movement and scroll, build_copy,
                   cursor_active, move_cursor, cursor_to, after_cursor_move
update/editor.rs   open_editor, begin_add_field, hash_add_blocked, name_part_key,
                   stage_editor, editor_key, staged_edit_found_key_gone
update/confirm.rs  confirm_key, mutation_settled, key_deleted, write_landed,
                   nothing_to_remove, not_written
update/mouse.rs    mouse_action, pane_at, scroll_at
```

**Two documented deviations from the review's sketch**, both because the sketch had no home for
them:

- `update/mouse.rs` is new. Mouse handling spans both panes (it focuses, scrolls and drags the
  divider), so it fits neither `keys.rs` nor `viewer.rs`.
- The shared read helpers (`issue_read`, `refetch`, `read_key`) stay in `update/mod.rs` rather than
  getting a file of their own. Four of the seven submodules call them; a separate `read.rs` would
  be a file every module depends on, which is what `mod.rs` already is.

Record both in the review doc in phase 4.

## The trick that makes this low-risk

Every test module opens with `use super::*`. If `update/mod.rs` re-exports its submodules' items:

```rust
use self::confirm::*;
use self::editor::*;
use self::keys::*;
// ...and so on
```

then `use super::*` in a test module still resolves every helper it was using, **so the production
code can move without touching a single test**. That is phase 2. Distributing the tests is phase 3
and is separately verifiable. Do not collapse the two phases: the whole point is that if phase 3
breaks something, phase 2 is known good.

## Phases

Each phase is one commit, ends at a checkpoint, and must be reported before starting the next.

### Phase 1 — Derive the Mode

1. In `update/mod.rs` (still `update.rs` at this point — do not create the directory yet), add:

   ```rust
   /// Which input mode is active. Derived from `State`, never stored: two
   /// fields that must agree are two fields that can disagree.
   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub(crate) enum Mode {
       Confirm,
       Editing,
       Filtering,
       Normal,
   }

   pub(crate) fn mode(state: &State) -> Mode { /* the precedence, once */ }
   ```

   The precedence is exactly the one at `update.rs:523–537` — confirm, then editor
   (`state.open.as_ref().and_then(OpenKey::typing).is_some()`), then `state.filtering`, then
   `Normal`. Carry the existing explanatory comments across; they are the reasoning for the order
   and must not be lost.

2. Rewrite `key_press`'s three early returns to `match mode(&state)`. Behaviour identical.
   Note that `confirm_key` needs the `PendingMutation` by value, so that arm still does
   `state.confirm.take()`.

3. Make `render` ask rather than re-derive: `render/mod.rs:53`, `:423` and `:1139` call `mode()`.
   Keep each call site's existing behaviour exactly — `:423` in particular is about whether to show
   the active-part marker, which is `Mode::Editing`, *not* "an editor exists".

4. Add tests for the precedence itself, in the existing `mod tests`: a State with both a confirm
   and a live editor is `Mode::Confirm`; one with both an editor and `filtering` is `Mode::Editing`;
   filter alone is `Mode::Filtering`; bare state is `Mode::Normal`.

**Checkpoint 1.** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`. Expect core **404** (400 + 4 new), golden **122** unchanged,
app **38**. Report the counts. If any golden frame changed, stop — `mode()` did not reproduce a
call site's behaviour.

### Phase 2 — Move the production code only

1. `git mv crates/core/src/update.rs crates/core/src/update/mod.rs`, then create the seven
   siblings.
2. Move the functions per the target layout. Mark each `pub(super)`; re-export from `mod.rs` with
   `use self::<module>::*;` so `use super::*` keeps resolving in the test modules.
3. **Extract the inline `Msg` arms** into named functions in their owning module, so `update()`
   becomes a dispatcher: `ValueLoaded`, `ValueGone`, `MetadataBatch`, `Connected`,
   `ConnectionLost`, `ReconnectScheduled`, `Invalidated`, `ServerState`, `ScanStarted`,
   `ReadIssued`, `Paste`. Same for any `key_press` arm whose body runs past ~15 lines. Body moves
   verbatim; only its home changes.
4. **Every test module stays in `mod.rs`, untouched, in this phase.** `mod.rs` will be large and
   lopsided at the end of it. That is expected and phase 3 fixes it.
5. Doc comments travel with the function they document. Where a comment explains a *rule* rather
   than a function (mode precedence, the cap, the re-arm invariants), make sure it lands in the
   file that now owns that rule.

**Checkpoint 2.** Same three commands, plus the boundary check
(`cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'` must print nothing).
Counts must be **identical to checkpoint 1**: core 404, golden 122, app 38. Also report
`wc -l crates/core/src/update/*.rs`. If a test needed editing to compile, the re-export is wrong —
fix the re-export, not the test.

### Phase 3 — Distribute the tests

Move each module to the file that owns its subject. Helpers travel with the module that uses them;
where two destinations need the same helper, duplicate it rather than inventing a shared test
module — a four-line `fn viewing() -> State` in two files is cheaper to read than an import.

| Test module | Tests | Destination |
|---|---|---|
| `hash_field_edit_tests` | 33 | `editor.rs` |
| `metadata_tests` | 32 | `scan.rs` |
| `cursor_mode_tests` | 12 | `viewer.rs` |
| `loading_indicator_tests` | 11 | `viewer.rs` |
| `scan_tests` | 10 | `scan.rs` |
| `liveness_invariants` | 10 | `link.rs` |
| `viewer_scroll_tests` | 8 | `viewer.rs` |
| `tree_fold_tests` | 8 | `keys.rs` |
| `honesty_tests` | 8 | **stays in `mod.rs`** |
| `stack_navigation_tests` | 5 | **stays in `mod.rs`** |
| `tests` | 74 | split by subject; see below |

`honesty_tests` stays whole. Its doc comment — "The three ways the app could quietly claim
something untrue" — is the point of the module; it is organised by a cross-cutting property, not by
a mechanism, and splitting it across `link.rs` and `mod.rs` would destroy the only thing holding
those eight tests together. `stack_navigation_tests` is Esc precedence and pane stacking, which is
`mod.rs`'s own subject.

The 74-test `mod tests` is the only one needing judgment. Distribute by subject — the mouse and
divider tests to `mouse.rs`, the delete/confirm tests to `confirm.rs`, the editor tests to
`editor.rs`, the scroll tests to `viewer.rs`, and so on. **Leave in `mod.rs` anything genuinely
cross-cutting**: `update_is_pure_same_input_same_output`, resize, quit, the help overlay, Tab focus,
and the Mode precedence tests from phase 1. When a test could sit in two places, leave it in
`mod.rs` — a wrong move is worse than an unmoved test, and `mod.rs` keeping a general suite is not
a defect.

Commit per destination file if that keeps the diff readable.

**Checkpoint 3.** Same commands. Counts still core 404, golden 122, app 38 — the total is the
assertion that nothing was dropped on the floor. Report `wc -l` for every file in `update/`.

### Phase 4 — Docs

1. `docs/PLAN.md` §2 (workspace layout): record the `update/` module layout. The review's
   order-of-work table names this as the doc to touch for M2.
2. `docs/reviews/2026-09-13-codebase-design-review.md` §9: change M2's row from **Deferred** to
   fixed, with this PR's commits. Add a line noting the two deviations from the sketched layout
   (`mouse.rs`; read helpers in `mod.rs`) and the correction about `pane_is_on_screen`.
3. `CLAUDE.md`: the architecture bullet says the cap is enforced in "`scan_batch` in `update.rs`".
   That path is now `update/scan.rs`. Fix it.
4. Grep the whole repo for `update.rs` and fix every stale path in docs and comments.

**Checkpoint 4.** Full local verification, then report. Do not open the PR — the main agent does
that.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'   # must print nothing
```

Expected after phase 1 and unchanged thereafter: **core 404, golden 122, app 38.**

The Docker integration suite (`cargo test -p redis-pane -- --ignored --test-threads=1`) is not
affected by a core-internal move, but run it once at checkpoint 4 if Docker is available.

## Out of scope

- **M3** (type knowledge leaking past the `Viewer` trait) stays deferred until a second collection
  type becomes editable. Do not start on it.
- **L3** (the widget in `State`) is unchanged; it waits on a `ratatui-textarea` bump.
- No behaviour change of any kind. No new features, no bug fixes, no clippy-driven rewrites beyond
  what the move itself requires.

## Found while moving

_(Executor: append anything noticed but deliberately not fixed, with `file:line`. Leave empty if
nothing.)_
