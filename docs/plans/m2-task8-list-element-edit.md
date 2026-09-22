# M2 task 8: edit, add and remove List elements

Status: **drafted 2026-09-22**, scope decision D1 confirmed by the user. The Redis facts below were
verified against a live server (`redis:8.4-alpine`) while drafting — phase 1 re-verifies them and
turns them into [ADR-0017](../adr/0017-list-elements-are-addressed-by-index.md). Execution by a
Sonnet subagent, phase by phase, stopping at each checkpoint.

**Base branch.** `main` (`fc2bdc4`). Nothing here stacks on unmerged work.

## Context

`PLAN.md` M2 row 8: "Value edit — List: index-addressed edit, insert, remove · Preview correctly
represents index shift on insert/remove." PRD R4.1 (in-place edit of collection members with a
diff-style confirm), R4.4 (preview), R3.8 (live updates held while editing), R3.13 (values are
bytes).

Tasks 6 (Hash, [ADR-0015](../adr/0015-hash-field-writes-are-guarded.md)) and 7 (Set,
[ADR-0016](../adr/0016-set-members-are-added-and-removed.md)) are the template, and their machinery
is reusable unchanged: one `Mutation` enum, one `Msg::MutationSettled`, one `redis::mutate::execute`
(review H1); `EditBuffer` + `EditTarget` + `EditPhase` (review H2); `e`/`a`/`d` focus-gated to the
value pane with a value fetched (ADR-0015 D4).

**What is genuinely new here.** List is the first type whose elements are addressed by *position*
rather than by name (Hash) or by their own bytes (Set). That changes the guard, and it is the whole
substance of this task — see D2.

### Redis facts that shape the write

Verified 2026-09-22 against a live `redis:8.4-alpine` via `scripts/resp.py`. **Phase 1 re-verifies
each one** and reports any difference — a wrong fact here invalidates D2 and D3.

- **`LSET` on a missing key is an error** — `ERR no such key`. Unlike `SADD` and `HSET`, `LSET`
  *cannot* recreate a key deleted under the confirm dialog. The hazard ADR-0014 and ADR-0015 exist
  to close does not apply to the List edit path.
- **`LSET` past the end is an error** — `ERR index out of range`.
- **`LPUSH`/`RPUSH` create the key when it does not exist.** The add path *does* have the
  recreate hazard, and needs ADR-0015's `EXISTS` guard.
- **`LREM key 1 <value>` removes by value, the first match from the head — not by index.** Redis
  has no remove-by-index command. Verified on `[x, y, x, z]`: `LREM 1 x` removed index 0, leaving
  `[y, x, z]`. A naive remove of the *second* `x` would delete the wrong element.
- **`LINSERT` is pivot-by-value and also takes the first match**, returning `-1` when the pivot is
  absent. This is why D1 keeps insert at the ends.
- **Emptying a list deletes the key**, exactly as `HDEL` of a last field and `SREM` of a last
  member do.
- **`LINDEX` on a missing key returns nil**, so a compare-and-set on the element subsumes an
  existence check — see D2 for why the scripts still check `EXISTS` separately.
- **Binary elements round-trip through `LSET` and `LINDEX` unchanged** (verified with
  `m\xff\x80`). `IndexedValue.items` is already `Vec<Vec<u8>>`, not `String` (review C2).
- **A key's own TTL is untouched by `LSET`/`LPUSH`/`RPUSH`/`LREM`.** There is no per-element TTL
  for lists — Redis 7.4's field expiry is Hash-only — so none of these scripts has a
  TTL-preservation branch, unlike `HASH_FIELD_EDIT_SCRIPT`.
- **The read window is the first 500 items**: `lrange(key, 0, WINDOW - 1)` in
  `crates/app/src/redis/read.rs`. Shown rows are therefore absolute indices `0..=499`, so a row's
  display position *is* its Redis index with no arithmetic. `IndexedValue.total` is `LLEN` and may
  exceed it.

## Decisions

