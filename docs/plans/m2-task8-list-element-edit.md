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
`__redis-pane-rp:<unique suffix>` rather than being a constant.

**The sentinel is minted in the shell** (`crates/app/src/redis/mutate.rs`), not the core, and does
not appear in `Mutation` at all: the core says "remove the element at index `i`, whose bytes were
`X`" and the shell decides how. The corrected plan text: an earlier draft of this decision said the
core would source the bytes from "the injected randomness the core already has (ADR-0011)". **It has
none** — there is no `rand` dependency anywhere in the workspace, and `update()`'s contract is
literally "no I/O, no clock, no randomness" (`crates/core/src/update/mod.rs:132`). Do not add one
for this. The shell composes the suffix from wall-clock nanoseconds plus a process-lifetime
`AtomicU64` counter, which needs no new dependency and is more than unique enough for something
whose only job is to not equal a real element of one list.

Verified: deleting index 2 of `[x, y, x, z]` leaves `[x, y, z]`, and deleting the only element
drops the key.

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

**Phase 2.** As task 7's phase 2 note warned, Rust's exhaustive-match checking forced touching
files outside the four declared in "New and changed types" the moment `Mutation::{SetListElement,
AddListElement, DeleteListElement}`, `NotWritten::ElementMoved`,
`PendingMutation::{SetListElement, AddListElement, DeleteListElement}` and
`EditTarget::{ListElement, NewListElement}` existed at all. Every arm added is real, working code —
not a stub — commented as unreachable-for-now and citing ADR-0017, exactly as task 7's phase 2 did,
since nothing in `update/`'s dispatch constructs any of these types yet:

- `crates/core/src/state/open.rs` (`edit_verb`) — `"✎ editing element"` / `"✎ adding element"` arms
  alongside the Hash/Set verbs.
