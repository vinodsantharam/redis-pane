# ADR-0019 — A TTL is edited as a duration

**Status:** Accepted · **Date:** 2026-09-23 · **Verified against Redis 8.4.6 (and Redis 6.2.19 for
the floor claim) on 2026-09-23**

## Context

PLAN.md's M2 row 10 asks for TTL editing: set, persist, extend and shorten (PRD R4.2, R4.1, R4.4,
R3.9, R3.8). Tasks 6–9 (ADR-0015 … ADR-0018) are the template — one `Mutation` enum, one
`Msg::MutationSettled`, one `redis::mutate::execute`; `EditBuffer` + `EditTarget` + `EditPhase`;
the confirm dialog with its command line, one muted guard line, a scalar `old → new`, and
read-only refused once at confirm.

A TTL is the first mutation target that is not part of a `Value`. Every prior edit reads a
`Value`, branches on its Redis type, and writes part of it back; a TTL belongs to the key itself,
and every type has exactly one, edited identically. It is also the first input whose meaning is
inferred from a grammar — one text field resolves to one of three different writes — rather than
declared by which key opened the editor.

Eight facts about `EXPIRE`, `PERSIST`, `TTL` and `CLIENT TRACKING` shape the write. All were
verified for this ADR against a live server (`redis:8.4-alpine`, reporting version `8.4.6`, via
`./scripts/redis-up.sh` and `scripts/resp.py`), plus one against a `redis:6.2-alpine` container
started directly with `docker run` for the floor claim, rather than carried over from the planning
agent's reading of redis.io. All eight held exactly as drafted:

- **`EXPIRE key N` on a missing key returns `0` and does not create it.** Verified: against a
  missing key it returned `0` and `EXISTS` stayed `0`. The same self-guarding property `ZADD XX`
  gave ADR-0018 and `SET … XX` gave ADR-0014, and here it is unambiguous — `0` from `EXPIRE` means
  exactly "no such key," nothing else. A plain `EXPIRE` needs no script for the set case (D5).
- **`EXPIRE key 0` deletes the key immediately and returns `1`.** Verified directly:
  `EXPIRE k 0` → `1`, and `EXISTS k` → `0` right after. A negative argument does the same:
  `EXPIRE k -5` → `1`, key gone. **This is the most important fact in this task** — it is why `0`
  is refused at the keyboard rather than sent (D4).
- **`EXPIRE key N NX|XX|GT|LT` is Redis 7.0+.** Verified against `redis:6.2-alpine`: `EXPIRE k 50
  GT` returned `ERR wrong number of arguments for 'expire' command` — a 6.x server does not
  recognize the fourth argument as a flag at all and treats it as an arity error, not as an
  unsupported-option error. A plain `EXPIRE k 200` (no flag) on the same 6.x server succeeded
  normally. The floor is 6.0 (ADR-0007), so `GT`/`LT` cannot be relied on, and this is why
  extend/shorten needs a script (D5).
- **`EXPIRE` replaces an existing TTL outright on 6.0+ — there is no "only if greater" without
  `GT`.** Verified: a key at `TTL 1000` re-`EXPIRE`d to `10` read back `TTL 10`, no flag involved.
- **`EXPIRE` refuses an expire time whose absolute epoch overflows, with `ERR invalid expire time
  in 'expire' command`.** Verified and the boundary measured by binary search against the live
  server: the largest accepted argument was `9223370246680933` seconds, the smallest refused was
  `9223370246680934`, both bracketing `(i64::MAX − now_ms) / 1000` as expected from an
  internal ms-epoch overflow check. That is roughly 2.9×10⁸ years out — six orders of magnitude
  past this app's own `i32::MAX` seconds (≈68 years), which is the ceiling D4 actually enforces.
  Redis's own bound is not the binding one; see D4.