**D1. Scope is edit in place, remove, and add at either end. No insert at a position.**
Confirmed by the user. `e` on a row edits that element (`LSET`), `d` removes it, and `a` adds a new
element at the head or the tail. Inserting *between* two existing elements is out: Redis's only
insert primitive is `LINSERT`, which addresses its pivot by value and takes the first match, so on a
list with duplicate elements it inserts in the wrong place. It can be done duplicate-safely inside a
script (`LSET` the target to a unique sentinel, `LINSERT BEFORE` the sentinel, `LSET` the sentinel
back), and that was offered and declined for this round: head and tail are how lists are actually
used, and a head push already shifts every index by one, which is the index-shift behaviour PLAN's
"Proves" column asks this task to get right. Insert-at-position, if it is ever wanted, is its own
row.

Unlike Set (ADR-0016 D1), **in-place edit is the natural operation here**: a list element has an
identity — its index — that survives its bytes changing. `LSET` is a real edit, not a rename.

**D2. Every write is a guarded Lua script, and the guard is compare-and-set on the element.**

The hazard List has is not the one Hash and Set have. `LSET` cannot recreate a gone key, so there is
nothing to guard there. What it *can* do is write to the wrong element: indices are not stable under
concurrent writes, and a single `LPUSH` from another client between staging and confirming shifts
every index by one. `LSET key 3 <new>` staged against what the reader saw would then silently
overwrite whatever moved into slot 3. That is strictly worse than the Hash/Set hazard — it corrupts
rather than refuses, and nothing on screen would say so.

So each script re-checks that the element at the index still holds the exact bytes the read
returned, and refuses otherwise. Verified working, including the stale case and the gone-key case:

```lua
-- edit: LSET key index new
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('LINDEX', KEYS[1], ARGV[1]) ~= ARGV[2] then
  return -2
end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3])
return 1
```

`-1` the key is gone, `-2` the element moved or changed, `1` written. The `EXISTS` check is
redundant for correctness — `LINDEX` on a missing key returns nil, which fails the compare anyway —
but it is kept so the two cases can be *reported* apart: "key no longer exists" and "that element
moved" are different things for the reader to see, and folding them would give a deleted key the
wrong words. This is the same reasoning `NotWritten::reason` is built on (`mutation.rs:118`).

```lua
-- delete: remove the element at index, duplicate-safely
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('LINDEX', KEYS[1], ARGV[1]) ~= ARGV[2] then
  return -2
end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3])
redis.call('LREM', KEYS[1], 1, ARGV[3])
return 1
```

`ARGV[3]` is a sentinel. Because a Lua script is atomic, no other client can ever observe the list
in its sentinel state. The sentinel must still not collide with a *real* element earlier in the
list, or `LREM 1` would remove that one instead — so it is generated per call as
`__redis-pane-rp:<random hex>` rather than being a constant. Phase 2 sources those bytes from the
injected randomness the core already has (ADR-0011), not from the shell. Verified: deleting index 2
of `[x, y, x, z]` leaves `[x, y, z]`, and deleting the only element drops the key.

```lua
-- add: LPUSH/RPUSH, never recreating the key
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call(ARGV[1], KEYS[1], ARGV[2])   -- ARGV[1] is 'LPUSH' or 'RPUSH'
```

Add has no compare-and-set: it does not address an existing element, so there is nothing to compare.
It has the ordinary ADR-0015 `EXISTS` guard, because `LPUSH`/`RPUSH` do recreate a gone key.

`EVAL` on every call, never `EVALSHA` — ADR-0015 and ADR-0016 both settle this, and the reason
(a `SCRIPT FLUSH` between load and use) has not changed.

**D3. A refused write says *which* guard refused.** Two new `NotWritten` variants:
`ElementMoved` ("that element changed — the list shifted") and nothing else; `KeyGone` is reused as
is. A stale index is expected to be the common refusal on a busy list, so its wording should tell
the reader to look again rather than suggesting the write failed: phase 1 settles the exact string
in the ADR.

**D4. Binary elements are not editable inline**, with a notice, exactly as binary Hash fields and
Set members are. Wording: "binary elements aren't editable here yet". The add form still produces
text only.

**D5. `d` on a List row stages the guarded remove**, extending ADR-0015 D4's focus-dependent `d`:
keys pane stages `DEL`, value pane stages the element remove. When the element is the list's only
one at staging time, the dialog carries the same key-will-go warning `DeleteHashField`'s
`last_field` and `DeleteSetMember`'s `last_member` already show.

