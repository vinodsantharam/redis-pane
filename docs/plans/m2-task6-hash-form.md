# M2 task 6 follow-up: focus-gated editing and an explicit FIELD / VALUE form

Status: decisions settled by the user 2026-09-11 (below). Executed by a Sonnet subagent on
`m2-hash-edit`, one checkpoint at the end. Builds on `docs/plans/m2-task6-hash-edit.md`
(Phases 1 and 2 done: commits `25b55dc`, `b6d1c3f`, `2d3b1c6`).

## What the user found while testing

1. **`a` adds a field from the key list.**
   - Moving the key cursor fetches nothing. But `Action::Edit` and `Action::AddField` only check
     that the value pane is *drawn* (`Action::pane_is_on_screen` → `pane_visible(Pane::Value)`), not
     that it is *focused*.
   - Above 70 columns both panes are always drawn. So `a` pressed in the key list acts on whatever
     key is open in the Viewer, which may not be the key under the cursor.
   - `d` already splits on focus; `e` and `a` don't.
2. **Adding a field doesn't say how to give the value.**
   - `a` opens a one-line name capture (`OpenKey::field_capture`).
   - `Enter` then swaps it for an empty editor under a `field <name>` row.
   - Nothing on screen says "now type the value", and the name can't be revisited.
3. **Adding an existing field name.** Today the data is safe, but the reader learns late:
   - `Enter` on a name already in the shown window → notice `field exists — e to edit`, stays in
     capture.
   - A name outside the 500-field window, or added by someone else under the dialog → `HSETNX` script
     returns 0 → `NotWritten::FieldExists` → error, text handed back.

## Decisions (settled)

- **G. Gating.** `e`, `a` and `d` act on the Open key only when the **value pane is focused** and
  the Open key **has a fetched value** (`open.value.is_some()`).
  - This includes String editing with `e`.
  - With the keys pane focused:
    - `e`/`a` don't open anything;
    - `d` stays `DEL` of the Selected key, unchanged.
- **F. Two-part form** in the value pane for Hash fields. Both parts are always visible, labelled
  `FIELD` and `VALUE` like the table columns, with the active part marked.
  - Adding (`a`): FIELD is active first; `Enter` moves to VALUE; `⌃S` stages.
  - Editing (`e` on a row): FIELD is shown read-only, VALUE is active.
  - Header: `✎ adding field` / `✎ editing field`.
  - String editing (task 4) is unchanged apart from G.
- **D. Duplicate name.**
  - While typing the name, a `⚠ exists` marker appears as soon as it equals a field in the fetched
    window.
  - `Enter`, `↓` and `⌃S` are blocked while it shows, and the hint says what to do.
  - Hidden duplicates stay protected by the `HSETNX` guard at write time (unchanged).
- **N. Navigation.**
  - `Enter` or `↓` in FIELD moves to VALUE.
  - `↑` in VALUE moves back to FIELD only when the value's cursor can't move up any further.
  - `Tab` still inserts a tab in VALUE.
  - `⌃S` stages from either part once the name is non-empty and not a shown duplicate.
  - `Esc` discards the whole add from either part.

## Ground rules for the executor

Same as `docs/plans/m2-task6-hash-edit.md`.
- Read `CLAUDE.md` and `CONTEXT.md` first.
- Keep the core/shell boundary; theme tokens only; keybindings and hints are data.
- **Git:**
  - Stay on `m2-hash-edit`.
  - Local commits only, staged by path.
  - Never push, rebase, amend, reset, or touch other branches.
  - Commit trailer lines exactly:
    ```
    Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
    Claude-Session: https://claude.ai/code/session_01J7nZhDQFsKnisrCKQ5ZHR6
    ```
- If the plan is wrong against the code, make the smallest faithful adjustment and report it; if a
  decision is needed, stop and report.

## Changes

### 1. Gating (`crates/core/src/update.rs`, `keymap/mod.rs`, `render/mod.rs`)

