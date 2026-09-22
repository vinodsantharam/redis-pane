# M2 task 9: edit a ZSet member's score, add and remove members

Status: **drafted 2026-09-22.** The Redis facts below were verified against a live server
(`redis:8.4-alpine`) while drafting — phase 1 re-verifies them and turns them into
[ADR-0018](../adr/0018-zset-scores-are-edited-members-are-not.md). Execution by a Sonnet subagent,
phase by phase, stopping at each checkpoint.

**Base branch.** `main` (`32f55c7`, after task 8 merged). Nothing here stacks on unmerged work.

## Context

`PLAN.md` M2 row 9: "Value edit — ZSet: edit member's score, add/remove member+score · Preview
shows score diff distinctly from membership diff." PRD R4.1, R4.4, R3.8, R3.13.

Tasks 6 (Hash, [ADR-0015](../adr/0015-hash-field-writes-are-guarded.md)), 7 (Set,
[ADR-0016](../adr/0016-set-members-are-added-and-removed.md)) and 8 (List,
[ADR-0017](../adr/0017-list-elements-are-addressed-by-index.md)) are the template. The chokepoint
machinery is reusable unchanged: one `Mutation` enum, one `Msg::MutationSettled`, one
`redis::mutate::execute` (review H1); `EditBuffer` + `EditTarget` + `EditPhase` (review H2);
`e`/`a`/`d` focus-gated to the value pane with a value fetched (ADR-0015 D4).

**What is genuinely new here.** ZSet is the first type where a row carries *two* pieces of data
with different rules: a member (identity, bytes, not editable — a change is a rename) and a score
(a number, editable in place, and the only thing `e` touches). It is also the first edit whose
input must be **validated as a number before it can be staged at all**. See D1 and D4.

**What is notably not new.** Task 8's compare-and-set guard does *not* recur here — see D7. That is
a deliberate finding, not an oversight.

### Redis facts that shape the write

Verified 2026-09-22 against a live `redis:8.4-alpine` via `scripts/resp.py`. **Phase 1 re-verifies
each one** and reports any difference.

- **`ZADD key XX <score> <member>` does not create a missing key.** Verified: against a missing
  key it returned `0` and `EXISTS` stayed `0`. Unlike `SADD`/`HSET`/`LPUSH`, the score-edit path is
  already self-guarding against the recreate hazard — the same property `SET … XX` gave ADR-0014.
- **`ZADD key NX <score> <member>` does create the key.** The add path *does* have the recreate
  hazard and needs ADR-0015's `EXISTS` guard.
- **`ZADD … NX` on a member that already exists returns `0` and does not overwrite its score.**
  Verified: `NX 99 alpha` left `alpha` at `1`. This is the duplicate guard, the same shape
  `HSETNX` gave Hash and `SADD`'s no-op gave Set.
- **`ZSCORE` on an absent member returns nil, which is `false` in Lua.** This is what lets the edit
  script tell "member gone" from "written" — see D2 for why the `CH` flag cannot.
- **`ZREM` of the last member deletes the key**, exactly as `HDEL`/`SREM`/`LREM` of a last entry
  do. `ZREM` of an absent member returns `0` and changes nothing.
- **Scores are f64. `inf`, `+inf` and `-inf` are valid and round-trip as `inf`/`-inf`; `nan` is
  refused by the server** with `ERR value is not a valid float`. The form must not send one (D4).
- **Score precision round-trips.** Verified `1.0000000000000002` and `0.1` both read back byte
  identical. `format_score` (`crates/core/src/state/value.rs`) prints an integral score via
  `as i64` and everything else via `{}`, which is Rust's shortest-round-trip representation, so
  display → parse → write is lossless in both branches. `f64::INFINITY.fract()` is NaN, so an
  infinite score correctly takes the `{}` branch and prints `inf`, matching what Redis accepts.
- **A key's own TTL is untouched by `ZADD`/`ZREM`.** Verified: `TTL` read `900` unchanged after a
  score edit. There is no per-member TTL for sorted sets.
- **Binary members work throughout** — `ZADD`/`ZSCORE` round-trip `m\xff\x80`. `ScoredValue.entries`
  is `Vec<(Vec<u8>, f64)>`, bytes not `String` (review C2).
- **The read window is the first 500 by rank**: `zrange(key, 0, WINDOW - 1, … withscores)` in
  `crates/app/src/redis/read.rs`. `ScoredValue.total` is `ZCARD` and may exceed it.

## Decisions (drafted — confirm at checkpoint 1)