**D6. The add form is one capture plus an end toggle.** `EditTarget::NewListElement { end }`, where
`end` is `ListEnd::{Head, Tail}`, defaulting to `Tail` — appending is the common case, and it is the
one that does not renumber the rows the reader is looking at. `Tab` toggles it while the form is
open, and the form's label says which end is active, so the choice is visible rather than
remembered. The confirm dialog shows `LPUSH key` or `RPUSH key` accordingly. There is no
shown-duplicate guard: a list is allowed duplicates, which is precisely what distinguishes it from
a Set.

**D7. Only the fetched window is editable.** A list longer than 500 items shows its first 500, and
rows past that cannot be selected, so they cannot be staged either. `IndexedValue.total` already
drives the header's "showing 500 of N" line (`Viewer::window`). No new refusal is needed — the
rows simply are not there — and phase 3 should add no code for this beyond a test pinning that
`total > 500` does not change what `e`/`d` do to a row inside the window.

**D8. The seven silent-fallback sites from the M3 inventory become exhaustive, here.**
List is the third editable collection type, which is the case the inventory in
`docs/reviews/2026-09-13-codebase-design-review.md` §9 was collected against. Twelve of its nineteen
sites are exhaustive matches that will simply fail to compile until this task handles them — those
need no attention beyond doing the work. The other seven have a `_` fallback or are sequential
`if let`s, and will silently do the wrong thing for a List:

| Site | What goes wrong silently |
|---|---|
| `state/mod.rs:321` `PendingMutation::guard_text` | `_ => None` drops the guard line from all three new dialogs |
| `state/editor.rs:254` `EditBuffer::active_part` | `_ => None`; D6's end toggle needs a part-like answer |
| `update/confirm.rs:143` `nothing_to_remove` | `_ => "field"` reports a removed element in a field's words |
| `update/editor.rs:20` `open_editor` | a missing `if let` falls through to `from_value`'s generic refusal |
| `update/editor.rs:116` `begin_add_field` | `_` arm gives List the "fields can only be added to a hash" refusal |
| `update/viewer.rs:343` `delete_hash_field` | `_` arm refuses a List delete that D5 says should stage |
| `render/mod.rs:1161` `hint_bar` | a missing `if` block falls through to the generic hint |

Per the inventory's own recommendation, prefer **turning each into an exhaustive `match`** over
`Value` or `EditTarget` rather than adding a third branch beside the existing two. The point is that
ZSet (task 9) should fail to compile, not fail silently. This is not review M3 — the trait redesign
stays deferred, and task 9 is when it has three real cases to be designed against.

## New and changed types

| Where | What |
|---|---|
| `core/src/mutation.rs` | `Mutation::{SetListElement, AddListElement, DeleteListElement}`; labels `LSET key <i>` / `LPUSH key` / `RPUSH key` / `LREM key <i>` |
| `core/src/mutation.rs` | `NotWritten::ElementMoved` (D3) |
| `core/src/state/mod.rs` | `PendingMutation::{SetListElement, AddListElement, DeleteListElement { last_element }}` with `command_text()` and `guard_text()` |
| `core/src/state/value.rs` | `ListEnd { Head, Tail }` (D6) |
| `core/src/state/editor.rs` | `EditTarget::{ListElement { index }, NewListElement { end }}`; `EditBuffer::list_element()` / `new_list_element()` |
| `app/src/redis/mutate.rs` | `LIST_ELEMENT_EDIT_SCRIPT`, `LIST_ELEMENT_DELETE_SCRIPT`, `LIST_ELEMENT_ADD_SCRIPT`; `set_list_element`, `add_list_element`, `delete_list_element`; three arms in `execute` |

Guard lines for the dialog:

- edit — **"only if that element is still there · index 3"**
- add — **"only if the key still exists"**
- delete — **"only if that element is still there · index 3"**

## Phases

Each phase is one commit, ends at a checkpoint, and must be reported before the next starts.

**Read task 7's "Found while building" first** (`m2-task7-set-member-edit.md`). Its phase 2 note is
the important one and applies here identically: the moment a new `Mutation`, `PendingMutation`,
`NotWritten` or `EditTarget` variant exists, *every* match on those types across both crates must be
exhaustive or the workspace does not build. `Mutation` is the core/shell seam, so **phase 2
necessarily includes the real `crates/app/src/redis/mutate.rs` arms** — a `todo!()` there would
violate CLAUDE.md's "never panic on a Redis error". That is planned, not a deviation.

