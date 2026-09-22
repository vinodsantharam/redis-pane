# ADR-0018 — A ZSet's score is edited, its member is not

**Status:** Accepted · **Date:** 2026-09-22

## Context

PLAN.md's M2 row 9 asks for edit of a ZSet member's score, plus add and remove of a member+score
pair, using the same preview/diff machinery ADR-0014 built for Strings and ADR-0015/ADR-0016/
ADR-0017 built for Hash fields, Set members and List elements (PRD R4.1, R4.4, R3.8, R3.13). Tasks
6 (Hash), 7 (Set) and 8 (List) are the template: one `Mutation` enum, one `Msg::MutationSettled`,
one `redis::mutate::execute` (review H1); `EditBuffer` + `EditTarget` + `EditPhase` (review H2);
`e`/`a`/`d` focus-gated to the value pane with a value fetched (ADR-0015 D4).

A ZSet row is the first one this project edits that carries two pieces of data with different
rules — a member (identity, bytes) and a score (a number hanging off that identity) — and the
first edit whose input must be validated as a number before it can be staged at all.

Nine facts about `ZADD`, `ZSCORE`, `ZREM`, score precision and ZSet TTLs shape the write. All were
re-verified for this ADR — against the redis.io command docs and against a real server
(`redis:8.4-alpine`, via `./scripts/redis-up.sh` and `scripts/resp.py`) — rather than carried over
from the planning agent's reading, and all nine held exactly as drafted:

- **`ZADD key XX <score> <member>` does not create a missing key.** Verified: against a missing
  key it returned `0` and `EXISTS` stayed `0`. Unlike `SADD`/`HSET`/`LPUSH`, the score-edit path is
  already self-guarding against the recreate hazard — the same property `SET … XX` gave ADR-0014.
- **`ZADD key NX <score> <member>` does create the key.** Verified: `EXISTS` went `0` → `1` across
  an `NX` call on a missing key. The add path *does* have the recreate hazard and needs
  ADR-0015's `EXISTS` guard.
- **`ZADD … NX` on a member that already exists returns `0` and does not overwrite its score.**
  Verified: `NX 99 alpha` against a set holding `alpha` at `1` returned `0`, and `alpha` read back
  at `1` afterward. This is the duplicate guard, the same shape `HSETNX` gave Hash and `SADD`'s
  no-op gave Set.
- **`ZSCORE` on an absent member returns nil, which is `false` in Lua.** Verified directly. This
  is what lets the edit script tell "member gone" from "written" — see D2 for why `ZADD XX CH`
  cannot.
- **`ZREM` of the last member deletes the key**, exactly as `HDEL`/`SREM`/`LREM` of a last entry
  do. Verified: `ZREM` on a one-member set returned `1` and `EXISTS` then read `0`. `ZREM` of an
  absent member returns `0` and changes nothing — verified on the now-gone key.
- **Scores are f64. `inf`, `+inf` and `-inf` are valid and round-trip as `inf`/`-inf`; `nan` is
  refused by the server** with `ERR value is not a valid float`. Verified all four directly against
  a live server: `ZADD key XX nan member` returned that exact error, and `inf`/`-inf` both wrote
  and read back unchanged.
- **Score precision round-trips.** Verified `1.0000000000000002` and `0.1` both read back byte
  identical via `ZSCORE`. `format_score` (`crates/core/src/state/value.rs`) prints an integral
  score via `as i64` and everything else via `{}`, which is Rust's shortest-round-trip
  representation, so display → parse → write is lossless in both branches. `f64::INFINITY.fract()`
  is NaN, so an infinite score correctly takes the `{}` branch and prints `inf`, matching what
  Redis accepts.
- **A key's own TTL is untouched by `ZADD`/`ZREM`.** Verified: `EXPIRE` to `900`, then a `ZADD XX`
  score edit and a non-last-member `ZREM` in sequence — `TTL` read `900` unchanged after both.
  There is no per-member TTL for sorted sets.