**D1. `e` edits the score. A member is never edited in place.**
A ZSet row is a member *and* a score, and they have different rules. The member is identity —
exactly as in a Set, where ADR-0016 D1 established that changing a member's bytes is a rename, not
an edit. The score is ordinary mutable data hanging off that identity, like a Hash field's value
hanging off its name. So `e` on a ZSet row opens a **score** editor seeded with the current score,
and member rename joins Hash field rename and Set member rename in PLAN M2 task 14.

This makes ZSet the first type where `e` edits something other than "the thing in the row" — worth
one word in the hint bar so it is not a surprise: `e score`, not `e edit`.

**D2. Score edit and add are guarded Lua scripts; remove is a plain `ZREM`.**

```lua
-- score edit
if redis.call('EXISTS', KEYS[1]) == 0 then return -1 end
if redis.call('ZSCORE', KEYS[1], ARGV[1]) == false then return -2 end
redis.call('ZADD', KEYS[1], 'XX', ARGV[2], ARGV[1])
return 1
```

`-1` key gone, `-2` member gone, `1` written. `ARGV[1]` is the member, `ARGV[2]` the score.

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

`-1` key gone, `0` the member is already there (its score untouched), `1` added.

Remove is `ZREM key member`, with `0` removed reported as a notice exactly as `HDEL`'s and
`SREM`'s are. `EVAL` on every call, never `EVALSHA` (ADR-0015, ADR-0016, ADR-0017).

**D3. Two `NotWritten` variants, one new.** `MemberExists` is reused as is from ADR-0016 — a ZSet
add that finds the member present means exactly what a Set add finding it present means. `KeyGone`
is reused. **`MemberGone` is new**: a score edit whose member vanished under the dialog. Set never
needed it because a Set member is never edited. Do not reuse `FieldGone` — CLAUDE.md's glossary
keeps a field and a member distinct, and `NotWritten::reason` exists to force the wording decision
per variant.

**D4. A score is validated before it can be staged.**
This is the first edit whose content can be *invalid* rather than merely unwanted. `Ctrl-S` is
blocked while the score text does not parse as an f64, with a live indicator in the form — the same
shape as the Hash add form's shown-duplicate block, which already blocks staging on a live
condition. Accept what Redis accepts: decimal and exponent floats, and `inf` / `+inf` / `-inf`
case-insensitively. Reject `nan` explicitly, along with anything that does not parse — the server
would refuse it with `ERR value is not a valid float`, and finding that out after the confirm dialog
is strictly worse than being told while typing (R4.4 is about knowing before it runs).

Rust's `str::parse::<f64>()` accepts `"inf"`, `"infinity"`, `"+inf"`, `"-inf"` and `"NaN"`
case-insensitively, so the parse alone is not the whole guard: **`nan` must be rejected explicitly**
after parsing, via `is_nan()`.

**D5. Binary members do not block a score edit.**
Hash, Set and List all refuse `e` on a non-UTF-8 row, because the thing being edited *is* the bytes
and a text editor cannot round-trip them. That reasoning does not apply here: a score edit never
touches the member's bytes — they travel to the server exactly as read, as `ARGV[1]` — and the score
itself is always ASCII. So `e` works on a binary member's score, and this is the first row-level
edit that a binary value does not refuse. The add form's *member* capture is still text-only and
still refuses, with the usual notice.

State this plainly in the ADR: it is a real difference from three prior ADRs and looks like an
oversight unless it is written down as a decision.

**D6. The add form is two-part, `MEMBER` then `SCORE`.**
It mirrors the Hash add form's `FIELD`/`VALUE` shape (ADR-0015, task 6 follow-up F), with `Enter`
advancing from the member part to the score part and `Ctrl-S` staging. The shown-duplicate guard
applies to the member part exactly as Hash's applies to the field name; D4's numeric guard applies
to the score part. Member first, because it is the identity and the thing a duplicate check is
about.

`EditBuffer::active_part` must return a real `Some(..)` for this target. The M3 inventory predicted
exactly this case — it named a ZSet score/member split as the one that would "also be correct,
silently" under the old `_ => None` fallback. That fallback is gone as of task 8's D8, so the
compiler will now force the answer. Good; that is the mechanism working.

**D7. Rank is not identity, so task 8's compare-and-set guard does not recur.**
A ZSet's rows are ordered by score, so a concurrent write elsewhere in the set *does* reorder them —
the same surface symptom that made List dangerous. It is not the same hazard. A staged List write
named an **index**, which another client's push could silently repoint at a different element. A
staged ZSet write names the **member's bytes**, captured when the row was picked, so it addresses
the same member no matter how the ranks have shuffled. Correct by construction rather than by guard.