- **`PERSIST key` returns `1` when a timeout was removed, `0` when the key has no timeout *or*
  does not exist.** Verified all three: a key with a TTL → `1` (and `TTL` then reads `-1`);
  re-running `PERSIST` on that same, now-timeoutless key → `0`; `PERSIST` on a name that was never
  set → `0`. The two `0` cases are indistinguishable from the reply alone — the same ambiguity
  `ZADD XX CH` had, and the same reason persist needs a script (D5).
- **`TTL key` returns `-2` for a missing key, `-1` for no expiry, else whole seconds.** Verified
  all three directly. Inside Lua these come back as ordinary integers, so the scripts' arithmetic
  reads them without any string parsing.
- **`EXPIRE`/`PERSIST` change nothing but the expiry** — not implied to be independently
  re-verified here since it follows from the two facts above (no value command was ever called),
  but it is the reason the post-write refetch settles as `ReadOutcome::Unchanged` and the header
  reads `● live · unchanged`. That is correct; nothing here "fixes" it.

**`CLIENT TRACKING` and a TTL-only change — the fact that changes what the product claims.**
ADR-0006 verified that a `DEL`, an expiry, and a hash-field write all push an invalidation, but it
never isolated a TTL-only change (an `EXPIRE`/`PERSIST` on a key whose value is untouched) from
those. Verified here over a raw RESP3 socket, the same way ADR-0006 did: two clients, one issuing
`HELLO 3`, `CLIENT TRACKING ON OPTIN`, `CLIENT CACHING YES`, then `GET` on a key; the other running
`EXPIRE` and separately `PERSIST` on that same key.

**Both fired.** Transcript (abbreviated, full RESP3 bytes captured during the run):

```
A: HELLO 3                          → map incl. version 8.4.6
A: CLIENT TRACKING ON OPTIN         → +OK
B: SET ttl:track v                  → +OK
B: EXPIRE ttl:track 1000            → :1
A: CLIENT CACHING YES               → +OK
A: GET ttl:track                    → $1 v
A: CLIENT TRACKINGINFO              → flags: on, optin

B: EXPIRE ttl:track 50              → :1
A: <push, unsolicited>              → >2 $10 invalidate *1 $9 ttl:track

A: CLIENT CACHING YES               → +OK   (re-arm)
A: GET ttl:track                    → $2 v
B: PERSIST ttl:track                → :1
A: <push, unsolicited>              → >2 $10 invalidate *1 $9 ttl:track
```

An `EXPIRE`-only change on a key already read under `CLIENT CACHING YES` produces an ordinary
`invalidate` push, indistinguishable on the wire from the `DEL`/hash-field pushes ADR-0006
verified. So does a `PERSIST`-only change on a re-armed read of the same key. **A TTL change by
another client is therefore live, exactly like any other change to the key**, under the same
two re-arm invariants ADR-0006 established (after every invalidation, after every reconnect) — see
D7 for what this means for this task, and note this narrows rather than removes the caveat the
plan was written expecting: the countdown can still be wrong for up to one round trip between the
server-side change and this session's Refetch landing, exactly as any other liveness update can,
never for longer.

Both D5 scripts (below) were run end to end against the live server: gone key, no-expiry key,
would-expire-now, happy path, and a persist on a key that already had no expiry — all five cases,
for both scripts. All matched what the plan drafted; see D5 for the observed return codes.

## Decision

**D1 — `t` edits the Open key's TTL and reads nothing about its value.** Every prior value edit
branches on `Value` and refuses types it cannot handle. A TTL does not: a String, a Hash, a Stream
and a binary blob all have exactly one TTL, edited identically. So the refusal ladder in
`open_ttl_editor` is shorter than every sibling's — value-pane focused, not `gone`, not
`is_editing`, and a value has been read at all — with no `cursor_active` prerequisite and no
per-type arm. A binary value does not refuse `t`, for the same reason ADR-0018 D5 let a binary
member's score be edited: the bytes never travel. The "a value has been read at all" clause stays
even though a TTL edit needs no value, because acting on a key whose read has not landed means
acting on a key that may not exist.