- **Binary members work throughout** — `ZADD`/`ZSCORE` round-trip `m\xff\x80` byte-identically,
  verified directly, including through an `XX` edit. `ScoredValue.entries` is `Vec<(Vec<u8>, f64)>`,
  bytes not `String` (review C2).
- **`ZADD`/`ZSCORE`/`ZREM` against a key of a different type return `WRONGTYPE`.** Verified: `SET`
  then `ZADD`/`ZSCORE`/`ZREM` on that key each returned `WRONGTYPE Operation against a key holding
  the wrong kind of value`. As with ADR-0015/0016/0017, the guards below do not check type, so a
  key deleted and recreated as another type under the dialog surfaces an ordinary error
  notification carrying the command (R7.4), the same accepted behaviour those ADRs' scripts have.
- **The read window is the first 500 by rank**: `zrange(key, 0, WINDOW - 1, … withscores)` in
  `crates/app/src/redis/read.rs`. `ScoredValue.total` is `ZCARD` and may exceed it — unchanged from
  the planning-time note, not independently re-verified here since it is a read-path fact, not a
  write one.

Both guarded scripts (D2) were run end to end against the live server, beyond the individual
command facts above: the gone-key refusal, the gone-member refusal, a score edit that keeps the
key's TTL, an edit to `inf`, an edit to an *unchanged* score, the add's duplicate refusal leaving
the existing score untouched, and the add's gone-key refusal not recreating the key. All matched
what the plan drafted — see D2 for the observed return codes.

**One planning-time claim did not hold and needed a correction**, not a discrepancy in Redis's own
behaviour: the plan drafted the score-edit guard line as "only if that member still exists · keeps
its rank order." Editing a member's score necessarily *changes* that member's own rank — it is
recomputed from the new score by definition. Verified directly: a three-member set with `beta` at
rank 1 moved to rank 2 after a `ZADD XX` raised its score above the top member's. "Keeps its rank
order" is not a real guarantee of this write and would tell the reader something false at exactly
the moment they are deciding whether to confirm it. The clause is settled at D2 below, not in this
paragraph, since it is the same kind of decision the sibling ADRs' guard lines record.

## Decision

**D1 — `e` edits the score. A member is never edited in place.** A ZSet row is a member *and* a
score, and they have different rules. The member is identity — exactly as in a Set, where
ADR-0016 D1 established that changing a member's bytes is a rename, not an edit. The score is
ordinary mutable data hanging off that identity, like a Hash field's value hanging off its name.
So `e` on a ZSet row opens a **score** editor seeded with the current score, and member rename
joins Hash field rename and Set member rename in PLAN M2 task 14.

This makes ZSet the first type where `e` edits something other than "the thing in the row" — the
hint bar says `e score`, not `e edit`, so this is not a surprise at the keyboard.

**D2 — score edit and add are guarded Lua scripts; remove is a plain `ZREM`.**

```lua
-- score edit
if redis.call('EXISTS', KEYS[1]) == 0 then return -1 end
if redis.call('ZSCORE', KEYS[1], ARGV[1]) == false then return -2 end
redis.call('ZADD', KEYS[1], 'XX', ARGV[2], ARGV[1])
return 1
```

`-1` key gone, `-2` member gone, `1` written. `ARGV[1]` is the member, `ARGV[2]` the score.
Verified end to end against a live server: against a missing key it returns `-1` and the key stays
missing; against an existing key with the named member absent it returns `-2` and nothing changes;
against a real member it returns `1`, the new score reads back, and the key's TTL is unchanged
before and after. Verified separately: an edit to `inf` returns `1` and `ZSCORE` reads back `inf`;
re-running the same edit with the score *unchanged* (`inf` → `inf` again) still returns `1`
(written), not `0` — this is the exact case D2's "why a script" paragraph below exists to get
right.

**Why a script when `ZADD XX` already refuses to create a key.** The `EXISTS` half is indeed
redundant for safety — it is kept only so a gone key can be *reported* apart from a gone member, the
same reason ADR-0017 D2 keeps its redundant `EXISTS`. The `ZSCORE` half is not redundant and is the
real reason for the script: `ZADD XX CH` returns the number of elements *changed*, so it answers `0`
both when the member is absent **and** when the member is present with that score already. Those are
completely different things to tell the reader, and a bare `ZADD` cannot separate them. Verified:
re-running the edit with an unchanged score returns `1` (written) through the script, where `CH`
would have said `0` (nothing happened).