Do not add a score-CAS "refuse if the score changed under me" check. Hash does not CAS a field's old
value either, and a score is ordinary mutable data in exactly that sense. **Write a test that pins
this**: reorder the set under a staged edit and prove the write still lands on the intended member.

**D8. Remove warns on the last member**, with the same `last_member`-shaped flag `DeleteSetMember`
already carries.

## New and changed types

| Where | What |
|---|---|
| `core/src/mutation.rs` | `Mutation::{SetZSetScore, AddZSetMember, DeleteZSetMember}`; labels `ZADD key <member>` / `ZADD key NX` / `ZREM key` |
| `core/src/mutation.rs` | `NotWritten::MemberGone` (D3) |
| `core/src/state/mod.rs` | `PendingMutation::{SetZSetScore, AddZSetMember, DeleteZSetMember { last_member }}` with `command_text()` and `guard_text()` |
| `core/src/state/editor.rs` | `EditTarget::{ZSetScore { member }, NewZSetMember { member, part }}`; `EditBuffer::{zset_score, new_zset_member}`; a numeric-validity helper for D4 |
| `app/src/redis/mutate.rs` | `ZSET_SCORE_EDIT_SCRIPT`, `ZSET_MEMBER_ADD_SCRIPT`; `set_zset_score`, `add_zset_member`, `delete_zset_member`; three arms in `execute` |

Guard lines for the dialog:

- score edit — **"only if that member still exists"**, one clause and no second *(settled in two
  steps. Phase 1 found the draft's "keeps its rank order" false — `ZADD XX` recomputes rank from the
  new score, verifiably moving a member past others — and proposed "keeps the key's TTL" instead.
  That was rejected on review: a guard line names what the script **checks**, and ADR-0015 earns its
  "keeps its TTL" by actually reading `HPEXPIRETIME` and reapplying it against a real hazard, where
  `ZADD` could not clear a key's TTL if it tried. Claiming the protection would invite the reader to
  see a guard where there is only Redis behaving normally. See ADR-0018.)*
- add — **"only if the key still exists · never overwrites a member's score"**
- remove — no guard line, as `DeleteSetMember` has none

