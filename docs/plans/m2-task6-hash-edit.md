# M2 task 6: edit Hash fields inline (edit value, add field, remove field)

Status: **decisions settled by the user 2026-09-11** (D1–D4 below). Execution by a Sonnet subagent,
phase by phase, stopping at each checkpoint — not started yet. Task 6 runs before task 5 (`$EDITOR` escape hatch)
by the user's choice on 2026-09-11.

## Context

- `PLAN.md` M2 row 6: "Hash: field/value inline edit, add/remove field — same preview/diff
  machinery as String, applied to one field at a time." PRD R4.1 (in-place edit of collection
  members with a diff-style confirm), R4.4 (preview), R3.8 (live updates held while editing).
- **What exists** after task 4 (PR #31):
  - The inline editor (`state/editor.rs`, `EditBuffer`) covers only `Value::Str`/`Value::Json`;
    `from_value` refuses a Hash with "only string values are editable so far".
  - The Edit flow and its guards: `open_editor`, `stage_editor`, `editor_key`, `confirm_key`,
    `clear_editing`, `staged_edit_found_key_gone` (`update.rs`). `OpenKey::editing` is the R3.8
    guard across the buffer, the confirm dialog and the write in flight. `OpenKey::editor` stays on
    screen while staged.
  - The own write is read back through Refetch with `PendingRead::own_write`, so it shows at once
    even with the cursor off the top row.
  - A String write is `SET key value KEEPTTL XX`. `Msg::ValueSetKeyGone` tombstones the key and
    hands the edited text back to the buffer, without ever recreating the key.
- **Hash reading** (`crates/app/src/redis/read.rs`):
  - `HLEN` plus a hand-driven `HSCAN` window of at most 500 fields.
  - The result goes into `PairValue { pairs: Vec<(String, String)>, total }`.
  - Row `i` of the Viewer is `pairs[i]`: no sorting or filtering in the body. So the value cursor
    (`OpenKey::cursor` while `cursor_active`) identifies a field exactly.
- **Keys:**
  - `e` = `Action::Edit`, scoped to the value pane.
  - `d` = `Action::Delete`, scoped to the keys pane (Selected key, `DEL`).
  - `Enter` = `EnterValueCursor`.
  - `a` is unbound.
  - `c` already acts on whichever pane has focus (`keys_pane_focused()`), which is the precedent
    for `d` below.

### Redis facts that shape the write

Verified against redis.io docs on 2026-09-11 (Sources below).

- `HSET` and `HSETNX` create the key when it doesn't exist. Editing or adding a field on a hash
  that expired under the dialog would bring the key back, with no TTL. That's the same hazard task 4
  closed for Strings.
- The key's own TTL is untouched by `HSET`/`HSETNX`/`HDEL`.
- **Field TTLs (Redis 7.4+, `HEXPIRE`)**: `HSET` overwriting a field **clears that field's TTL**.
- `HSETEX key FXX KEEPTTL FIELDS 1 field value` would be exact and atomic: it writes only if the
  field exists and keeps the field's TTL. But it is **Redis 8.0+ only**; the floor is 6.0 (ADR-0007).
  `HSETEX … FNX` still creates a missing key, so it doesn't guard adds.
- `HDEL` never creates anything. Deleting the **last** field deletes the key.
- fred 10.1 exposes `eval` only behind the `i-scripts` feature, which `crates/app/Cargo.toml`
  doesn't enable yet.

## Decisions (settled by the user, 2026-09-11)

**D1. Edit and add are written as one guarded Lua script per write, sent with `EVAL`**, on every
server version.
- *edit*: if the key is gone, return -1; if the field is gone, return 0. Otherwise:
  1. read the field's expire time with `redis.pcall('HPEXPIRETIME', …)`, which fails harmlessly
     before 7.4;
  2. `HSET`;
  3. re-apply `HPEXPIREAT` if the field had a TTL;
  4. return 1.
- *add*: if the key is gone, return -1; otherwise return `HSETNX`. An existing field — including one
  outside the 500-field window — is never overwritten.
- Atomic, never recreates the key, keeps the key's and the field's TTL. One code path for 6.0–8.x.
- An ACL that denies `@scripting` refuses it; that surfaces as a normal error notification carrying
  the command (R7.4), never silently.
- Rejected, for ADR-0015:
  - `HSETEX FXX KEEPTTL` on 8.0+ with the script below 8.0: two paths, and the core would have to
    learn the server version.
  - Plain `HSET`/`HSETNX`: recreates a gone key and clears a field's TTL.

**D2. The command preview for a scripted write shows the command it performs, plus a guard line.**
- The first line is the effective command (`HSET user:1 token`), with the diff below, plus one
  muted guard line: `only if the field still exists · keeps its TTL`.
- R4.4 and CONTEXT.md's **Command preview** say "the literal command", so both gain one clause:
  *for a guarded write, the command it performs and the guard it runs under*.
- Rejected: the literal `EVAL "<script>" 1 key field value`, which is unreadable in the dialog.

**D3. Scope: edit a field's value, add a field, remove a field.**
- Renaming a field (atomic `HSETNX new` + `HDEL old` in one script) becomes a follow-up row in
  PLAN.md.

**D4. Keys: `e` / `a` / `d` act on the value cursor's row.** With the value pane focused, a Hash
open and the value cursor on a row (`Enter` first):
- `e` edits that field's value;
- `a` adds a field;
- `d` stages removing that field.

`d` becomes focus-dependent like `c`:
- keys pane focused → `DEL` of the Selected key, unchanged;
- value pane focused → `HDEL` of the field under the cursor.
- The preview's first line (`DEL …` vs `HDEL …`) tells them apart before anything runs.

Without an active cursor on a Hash, `e`/`d` give the notice `Enter to pick a field`; `a` still
works, because it needs no row.

## Ground rules for the executor

- Read `CLAUDE.md` and `CONTEXT.md` first; use the glossary's words.
- The core/shell boundary is enforced: `crates/core` must not depend on crossterm, tokio or fred.
- `source ~/.cargo/env` if `cargo` isn't found.
- **Git:**
  - Work on a new local branch `m2-hash-edit` from an up-to-date `main`.
  - Local commits only. **Never push, never open a PR, never touch `main`.**
  - Nothing destructive (`reset --hard`, `checkout -- .`, `clean`).
  - Don't commit `crates/core/examples/editor_spike.rs` (untracked).
- Stop at each ⛳ checkpoint and report; don't continue on your own.
- Match the surrounding code's comment density and tone.
- Theme tokens only, never colour literals.
- Every error is a visible notification, never a bare `Err(_) => return`.
- Build on task 4's machinery; don't fork it. If something here turns out wrong against the code,
  stop and report rather than improvising a different design.

## Phase 1: shell writes, proven against real Redis ⛳ CHECKPOINT

Written test-first against Docker, before any UI, because the scripts are the risky part.

0. **Check binary safety first.** Find out what fred 10.1 does when `HSCAN` returns a field name or
   value that isn't valid UTF-8 into `Vec<String>`: an error, or a lossy conversion.
   - Write an ignored integration test that seeds such a hash and opens it through
     `read_value`.
   - If it's **lossy**, editing must refuse any pair that can't round-trip: note this for
     Phase 2 and report it.
   - If it **errors**, the whole read already fails today. Report it; don't fix it here.
1. `crates/app/Cargo.toml`: add fred's `i-scripts` feature.
2. `crates/app/src/redis/mod.rs` (next to `set_value`/`delete_key`, same doc-comment standard):
   - `set_hash_field(client, name, field, value) -> Result<FieldWrite, Error>`: the edit script
     from D1. `FieldWrite::{Written, FieldGone, KeyGone}`.
   - `add_hash_field(client, name, field, value) -> Result<FieldAdd, Error>`: the add script.
     `FieldAdd::{Added, FieldExists, KeyGone}`.
   - `delete_hash_field(client, name, field) -> Result<bool, Error>`: `HDEL`; `false` means the
     field was already gone.
   - Keys and fields built as binary-safe `Key`/bytes exactly like `delete_key`. Remember the
     `del` byte-vector trap documented there.
   - Keep the Lua in `const` strings with a short comment on each guard. Use `EVAL`, not `EVALSHA`.
3. **Integration tests** (`crates/app/tests/integration.rs`, `#[ignore = "needs docker"]`):
   - edit overwrites a field; the key's TTL survives;
   - on `redis:7.4-alpine` **and** `redis:8.4-alpine`: a field with `HEXPIRE` keeps its TTL after an
     edit;
   - on `redis:6.2-alpine`: the edit script still works (the `HPEXPIRETIME` `pcall` is harmless);
   - edit on a gone field → `FieldGone`, nothing written; on a gone key → `KeyGone`, key not
     recreated;
   - add on a new field → `Added`; on an existing field → `FieldExists` and the value is unchanged;
     on a gone key → `KeyGone`, key not recreated;
   - delete → field removed; delete of an already-gone field → `false`; delete of the last field →
     key gone;
   - a byte-y field name like `[7, 8]` touches only that field (mirror of the existing `del` test).
4. **Run the checks:**
   - `cargo test -p redis-pane -- --ignored --test-threads=1` (whole suite, not just the new tests);
   - `cargo clippy --workspace --all-targets -- -D warnings`;
   - `cargo fmt --all -- --check`.
   Commit.
5. **⛳ CHECKPOINT.** Report:
   - the UTF-8 finding from step 0;
   - the exact scripts;
   - the test results per Redis version;
   - any deviation from D1.

## Phase 2: core, render, docs ⛳ CHECKPOINT

### Core (`crates/core`)

**Edit target.**
- `EditBuffer` gains what it edits: an `EditTarget` enum stored on the buffer.
  - `Value`: today's String/JSON behaviour, unchanged.
  - `HashField { field: String }`.
  - `NewHashField { field: String }`.
- Add `EditBuffer::for_hash_field(field, value, …)`:
  - opens the **raw** field value, never reformatted;
  - `was_json` = the value parses as JSON, so the dialog can warn;
  - cursor at the start;
  - same `MAX_EDIT_BYTES` refusal and same `WordOrGlyph` wrap.
- `from_value` keeps refusing collections for `e`-without-a-field paths.

**Mutations** (`state/mod.rs`, `command.rs`). New `PendingMutation` variants, each with
`command_text()` per D2 and `into_commands()`:
- `SetHashField { name, field, old, new, was_json }` → `Command::SetHashField`.
- `AddHashField { name, field, value }` → `Command::AddHashField`.
- `DeleteHashField { name, field, last_field: bool }` → `Command::DeleteHashField`.
  - `last_field` is `total == 1` at staging.

**Messages** (`msg.rs`).
- Success reuses `Msg::ValueSet { name }`: Refetch with `own_write`.
- Generalize the just-shipped `Msg::ValueSetKeyGone` into
  `Msg::NotWritten { name, why: NotWritten::{KeyGone, FieldGone, FieldExists}, at_ms }`.
  Migrate the String path and its tests to it.
- `KeyGone`: exactly today's behaviour (tombstone, hand the text back).
- `FieldGone` / `FieldExists`:
  - not a tombstone;
  - error notice naming the command (`HSET user:1 token: field no longer exists — nothing written,
    edit kept`);
  - hand the text back to the buffer;
  - emit a Refetch. Its reply is held under R3.8 because the buffer is open again, so the header
    shows `held`.
- A `DeleteHashField` whose `HDEL` returns `false`: notice `field already gone`, then a Refetch.

**Flow** (`update.rs`).
- `e` on a Hash:
  - requires `cursor_active` on a row; otherwise notice `Enter to pick a field`;
  - refused on a gone key, and while `editing` (`still saving the last edit`), as for Strings;
  - opens `for_hash_field` with the row's field.
- `a` (new `Action::AddField`, value-pane scoped, default `a`, label `add`):
  - on a Hash, opens a **one-line field-name capture**, shaped like `filter_key`: chars append,
    Backspace, Paste appends with newlines stripped, Esc discards, Enter continues;
  - Enter with an empty name does nothing;
  - with a name already present in `pairs`: notice `field exists — e to edit`;
  - otherwise opens an empty `EditBuffer` targeting `NewHashField { field }`;
  - `editing = true` from the moment the capture opens;
  - on non-Hash types: notice.
- `⌃S`:
  - `stage_editor` builds `SetHashField` or `AddHashField` from the target;
  - an unchanged field value closes silently, as for Strings;
  - adding a field with an empty value is allowed, since Redis allows it.
- `d`: `Action::Delete` becomes focus-dependent (D4).
  - Keys pane: unchanged.
  - Value pane on a Hash with an active cursor: stages `DeleteHashField`.
  - Value pane otherwise: notice.
  - Update `pane_is_on_screen` so `Delete` is on screen when the focused pane is.
- `staged_edit_found_key_gone`: generalize the dialog match from `SetString` to every mutation on
  the Open key's name.
  - A `DeleteHashField` dialog simply closes with the notice; there's no buffer.
- `confirm_key`, Read-only refusal, `clear_editing`, the `Failed` path, paste routing: extend to the
  new variants without changing their rules.
- After a successful write or delete the Refetch's `own_write` applies at once; the existing clamp
  keeps `cursor` in range when a row disappears.

### Render (`render/mod.rs`)

- **Editing a field:** the first body row shows the field name (`Token::Muted` label, `Token::Text`
  name); the editor fills the rows below. Header `✎ editing` and the JSON indicator as today.
- **Field-name capture:** the same row as an input line with a cursor, the editor area empty below.
- **Confirm dialog:**
  - `SetHashField`: the command line, a muted guard line (D2), then the stacked `-`/`+` diff.
  - `AddHashField`: the command line, a guard line (`only if the key still exists · never
    overwrites a field`), then `+` side only.
  - `DeleteHashField`: `HDEL key field`, and `Token::Warn` `last field — the key will be deleted`
    when `last_field`.
- **Hint bar:**
  - value pane, cursor active on a Hash: `e edit · a add · d remove`;
  - while editing a field: the editor hints;
  - while capturing a name: `Enter next · Esc cancel`.
  - Every hint is read from the keymap (R7.5).

### Docs (same change)

- **New ADR-0015** `docs/adr/0015-hash-field-writes-are-guarded.md`, in the existing ADR format.
  - Decision: D1 and D2.
  - Context: the Redis facts above.
  - Alternatives: the two D1 rejections; `WATCH`/`MULTI` (needs a dedicated connection on a
    multiplexed client); a field-level memo (forbidden by ADR-0006).
  - Sources.
- **`docs/DESIGN.md`:**
  - §4 keymap: `e`, `a`, `d` rows with their new scopes;
  - §6.5: a paragraph on editing Hash fields — cursor picks the field, the preview guard line,
    last-field warning, nothing recreated, text handed back.
- **`docs/PRD.md` R4.4** and **`CONTEXT.md` Command preview:** the D2 clause.
- **`docs/PLAN.md` M2:** mark row 6 done with its verification; add a follow-up row for field rename
  (D3); note task 6 ran before task 5.
- **`README.md` Status**, if it lists what is editable.

### Phase 2 verification ⛳ CHECKPOINT

**Core unit tests** (`update.rs`, `state/editor.rs`):
- `e` without a cursor gives the notice.
- `e` on a row opens the raw field value, not reformatted JSON.
- ⌃S stages `SetHashField` with the right `field`/`old`/`new` and `command_text`.
- Unchanged closes silently.
- `a` → name capture → Enter → editor → ⌃S stages `AddHashField`.
- Empty name does nothing.
- A name already in `pairs` gives the notice.
- `d` in the value pane stages `DeleteHashField`, and `last_field` is true when `total == 1`.
- `d` in the keys pane still stages `DeleteKey`.
- Read-only refuses at `y` for all three.
- `NotWritten::{KeyGone, FieldGone, FieldExists}` each behave as specified, including the String
  path migrated from `ValueSetKeyGone`.
- Key gone under each dialog.
- An update arriving while a field is edited is held (R3.8).
- After a delete read back, the cursor is clamped.

**Golden frames** (regenerate intended changes with `UPDATE_GOLDEN=1 cargo test -p redis-pane-core`,
and read every diff):
- editing a field (name row + editor);
- the field-name capture;
- the three confirm dialogs, including the last-field warning;
- the Hash hint bar with an active cursor.

**All must pass:**
- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- the boundary check (`cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'`
  prints nothing)
- the Docker suite `cargo test -p redis-pane -- --ignored --test-threads=1`

Commit on `m2-hash-edit`, then **⛳ CHECKPOINT.** Report:
- changes file by file;
- test counts;
- deviations and why;
- anything unverified.

**Manual checks, for the user after the checkpoint:**
- `./scripts/redis-up.sh`, `python3 scripts/fixtures.py --flush`, `cargo run -p redis-pane`.
- Open a `user:*:session` hash, `Enter`, move to a field:
  - `e` → edit → ⌃S → `y`: the row updates at once;
  - `a`: add a field;
  - `d`: remove a field; on a one-field hash, see the warning and the key go.
- In `./scripts/redis-up.sh cli`, give a field a TTL with `HEXPIRE key 600 FIELDS 1 f`, edit it,
  then check `HTTL key FIELDS 1 f` is still counting down.
- Let a TTL'd hash expire with the dialog up: nothing is recreated.
- Run `python3 scripts/churn.py --focus <key>` while editing: `held` in the header.

## Sources

- Key and field expiration behaviour (HSET/HDEL clear field TTLs):
  https://redis.io/docs/latest/develop/ai/search-and-query/advanced-concepts/expiration/
- Field-level expiration in hashes (Redis KB):
  https://support.redislabs.com/hc/en-us/articles/30050967065874-Using-Field-Level-Expiration-in-Redis-Hashes
- `HSETEX` (8.0+, `FNX`/`FXX`, `KEEPTTL`): https://redis.io/docs/latest/commands/hsetex/
- `HPERSIST` / field expiration command family (7.4+): https://redis.io/docs/latest/commands/HPERSIST/
- Previous task's decision on never recreating a gone key: `docs/adr/0014-values-are-edited-inline.md`