```lua
-- add member + score
if redis.call('EXISTS', KEYS[1]) == 0 then return -1 end
return redis.call('ZADD', KEYS[1], 'NX', ARGV[2], ARGV[1])
```

`-1` key gone, `0` the member is already there (its score untouched), `1` added. Verified end to
end: against a missing key it returns `-1` and `EXISTS` stays `0` — nothing is recreated; against
an existing key it adds a genuinely new member and returns `1`, with the score reading back
correctly; against an existing key with the member already present it returns `0` and the
member's *original* score is provably unchanged — a second call naming a different score for the
same member left the first score in place.

Remove is `ZREM key member`, with `0` removed reported as a notice exactly as `HDEL`'s and
`SREM`'s are. `EVAL` on every call, never `EVALSHA` (ADR-0015, ADR-0016, ADR-0017) — no script
cache to manage, no fallback path for a cache miss on a fresh connection, and atomicity that
doesn't need a connection reserved for `WATCH`.

**The score-edit guard line has no second clause, settled.** The plan's draft — "only if that
member still exists · keeps its rank order" — is not true: `ZADD XX` recomputes the member's rank
from its new score, so a score edit *routinely* changes that member's own rank, and verifiably can
move it past every other member's (see Context: editing `beta` from `2` to `100` took it from rank
1 to rank 2, past `gamma`). The line must not claim rank is preserved.

"keeps the key's TTL" was considered as the replacement and rejected, because it would be a
different kind of untrue. A guard line names what the script *checks* — the things that could have
gone wrong and did not. ADR-0015's Hash-field edit earns its "keeps its TTL" clause: `HSET` would
otherwise clear a field's own TTL, so the script reads `HPEXPIRETIME` and reapplies it with
`HPEXPIREAT`, and that clause reports real work done under a real hazard. Here there is no hazard
and no work: `ZADD` simply never touches a key's TTL, and no version of this script could make it
do so. Telling the reader their TTL is being protected, at the moment they are deciding whether to
run a write, would invite them to read protection into a line that is only describing the weather.

So the guard line is the first clause alone:

> **only if that member still exists**

That is exactly what the script checks and all it checks. A guard line that overstates what is
guaranteed is worse than a short one, and "shorter than its siblings" is not a defect — it is
ZSet's edit genuinely having one hazard where Hash's has two.

**D3 — two `NotWritten` variants, one new.** `MemberExists` is reused as is from ADR-0016 — a ZSet
add that finds the member present means exactly what a Set add finding it present means. `KeyGone`
is reused. **`MemberGone` is new**: a score edit whose member vanished under the dialog. Set never
needed it because a Set member is never edited. `FieldGone` is not reused — CLAUDE.md's glossary
keeps a field and a member distinct, and `NotWritten::reason` exists to force the wording decision
per variant.

**D4 — a score is validated before it can be staged.** This is the first edit whose content can be
*invalid* rather than merely unwanted. `Ctrl-S` is blocked while the score text does not parse as
an f64, with a live indicator in the form — the same shape as the Hash add form's shown-duplicate
block, which already blocks staging on a live condition. Accept what Redis accepts: decimal and
exponent floats, and `inf` / `+inf` / `-inf` case-insensitively. Reject `nan` explicitly, along
with anything that does not parse — the server would refuse it with `ERR value is not a valid
float`, and finding that out after the confirm dialog is strictly worse than being told while
typing (R4.4 is about knowing before it runs).

Rust's `str::parse::<f64>()` accepts `"inf"`, `"infinity"`, `"+inf"`, `"-inf"` and `"NaN"`
case-insensitively (and other case variants of `"nan"`), so the parse alone is not the whole
guard: **`nan` must be rejected explicitly**, after parsing, via `is_nan()` — a parse that
succeeds is not by itself proof the value is one Redis will accept.