**D4 — the duration grammar, and what it refuses.** Follows ADR-0018 D4 exactly: validation blocks
`Ctrl-S`, with a live indicator, and never fails at the confirm dialog (R4.4 is about knowing
before it runs).

*Accepted* (case-insensitive, surrounding whitespace trimmed):

- An optional leading `+` or `-`.
- Then either a bare non-negative integer (seconds), or one or more `<integer><unit>` segments
  with units `s`/`m`/`h`/`d` in strictly descending unit order: `2h30m` yes, `1d2h30m10s` yes,
  `30m2h` no, `1h1h` no.
- `never`, or the empty string, → persist.

*Rejected*, each with its own live line under the field and its own `Ctrl-S` block:

| Input | Line under the field |
|---|---|
| unparseable (`abc`, `1.5h`, `5 m`, `30m2h`) | `·· can't read that — try 5m, +30m, or never` |
| `0`, or a set resolving to `0` | `·· 0 deletes the key — use d` |
| `+30m`/`-10m` on a key with `ttl == -1` | `·· no expiry to change — type 30m to set one` |
| a shorten resolving to `≤ 0` | `·· that would expire it now — use d to delete` |
| a set above the ceiling | `·· too long — the most is about 68 years` |
| `never`/empty on a key already at `∞` | `·· already never expires` |

**`0` is the important one.** `EXPIRE key 0` deletes the key — verified above. A TTL field that
deletes a key when you type one character is a delete wearing a TTL's clothes, and `d` is how this
app deletes keys, with a preview that says `DEL` and confirmation that scales on `prod`. Typing `0`
must never route around that; this is not a parse failure, so it gets its own wording naming `d`.

**`+30m` on a key with no TTL is refused, not a silent no-op.** A silent no-op is the failure mode
ADR-0006 was written about: the reader presses a thing, nothing happens, and they cannot tell which
of two reasons it was. The wording names the fix.

**A shorten below zero is refused, not clamped.** Clamping to `1s` writes a number nobody typed —
the same objection that settled ADR-0018's staging fallback.

**The ceiling is the tighter of two limits.** Redis's own bound, measured above, is
`9223370246680933` seconds — roughly 2.9×10⁸ years given the current epoch. This app's own bound is
`i32::MAX` seconds (`2147483647`, ≈68 years), because `OpenKey::ttl_seconds`, `LoadedSet::ttls` and
`format_ttl` are all `i32`. The app's bound is six orders of magnitude tighter, so it is the one
that binds in practice; Redis's own boundary is recorded here for completeness and because ADR-0018
and this task's own plan both asked that it be found on the record rather than assumed, not because
a reader will ever reach it. Parsing must saturate rather than overflow, and the saturated value is
then refused by the ceiling check — no wrapping, no release-mode surprise.

**The rejection line names the ceiling in years, not through `format_duration`.** Settled at
checkpoint 1. `format_duration`'s two-most-significant-units rule (D10) renders `i32::MAX` seconds as
`24855d 3h`, which is arithmetically right and useless: a reader who has just typed something absurd
needs to know the shape of the limit, not its day count. The line is therefore fixed text:

> `·· too long — the most is about 68 years`

"about" is doing real work — the bound is an implementation artifact of `i32`, not a product
decision anyone made, and stating it to the second would dress it up as one. This is the only
refusal line in D4 that does not interpolate a live figure, for that reason.

**The `Ctrl-S` block reads `open.ttl_seconds` — the raw read TTL — not the counted-down one,
deliberately.** `editor_key` and `stage_editor` have no clock (ADR-0011). The two checks needing a
current TTL split cleanly: the `ttl == -1` check does not change with time and is exact; the
shorten-below-zero check is approximate and is the looser of the two on purpose. The script's
`n <= 0` guard is the tight one, running atomically at the server against the real TTL and refusing
with `NotWritten::WouldExpireNow`. The local check is a courtesy that catches the obvious case at
the keyboard; the server is the authority. The live preview line, which renders and therefore does
have a clock, shows the counted-down figure.