- `crates/core/src/update/editor.rs` (`stage_editor`) — `EditTarget::ListElement` stages
  `PendingMutation::SetListElement`, `EditTarget::NewListElement` stages `AddListElement`, both the
  same shape their Hash/Set counterparts use. Also extended `is_new_field` (the "no prior value to
  be unchanged from" check a few lines above) to include `NewListElement`, for the same reason it
  already includes `NewHashField`/`NewSetMember` — a brand-new element has no original to be dirty
  against, and Redis allows an empty element the same as an empty Hash field value or Set member.
- `crates/core/src/update/confirm.rs` (`not_written`) — `NotWritten::ElementMoved` folded into the
  same arm as `FieldGone`/`FieldExists`/`MemberExists`: the write is dropped, the buffer comes back,
  and the Viewer re-reads. This is the correct behaviour for a stale-index refusal too, not merely
  the arm that happened to compile.
- `crates/core/src/render/mod.rs` (`confirm_overlay`) — `SetListElement`/`AddListElement`/
  `DeleteListElement` dialog lines, mirroring the Hash/Set add and last-element arms. No golden frame
  exercises them (nothing stages any of the three variants yet), so the golden count is unchanged
  from baseline.

Two sites the D8 table names as reached by this phase — `PendingMutation::guard_text` and
`EditBuffer::active_part` — were given their exhaustive form as asked, not a `_ => None` fallback.
`guard_text` also changed its return type from `Option<&'static str>` to `Option<String>`: the List
guard's wording carries the staged index ("only if that element is still there · index 3"), which no
`'static` string literal can hold. Every existing call site and test was updated (`.as_deref()` in
tests, `guard.to_string()`/`guard` in `render/mod.rs`, which compiles unchanged against either
return type). `active_part` stays `Option<FieldPart>` — a List target answers `None` explicitly now,
rather than through the wildcard, but D6's Head/Tail toggle is *not* a `FieldPart` and gets no
mechanism in this phase; phase 3 wires `Tab` some other way.

`crates/core/src/update/editor.rs`'s `staged_edit_found_key_gone` (`dialog_up`, ~line 437) matches
on `PendingMutation` with an or-pattern inside a `matches!` macro, which is not exhaustiveness-
checked — it compiles fine without a List arm, and silently treats a staged List mutation as "no
dialog up" (falls through to the `!open.is_editing()` branch). Not fixed here: nothing constructs a
List `PendingMutation` via a live dialog yet, so it is unreachable, but phase 3's `d`/`e`/`a` wiring
should add `SetListElement`/`AddListElement`/`DeleteListElement` to that pattern when it makes the
dialogs reachable — flagging it now so phase 3 does not have to rediscover it
(`crates/core/src/update/editor.rs:437`).

The five other D8 sites (`nothing_to_remove`, `open_editor`, `begin_add_field`, `delete_hash_field`,
`hint_bar`) use a wildcard `_` or a sequential `if let`, not an exhaustive match — they compile
unchanged and were deliberately left alone, per the plan's explicit instruction not to patch them
with a fallback. They remain phase 3's job.

Counts at checkpoint 2: `cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets
-- -D warnings` clean, `cargo test --workspace` — core 448 (baseline 439, +9: `command_label`/`key`
table rows, the `ElementMoved` wording test, `list_element`/`new_list_element` constructor tests, and
the `PendingMutation` `command_text`/`guard_text`/`last_element`/`into_command` tests), golden 129
(baseline 129, unchanged — no wiring, no new frames), app 38 (baseline 38, unchanged). The boundary
check (`cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'`) printed nothing.

**Phase 3.** `e`/`a`/`d` wired for a List in `crates/core/src/update/editor.rs`, `viewer.rs` and
`confirm.rs`, and all five remaining D8 sites finished:

- `open_editor` (`editor.rs`) gained a `Value::List` branch, ahead of the generic
  `EditBuffer::from_value` fallback: with the value cursor active, `e` calls
  `EditBuffer::list_element(open.cursor, element)`, refusing a non-UTF-8 element with D4's exact
  wording, "binary elements aren't editable here yet". `open.cursor` is used directly as the index —
  D7 means the fetched window always starts at 0, so a row's position *is* its Redis index.
- `begin_add_field` **renamed to `begin_add_entry`** (D8 asked for this — it now opens the add form
  for three collection types, not one) and given an exhaustive match over `Value` in place of the
  `_ => refuse` fallback: `Value::List(_) => EditBuffer::new_list_element()` joins the Hash/Set arms,
  and the refusal wording grew a third clause, "elements to a list".
- `delete_hash_field` (`viewer.rs`) **renamed to `delete_value_row`** for the same reason — D5 gives
  it a `Value::List` arm that stages `PendingMutation::DeleteListElement`, with `last_element` taken
  as a `total == 1` snapshot at staging time, the same discipline `last_field`/`last_member` already
  use. The match over `open.value` is now exhaustive (`Some(Value::Str(_) | Value::ZSet(_) | ... ) |
  None` all fall to the shared refusal), rather than a trailing `_`.
- `nothing_to_remove` (`confirm.rs`) is now an exhaustive match over `Mutation`: `DeleteHashField` →
  `"field"`, `DeleteSetMember` → `"member"`, every other variant (including all three List
  mutations) → `"entry"`. Behaviour is unchanged — no List write ever settles as
  `MutationOutcome::NothingToRemove` (ADR-0017's guard always resolves to `KeyGone`/`ElementMoved`/
  `Done` instead) — but the match is exhaustive so a future write that *does* start using
  `NothingToRemove` (a ZSet member removed twice, task 9) has to choose its own noun instead of
  silently inheriting `"field"`.
- `hint_bar` (`render/mod.rs`) gained two things: a `Value::List` cursor-active arm mirroring the
  Hash one (`e edit · a add · d remove`, ADR-0017's Consequences), and a dedicated branch inside the
  editor-open hint for `EditTarget::NewListElement` naming `Tab` explicitly — `Tab` means "insert a
  tab character" everywhere else in the editor, so the hint has to say when it means something else.
- **D6's Head/Tail toggle**: `EditBuffer` gained `toggle_list_end()`, a no-op outside
  `EditTarget::NewListElement`. `editor_key`'s `KeyCode::Tab` arm calls it when the target is
  `NewListElement`, falling through to the existing `insert_tab()` otherwise — the only target where
  `Tab` means something other than "insert a tab". The toggle's state lives where D6 put it,
  `EditTarget::NewListElement { end }` inside the `EditBuffer` (built in phase 2) — nothing new was
  added to `State` for it. The form's label requirement ("must show which end is active") is met by
  `OpenKey::edit_verb` (`state/open.rs`), which changed its return type from `&'static str` to
  `String` so it can format `"✎ adding element (head)"`/`"✎ adding element (tail)"` — the same reason
  `guard_text` went from `&'static str` to `String` in phase 2.
- **The `matches!`/or-pattern gap** flagged in phase 2 (`staged_edit_found_key_gone`,
  `update/editor.rs`) is fixed by replacing the `matches!` call with a real `match` expression that
  computes the same `bool`, still using or-patterns internally but with **no wildcard arm** —
  `matches!` always expands to a `match` with a trailing `_ => false`, which is what made it
  immune to exhaustiveness checking regardless of the or-pattern; a bare `match` with every
  `PendingMutation` variant named (the nine "has a name" variants in one arm, `DeleteKey`/`None` in
  the other) has no such fallback, so a tenth variant is a compile error here until this function
  decides what it means. This is a genuinely compiler-enforced fix, not a partial one — no tradeoff
  beyond the arm now looking mildly unusual: a `bool`-producing `match` doing the job a boolean
  expression normally would. It was the only version found that gets equivalent exhaustiveness
  checking; a version that kept `matches!` and added `Some(_) if false =>` guard tricks was
  considered and rejected as strictly harder to read for identical safety.

Tests added: `list_element_edit_tests` (31 tests, `update/editor.rs`) covering the binary refusal
(D4), the raw-value open keyed on index, focus gating in both panes for `e`/`a`/`d`, the Head/Tail
toggle (including that `Tab` still inserts on an in-place edit, not the add form), all three confirm
dialogs' `command_text`/`guard_text`, the last-element warning, the duplicate-element case (`d`
removes the row actually under the cursor, not the first matching value — the ADR-0017 D2 hazard
this task exists to close), read-only refusal at confirm and not at the keypress, `NotWritten::
ElementMoved`/`KeyGone` handling, the `dialog_up` fix (a staged `SetListElement`'s gone-key dialog
now actually closes, which is the regression this test would have caught), R3.8 held-while-editing
for both the in-place edit and the add form, and D7's long-list case (`total: 12_000` with a 500-item
window does not change what `e`/`d` do to a row inside it). `toggle_list_end_flips_head_and_tail_and_
is_a_no_op_elsewhere` (`state/editor.rs`) and five `hint_bar_tests` (`render/mod.rs`) round out the
Tab-toggle and hint coverage.