### Phase 1 — Verify the Redis facts, then write the decisions down

Docs only, no code. **This checkpoint exists so the decisions are reviewed before anything is built
on them.**

1. Re-verify every bullet under "Redis facts that shape the write" against redis.io and a real
   server (`./scripts/redis-up.sh`, then `./scripts/redis-up.sh cli`). Report anything that differs.
2. Write **ADR-0017 — List elements are addressed by index, so writes compare before they set**.
   Its subject is D2: the compare-and-set guard and why List's hazard is a different hazard from
   ADR-0015's. Record the verified facts, all three scripts, the per-call sentinel and why it is not
   a constant, and the rejected alternatives — a plain `LSET` (rejected: silently writes the wrong
   element after a concurrent push), `LREM` by value (rejected: removes the wrong duplicate),
   `LINSERT` for insert-at-position (rejected per D1, with the sentinel technique recorded so a
   future row does not have to rediscover it), and re-reading before writing instead of guarding
   (rejected: a read-then-write is not atomic, so it narrows the race without closing it). Settle
   D3's exact refusal wording. Follow ADR-0016's structure and length.
3. Update `PLAN.md` M2 row 8's "Proves" column to name what will actually be tested, and note D1's
   scope narrowing — the row currently says "insert", which now means at the ends only.

**Checkpoint 1.** Report the fact-check results and the ADR. **Stop here** — do not start phase 2
until the main agent confirms the decisions.

### Phase 2 — Core types, previews, and the shell arms exhaustiveness forces

`mutation.rs`, `state/mod.rs`, `state/value.rs`, `state/editor.rs` per the table above, plus the
minimal arms every other match needs to compile, plus the real `redis/mutate.rs` implementations of
all three scripts. Unit tests for `command_label`, `command_text`, `guard_text`, the `last_element`
warning, `NotWritten::ElementMoved`'s wording, and the `EditBuffer` constructors. No wiring into
`update/`'s dispatch yet — `e`/`a`/`d` on a List still behave as they do today.

Do **not** patch the seven D8 sites with a fallback to keep the build green. `guard_text` and
`active_part` are two of them and will be reached by this phase's types; give them their exhaustive
form now, as D8 asks.

**Checkpoint 2.** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`. Report counts and what you added.

### Phase 3 — Core wiring, and the rest of D8

`crates/core/src/update/editor.rs`, `viewer.rs` and `confirm.rs`: `e` opens the element editor, `a`
opens the add form with its end toggle, `d` stages the remove. Focus gating per ADR-0015 D4. R3.8
held-while-editing must hold for a List exactly as it does for a Hash. Finish the remaining D8
sites.

Tests: focus gating including the binary refusal (D4), the end toggle, all three confirm dialogs
including the last-element warning, read-only refusal at confirm, a live update held while the
editor is open, and D7's long-list case.

**Checkpoint 3.** Same three commands. Report counts.

### Phase 4 — Docker-backed proof of the shell

Integration tests in `crates/app/tests/integration.rs`, every one `#[ignore]`d, proving against a
real server: an edit lands; an edit refuses without writing when the list shifted under it (push a
new head between read and write — this is the test the whole task exists for); an edit refuses on a
gone key and does not recreate it; an add refuses on a gone key and does not recreate it; a delete
removes the *right* element on a list with duplicate values; a delete of the last element takes the
key with it; a binary element round-trips; and `WRONGTYPE` on a key of another type surfaces as an
error rather than a panic.

**Checkpoint 4.** The three commands, plus `cargo test -p redis-pane -- --ignored
--test-threads=1` (needs Docker). Report both counts.

### Phase 5 — Render, golden frames, docs

1. Hint bar for a List with the value pane focused; golden frames for the element editor, the add
   form in both `Head` and `Tail` states, all three confirm dialogs, and the last-element warning.
2. Update `CONTEXT.md` if any term here is new, `PLAN.md` M2 row 8 to **done**, and ADR-0017's
   Consequences to match what was built.
3. Update `docs/reviews/2026-09-13-codebase-design-review.md` §9: mark the seven D8 sites resolved
   with the SHA, and re-state what is left of the M3 inventory now that a third type exists. Say
   plainly whether three cases made the trait's shape obvious or still did not — that is the
   question task 9 inherits.