**D5 — binary members do not block a score edit.** Hash, Set and List all refuse `e` on a
non-UTF-8 row, because the thing being edited *is* the bytes and a text editor cannot round-trip
them. That reasoning does not apply here: a score edit never touches the member's bytes — they
travel to the server exactly as read, as `ARGV[1]` — and the score itself is always ASCII. So `e`
works on a binary member's score, verified directly (`m\xff\x80`'s score round-trips through the
edit script unchanged in its member bytes). This is the first row-level edit that a binary value
does not refuse — a real difference from three prior ADRs, stated here plainly so it does not read
as an oversight. The add form's *member* capture is still text-only and still refuses, with the
usual notice.

**D6 — the add form is two-part, `MEMBER` then `SCORE`.** It mirrors the Hash add form's
`FIELD`/`VALUE` shape (ADR-0015, task 6 follow-up F), with `Enter` advancing from the member part
to the score part and `Ctrl-S` staging. The shown-duplicate guard applies to the member part
exactly as Hash's applies to the field name; D4's numeric guard applies to the score part. Member
first, because it is the identity and the thing a duplicate check is about.

**D7 — rank is not identity, so ADR-0017's compare-and-set does not recur here.** A ZSet's rows
are ordered by score, so a concurrent write elsewhere in the set *does* reorder them — the same
surface symptom that made List dangerous. It is not the same hazard. A staged List write named an
**index**, which another client's push could silently repoint at a different element. A staged
ZSet write names the **member's bytes**, captured when the row was picked, so it addresses the
same member no matter how the ranks have shuffled — correct by construction rather than by guard.
Adding a score-CAS ("refuse if the score changed under me") would be inconsistent with how this
project already treats mutable data: Hash does not CAS a field's old value either, and a score is
ordinary mutable data in exactly that sense. Phase 4 pins this with a test that reorders the set
under a staged edit and proves the write still lands on the intended member.

**D8 — remove warns on the last member**, with the same `last_member`-shaped flag
`DeleteSetMember` already carries.

## Alternatives considered

- **Editing the member in place.** Rejected per D1: a member's bytes are its identity, exactly as
  ADR-0016 D1 established for Set. A changed member is a rename — `ZREM old` + `ZADD new` in one
  script — and building it here would either duplicate the rename ADR-0016 and ADR-0015 already
  deferred to task 14, or force task 14 to retrofit itself onto whatever shape got built first.
  Deferred to task 14 alongside Hash field rename and Set member rename, so all three ADRs defer to
  the same row and it can give them one shape.
- **A bare `ZADD XX CH`, without the script.** Rejected per D2: `CH` reports the count of elements
  *changed*, so it cannot distinguish "member absent" from "member present with that score
  already" — both answer `0`. Those are different things to tell the reader (one is a refusal, one
  is a real write of an unchanged value), and folding them together would misreport a successful
  write as nothing having happened.