**D7 — the refetch is the only path by which a new TTL reaches the Viewer.** No local apply.
`write_landed` already issues a refetch after every write and sets `own_write` so the reply applies
immediately rather than being held. The new TTL arrives through `Msg::ValueLoaded` like every other
fact about the key, and `ttl_now` counts down locally from there.

What PLAN row 10's "no round trip" means: it is M1.11's existing property — the countdown ticks
locally between reads rather than polling the server (R3.9) — not a demand that the header update
before the network. The row's wording is amended to say so (PLAN.md M2 row 10).

**Why not apply optimistically.** It would carve a TTL-shaped exception into the invariant
`write_landed`'s own doc comment states: the reply is what reaches the Viewer, never the bytes this
session already knew it sent (ADR-0006: no value cache, not even a one-message-long one). The gain
is one round trip against a countdown that ticks once a second — imperceptible. The cost is real:
extend would display `staged_old + delta` where the server applied `ttl_at_write + delta`, a
knowingly-wrong number off by however long the confirm dialog was open. Waiting makes extend exact.

**A trap worth recording even though this task does not walk into it.** `OpenKey::read_at_ms`
anchors both `ttl_now`'s countdown and `currency`'s `read {ago}` line. Any future local TTL apply
routed through it would silently tell the reader their value had just been re-read when nothing of
the sort happened — a lie of exactly the class ADR-0006 was written about, in the header ADR-0006
added. If optimistic TTL updates are ever revisited, `OpenKey` needs a separate `ttl_read_at_ms`
first, the way `LoadedSet` already keeps `ttls` and `ttl_read_at` apart.

**The tracking finding narrows this, it does not remove it.** Because `EXPIRE`/`PERSIST` alone are
confirmed to push an invalidation (Context, above), a TTL change made by another client while this
key is open is live in the ordinary sense: it triggers a Refetch, and the header's `● live` claim
holds. What D7 forecloses is only this session's *own* write short-circuiting that same path —
there is exactly one way a new TTL figure reaches the screen, whether the write was this session's
or someone else's, and it is the Refetch.

**D5 — one bare command, two guarded scripts.** *Set* is a plain `EXPIRE key <seconds>`. No script:
`EXPIRE` already refuses to create a missing key and its `0` means exactly that and nothing else —
unlike `PERSIST`'s `0` and unlike `ZADD XX CH`'s.

*Persist* needs a script:

```lua
if redis.call('EXISTS', KEYS[1]) == 0 then return -1 end
return redis.call('PERSIST', KEYS[1])
```

`-1` key gone, `0` the key already had no expiry, `1` the expiry was removed. Verified end to end:
gone key → `-1`; a key with no expiry → `0`; a key with an expiry → `1`, and `TTL` then reads `-1`;
re-running persist on that same, now-timeoutless key → `0` again. A bare `PERSIST` answers `0` to
both of the first two, and "the key you were looking at is gone" and "it already never expired" are
completely different things to tell a reader who just asked for persist.

*Extend/shorten* needs a script because the delta must apply to the TTL as the server sees it at
write time, and because `EXPIRE … GT/LT` is 7.0+ and the floor is 6.0 (verified above):

```lua
local t = redis.call('TTL', KEYS[1])
if t == -2 then return -1 end
if t == -1 then return -2 end
local n = t + tonumber(ARGV[1])
if n <= 0 then return -3 end
redis.call('EXPIRE', KEYS[1], n)
return 1
```

`-1` key gone, `-2` no expiry to change, `-3` would expire it now, `1` written. `ARGV[1]` is the
signed delta in seconds. Verified end to end: gone key → `-1`; a key with no expiry, extend by
`30` → `-2`; a key at `TTL 5`, shorten by `-10` → `-3`, and `TTL` still read `5` right after — the
key was not touched; a key at `TTL 500`, extend by `100` → `1`, and `TTL` then read `600`; the same
key, shorten by `-100` → `1`, and `TTL` then read `500`.