- `Action::Edit` and `Action::AddField` with the keys pane focused:
  - no buffer, no `editing`;
  - a short notice: `Tab to the value pane to edit` when a key is open, `open a key first` when none.
  - Implement it the way `Action::Delete` already splits on `keys_pane_focused()`, and update
    `pane_is_on_screen` so `Edit`/`AddField` follow focus like `Delete`.
- Value pane focused but `open.value` is `None` (never read, e.g. the `OpenKey::gone` placeholder):
  refuse with the existing `nothing open to edit` notice. The gone and still-saving refusals stay as
  they are.
- **Hints:** no hint anywhere may advertise `e`/`a` while the keys pane is focused. Check `hint_bar`
  and the help overlay's scope wording.
- **Existing tests and golden tests that press `e` with `opened()`:** they now need
  `state.focus = Pane::Value` (`opened()` leaves focus on the keys pane). Fix the tests, not the
  fixtures.
  - Every existing String-editor fixture must come out **unchanged**. Any change there is a bug in
    this step.

### 2. One model for the add form (`state/editor.rs`, `state/open.rs`, `update.rs`)

- **Remove `OpenKey::field_capture`** and `field_name_key`.
  - Adding becomes an `EditBuffer` from the start: `a` opens
    `EditBuffer::new_hash_field()` with an empty name and an empty value.
  - `editing = true` from that moment, as today.
- `EditTarget::NewHashField` carries the editable name and which part is active, e.g.
  `NewHashField { field: String, part: FieldPart }` with `enum FieldPart { Name, Value }`.
  - `EditTarget::HashField { field }` (editing) is always on the value part.
  - Keep `PartialEq` covering the new state.
- **Routing:** `editor_key` routes by the target's active part.
  - **Name part:**
    - plain chars append; `Backspace` pops;
    - `Msg::Paste` appends with `\r`/`\n` stripped;
    - `Enter`/`↓` → value part, unless the name is empty or a shown duplicate;
    - `⌃S` → stage, same conditions;
    - `Esc` → discard the add;
    - everything else ignored.
  - **Value part:** today's editor keys, except `↑`. Try `CursorMove::Up`; if the cursor didn't
    move (already on the top screen row, including inside a wrapped first line), switch to the name
    part. `⌃S` stages; `Esc` discards.
- **Staging:**
  - `stage_editor` for `NewHashField` builds `AddHashField { name, field, value }` exactly as now.
  - An empty value is allowed.
  - A shown-duplicate name never stages.
  - The confirm dialog, the `HSETNX` guard, `NotWritten::FieldExists` handling and the gone-key
    handling are unchanged. After a not-written reply the text is handed back on the value part.
- **Duplicate test:** exact byte equality of the name with a field in the fetched `PairValue.pairs`.
- **Delete these tests:** `a_then_a_name_then_enter_opens_an_editor_then_ctrl_s_stages_add_hash_field`,
  `enter_on_an_empty_field_name_does_nothing`,
  `a_name_already_present_gives_the_notice_and_stays_in_capture`, `esc_during_capture_discards_it`.
  Replace them with tests of the new model (see Verification).

### 3. Rendering (`render/mod.rs`)

**Form layout.** For Hash targets the body is the form:

```
▌FIELD  country█              (adding, name part active)
 VALUE  ·· Enter to write the value
```
```
 FIELD  country               (adding, value part active; or editing, where FIELD is read-only)
▌VALUE  fr█
        …the value editor fills the remaining rows, starting on this line's column
```

- **Labels:** `FIELD` / `VALUE` in `Token::Muted`, padded to the same width.
- **Active-part marker:** a one-cell `▌` in `Token::Selected` before the active label, plus the
  active label in normal text. It must survive monochrome: the marker glyph carries the meaning,
  never colour alone. Check `DESIGN.md` §7 for an existing ASCII fallback convention for block
  glyphs and follow it if there is one.
- **Name cursor:** the same `Token::Selected` cell the capture line uses today.
- **Placeholder:** `·· Enter to write the value` (`Token::Muted`), shown only while the name part
  is active and the value is empty.
