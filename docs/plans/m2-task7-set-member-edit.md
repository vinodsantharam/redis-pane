# M2 task 7: add and remove Set members

Status: **decisions D1–D6 confirmed by the user 2026-09-21**, after phase 1 verified every Redis
fact they rest on against a live server ([ADR-0016](../adr/0016-set-members-are-added-and-removed.md)).
Execution by a Sonnet subagent, phase by phase, stopping at each checkpoint. Phase 1 is done.

**Base branch.** This stacks on `split-update-module` (PR #35, review M2), not on `main`. Phase 3
edits `crates/core/src/update/editor.rs`, which only exists after that split. If #35 merges first,
rebase onto `main` — it is a clean fast-forward, since nothing here touches a file #35 did not
already move.

## Context

`PLAN.md` M2 row 7: "Value edit — Set: add/remove member · Membership diff in the preview; no
ordering assumptions." PRD R4.1 (in-place edit of collection members with a diff-style confirm),
R4.4 (preview), R3.8 (live updates held while editing).

Task 6 (Hash) is the template, and the machinery it built is almost all reusable:

- **The write path** is one `Mutation` enum, one `Msg::MutationSettled`, one
  `redis::mutate::execute` (review H1). Adding a mutation touches `crates/core/src/mutation.rs`,
  the `PendingMutation` that previews it, and the shell's `execute` — and nothing else.
- **The guard shape** is [ADR-0015](../adr/0015-hash-field-writes-are-guarded.md): a write that
  could recreate a key deleted under the confirm dialog is sent as a Lua script with an `EXISTS`
  check, via `EVAL` on every supported server version.
- **The edit surface** is `EditBuffer` + `EditTarget` (`crates/core/src/state/editor.rs`) and
  `EditPhase` (review H2). `a` opens a buffer directly; a shown duplicate blocks staging.
- **`e`/`a`/`d` are focus-gated** to the value pane with a value fetched (ADR-0015 D4), and `d` is
  focus-dependent: keys pane stages `DEL`, value pane stages the collection-member delete.

### Redis facts that shape the write

**Verify each of these before writing code** (phase 1) — against redis.io and against a real
server via `./scripts/redis-up.sh cli`. They are the planning agent's reading, not yet confirmed.

- **`SADD` creates the key when it does not exist.** Adding a member to a set that expired or was
  deleted under the confirm dialog would bring the key back — the same hazard ADR-0014 closed for
  Strings and ADR-0015 closed for Hash fields. There is no `SADD … XX`.
- **`SADD` on a member already present is a no-op returning 0**, which is the duplicate guard, and
  is the exact shape `HSETNX` gave the Hash add.
- **`SREM` never creates anything.** Removing the last member deletes the key — Redis's own
  behaviour for an emptied collection, the same as `HDEL` on a last field. Nothing to guard.
- **There is no per-member TTL for sets.** Redis 7.4's field expiry (`HEXPIRE`) is Hash-only, so
  the set add script has no TTL-preservation branch and is strictly simpler than
  `HASH_FIELD_EDIT_SCRIPT`.
- **A key's own TTL is untouched by `SADD`/`SREM`.**
- **`SADD`/`SREM` against a key that is now a different type return `WRONGTYPE`.** The `EXISTS`
  guard does not check type, so a key deleted and recreated as another type under the dialog
  surfaces an ordinary error notification carrying the command (R7.4). This is the same accepted
  behaviour ADR-0015's scripts have; do not add a type check without saying why in the ADR.
- Sets are unordered, and `SSCAN` gives no stable order. The read already windows at 500 members
  (`sscan_window`, `crates/app/src/redis/read.rs`), and `MemberValue { members: Vec<Vec<u8>>,
  total }` holds bytes, not `String` (review C2).

## Decisions (drafted — confirm at checkpoint 1)

**D1. Scope is add and remove. A member is never edited in place.**
A Set member has no identity apart from its bytes: there is no field name to keep while the value
changes. "Editing" one is `SREM old` + `SADD new`, which is a *rename*, not an edit — and Hash
field rename is already deferred to its own row (PLAN M2 task 14, ADR-0015 D3). Set member rename
belongs with it, not here. `e` on a Set row gives a notice rather than opening a buffer.

**D2. Add is a guarded Lua script; remove is a plain `SREM`.**
Mirrors ADR-0015 exactly, including `EVAL`-not-`EVALSHA`:

```lua
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('SADD', KEYS[1], ARGV[1])
```

`-1` the key is gone (nothing written, nothing recreated), `0` the member was already there, `1`
added. Remove is `SREM key member`, with `0` removed reported as a notice exactly as `HDEL`'s is.

**D3. The add form is one part, not two.**
The Hash add form is a two-part `FIELD`/`VALUE` form because a field has a name *and* a value. A
Set member is only a value, so `EditTarget::NewSetMember` carries no `FieldPart` and the form is a
single capture. The shown-duplicate guard still applies — exact byte equality against a member in
the fetched window blocks staging — and a duplicate hidden outside the 500 window is still caught
at write time by `SADD` returning 0, the same division ADR-0015 accepted.

**The guard blocks `Ctrl-S`, not `Enter`** — corrected during phase 3, where this decision
originally said both. `Enter`'s job in the Hash add form is to *advance from the name part to the
value part*, not to submit; with one part there is nothing to advance to, so `Enter` keeps its
ordinary editor meaning and inserts a newline, exactly as it does in a String buffer. `Ctrl-S`
(`Action::EditorStage`) is the stage key in every buffer, and it is the one the guard belongs on.
A member that picks up a stray newline this way is not silently written: the confirm dialog shows
it before anything runs, which is what R4.4's preview is for.

**D4. Binary members are refused**, with a notice, exactly as binary Hash fields are
(`editor.rs`: "binary fields aren't editable here yet"). Wording: "binary members aren't editable
here yet".

**D5. `d` on a Set row stages `SREM`**, extending ADR-0015 D4's focus-dependent `d`. When the
member is the set's only one at staging time, the dialog warns that the key itself will go — the
same warning, and the same `last_field`-shaped flag, that the Hash delete already shows.

**D6. `Action::AddField` is renamed `Action::Add`.**
It now serves Hash fields and Set members both. The user-facing label is already the neutral
`"add"` (`keymap/mod.rs:200`) and action names do not appear in user config, so this is an
internal rename with no compatibility surface. Leaving it as `AddField` would be a type name
leaking into a shared action — exactly the kind of thing review M3 is about.

## New and changed types

| Where | What |
|---|---|
| `core/src/mutation.rs` | `Mutation::{AddSetMember, DeleteSetMember}`; `command_label` → `SADD key` / `SREM key` |
| `core/src/mutation.rs` | `NotWritten::MemberExists` — a new variant, not a reuse of `FieldExists`; the glossary's distinctions are load-bearing (CLAUDE.md) |
| `core/src/state/mod.rs` | `PendingMutation::{AddSetMember, DeleteSetMember { last_member }}`, with `command_text()` and `guard_text()` |
| `core/src/state/editor.rs` | `EditTarget::NewSetMember`; `EditBuffer::new_set_member()` |
| `core/src/keymap/mod.rs` | `Action::AddField` → `Action::Add` (D6) |
| `app/src/redis/mutate.rs` | `SET_MEMBER_ADD_SCRIPT`, `add_set_member`, `delete_set_member`, `MemberAdd` enum; two arms in `execute` |

`guard_text` for the add: **"only if the key still exists · never duplicates a member"**. Delete
has no guard line, as `DeleteHashField` has none.

## Phases

Each phase is one commit, ends at a checkpoint, and must be reported before the next starts.

### Phase 1 — Verify the Redis facts, then write the decisions down

Docs only, no code. **This checkpoint exists so the decisions are reviewed before anything is
built on them.**

1. Verify every bullet under "Redis facts that shape the write" against redis.io and a real server
   (`./scripts/redis-up.sh`, then `./scripts/redis-up.sh cli`). Report anything that differs from
   what is written above — a wrong fact here invalidates D1–D3.
2. Write **ADR-0016 — Set members are added and removed, never edited in place**. Its subject is
   D1: the genuinely new decision. Record the verified Redis facts, the guarded-add script (D2),
   and the rejected alternatives — in-place edit as `SREM`+`SADD` in one script (rejected: it is a
   rename, and renames are task 14's, where Hash and Set can get one shape between them), and a
   plain `SADD` (rejected: recreates a key that went away under the dialog). Follow ADR-0015's
   structure and length; it is the closest sibling.
3. Update `PLAN.md` M2 row 7's "Proves" column to name what will actually be tested.

**Checkpoint 1.** Report the fact-check results and the ADR. **Stop here** — do not start phase 2
until the main agent confirms the decisions.

### Phase 2 — Core types and previews

`mutation.rs`, `state/mod.rs`, `state/editor.rs`, `keymap/mod.rs` per the table above. Unit tests
for `command_label`, `command_text`, `guard_text`, the `last_member` warning, and
`EditBuffer::new_set_member`. No wiring into `update/` yet.

**Checkpoint 2.** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`. Report counts and what you added.

### Phase 3 — Core wiring

`crates/core/src/update/editor.rs` and `confirm.rs`: `a` opens the set add form, `d` stages the
remove, `e` refuses with D1's notice. Focus gating per ADR-0015 D4. Shown-duplicate guard per D3.
R3.8 held-while-editing must hold for a Set exactly as it does for a Hash.

Tests: focus gating including both refusal notices, the duplicate block, the two dialogs
(including the last-member warning), read-only refusal at confirm, and a live update held while
the add form is open.

**Checkpoint 3.** Same three commands. Report counts.

### Phase 4 — Docker-backed proof of the shell

**The shell code already exists.** `SET_MEMBER_ADD_SCRIPT`, `add_set_member`, `delete_set_member`,
the `MemberAdd` enum and both `execute` arms landed in phase 2: adding a `Mutation` variant makes
`execute`'s match non-exhaustive, and the honest options there were a real implementation or a
`todo!()` that CLAUDE.md forbids. The phase boundary was drawn in the wrong place — `Mutation` *is*
the seam between core and shell, so a phase that adds a variant cannot stop at the crate edge. See
"Found while building".

So phase 4 is proof, not construction. **Read `redis/mutate.rs` first and extend it only if a test
finds it wrong** — do not rewrite what is there.

Integration tests in `crates/app/tests/integration.rs`, every one `#[ignore]`d, proving against a
real server: the add script never recreates a gone key; it refuses a duplicate without writing;
`SREM` removes the last member and the key goes with it; a binary member round-trips; and
`WRONGTYPE` surfaces as an error rather than a panic.

**Checkpoint 4.** The three commands, plus `cargo test -p redis-pane -- --ignored
--test-threads=1` (needs Docker). Report both counts.

### Phase 5 — Render, docs, and the M3 inventory

1. Hint bar for a Set with the value pane focused; golden frames for the add form (empty, typing,
   shown-duplicate), both confirm dialogs, and the last-member warning.
2. Update `CONTEXT.md` if any term here is new, `PLAN.md` M2 row 7 to **done**, and ADR-0016's
   Consequences to match what was actually built.
3. **The M3 inventory.** This change makes Set the second editable collection type, which is the
   condition review M3 was deferred until. Do not do M3. Instead, append to
   `docs/reviews/2026-09-13-codebase-design-review.md` §9 a short list of every place a `match` on
   `Value`, `EditTarget` or `PendingMutation` now has a Hash arm and a Set arm side by side, with
   `file:line`. That list is M3's actual scope, and it is cheapest to collect while the second case
   is fresh.

**Checkpoint 5.** Full local verification. Do not open the PR — the main agent does that.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'   # must print nothing
cargo test -p redis-pane -- --ignored --test-threads=1                      # phase 4 on; needs Docker
```

Baseline on this branch before any change: **core 404, golden 122, app 38**, integration **54**.
Counts only go up.

## Manual test plan

```bash
./scripts/redis-up.sh
python3 - <<'EOF'
import sys; sys.path.insert(0, "scripts")
from resp import Resp
r = Resp()
r.call("SADD", "edit:set", "alpha", "beta", "gamma")
r.call("SADD", "one:set", "only")
r.call("SADD", "bin:set", b"m\xff\x80", "plain")
EOF
cargo run -p redis-pane
```

- [ ] **Add.** Open `edit:set`, press `a`, type a new member, `Ctrl-S`, check the dialog shows
      `SADD edit:set` with the guard line, `y`. It appears, and the header returns to live.
- [ ] **Shown duplicate.** `a`, type `alpha`. `Enter`/`Ctrl-S` are blocked while it matches.
- [ ] **Hidden duplicate.** Stage a member, then add it from a second terminal before pressing
      `y`. The write refuses with "already a member" and nothing changes.
- [ ] **Gone key.** Stage an add, `DEL edit:set` from a second terminal, then `y`. Refused, and the
      key is *not* recreated.
- [ ] **Remove, and the last one.** `Enter` to pick a row, `d`, `y`. On `one:set` the dialog warns
      the key will go, and it does.
- [ ] **No in-place edit.** `e` on a Set row gives D1's notice rather than opening a buffer.
- [ ] **Binary.** `bin:set` shows `m\xff\x80` escaped; `e`/`a` on it give D4's notice.
- [ ] **Read-only.** `⌃R`, then stage any of the above: refused at the dialog, nothing sent.
- [ ] **Held while editing.** With the add form open, change the set from a second terminal. The
      header reads held, and the form is untouched.

## Out of scope

- **Member rename** (`SREM`+`SADD` in one script) — task 14, alongside Hash field rename.
- **Review M3** — inventory only, per phase 5.
- **List, ZSet, TTL editing** — tasks 8, 9, 10, in that order.

## Found while building

_(Executor: append anything noticed but deliberately not fixed, with `file:line`.)_

**Phase 2.** Rust's exhaustive-match checking forced touching files outside phase 2's declared
scope (`mutation.rs`, `state/mod.rs`, `state/editor.rs`, `keymap/mod.rs`) the moment the new
`Mutation`/`PendingMutation`/`NotWritten`/`EditTarget` variants existed at all — every `match` over
one of these four types anywhere in the workspace has to be exhaustive or the crate that owns it
fails to build, regardless of whether anything can reach the new arm yet. Rather than leave the
build red for two phases, each of the following got the minimal arm needed to compile, each
commented as unreachable-for-now and citing ADR-0016/D3 so phase 3/4's author knows why it is
there and can extend it rather than re-derive it:

- `crates/core/src/render/mod.rs` (`confirm_overlay`) — `AddSetMember`/`DeleteSetMember` dialog
  lines, mirroring the Hash add/last-field arms. No golden frame exercises them (nothing stages
  either variant yet), so the golden count is unaffected.
- `crates/core/src/update/confirm.rs` (`not_written`) — `NotWritten::MemberExists` folded into the
  existing field-refusal arm with member-specific wording.
- `crates/core/src/update/editor.rs` (`stage_editor`) — `EditTarget::NewSetMember` staged as
  `PendingMutation::AddSetMember`, one field narrower than `NewHashField`'s arm. Nothing opens this
  `EditTarget` yet (that is phase 3's `a`-on-a-Set wiring), so this arm is currently dead code
  reachable only by a future caller.
- `crates/core/src/state/open.rs` (`edit_verb`) — `"✎ adding member"` for `NewSetMember`, alongside
  the existing Hash verbs.
- `crates/app/src/redis/mutate.rs` (`execute`) — this is the one that actually crosses the
  `crates/app` boundary the phase description asked to leave alone. `Mutation::AddSetMember`/
  `DeleteSetMember` needed real arms, not stubs (a `todo!()`/panic would violate CLAUDE.md's "never
  panic on a Redis error" even for an unreachable path, and an `Err(_)` arm would misreport a
  success as a failure the moment phase 3 makes it reachable). Implemented the guarded add exactly
  as ADR-0016 D2 specifies — `SET_MEMBER_ADD_SCRIPT`, `add_set_member`, `delete_set_member`, and a
  `MemberAdd` enum — which happens to be the exact shape the "New and changed types" table assigns
  to phase 4. No integration test was added; phase 4 should treat this as already done and add the
  `#[ignore]`d proof against a real server rather than rewriting it.

None of this wires a keypress to either new mutation — `a`/`d`/`e` on a Set still behave exactly as
before this phase, since nothing in `update/keys.rs` or `update/mod.rs`'s dispatch constructs
`EditTarget::NewSetMember` or stages `DeleteSetMember`. Phase 3 is still the phase that makes any
of this reachable.

**Phase 3.** Two places where D3's wording needed a judgment call the ADR did not fully settle,
plus one bug the wiring exposed:

- D3 says the shown-duplicate guard "blocks `Enter`/`Ctrl-S` while typing," echoing ADR-0015's
  words for the Hash *name part*. But `EditTarget::NewSetMember` has no `FieldPart` — `active_part()`
  returns `None` — so it never routes through `name_part_key`, the only place `Enter` means
  "advance/stage" rather than "insert a newline." Blocking literal `Enter` on the Set add form's
  single `TextArea` would just prevent multi-line members, which is not what a duplicate guard is
  for. Implemented: the guard blocks `⌃S` (`Action::EditorStage`) only —
  `crates/core/src/update/editor.rs`'s `set_member_blocked`, called from `editor_key`'s
  `Action::EditorStage` arm — and `Enter` keeps its ordinary meaning (insert a newline) for every
  `EditTarget`, Set included. If literal `Enter`-as-stage is wanted for the Set form, that is a
  follow-up, not a read of D3 phase 3 could safely make unilaterally.
- D4 ("binary members are refused ... 'binary members aren't editable here yet'") does not say
  where this notice appears, since D1 already refuses `e` on every Set row unconditionally. Read it
  as governing `e`'s *wording*: a non-UTF-8 member gets D4's notice, a UTF-8 one gets D1's
  rename-notice — mirroring how Hash's `for_hash_field` only special-cases binary content, never a
  blanket refusal. Implemented in `open_editor` (`crates/core/src/update/editor.rs`).
- `crates/core/src/update/confirm.rs:151` (`nothing_to_remove`) hard-coded "field already gone" for
  every `MutationOutcome::NothingToRemove`, including a `DeleteSetMember` whose `SREM` found the
  member already gone. Phase 2 could not have caught this — nothing staged a `DeleteSetMember`
  yet — but it is exactly the kind of member/field wording bug CLAUDE.md's glossary section warns
  about, and phase 3's own `d`-wiring is what makes it reachable. Fixed in this phase: the notice
  now reads "member already gone" for `DeleteSetMember`, "field already gone" for everything else,
  decided once from the `Mutation` itself.