`EVAL` on every call, never `EVALSHA` (ADR-0015, ADR-0016, ADR-0017, ADR-0018) — no script cache to
manage and no fallback path for a cache miss on a fresh connection.

**The script returns `1`, not the new TTL, and that is a decision.** Returning `n` would let the
core show the server's own arithmetic. It is refused because `MutationOutcome` is a closed enum
(`Done`/`NotWritten`/`NothingToRemove`) and widening it to carry data off a write's reply is exactly
what `Command::Execute`'s doc comment forbids — the core/shell seam would start carrying server
state back through the write path, one number at first. Under D7 there is no cost to refusing: the
refetch brings the real figure a round trip later, by the one legitimate path.

**D8 — guard lines**, each naming only what is actually checked (ADR-0018's ruling, which refused
both a false clause and a true-but-unearned one):

- **set** — `only if the key still exists`. Earned: `EXPIRE` genuinely refuses on a missing key and
  reports it unambiguously.
- **persist** — `only if the key still exists`. Earned by the script's `EXISTS`, which exists
  precisely so a gone key can be reported apart from a key that already had no expiry.
- **extend / shorten** — `only if the key still has an expiry · never expires it immediately`. Both
  clauses are literally the script's `t == -1` and `n <= 0` branches.

The tempting third clause is "never deletes the key" and it must not be added — that is Redis
behaving normally, not work the script does, and it would invite the reader to see protection where
there is only weather. Same ruling as ADR-0018 D2's rejection of "keeps the key's TTL," one type
over.

**D11 — the capture is the existing one-line hand-painted one, not a `TextArea`.** The Hash/ZSet
add form's *name* half is already a one-line capture backed by `EditBuffer` via `name_push`/
`name_pop`/`name_push_str`, painted by hand, with input routed by `name_part_key` ahead of
everything else in `editor_key`. `EditTarget::Ttl { text: String }` uses the same mechanism.

**The mechanical payoff is real: the TTL field does not inherit the seeded-cursor-at-position-0
defect** recorded in task 9's phase-3 notes. `EditBuffer::zset_score`/`for_hash_field`/
`list_element` all seed a `TextArea` whose cursor starts at `(0,0)`, so typing prepends and a bare
`Backspace` does nothing. The name-half capture has no cursor to be in the wrong place: characters
append, `Backspace` removes the last one. Seeding it with `42m` therefore behaves the way a reader
expects a seeded short scalar to behave — this is the first target that gets this right, and
knowing why is what will eventually fix the other three. Seeding uses raw `open.ttl_seconds`, not
`ttl_now`, since `open_ttl_editor` runs in `update`, which has no clock (ADR-0011); the seed is at
most a few seconds stale, and the live resolution line directly beneath it — which does render with
a clock — shows the true current figure immediately.

## Alternatives considered

- **Applying the TTL locally after a successful write, rather than waiting for the refetch.**
  Rejected per D7: it carves a TTL-shaped exception into ADR-0006's no-value-cache invariant for an
  imperceptible gain against a countdown that already ticks once a second, and it makes extend
  approximate rather than exact — the server's arithmetic uses the TTL *at write time*, which can
  differ from the TTL staged at open time by however long the confirm dialog sat open.
- **Returning the new TTL from the persist/shift scripts**, so the core could show the server's own
  arithmetic without a round trip. Rejected per D5: `MutationOutcome` is a closed enum and widening
  it to carry data off a write's reply is exactly what `Command::Execute`'s doc comment forbids.
- **One `Mutation` variant carrying resolved seconds**, computed once by the core, for all three
  operations. Rejected per D6 (in the plan): it cannot express extend atomically — the core would
  have to read the current TTL, compute the delta, and send a plain `EXPIRE`, reintroducing the
  read-then-write race the script exists to close.