- **Duplicate marker:** after the name, one space then `⚠ exists` in `Token::Warn`.
- **Editing an existing field:** no marker on FIELD (read-only, plain text); `▌` on VALUE.
- **Staged under the confirm dialog:** the form stays drawn exactly as it was (the buffer is
  staged), with no active marker.

**Header currency:**
- adding → `✎ adding field`;
- editing a Hash field → `✎ editing field`.
- Keep any `· changed · held` suffix logic.
- String editing keeps `✎ editing`.
- Find where the `✎ editing` text is built (`OpenKey::currency` or render) and branch there.

**Hint bar**, read from the keymap where the action is bound:

| State | Hint |
|---|---|
| name part | `Enter value · Esc cancel` |
| name part, duplicate | `field exists — Esc, then e to edit` |
| value part while adding | `⌃S stage · ↑ field · ⌃Z undo · Esc cancel` |
| editing an existing field | today's editor hints |

### 4. Docs

- **`docs/DESIGN.md`:**
  - §4 keymap: scope of `e` / `a` / `d` says "value pane, focused". `d` keeps its key-list row.
  - §6.5 Hash-field paragraph: the two-part form, `Enter`/`↑` between parts, the live `⚠ exists`
    marker, and that `e`/`a` need the value pane focused with the key fetched. Also the String `e`
    sentence.
- **`docs/adr/0015-hash-field-writes-are-guarded.md` Consequences:**
  - replace the field-name-capture description with the form;
  - note that shown duplicates are caught while typing and hidden ones by `HSETNX`.
- **`docs/PLAN.md` M2 row 6:** mention the form and the focus gating in its description and
  verification.
- **`CONTEXT.md`:** no new term is needed. If you find yourself naming the form in UI or code, use
  plain words ("the add form" in comments), not a new glossary term.

## Verification ⛳ CHECKPOINT

**Core unit tests** (`update.rs`):
- **Gating:**
  - `e`, keys pane focused, String open → no editor, notice.
  - `a`, keys pane focused, Hash open → no buffer, notice.
  - `d`, keys pane focused → still `DeleteKey`.
  - `e`/`a`, value focused, `open.value == None` → refused.
  - `e`/`a`/`d`, value focused with a fetched Hash → work as before.
- **Adding:**
  - `a` opens a buffer on the name part.
  - Typing and `Backspace` edit the name.
  - `Enter` on an empty name does nothing.
  - `Enter` moves to the value part; typing goes into the value; `⌃S` stages `AddHashField` with
    both.
  - `↓` also moves to the value part.
  - `↑` on the value's top row returns to the name, with the value text kept.
  - `↑` inside a multi-line value moves up a line and stays in the value part.
  - `⌃S` from the name part stages with an empty value.
  - `Esc` from either part discards and clears `editing`.
  - A paste into the name strips newlines.
- **Duplicate:**
  - The name equals a shown field → `Enter`, `↓` and `⌃S` are all blocked (nothing staged, still on
    the name part).
  - Removing a character un-blocks.
- **Hint bar** strings for each state above.
- **Unchanged:**
  - `NotWritten::FieldExists` hands the text back on the value part.
  - Key gone under the add dialog → dialog closes, form handed back.

**Golden frames** (read every diff):
- New fixtures:
  - add form, name part with placeholder;
  - add form, value part;
  - add form, duplicate marker;
  - editing an existing field (read-only FIELD, active VALUE).
- **Remove** `field_name_capture.txt` and `editor_hash_field.txt`, replaced by the new ones.
- `confirm_add_hash_field.txt` and `confirm_set_hash_field.txt` may change only in the form drawn
  behind the dialog.
- All String-editor fixtures must be **unchanged**.

**All must pass:**
- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'` prints nothing
- `cargo test -p redis-pane -- --ignored --test-threads=1` (Docker)

**Report, then stop:**
- commits;
- changes file by file;
- exact UI strings;
- test counts and new test names;
- every golden fixture added, removed or changed, with the diff of each changed one;
- verification results;
- deviations;
- `git status --short`.