Counts at checkpoint 3: `cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets --
-D warnings` clean, `cargo test --workspace` — core 485 (448 + 37: 31 in `list_element_edit_tests`, 5
in `hint_bar_tests`, 1 `toggle_list_end` test), golden 129 (unchanged — phase 3 wires behaviour, phase
5 adds frames), app 38 (unchanged), integration 61 ignored (unchanged — phase 4's job). The boundary
check printed nothing.

Noticed but deliberately not fixed (out of this phase's scope):

- `crates/core/src/update/editor.rs`, `EditBuffer::from_value`'s `Value::List` arm still returns
  "only string values are editable so far" — now dead code from `open_editor`'s call site, since the
  new List branch is checked first and never falls through to it. Left in place rather than removed:
  `from_value` is a public associated function with its own direct unit tests
  (`state/editor.rs::tests`) that exercise this exact arm, and removing it would either break those
  tests or require them to construct a List value through some other path that does not exist. A
  future cleanup could special-case `Value::List` out of `from_value` entirely now that no live
  caller reaches it that way, but that is a refactor with no behavioural motivation right now.
- `crates/core/src/render/mod.rs`, `confirm_overlay`'s List arms (written in phase 2) show the
  compare-and-set guard line and the last-element warning but were never exercised by a real staged
  dialog until this phase — phase 3's tests confirm the *values* (`command_text`/`guard_text`) but do
  not render a frame through `confirm_overlay` itself; that is phase 5's golden-frame job.

**Phase 4.** Nine `#[ignore]`d integration tests added to `crates/app/tests/integration.rs`
(`redis:7-alpine`, following the Hash/Set sections' structure and naming exactly), proving the
phase 2 implementation against a real server — no implementation changes were needed; every test
passed on first run:

- `editing_a_list_element_overwrites_it_and_keeps_the_keys_ttl` — `LSET` via
  `set_list_element` lands, the key's own TTL survives (ADR-0017's verified fact, now pinned).
- `editing_a_list_element_refuses_without_writing_when_the_list_shifted_under_it` — **the test
  the whole task exists for.** See below for how it is made to exercise the real race rather than
  simulate it.
- `editing_a_list_element_on_a_gone_key_does_not_recreate_it` — `KeyGone`, key stays absent.
- `adding_a_list_element_to_a_gone_key_does_not_recreate_it` — `KeyGone`, key stays absent.
- `adding_a_list_element_lands_at_the_correct_end_for_head_and_tail` — one test covering both
  `ListEnd::Head` (lands at index 0) and `ListEnd::Tail` (lands at the end) against the same list,
  plus a TTL-survives assertion.
- `deleting_a_list_element_removes_the_right_duplicate` — `[x, y, x, z]`, delete index 2, result
  `[x, y, z]` — the exact ADR-0017 D2 hazard this task exists to close, plus a TTL-survives
  assertion.
- `deleting_the_last_list_element_deletes_the_key`.
- `a_binary_list_element_round_trips_through_edit_and_read` — `m\xff\x80` read as the expected
  bytes, edited to `e\xff\x802`, read back exactly.
- `editing_a_list_element_against_a_wrong_type_key_surfaces_an_error_not_a_panic` — `LINDEX`
  against a String key surfaces `WRONGTYPE` as an `Err`, not a panic; the key is untouched.

TTL-survives assertions were added wherever natural (edit, add, delete-not-last) rather than as a
separate test, following the Hash tests' own pattern of folding the TTL check into the write test
it belongs to.

**(d) Whether test #2 exercises the real race or only simulates it, and how it is made
deterministic.** It exercises the real race, not a timing-based simulation of it: a second `fred`
client (`second_client`, a genuinely separate TCP connection, standing in for a second session)
issues a real `LPUSH` against the real server, and that call is `.await`ed to completion *before*
the guarded `set_list_element` call runs against the original index (1) and the original expected
bytes (`"beta"`, read from the server before the push). There is no sleep and no timing window to
get lucky or unlucky on — determinism comes from ordering two real commands on two real
connections in program order (the push happens-before the guarded write, enforced by `await`, not
by wall-clock margin), which is exactly the sequence a real "push arrives while the dialog sits
open" race produces, just without needing a human to actually pause. The assertion checks both
that the outcome is `ElementMoved` and that the list is byte-for-byte unchanged from what the
`LPUSH` alone produced (`["head", "alpha", "beta", "gamma"]`) with no `"CORRUPTED"` anywhere and
the TTL intact — so a regression to a plain unguarded `LSET` (the rejected alternative ADR-0017
records) would fail this test on the "must not have touched any element" assertion even if it
happened to also return the right enum variant.

**(c) Any place the phase 2 implementation turned out to be wrong.** None. All nine tests passed
on the first run with no changes to `crates/app/src/redis/mutate.rs`. Phase 1's live verification
and ADR-0017's own worked examples anticipated every case this phase automated.

Counts at checkpoint 4: `cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets
-- -D warnings` clean, `cargo test --workspace` — core 485 (unchanged), golden 129 (unchanged), app
38 (unchanged); `cargo test -p redis-pane -- --ignored --test-threads=1` — 70 passed (baseline 61 +
9 new), 0 failed, needs Docker. The boundary check was not re-run this phase (no core/dependency
changes) but remains satisfied by construction — phase 4 touches only `crates/app/tests/`.

Noticed but deliberately not fixed (out of this phase's scope):

- `crates/app/tests/integration.rs`, the new List section reuses `redis:7-alpine` throughout,
  matching the Hash/Set sections' default image rather than pinning `8.4-alpine` (the version
  ADR-0017's Lua scripts were verified against) or `6.2-alpine` (the version the Hash edit script's
  `HPEXPIRETIME`-`pcall` compatibility test specifically targets). List's scripts use no
  version-gated command — `EXISTS`/`LINDEX`/`LSET`/`LPUSH`/`RPUSH`/`LREM` are all pre-6.0 — so
  there is no `the_edit_script_still_works_on_redis_6_2_...`-shaped test needed for List the way
  there is for Hash's field-TTL `pcall`. Not fixed because there is nothing version-specific to
  pin; noted so a future reader does not go looking for a List/6.2 test that has no reason to
  exist.

**Phase 5.** Eight golden frames added to `crates/core/tests/golden.rs`, following the Hash/Set
sections' naming and `opened()`/`update()`-flow pattern exactly: `list_form_edit_element` (the
element editor open on a row, raw value, keyed on index), `list_form_add_tail`/`list_form_add_head`
(D6's toggle — confirmed **visibly** different, not just internally different: the header line
reads "✎ adding element (tail)" vs "(head)"), `confirm_set_list_element`, `confirm_add_list_element`,
`confirm_delete_list_element` (not last), `confirm_delete_list_element_last_element`, plus a plain
`assert_eq!` pin of the List hint bar (`e edit · a add · d remove`) matching the Hash/Set hint-bar
tests' shape.

**A real rendering bug was found and fixed, not just noticed.** `PendingMutation::json_warning`
(`state/mod.rs`) matched only `SetString`/`SetHashField` with `was_json: true`, falling through
`_ => None` for every other variant — including `SetListElement`, which phase 2 gave a `was_json`
field for exactly this purpose (`EditBuffer::list_element` classifies it with the same
`looks_like_json` Hash uses). The confirm dialog's `if pending.json_warning() == Some(true)` check
in `render/mod.rs`'s `SetListElement` arm (written in phase 2, per its own note, "never rendered
until phase 5") therefore could never fire for a List: staging an edit that broke a JSON-shaped
element would silently drop the "⚠ no longer valid JSON" line a Hash field edit shows for the
identical case. This is exactly the class of defect the D8 table exists to prevent, just one level
down — not a missing `match` arm that fails to compile, but a `match` that compiles fine because
its wildcard silently absorbs a variant that should have had a real answer. Fixed by adding
`PendingMutation::SetListElement { was_json: true, new, .. }` to the same or-pattern arm
`SetHashField` uses; a regression test,
`list_element_edit_tests::a_json_element_edited_into_something_invalid_warns`, pins it directly
(`crates/core/src/update/editor.rs`), mirroring the existing Hash test of the same shape.

The seven D8 sites, `nothing_to_remove`'s exhaustiveness, and the `PLAN.md`/ADR-0017/review-§9 docs
were all confirmed to already read correctly for what phases 2–4 actually built, except ADR-0017's
Consequences section, which still described `guard_text`/`edit_verb` as returning `&'static str`
and referred to `begin_add_field`/`delete_hash_field` by their pre-rename names — corrected to match
the code, including the `json_warning` fix above. `docs/reviews/2026-09-13-codebase-design-review.md`
§9 gained a dated update marking all seven fallback sites resolved (with commit SHAs) and a
re-stated M3 inventory answering task 9's inherited question directly: three cases did **not** make
a shared `Viewer`-sibling trait's shape obvious — the write guard, the identity model, and the add
form each diverge per type (compare-and-set-on-index vs. `EXISTS`-only; index-identity vs.
bytes-identity vs. name-identity; two-part vs. one-part-with-duplicate-check vs.
one-part-with-end-toggle), while what genuinely converged (the `Mutation`/`PendingMutation`/
`Command::Execute` chokepoint) already existed after Hash and did not need a third case to find.

Counts at checkpoint 5: `cargo fmt --all -- --check` clean, `cargo clippy --workspace --all-targets
-- -D warnings` clean, `cargo test --workspace` — core 486 (485 + 1: the `json_warning` regression
test), golden 137 (129 + 8: seven `#[test]`s each pinning one of the fixtures listed above, plus
one non-golden hint-bar `assert_eq!` test, matching the Hash/Set sections' own mix), app 38 (unchanged); `cargo test -p redis-pane -- --ignored --test-threads=1` — 70 passed (unchanged
— phase 5 is render/docs only, no new integration tests), 0 failed, needs Docker. The boundary
check (`cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'`) printed nothing.

Noticed but deliberately not fixed (out of this phase's scope):

- `CLAUDE.md`'s CONTEXT.md glossary was checked for new load-bearing terms this task introduces
  ("List element", "index", "sentinel", "ListEnd", "Head"/"Tail") and none were added — task 7
  (Set) added no glossary entries either despite Set member add/remove being an equally new
  mutation surface, so no new term here is genuinely load-bearing beyond what "Command preview"
  already generalizes over ("a Hash field edit or add, ADR-0015" — left as its existing Hash
  example rather than expanded to list all three types, matching task 7's own choice not to touch
  it for Set).