**Preview must show a score diff distinctly from a membership diff** (PLAN row 9's "Proves"). A
score edit shows `member` unchanged with `old → new` on the score; an add shows the whole
`member + score` pair on the `+` side; a remove shows it on the `−` side.

## Phases

Each phase is one commit, ends at a checkpoint, and must be reported before the next starts.

**Read task 8's "Found while building" first.** Its phase 2 note applies identically: the moment a
new `Mutation`/`PendingMutation`/`NotWritten`/`EditTarget` variant exists, every exhaustive match
over those types across both crates must handle it or the workspace does not build. `Mutation` is
the core/shell seam, so **phase 2 necessarily includes the real `crates/app/src/redis/mutate.rs`
arms** — a `todo!()` there would violate CLAUDE.md. That is planned, not a deviation.

**Expect more compile errors than task 8 had, and treat them as the point.** Task 8's D8 converted
seven wildcard/`if let` sites into exhaustive matches specifically so that this task would fail to
compile at each one instead of silently falling through. Every such error is a place that needs a
real ZSet answer. **If any of those seven sites does *not* produce an error, say so in your report**
— that means the exhaustiveness fix did not hold, which is a finding about task 8, not about ZSet.

### Phase 1 — Verify the Redis facts, then write the decisions down

Docs only, no code.

1. Re-verify every bullet under "Redis facts that shape the write", and both D2 scripts end to end,
   against redis.io and a real server. Report anything that differs.
2. Settle the score-edit guard line's second clause (see the table above).
3. Write **ADR-0018 — A ZSet's score is edited, its member is not**. Subject is D1 and D5. Record
   the verified facts, both scripts, D4's validation rule including the explicit `nan` rejection,
   and D7's reasoning for why ADR-0017's compare-and-set does not recur. Rejected alternatives:
   editing the member in place (a rename — task 14), a bare `ZADD XX CH` without the script (cannot
   tell a gone member from an unchanged score), a score-CAS (inconsistent with Hash, and a score is
   ordinary mutable data), and refusing `e` on a binary member (D5 — the score edit does not touch
   the member's bytes). Follow ADR-0017's structure and length.
4. Update `PLAN.md` M2 row 9's "Proves" column to name what will actually be tested.

**Checkpoint 1.** Report the fact-check results and the ADR. **Stop here** — do not start phase 2
until the main agent confirms the decisions.

### Phase 2 — Core types, previews, and the shell arms exhaustiveness forces

`mutation.rs`, `state/mod.rs`, `state/editor.rs` per the table, plus the real `redis/mutate.rs`
implementations, plus the minimal arms every other exhaustive match needs. Unit tests for
`command_label`, `command_text`, `guard_text`, the `last_member` warning, `NotWritten::MemberGone`'s
wording, D4's numeric validation (including `inf`, `-inf`, `nan` and garbage), and the `EditBuffer`
constructors.

No wiring into `update/`'s dispatch yet — `e`/`a`/`d` on a ZSet still behave as they do today.

**Checkpoint 2.** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`, and the boundary check. Report counts, and the list of sites
that forced a compile error.

### Phase 3 — Core wiring

`crates/core/src/update/editor.rs`, `viewer.rs` and `confirm.rs`: `e` opens the score editor, `a`
opens the two-part add form, `d` stages the remove. Focus gating per ADR-0015 D4. D4's numeric guard
on `Ctrl-S`. D5: `e` must work on a binary member's score.

Tests: focus gating, D5's binary-member score edit, D4's numeric block including `nan`, the add
form's two parts and its shown-duplicate block, all three confirm dialogs including the last-member
warning, read-only refusal at confirm, R3.8 held-while-editing, and a long-ZSet case past the 500
window.

**Checkpoint 3.** Same commands. Report counts.

### Phase 4 — Docker-backed proof of the shell

Integration tests in `crates/app/tests/integration.rs`, every one `#[ignore]`d, proving: a score edit
lands and keeps the key's TTL; it refuses on a gone key without recreating it; it refuses on a gone
member; **it lands on the right member after the set has been reordered under it** (D7 — the test
that proves rank is not identity); an add refuses on a gone key without recreating it; an add refuses
a duplicate member without changing its score; a remove of the last member takes the key; a binary
member's score round-trips; `inf` and `-inf` round-trip; a high-precision score round-trips
byte-identically; and `WRONGTYPE` surfaces as an error rather than a panic.

**Checkpoint 4.** The three commands plus `cargo test -p redis-pane -- --ignored --test-threads=1`
(needs Docker). Report both counts.

### Phase 5 — Render, golden frames, docs

1. Hint bar for a ZSet (`e score · a add · d remove`, per D1); golden frames for the score editor,
   the add form in both `MEMBER` and `SCORE` parts plus its duplicate and invalid-score states, all
   three confirm dialogs, and the last-member warning. **The score-edit dialog must visibly differ
   from the add/remove dialogs** — that is PLAN row 9's "Proves" clause, so pin it.
2. `CONTEXT.md` if any term is new; `PLAN.md` M2 row 9 to **done**; ADR-0018's Consequences to match
   what was built.
3. `docs/reviews/2026-09-13-codebase-design-review.md` §9: **revisit the M3 trait question with four
   cases.** Task 8 recorded that three types diverged. State whether ZSet changes that answer or
   confirms it, and if the four now suggest a shape, describe it concretely enough to be acted on.
   Do **not** implement it here.

**Checkpoint 5.** Full local verification. Do not open the PR — the main agent does that.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'   # must print nothing
cargo test -p redis-pane -- --ignored --test-threads=1                      # phase 4 on; needs Docker
```

Baseline on `main` before any change: **core 486, golden 137, app 38**, integration **70**. Counts
only go up.

## Manual test plan

```bash
./scripts/redis-up.sh
python3 - <<'EOF'
import sys; sys.path.insert(0, "scripts")
from resp import Resp
r = Resp()
r.call("DEL", "edit:zset", "one:zset", "bin:zset", "big:zset")
r.call("ZADD", "edit:zset", "1", "alpha", "2", "beta", "3.5", "gamma")
r.call("ZADD", "one:zset", "1", "solo")
r.call("ZADD", "bin:zset", "1", b"m\xff\x80", "2", "plain")
for i in range(600):
    r.call("ZADD", "big:zset", str(i), f"member-{i}")
EOF
cargo run -p redis-pane -- --url redis://127.0.0.1:6379
```

- [ ] **Score edit.** Open `edit:zset`, select `beta`, `e` — the editor opens on the *score*, not
      the member. Change it to `10`, `Ctrl-S`. The dialog shows the score diff distinctly. `y`
      writes it, and the row moves in rank order.
- [ ] **Invalid score.** `e`, type `abc`, then `nan`. `Ctrl-S` is blocked both times with a visible
      indicator. `inf` is accepted.
- [ ] **Gone member.** Stage a score edit, `ZREM edit:zset beta` from a second terminal, `y`.
      Refused with D3's wording; nothing written.
- [ ] **Reordered under the edit.** Stage a score edit of `beta`, then change *other* members'
      scores from a second terminal so the ranks shuffle, then `y`. It must still land on `beta`.
- [ ] **Add.** `a`, type a member, `Enter` to the score part, type a score, `Ctrl-S`. Duplicate
      members are blocked on the member part; a non-numeric score is blocked on the score part.
- [ ] **Gone key.** Stage an add, `DEL edit:zset` elsewhere, `y`. Refused, not recreated.
- [ ] **Remove, and the last one.** `d`, `y`. On `one:zset` the dialog warns the key will go.
- [ ] **Binary member.** `bin:zset` shows `m\xff\x80` escaped, and `e` on it **works** — the score
      is editable even though the member is not text (D5).
- [ ] **Long ZSet.** `big:zset` says 600 members with 500 shown; editing a row inside the window
      addresses the right member.
- [ ] **Read-only.** `⌃R`, then stage any of the above: refused at the dialog, nothing sent.
- [ ] **Held while editing.** With the editor open, change the set from a second terminal. The
      header reads held, and the buffer is untouched.

## Out of scope

- **Member rename** — task 14, alongside Hash field rename and Set member rename. This is now the
  third ADR to defer to that row; task 14 should give all three one shape.
- **`ZINCRBY`-style relative score edits** — not in PLAN M2. The editor sets an absolute score.
- **Sorting or filtering by score in the Viewer** — a read concern, not this row's.
- **Stream editing** — not in PLAN M2 at all.
- **Implementing review M3's trait** — phase 5 re-answers the question with four cases; it does not
  act on the answer.

## Found while building

_(Executor: append anything noticed but deliberately not fixed, with `file:line`.)_

**Phase 2.** All three types built per the table: `Mutation::{SetZSetScore, AddZSetMember,
DeleteZSetMember}` and `NotWritten::MemberGone` (`crates/core/src/mutation.rs`);
`PendingMutation::{SetZSetScore, AddZSetMember, DeleteZSetMember { last_member }}` with
`command_text()`/`guard_text()`/`into_command()` (`crates/core/src/state/mod.rs`);
`EditTarget::{ZSetScore { member }, NewZSetMember { member, part }}` and
`EditBuffer::{zset_score, new_zset_member}` (`crates/core/src/state/editor.rs`); the real
`ZSET_SCORE_EDIT_SCRIPT`/`ZSET_MEMBER_ADD_SCRIPT` and `set_zset_score`/`add_zset_member`/
`delete_zset_member`, plus three arms in `execute` (`crates/app/src/redis/mutate.rs`).

**`f64` has no `Eq`, and both `Mutation` and `PendingMutation` used to derive it.** Not anticipated
by the plan's type table. Both enums now implement `Eq` by hand (`impl Eq for Mutation {}` /
`impl Eq for PendingMutation {}`, right after their `#[derive(Debug, Clone, PartialEq)]`), the same
way `crates/core/src/state/value.rs`'s `ScoredValue` already does — safe because D4/ADR-0018 rejects
`nan` before a score can ever be staged, so every `f64` either type holds compares reflexively.

**The seven D8 sites: three forced a compile error, four did not — exactly as task 8's phase 2
found for List, not a regression in task 8's exhaustiveness fix.** Forced:
`PendingMutation::guard_text` and `EditBuffer::active_part` (both exhaustive over the type gaining
new variants), and `update/confirm.rs`'s `nothing_to_remove` (exhaustive over `Mutation`).
`EditBuffer::active_part` is exactly the case the M3 inventory predicted — `NewZSetMember { part,
.. } => Some(*part)` is a real `Some(..)`, not a wildcard's `None`. Not forced, because all four are
`if let`/sequential-`matches!` chains over `Value` rather than exhaustive matches over
`EditTarget`/`Mutation`/`PendingMutation`/`NotWritten`, and `Value` itself is unchanged this phase
(ZSet was already one of its variants): `open_editor`, `begin_add_entry`
(`crates/core/src/update/editor.rs`), `delete_value_row` (`crates/core/src/update/viewer.rs`,
unmodified — `git diff` on it is empty) and `hint_bar` (`crates/core/src/render/mod.rs`). This
matches PLAN M2 task 8's phase 2 report precisely: List's phase 2 also forced only `guard_text` and
`active_part`, with the other five (including these same four) reached only once phase 3 wired
`e`/`a`/`d`'s dispatch. Left untouched here for the same reason and by the same "no wiring into
`update/`'s dispatch" instruction — phase 3's job.

**Two more exhaustive matches were forced beyond the plan's declared four files**, the same way task
8's phase 2 also touched files outside its own table: `crates/core/src/state/open.rs`'s `edit_verb`
(matches `Option<&EditTarget>` with `Some(EditTarget::Value) | None` as its last arm, not a
wildcard) and `crates/core/src/update/editor.rs`'s `stage_editor` (matches `EditTarget` by name,
turning it into the matching `PendingMutation`) and `staged_edit_found_key_gone`'s `dialog_up`
check (an or-pattern under `Some(...)` with `Some(DeleteKey) | None` as the rest — task 8 phase 3
had already converted this from a bare `matches!` to a real exhaustive `match`, so it forced an
error here too, one phase earlier than task 8 needed it to). All three got real arms, not stubs, and
are unreachable today for the same reason the List ones were at task 8 phase 2: nothing in
`update/`'s dispatch constructs a ZSet `EditTarget` yet. `render/mod.rs`'s `confirm_overlay` (also
exhaustive over `PendingMutation`) was likewise forced and given real preview lines — this is where
D2's guard-line wording and the score-diff-vs-membership-diff distinction (ADR-0018's preview
requirement) actually live; no golden frame exercises them yet since nothing stages these three
variants.