- **A separate keystroke per operation** (`t` for set, `T` for persist, `Ctrl-T` for extend/shorten).
  Rejected: three bindings for one field, against CLAUDE.md's "only frequent actions earn a key,"
  and it moves disambiguation from typing time (where the resolution line already shows it) to
  binding-recall time.
- **An overlay menu** offering set/persist/extend/shorten as choices. Rejected: a third overlay and
  a fourth `Mode`, against "screen space is a budget, not a canvas" (CLAUDE.md).
- **`EXPIRE … NX|XX|GT|LT`** for extend/shorten directly, skipping the script. Rejected: verified
  above to be 7.0+, below the 6.0 floor (ADR-0007). Noted as the future simplification it is: if the
  floor ever rises past 7.0, the `ShiftTtl` script collapses into a single `EXPIRE … GT`/`LT` call.
- **Clamping a shorten that lands at or below zero to `1s`.** Rejected: it writes a number nobody
  typed, the same objection that settled ADR-0018's staging-fallback question.

## Consequences

Revised at phase 5 to describe what was actually built. Four things landed differently from, or in
addition to, what the decisions above anticipated — three of them defects found during execution,
each recorded here rather than only in the plan, because each is a trap the next person to touch
this code could walk into again.

**The grammar had to admit whitespace between segments, or the field refused its own seed.**
`format_duration` emits `1h 12m`, and D4's grammar originally rejected every internal space — so
opening `t` on a key at 4320s displayed `1h 12m` and then answered `can't read that` on `⌃S`. The
parser now allows whitespace *between* segments while still refusing it *within* one, so `1h 12m`
parses and `5 m` remains the typo it is. A round-trip test pins the invariant: every figure
`format_duration` can emit must parse back, and must never parse back as more time than it was made
from.

**An untouched TTL field must not stage, because its seed is lossy.** `format_duration` shows two
units, so a key at `1d 2h 30m 10s` seeds `1d 2h`. `EditTarget::Ttl` was briefly folded into
`stage_editor`'s `is_new_field` group, which bypasses the dirty check — meaning `⌃S` on an untouched
field would have written a TTL forty minutes shorter than the key had. It is deliberately **not** in
that group, and `EditBuffer::is_dirty` answers for a TTL by reading the target's own `text` rather
than the always-empty `TextArea` (D11). An untouched field closes without writing, exactly as every
value editor does, and the lossy seed can only reach the server once a reader has edited it.

**`paste` needed the same routing fix `editor_key` got.** Both routed on `active_part() ==
Some(FieldPart::Name)`, which is honestly `None` for a TTL (D11) — so a pasted duration went into
the buffer's unused `TextArea` and vanished, leaving the field looking untouched. Both now use
`EditBuffer::is_single_line_capture()`, which is the property each actually wanted.

**A key with no expiry rendered as `ttl 0s`, contradicting its own warning.** `TTL_NONE` is `-1` and
`format_duration` clamps a negative to `0s`, so the `SetTtl` dialog read `ttl 0s → 5m` — claiming the
key was about to expire — directly above `⚠ this key had no expiry` saying the opposite. Two adjacent
lines disagreeing, in the dialog where the reader decides. The `old` side of every TTL dialog now goes
through `render::ttl_before`, which says `never` for `TTL_NONE`. Found by the first golden frame ever
to render these arms, which is what those frames are for.

The rest landed as anticipated:

- `crates/core/src/state/ttl.rs` (new) carries `TtlEdit`, `TtlEditRefusal`, `parse_ttl_edit`,
  `resolve_ttl_edit` and `format_duration` — pure, clock-free, shared by the `Ctrl-S` block in
  `update` (reads raw `ttl_seconds`) and the resolution line in `render` (reads counted-down
  `ttl_now`), so the two cannot drift onto different grammars.
- `crates/core/src/mutation.rs` gains `Mutation::{SetTtl, PersistTtl, ShiftTtl}` and
  `NotWritten::{NoExpiry, WouldExpireNow}`.