**Checkpoint 5.** Full local verification. Do not open the PR — the main agent does that.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'   # must print nothing
cargo test -p redis-pane -- --ignored --test-threads=1                      # phase 4 on; needs Docker
```

Baseline on `main` before any change: **core 439, golden 129, app 38**, integration **61**. Counts
only go up.

## Manual test plan

```bash
./scripts/redis-up.sh
python3 - <<'EOF'
import sys; sys.path.insert(0, "scripts")
from resp import Resp
r = Resp()
r.call("DEL", "edit:list", "one:list", "dup:list", "bin:list", "big:list")
r.call("RPUSH", "edit:list", "alpha", "beta", "gamma")
r.call("RPUSH", "one:list", "solo")
r.call("RPUSH", "dup:list", "x", "y", "x", "z")
r.call("RPUSH", "bin:list", b"e\xff\x80", "plain")
r.call("RPUSH", "big:list", *[f"item-{i}" for i in range(600)])
EOF
cargo run -p redis-pane -- --url redis://127.0.0.1:6379
```

- [ ] **Edit.** Open `edit:list`, select `beta`, `e`, change it, `Ctrl-S`. The dialog shows
      `LSET edit:list 1` with the guard line and a red/green diff. `y` writes it.
- [ ] **Stale index.** Stage an edit of `beta`, then `LPUSH edit:list head` from a second terminal,
      then `y`. Refused with D3's wording, and `beta` is **unchanged** — this is the whole point.
- [ ] **Gone key.** Stage an edit, `DEL edit:list` elsewhere, `y`. Refused; the key is not recreated.
- [ ] **Add at both ends.** `a`, type a value, `Tab` to flip `Head`/`Tail`, `Ctrl-S`. The dialog
      names `LPUSH` or `RPUSH` to match, and the element lands at the end shown.
- [ ] **Add to a gone key.** Stage an add, `DEL` elsewhere, `y`. Refused, not recreated.
- [ ] **Remove the right duplicate.** On `dup:list`, remove the *second* `x` (index 2). The list
      becomes `[x, y, z]`, not `[y, x, z]`.
- [ ] **Remove the last one.** On `one:list`, `d`, `y`. The dialog warns the key will go, and it does.
- [ ] **Binary.** `bin:list` shows `e\xff\x80` escaped; `e` on it gives D4's notice.
- [ ] **Long list.** `big:list` header says 600 items with 500 shown; editing a row inside the
      window works and addresses the right index.
- [ ] **Read-only.** `⌃R`, then stage any of the above: refused at the dialog, nothing sent.
- [ ] **Held while editing.** With the editor open, change the list from a second terminal. The
      header reads held, and the buffer is untouched.

## Out of scope

- **Insert at a position** — D1. The duplicate-safe sentinel technique is recorded in ADR-0017 so a
  future row need not rediscover it.
- **Review M3's trait redesign** — D8 fixes the silent fallbacks only. Task 9 is where three cases
  can inform a design.
- **Trimming, rotating, or any multi-element write** (`LTRIM`, `LMOVE`) — not in PLAN M2 at all.
- **ZSet, TTL editing** — tasks 9 and 10.

## Found while building

_(Executor: append anything noticed but deliberately not fixed, with `file:line`.)_

**Phase 1.** All nine bullets under "Redis facts that shape the write" were re-verified against a
live `redis:8.4-alpine` and confirmed exactly as drafted — no differences, see ADR-0017 for the
observed output of each. All three D2 scripts were verified end to end, including the happy path,
the stale-index refusal (`-2`), and the gone-key refusal (`-1`) for both edit and delete, and the
duplicate-safe delete on `[x, y, x, z]`.

D3's exact wording was settled as **"that element moved — the list changed underneath it, look
again"**, not the shorter "that element changed" the plan's draft used as a placeholder. Reasoning
recorded in ADR-0017 D3: `ElementMoved` is expected to be the *routine* refusal on a busy list
(any concurrent write anywhere in the list can shift an index, not just a write to the same
element), so it needs to read as "look again," and naming the mechanism ("the list changed
underneath it") avoids implying the element's own value raced, which is the less common case.