**`stage_editor`'s parse-back fallback uses `f64::NAN`, not `0.0`, deliberately.** Turning a staged
score's raw text back into `f64` for `PendingMutation::SetZSetScore`/`AddZSetMember` can only fail
if it is ever reached with text D4's `is_valid_zset_score` would have rejected — which phase 3's
`⌃S` guard is what actually prevents, and phase 2 does not wire. `unwrap_or(f64::NAN)` was chosen
over `unwrap_or(0.0)` so that if this fallback is ever reached despite the guard, the write is
refused visibly by the server (`ERR value is not a valid float`) rather than silently staging a
fabricated `0.0` nobody typed — CLAUDE.md's "never swallow a bad write silently" extended to a
theoretical path, not just the real ones.

**`format_score` round-trip: lossless for the cases D4 asked for, with one caveat not in scope.**
`format_score_round_trips_losslessly` (`crates/core/src/state/value.rs`) pins `0.0`, `±3.0`,
`100.0`, an integral value near the `1e15` cutoff, `1.5`, `0.1`, `1.0000000000000002`, and both
infinities — all exact via plain `==`. Not tested: `-0.0`. `format_score(-0.0)` takes the integral
branch (`(-0.0).fract() == 0.0` is true) and prints `"0"` via `-0.0 as i64` → `0i64`, so the text
loses the sign bit — `parsed == s` still holds (`0.0 == -0.0` is `true` in IEEE 754), but
`parsed.to_bits() != s.to_bits()`. Not a defect against what D4/ADR-0018 actually claim (numeric
losslessness, verified for `1.0000000000000002` and `0.1`), and not fixed here — noted because it
is a genuine asymmetry in `format_score`'s two branches, and a future caller that compares scores by
bits rather than by value should know about it (`crates/core/src/state/value.rs`, `format_score`,
around the `s.fract() == 0.0` branch).