- `crates/core/src/state/mod.rs` gains `PendingMutation::{SetTtl, PersistTtl, ShiftTtl}`, each with
  `command_text()` and D8's `guard_text()`.
- `crates/core/src/state/editor.rs` gains `EditTarget::Ttl { text }` and
  `EditBuffer::{ttl, ttl_text, is_single_line_capture}`; `field_name()` gains a `Ttl => None` arm so
  the TTL text never feeds the shown-duplicate checks it was never meant for.
- `crates/core/src/keymap/mod.rs` and `update/mod.rs` gain the `Action::ToggleTree` focus split —
  keys pane still toggles the tree, value pane opens the TTL editor. **The name `ToggleTree` is
  kept, settled at checkpoint 1**, with a doc comment naming both halves and pointing at `label_in`.

  The deciding argument is that this is not a new pattern: `Action::Refetch` already means *refetch*
  in the value pane and *rescan* in the keys pane — two different verbs under one variant named
  after only one of them, disambiguated for the reader by `label_in`. `ToggleTree` doing the same is
  consistent with a shape the keymap already has, rather than a novel compromise. The alternatives
  were each worse: a second `Action::EditTtl` bound to `t` is **unreachable**, since `action_for` is
  first-match, so it would compile, appear in help, and never fire; and a rename needs a name
  covering both halves, which `t`'s two unrelated verbs do not admit without something like
  `TreeOrTtl` that describes the keybinding rather than the behaviour.

  What the reader sees is unaffected either way: `label_in` says `tree` or `ttl` per pane, and
  `label()` says `tree / edit ttl` in the help overlay. The awkwardness is confined to one variant
  name inside the core, which is the cheapest place for it to live.
- `crates/app/src/redis/mutate.rs` gains the two Lua scripts above, `set_ttl`/`persist_ttl`/
  `shift_ttl`, and three arms in `execute` — no new `Command`/`Msg` variants, the same shape
  ADR-0018's ZSet arm reused.
- **Practical consequence of the tracking finding**: nothing in this task needs to build a
  degraded-liveness path for TTL changes specifically. A TTL change by another client behaves like
  any other change to the open key — it invalidates, the Viewer refetches, and the header stays
  accurate — under the same two re-arm invariants (after every invalidation, after every reconnect)
  every other write in this app already relies on. The manual test plan's "another client's TTL
  change" step is expected to show the countdown catching up promptly, not staying wrong until `r`.

## Sources

- `EXPIRE` (missing-key `0`, `key 0`/negative deletes immediately, replaces outright without a
  flag, `NX`/`XX`/`GT`/`LT` 7.0+, overflow error text):
  <https://redis.io/docs/latest/commands/expire/>
- `PERSIST` (`1`/`0` and the missing-key/no-timeout ambiguity):
  <https://redis.io/docs/latest/commands/persist/>
- `TTL` (`-2`/`-1`/seconds): <https://redis.io/docs/latest/commands/ttl/>
- `EVAL`/scripting: <https://redis.io/docs/latest/commands/eval/>
- `CLIENT TRACKING` / `CLIENT CACHING`: <https://redis.io/docs/latest/commands/client-tracking/>
- Live verification against `redis:8.4-alpine` (version `8.4.6`) and, for the `NX`/`XX`/`GT`/`LT`
  floor claim, a directly-run `redis:6.2-alpine` container: `EXPIRE`/`PERSIST`/`TTL`/`EXISTS`/`EVAL`
  round trips and both guarded scripts' five-case end-to-end runs described inline above, plus the
  raw RESP3 `CLIENT TRACKING ON OPTIN`/`CLIENT CACHING YES` invalidation transcript for `EXPIRE` and
  `PERSIST`, run 2026-09-23.
- Sibling decisions this one mirrors and diverges from:
  `docs/adr/0006-liveness-without-a-refresh-button.md`,
  `docs/adr/0015-hash-field-writes-are-guarded.md`,
  `docs/adr/0018-zset-scores-are-edited-members-are-not.md`.