- **A score-CAS ("refuse if the score changed under me since it was read").** Rejected per D7:
  inconsistent with how every other mutable-data edit in this project works (Hash does not CAS a
  field's prior value), and unnecessary — the write already addresses the member by its bytes, not
  by its rank or its old score, so a concurrent reorder elsewhere in the set cannot misdirect it.
- **Refusing `e` on a binary member, mirroring Hash/Set/List.** Rejected per D5: those ADRs refuse
  because the edit target *is* the bytes being edited. A score edit never sends the member's bytes
  back changed — only the score, which is always ASCII — so the reasoning that produced those three
  refusals does not apply, and refusing here would just be a needless inconsistency copied forward
  without a reason.

## Consequences

- `crates/core/src/mutation.rs` gains `Mutation::{SetZSetScore, AddZSetMember,
  DeleteZSetMember}`, with `command_label` → `ZADD key <member>` / `ZADD key NX` / `ZREM key`, and
  `NotWritten::MemberGone` — a new variant (D3), not a reuse of `FieldGone`, because a ZSet member
  and a Hash field are different things in this codebase's glossary (CONTEXT.md).
- `crates/core/src/state/mod.rs` gains `PendingMutation::{SetZSetScore, AddZSetMember,
  DeleteZSetMember { last_member } }`, each with `command_text()` and `guard_text()`:
  - score edit — "only if that member still exists" (one clause; see above for why it has no
    second one)
  - add — "only if the key still exists · never overwrites a member's score"
  - remove — no guard line, as `DeleteSetMember` has none

  The delete variant carries the same `last_member`-shaped warning ADR-0016 gave Set's last-member
  delete.
- `crates/core/src/state/editor.rs` gains `EditTarget::{ZSetScore { member }, NewZSetMember {
  member, part }}`, `EditBuffer::{zset_score, new_zset_member}`, and a numeric-validity helper for
  D4 (parses as f64, then explicitly rejects `is_nan()`). `EditBuffer::active_part` must return a
  real `Some(..)` for the add target — the M3 inventory predicted exactly this case as the one that
  would "also be correct, silently" under the old `_ => None` fallback (task 8's D8 removed that
  fallback), so the compiler now forces the answer.
- `crates/app/src/redis/mutate.rs` gains `ZSET_SCORE_EDIT_SCRIPT`, `ZSET_MEMBER_ADD_SCRIPT`,
  `set_zset_score`, `add_zset_member`, `delete_zset_member`, and three arms in `execute` — the same
  `Command::Execute`/`Msg::MutationSettled` shape review H1 already generalized Hash's, Set's and
  List's writes onto; no new `Command`/`Msg` variants are needed for this type.
- `e`/`a`/`d` stay focus-gated to the value pane with a value fetched, per ADR-0015 D4. Unlike
  Hash/Set/List, `e` on a ZSet row does not refuse a binary member (D5) — it is the first
  row-level edit a binary value does not block.
- The preview must show a score diff distinctly from a membership diff (PLAN row 9's "Proves"): a
  score edit shows `member` unchanged with `old → new` on the score; an add shows the whole
  `member + score` pair on the `+` side; a remove shows it on the `−` side. The score-edit dialog
  must visibly differ from the add/remove dialogs — a golden-frame requirement, not just a preview
  requirement (phase 5).
- The hint bar (`crates/core/src/render/mod.rs`) gains a ZSet-shaped arm alongside Hash, Set and
  List's: `e score · a add · d remove` (D1) — `score`, not `edit`, so the hint does not promise
  `e` opens the member.

## Sources

- `ZADD` (`XX` does not create a key, `NX` does, `NX` on an existing member is a no-op that leaves
  its score untouched, `CH` counts elements *changed* not elements addressed):
  <https://redis.io/docs/latest/commands/zadd/>
- `ZSCORE` (nil on an absent member or missing key):
  <https://redis.io/docs/latest/commands/zscore/>
- `ZREM` (never creates anything, deletes the key when the last member goes, `0` on an absent
  member): <https://redis.io/docs/latest/commands/zrem/>
- `ZRANK` (rank is recomputed from the current score, not stable across a score change):
  <https://redis.io/docs/latest/commands/zrank/>
- Sorted set scores as IEEE 754 double precision floats (`inf`/`-inf` valid, `nan` refused):
  <https://redis.io/docs/latest/develop/data-types/sorted-sets/>
- `EVAL`/scripting: <https://redis.io/docs/latest/commands/eval/>
- Live verification against `redis:8.4-alpine` (`./scripts/redis-up.sh`, `scripts/resp.py`):
  `EXISTS`/`ZADD`/`ZSCORE`/`ZREM`/`ZRANK`/`ZRANGE`/`TTL`/`EXPIRE`/`SET`/`EVAL` round trips
  described inline above, including both guarded scripts' happy path, gone-key refusal, gone-member
  refusal, TTL preservation, `inf`/`-inf` round-trip, unchanged-score re-edit, and the add script's
  duplicate refusal leaving the existing score untouched, run 2026-09-22.
- Sibling decisions this one mirrors and diverges from:
  `docs/adr/0015-hash-field-writes-are-guarded.md`, `docs/adr/0016-set-members-are-added-and-removed.md`,
  `docs/adr/0017-list-elements-are-addressed-by-index.md`.
