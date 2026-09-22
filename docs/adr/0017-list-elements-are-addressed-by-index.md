# ADR-0017 — List elements are addressed by index, so writes compare before they set

**Status:** Accepted · **Date:** 2026-09-22

## Context

PLAN.md's M2 row 8 asks for edit, add and remove of List elements, using the same preview/diff
machinery ADR-0014 built for Strings and ADR-0015/ADR-0016 built for Hash fields and Set members
(PRD R4.1, R4.4, R3.8). Tasks 6 (Hash) and 7 (Set) are the template: one `Mutation` enum, one
`Msg::MutationSettled`, one `redis::mutate::execute` (review H1); `EditBuffer` + `EditTarget` +
`EditPhase` (review H2); `e`/`a`/`d` focus-gated to the value pane with a value fetched (ADR-0015
D4).

Nine facts about `LSET`, `LPUSH`/`RPUSH`, `LREM`, `LINSERT` and list TTLs shape the write. All were
re-verified for this ADR — against the redis.io command docs and against a real server
(`redis:8.4-alpine`, via `./scripts/redis-up.sh` and `scripts/resp.py`) — rather than carried over
from the planning agent's reading, and all nine held exactly as expected:

- **`LSET` on a missing key is an error, not a create.** `LSET vlist 0 x` against a missing key
  returned `ERR no such key`. Unlike `SADD` and `HSET`/`HSETNX`, `LSET` *cannot* recreate a key
  deleted under the confirm dialog — the hazard ADR-0014 and ADR-0015 exist to close does not apply
  to the List edit path.
- **`LSET` past the end is an error.** `LSET vlist 5 x` on a 3-element list returned `ERR index out
  of range`.
- **`LPUSH`/`RPUSH` create the key when it does not exist.** `EXISTS vlist2` returned `0`; `RPUSH
  vlist2 z` returned `1`; `EXISTS vlist2` then returned `1`. The add path *does* have the recreate
  hazard ADR-0015 closed for Hash, and needs the same `EXISTS` guard.
- **`LREM key 1 <value>` removes by value, the first match from the head — not by index.** Redis
  has no remove-by-index command. Verified on `[x, y, x, z]`: `LREM vlist3 1 x` returned `1` and
  left `[y, x, z]` — the *first* `x` (index 0), not the one the reader may have had selected. A
  naive remove-by-value of a row the reader picked at index 2 would silently delete the wrong
  element whenever the list has a duplicate earlier in it.
- **`LINSERT` is pivot-by-value and also takes the first match**, returning `-1` when the pivot is
  absent. Verified: `LINSERT vlist3 BEFORE nope w` on `[x, y, x, z]` returned `-1` and left the list
  unchanged; `LINSERT vlist3 BEFORE x w` inserted before the *first* `x`, giving `[w, x, y, x, z]`.
  This is why D1 keeps insert at the ends only — see Alternatives.
- **Emptying a list deletes the key**, exactly as `HDEL` of a last field and `SREM` of a last
  member do. Verified: `RPUSH vlist4 solo` then `LPOP vlist4` left `EXISTS vlist4` at `0`.
- **`LINDEX` on a missing key returns nil**, so a compare-and-set on the element subsumes an
  existence check — see D2 for why the scripts still check `EXISTS` separately.
- **Binary elements round-trip through `LSET` and `LINDEX` unchanged.** Verified with `m\xff\x80`:
  `LINDEX` returned the exact bytes back, and an `LSET` to `m\xff\x802` round-tripped identically.
  `IndexedValue.items` is already `Vec<Vec<u8>>`, not `String` (review C2).