**`NewZSetMember`'s `Enter`-advance/typing mechanics (`name_push`/`name_push_str`/`name_pop`/
`advance_to_value`/`return_to_name`) are untouched** — they still only match
`EditTarget::NewHashField`. These are `if let`, not exhaustive matches, so they compiled clean
without a ZSet arm and are correctly phase 3's job (D6's `Enter` advance from `MEMBER` to `SCORE`),
not this phase's — flagging here only so phase 3 does not have to rediscover which mechanics still
need wiring, the same courtesy task 8's phase 2 note extended to `staged_edit_found_key_gone`.

**Phase 3.** `e`/`a`/`d` wired in `crates/core/src/update/editor.rs` (`open_editor`,
`begin_add_entry`, `stage_editor`'s guards) and `crates/core/src/update/viewer.rs`
(`delete_value_row`); the hint bar in `crates/core/src/render/mod.rs`; a `zset_member_shown_duplicate`
added to `crates/core/src/state/open.rs`, mirroring `hash_field_shown_duplicate`.
`crates/core/src/update/confirm.rs` needed **no changes at all** — `confirm_key`,
`mutation_settled`, `not_written` and `nothing_to_remove` were already exhaustive over
`PendingMutation`/`Mutation`/`NotWritten` from phase 2 and simply started being exercised the
moment phase 3 could stage the three new variants; every test that exercises read-only refusal,
`NotWritten::MemberGone`/`MemberExists`, and the "member already gone" `ZREM` notice passed against
unmodified phase-2 code.

**The five `EditTarget::NewHashField`-only typing mechanics were made exhaustive**, per this
phase's brief: `name_push`, `name_push_str`, `name_pop`, `advance_to_value`, `return_to_name` in
`crates/core/src/state/editor.rs` now match every `EditTarget` variant by name, with
`NewZSetMember`'s `Enter`-advance sharing the same arm as `NewHashField`'s (both are "move from a
name part to a value part") and every other variant landing in an explicit empty arm. This is the
same shape task 8's D8 established for `Mutation`/`PendingMutation`/`NotWritten`/`EditTarget`
matches at the update/render layer, extended one layer down to `EditBuffer`'s own mutators — a
third target with a name-typed half (Set member rename or List index rename at task 14, say) will
now fail to compile here instead of silently doing nothing for it.

**D4's live indicator is the hint bar**, not a widget-level color change: `hint_bar` in
`crates/core/src/render/mod.rs` checks `is_valid_zset_score` against the buffer's raw text on
every render and swaps the hint to `invalid score · …` — covering both the existing-member score
editor (`EditTarget::ZSetScore`, active_part `None`) and the add form's score part
(`EditTarget::NewZSetMember { part: FieldPart::Value, .. }`) from one helper. The actual block is
`value_part_stage_blocked` (`update/editor.rs`), which `Ctrl-S` and the generic `Enter`-stages-
everything-but-`Value` path both route through — one guard, so `Enter` cannot let through what
`Ctrl-S` refuses, matching the discipline the Set duplicate guard already established.

**D5 stayed working by construction, not by a new check**: `open_editor`'s new ZSet arm calls
`EditBuffer::zset_score(member, score)` directly — that constructor takes `member: &[u8]` and is
infallible (phase 2 built it that way specifically so no `Result`/refusal branch could be added by
mistake), unlike the `Result`-returning `for_hash_field`/`list_element` next to it that do refuse
non-UTF-8 bytes. There is no `std::str::from_utf8` check to accidentally add on this path; the test
`e_on_a_binary_zset_member_edits_the_score_anyway` (`crates/core/src/update/editor.rs`) pins that
opening `e` on a `[0xff, 0x80]` member succeeds and seeds the score text normally.

**One naming decision not in the plan's table**: the ZSet add form's *member*-part duplicate check
needed its own function (`OpenKey::zset_member_shown_duplicate`, `state/open.rs`) rather than
reusing `set_member_shown_duplicate` — a ZSet add's member is a *name* being typed
(`EditBuffer::field_name()` returns it, the same shape a Hash field name is), where a Set add's
member is the *whole buffer text* with no name/value split at all (ADR-0016 D3). Mirrors
`hash_field_shown_duplicate` one type over, not `set_member_shown_duplicate`, per D6's own framing
("mirrors the Hash add form's `FIELD`/`VALUE` shape").

The four D8-flagged `Value`-matching sites (`open_editor`, `begin_add_entry`, `delete_value_row`,
`hint_bar`) all got real ZSet arms as this phase's actual job, not stubs, and every arm is now
exercised by a passing test.

**Left unfixed, and it is not only a test gotcha: a seeded editor opens with the cursor at
position 0, so typing prepends.** `e` on a score of `1`, then typing `9.5`, leaves `9.51` — the new
digits inserted in front of the old score rather than replacing it. A bare `Backspace` right after
`e` also deletes nothing, there being nothing to the cursor's left. The phase-3 tests do not show
this because their `retype_score` helper clears the buffer first; **needing that helper is the
symptom, not the workaround.**

This is **not** ZSet-specific and was not introduced here. `EditBuffer::for_hash_field` and
`EditBuffer::list_element` seed the same way — `TextArea::new` starts at `(0, 0)` and nothing moves
it — so Hash field values and List elements have always behaved like this. It is more acutely wrong
for a score than for the others: a score is a short scalar a reader replaces wholesale, where a
Hash value is often a document they want to position a cursor inside.

**Deliberately not fixed here, because there is no contained correct fix.** Seeding at the end
would make `Backspace` behave but still appends (`19.5`), so neither end of the line gives
type-to-replace; that wants a selection or a replace-on-first-keystroke mode the editor does not
have. And changing only ZSet's seeding would make it inconsistent with the two types beside it,
while changing all three is a cross-type behaviour change well outside a ZSet row's scope. Raised
for a decision rather than settled unilaterally — it belongs with task 5's `$EDITOR` work or its own
row, not here.

**Phase 4.** 11 `#[ignore]`d integration tests added to `crates/app/tests/integration.rs`, under a
new `// ── PLAN M2 task 9 — ZSet score edit, add, delete (ADR-0018) ──` section following the List
tests exactly in structure and naming: `editing_a_zset_score_overwrites_it_and_keeps_the_keys_ttl`,
`editing_a_zset_score_on_a_gone_key_does_not_recreate_it`,
`editing_a_zset_score_refuses_without_writing_when_the_member_is_gone_under_it`,
`editing_a_zset_score_lands_on_the_right_member_after_a_reorder_under_it` (D7's test),
`adding_a_zset_member_to_a_gone_key_does_not_recreate_it`,
`adding_a_duplicate_zset_member_refuses_without_changing_its_score`,
`removing_the_last_zset_member_deletes_the_key`,
`a_binary_zset_members_score_round_trips_through_edit_and_read`,
`inf_and_negative_inf_round_trip_through_a_score_edit`,
`a_high_precision_score_round_trips_byte_identically` (asserted via `to_bits()`, not `==`, so a
lossy round-trip that happened to still compare equal would not slip past it — see the `format_score`
`-0.0` caveat phase 2 already flagged), and
`editing_a_zset_score_against_a_wrong_type_key_surfaces_an_error_not_a_panic`. All 81 integration
tests pass (70 baseline + 11 new) against `redis:7-alpine`, `#[test-threads=1]`, Docker.

**The phase 2 implementation needed no changes.** `set_zset_score`/`add_zset_member`/
`delete_zset_member` and both scripts in `crates/app/src/redis/mutate.rs` worked exactly as built —
every test passed on the first Docker run once the suite itself compiled. Nothing in D1–D8 or the
scripts was found wrong.

**D7's reorder test (#4) exercises the real race, not a simulation of it**, the same way task 8's
list-shift test does and for the same reason: the second client's three `ZADD`s (moving `alpha` to
`500`, `gamma` to `1`, `delta` to `0.5`, leaving `beta` untouched) genuinely run against the real
server on a second real connection, `await`ed to completion before the staged `set_zset_score` call
executes. Determinism comes from sequencing two real commands in program order — no sleep, no
timing margin. The test first asserts via `ZRANK` that `beta` actually moved from rank 1 to rank 2
(proving the reorder really happened, not just that scores changed), then asserts the *whole* set's
final state via four separate `ZSCORE` reads (`alpha`, `beta`, `gamma`, `delta`) rather than just the
`Written` return value — a write that landed on whatever now sits at the *old* rank 1 (`alpha`, post-
reorder) instead of on `beta` by name would still return `Written`, but would fail the `alpha`/`beta`
assertions.

**One naming fix needed against fred's actual signature, not the plan's**: `fred` 10.1's `zrank`
takes three arguments (`key`, `member`, `withscore: bool`), not two — `writer.zrank("zs:3", "beta")`
was written from the sibling tests' two-argument shape before checking fred's actual signature and
corrected to `writer.zrank("zs:3", "beta", false)`. A `cargo test --no-run` catch, not a Redis-facts
discrepancy.

**Not fixed, and not this phase's concern:** the same seeded-cursor-at-position-0 typing defect
phase 3 already recorded (`crates/core/src/state/editor.rs`, `EditBuffer::zset_score`/
`for_hash_field`/`list_element`) has no integration-suite angle — it is a UI/`EditBuffer` behaviour,
not something a headless `mutate::execute` call against a real server can observe. Left as phase 3
left it.

**A pre-existing integration test is flaky, found while verifying this phase.**
`a_freshly_added_entry_reads_as_just_added` (`crates/app/tests/integration.rs:1071`) failed on a
review re-run of the full `--ignored` suite, and passed on its own immediately afterwards. Phase 4's
own run had it passing, so it is a genuine intermittent, not a regression — and nothing on this
branch touches it (`git diff main..HEAD` on that file does not mention it).

**Cause.** `stream_entry_age` (`crates/core/src/state/value.rs:418`) returns `"just now"` only for
`secs == 0`, so the test has to complete an `XADD`, a full `read_value` round trip (`TYPE` plus the
metadata pipeline) *and* its own `SystemTime::now()` inside **one second**. Under full-suite load
with container startup contention, that window is missed. The test is correct about what it wants to
prove — that a Redis-assigned stream ID really does carry wall-clock epoch millis — it is just
asserting it through a sub-second deadline it does not control.

**Deliberately not fixed here**, because it is unrelated to ZSet and rewriting another feature's
test inside this row would bury it. The fix is small when someone takes it: assert the *shape* of a
recent age rather than the exact string — accept `"just now"` or `"Ns ago"` for a small `N` — which
still proves the ID parses as epoch millis without racing a one-second boundary. **Worth doing
soon**: CLAUDE.md has the integration job running on every push and nightly, so this will redden CI
at random, and an intermittently red suite is how people learn to stop reading it.