- **A key's own TTL is untouched by `LSET`/`LPUSH`/`RPUSH`/`LREM`.** Verified: `EXPIRE vlist 1000`,
  then one of each of `LSET`, `RPUSH`, `LPUSH`, `LREM` in sequence — `TTL vlist` read `1000`
  unchanged after every one. There is no per-element TTL for lists (Redis 7.4's field expiry is
  Hash-only, ADR-0015's `HEXPIRE`/`HPEXPIRETIME` facts), so none of List's scripts needs a
  TTL-preservation branch, unlike `HASH_FIELD_EDIT_SCRIPT`.
- **`LSET`/`LPUSH`/`RPUSH`/`LREM` against a key of a different type return `WRONGTYPE`.** Verified:
  `SET vstr hello` then `LSET vstr 0 x` returned `WRONGTYPE Operation against a key holding the
  wrong kind of value`. As with ADR-0015 and ADR-0016, the guards below do not check type, so a key
  deleted and recreated as another type under the dialog surfaces an ordinary error notification
  carrying the command (R7.4), the same accepted behaviour those ADRs' scripts have.
- **The read window is the first 500 items.** `lrange(key, 0, WINDOW - 1)` in
  `crates/app/src/redis/read.rs` (`WINDOW = 500`). Shown rows are therefore absolute indices
  `0..=499`, so a row's display position *is* its Redis index with no arithmetic. `IndexedValue.total`
  is `LLEN` and may exceed it.

The genuinely new decision is what List's hazard actually is, because it is a different hazard
from Hash's and Set's.

## Decision

**D1 — scope is edit in place, remove, and add at either end. No insert at a position.** `e` on a
row edits that element (`LSET`), `d` removes it, and `a` adds a new element at the head or the
tail. Inserting *between* two existing elements is out of scope for this row — see Alternatives for
why, and for the sentinel technique that would do it duplicate-safely if a future row wants it.
Unlike Set (ADR-0016 D1), in-place edit is the natural operation here: a list element has an
identity — its index — that survives its bytes changing. `LSET` is a real edit, not a rename.

**D2 — every write is a guarded Lua script, and the guard is compare-and-set on the element, not
`EXISTS` alone.** The hazard List has is not the one Hash and Set have. `LSET` cannot recreate a
gone key, so there is nothing to guard there the way ADR-0015/ADR-0016 guard `HSET`/`SADD`. What
`LSET` *can* do instead is write to the wrong element: indices are not stable under concurrent
writes, and a single `LPUSH` from another client between staging and confirming shifts every index
by one. `LSET key 3 <new>` staged against what the reader saw would then silently overwrite
whatever moved into slot 3 — verified directly: after `LPUSH vlist head` shifted `beta` from index
1 to index 2, re-running the edit script staged against index 1 (still holding `beta` as its
expected value) refused rather than overwriting `alpha`, which had moved into slot 1. That is
strictly worse than the Hash/Set hazard — it corrupts silently rather than refuses, and nothing on
screen would say so, which is exactly the class of defect CLAUDE.md's "never swallow a Redis error
silently" section warns about, one level down (a wrong write, not a dropped one).

So each script re-checks that the element at the index still holds the exact bytes the read
returned, and refuses otherwise. Verified working end to end against a live server, including the
happy path, the stale-index refusal and the gone-key refusal:

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

`-1` the key is gone, `-2` the element moved or changed, `1` written. Verified: staged against a
freshly-read `beta` at index 1, the edit returns `1` and the list reads back with the new value in
place; after another client's `LPUSH` shifts everything by one, re-running the same call against
the stale index and stale expected-value returns `-2` and the list is provably unchanged; against a
key deleted in between, it returns `-1` and the key stays missing. The `EXISTS` check is redundant
for correctness — `LINDEX` on a missing key returns nil, which fails the compare anyway — but it is
kept so the two cases can be *reported* apart: "key no longer exists" and "that element moved" are
different things for the reader to see, and folding them would give a deleted key the wrong words.
This is the same reasoning `NotWritten::reason` is built on (`mutation.rs:118`), and the same
reasoning ADR-0015 D2 gave the Hash edit script's `-1`/`0` split.

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
in its sentinel state, so this cannot be seen as a spurious flicker by a concurrent reader. The
sentinel must still not collide with a *real* element earlier in the list, or `LREM 1` would remove
that one instead of the freshly-set sentinel — so it is generated per call as
`__redis-pane-rp:<random hex>` rather than being a constant. A constant sentinel would be wrong the
moment two elements of a list ever happened to equal it, however unlikely, and there is no reason
to accept that risk when a fresh random suffix removes it entirely. Phase 2 sources those bytes
from the injected randomness the core already has (ADR-0011), not from the shell — matching how the
clock and terminal size are already injected rather than read live. Verified: deleting index 2 of
`[x, y, x, z]` (the *second* `x`) leaves `[x, y, z]`, not `[y, x, z]` — the LREM-by-value hazard
above, closed. Deleting the only element of a one-element list drops the key, and the
already-verified stale-index and gone-key refusals apply identically to delete as to edit, since
the guard is the same two checks.

```lua
-- add: LPUSH/RPUSH, never recreating the key
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call(ARGV[1], KEYS[1], ARGV[2])   -- ARGV[1] is 'LPUSH' or 'RPUSH'
```

Add has no compare-and-set: it does not address an existing element, so there is nothing to compare
against. It has the ordinary ADR-0015-shaped `EXISTS` guard, because `LPUSH`/`RPUSH` do recreate a
gone key. Verified: `RPUSH`/`LPUSH` against an existing key return the new length and land the
element at the requested end; against a key deleted in between, the script returns `-1` and
`EXISTS` stays `0` — nothing is recreated.

`EVAL` on every call, never `EVALSHA` — ADR-0015 and ADR-0016 both settle this, and the reason (no
script cache to manage, no fallback path for a cache miss on a fresh connection, and atomicity that
doesn't need a connection reserved for `WATCH`) has not changed.

**D3 — a refused write says *which* guard refused, and a stale index reads as "look again," not
"it failed."** Two `NotWritten` variants cover List's writes: `KeyGone` is reused as-is from
ADR-0015/ADR-0016 (return `-1` above); a new `NotWritten::ElementMoved` covers return `-2`. A stale
index is expected to be the *common* refusal on a busy list — any concurrent push or pop anywhere
in the list shifts it, not just a write to the same element, unlike Hash/Set where only a write to
the same field/member causes a refusal. The wording should tell the reader to look again rather
than imply the write itself is broken:

> **that element moved — the list changed underneath it, look again**

This is deliberately longer and more explanatory than `KeyGone`'s existing wording, because
`ElementMoved` is the refusal a reader will see routinely on a list under churn, where `KeyGone` is
rare by comparison for Hash/Set. Saying only "that element changed" (D3 as drafted in the plan)
reads as if the *value* changed, when the far more common cause is that some *other* element's
write shifted this one's position — "the list changed underneath it" names the actual mechanism
instead of implying a race on the exact element the reader was looking at.

## Alternatives considered

- **A plain, unguarded `LSET`.** Rejected: after a concurrent `LPUSH`/`RPUSH`/`LREM`/`LSET`
  elsewhere, the index the dialog staged against may now hold a different element, and a plain
  `LSET` overwrites it with no way to tell the reader anything went wrong — it looks identical to a
  successful, correct edit. This is worse than Hash/Set's recreate hazard: those refuse loudly
  (nothing) or land visibly wrong (a phantom key with one field); this corrupts an existing,
  unrelated element silently.
- **`LREM` by value for delete.** Rejected per the verified fact above: `LREM key 1 <value>` removes
  the *first* match from the head, not the element at the index the reader selected. On a list with
  a duplicate earlier than the selected row, this deletes the wrong element. The delete script's
  compare-and-set plus disposable sentinel is what makes a duplicate-safe delete-by-index possible
  without a native primitive for it.
- **`LINSERT` for insert-at-position (D1).** Rejected for this round. `LINSERT`'s pivot is
  addressed by value and matches the first occurrence, so on a list with duplicate elements it
  inserts relative to the wrong occurrence of the pivot. It *can* be done duplicate-safely inside a
  script — `LSET` the target index to a unique sentinel, `LINSERT BEFORE`/`AFTER` the sentinel, then
  `LSET` the sentinel back to its original value — using the same disposable-sentinel technique D2's
  delete script uses. That was offered and declined for this round (PLAN M2 task 8, D1): head and
  tail are how lists are actually used in practice, and a head push already shifts every index by
  one, which is the index-shift behaviour PLAN's "Proves" column asks this task to get right without
  needing insert-at-position to prove it. The technique is recorded here so a future insert-at-
  position row does not have to rediscover it.
- **Re-reading the element immediately before writing, instead of a server-side guard.** Rejected:
  a client-side read-then-write is not atomic — another client can write between the re-read and the
  `LSET`, narrowing the race window without closing it. A Lua script runs atomically on the server,
  closing it completely, for the same reason ADR-0015 rejected `WATCH`/`MULTI`/`EXEC` (this project's
  client is multiplexed, not reserved per operation) and the same reason a field-level memo is
  rejected by ADR-0006 in the first place — the guard has to live where the write happens, not in
  a read the core remembers.
- **`WATCH`/`MULTI`/`EXEC`.** Rejected for the same reason ADR-0015 rejected it: it needs a
  connection dedicated to the transaction, and this project's client is multiplexed across the whole
  app rather than reserved per operation.

## Consequences

- `crates/core/src/mutation.rs` gains `Mutation::{SetListElement, AddListElement,
  DeleteListElement}`, with `command_label` → `LSET key <i>` / `LPUSH key` or `RPUSH key` / `LREM
  key <i>`, and `NotWritten::ElementMoved` — a new variant, not a reuse of `FieldExists`/
  `MemberExists`, because the wording (D3) is specific to what a stale index means and neither
  existing variant fits.
- `crates/core/src/state/mod.rs` gains `PendingMutation::{SetListElement, AddListElement,
  DeleteListElement { last_element } }`, each with `command_text()` and `guard_text()`:
  - edit — "only if that element is still there · index 3"
  - add — "only if the key still exists"
  - delete — "only if that element is still there · index 3"

  The delete variant carries the same `last_field`/`last_member`-shaped warning ADR-0015 D4 and
  ADR-0016 gave their last-element deletes, renamed to what it actually is for a List.
- `crates/core/src/state/value.rs` gains `ListEnd { Head, Tail }` (D1's add-at-either-end choice),
  defaulting to `Tail`.
- `crates/core/src/state/editor.rs` gains `EditTarget::{ListElement { index }, NewListElement {
  end } }` and `EditBuffer::list_element()`/`new_list_element()`. A List element opens its **raw**
  value, never reformatted, mirroring the Hash-field path (ADR-0015).
- `crates/app/src/redis/mutate.rs` gains `LIST_ELEMENT_EDIT_SCRIPT`, `LIST_ELEMENT_DELETE_SCRIPT`,
  `LIST_ELEMENT_ADD_SCRIPT`, `set_list_element`, `add_list_element`, `delete_list_element`, and
  three arms in `execute` — the same `Command::Execute`/`Msg::MutationSettled` shape review H1
  already generalized Hash's and Set's writes onto; no new `Command`/`Msg` variants are needed for
  this type.
- `e`/`a`/`d` stay focus-gated to the value pane with a value fetched, per ADR-0015 D4; `d` stays
  focus-dependent — keys pane stages `DEL`, value pane on a List stages the guarded element remove.
  Binary elements are refused with a notice, exactly as binary Hash fields and Set members are:
  "binary elements aren't editable here yet" (D4 of the plan).
- The hint bar (`crates/core/src/render/mod.rs`) gains a List-shaped arm alongside the Hash and Set
  ones: `e edit · a add · d remove`.
- Only the fetched window (first 500 items, `IndexedValue.total` may exceed it) is editable — a row
  past the window is not rendered, so `e`/`a`/`d` have nothing to reach past it with; no new
  refusal is needed for this.

## Sources

- `LSET` (error on a missing key, error past the end, `WRONGTYPE` on the wrong type):
  <https://redis.io/docs/latest/commands/lset/>
- `LPUSH`/`RPUSH` (create the key when missing):
  <https://redis.io/docs/latest/commands/lpush/>, <https://redis.io/docs/latest/commands/rpush/>
- `LREM` (removes by value, first match from the head when `count` is positive):
  <https://redis.io/docs/latest/commands/lrem/>
- `LINSERT` (pivot by value, first match, `-1` when the pivot is absent):
  <https://redis.io/docs/latest/commands/linsert/>
- `LINDEX` (nil on a missing key or out-of-range index):
  <https://redis.io/docs/latest/commands/lindex/>
- `HEXPIRE` (field-level TTL is Hash-only, `group: hash`, no List equivalent — reused from
  ADR-0016's verification): <https://redis.io/docs/latest/commands/hexpire/>
- `EVAL`/scripting: <https://redis.io/docs/latest/commands/eval/>
- Live verification against `redis:8.4-alpine` (`./scripts/redis-up.sh`, `scripts/resp.py`):
  `EXISTS`/`LSET`/`LPUSH`/`RPUSH`/`LREM`/`LINSERT`/`LINDEX`/`LPOP`/`TTL`/`EXPIRE`/`SET`/`EVAL`
  round trips described inline above, including all three guarded scripts' happy path, stale-index
  refusal and gone-key refusal, run 2026-09-22.
- Sibling decisions this one mirrors and diverges from: `docs/adr/0015-hash-field-writes-are-guarded.md`,
  `docs/adr/0016-set-members-are-added-and-removed.md`.
