# M2 task 10: edit a key's TTL — set, persist, extend and shorten

Status: **drafted 2026-09-23**, after a design discussion with the user. The Redis facts below are
read off redis.io and are **not yet verified against a live server** — phase 1 verifies each one and
turns them into [ADR-0019](../adr/0019-a-ttl-is-edited-as-a-duration.md). Execution by a Sonnet
subagent, phase by phase, stopping at each checkpoint.

**Base branch.** `main` (`0c21315`, after task 9 merged). Nothing here stacks on unmerged work.

## Context

`PLAN.md` M2 row 10: "TTL editing: set / persist / extend · Local countdown (M1.11) reflects the new
TTL immediately post-confirm, no round trip; `PERSIST` clears it (R4.2)." PRD R4.2 (TTL editing as a
first-class action), R4.1, R4.4, R3.9, R3.8.

Tasks 6–9 ([ADR-0015](../adr/0015-hash-field-writes-are-guarded.md) …
[ADR-0018](../adr/0018-zset-scores-are-edited-members-are-not.md)) are the template, and almost all
of their machinery is reusable unchanged: one `Mutation` enum, one `Msg::MutationSettled`, one
`redis::mutate::execute` (review H1); `EditBuffer` + `EditTarget` + `EditPhase` (review H2); the
confirm dialog with its command line, one muted guard line, a scalar `old → new`, and read-only
refused once at confirm (`update/confirm.rs:19-35`).

**What is genuinely new here.**

1. **The first mutation whose target is the key's metadata, not its contents.** Every edit so far
   reads a `Value`, branches on its type, and writes part of it back. A TTL belongs to the key:
   every Redis type has exactly one and it is edited the same way. So there is no type branch, no
   cursor prerequisite, and no binary refusal (D1).
2. **The first input whose meaning is inferred from a grammar rather than declared by a keystroke.**
   One field resolves to three different writes (D3, D4).
3. **The first `Action` focus-split across two unrelated verbs.** `d` splits between two deletes;
   `t` splits between toggling the tree and editing a TTL (D2).
4. **The first guarded script whose guard is arithmetic** rather than existence (D5).

**What is notably not new.** Task 9's validation-blocks-`⌃S` shape (ADR-0018 D4) recurs almost
exactly, one grammar wider. ADR-0018's ruling that a guard line names only what the script *checks*
is the rule every guard line below is written against. Neither is re-litigated.

**One thing deliberately not done.** The countdown updates on the refetch, not optimistically — see
D7. That was the design discussion's main fork and it is settled.

### Redis facts that shape the write

**Drafted from redis.io, not yet verified.** Phase 1 verifies every one against a live
`redis:8.4-alpine` via `scripts/resp.py`, and the floor claims against a 6.x container, and reports
anything that differs.

- **`EXPIRE key N` on a missing key returns `0` and does not create it.** The same self-guarding
  property `ZADD XX` gave ADR-0018 and `SET … XX` gave ADR-0014 — and here it is *unambiguous*: `0`
  from `EXPIRE` means exactly "no such key". So a plain `EXPIRE` needs no script (D5).
- **`EXPIRE key 0` deletes the key immediately** and returns `1`. A negative argument does the same.
  **This is the most important fact in this task** and the reason `0` is refused at the keyboard
  (D4).
- **`EXPIRE key N NX|XX|GT|LT` is Redis 7.0+.** The floor is 6.0 (ADR-0007), so it cannot be relied
  on. Record the exact error a 6.x server gives, so the floor argument is on the record rather than
  assumed. This is why extend/shorten needs a script (D5).
- **`EXPIRE` replaces an existing TTL outright** on 6.0 — there is no "only if greater" without `GT`.
- **`EXPIRE` refuses an expire time whose absolute epoch overflows**, with `ERR invalid expire time
  in 'expire' command`. Find the actual boundary on a live server; D4 takes the tighter of that and
  this app's own `i32`.
- **`PERSIST key` returns `1` when a timeout was removed, `0` when the key has no timeout *or* does
  not exist.** The two are indistinguishable from the reply — the same ambiguity `ZADD XX CH` had,
  and the same reason it needs a script (D5).
- **`TTL key` returns `-2` for a missing key, `-1` for no expiry**, else whole seconds. Inside Lua
  these come back as integers, so the script's arithmetic reads them directly.
- **`EXPIRE`/`PERSIST` change nothing but the expiry** — not the value, not the type, not the
  reported memory. The post-write refetch therefore returns an identical value and settles as
  `ReadOutcome::Unchanged`, so the header reads `● live · unchanged`. That is correct; do not "fix"
  it.
- **`CLIENT TRACKING` and a TTL-only change — the one fact that changes what the product claims.**
  ADR-0006 verified that a `DEL`, an expiry and a hash-field write all push an invalidation. **It
  never tested whether `EXPIRE`/`PERSIST` alone does.** Phase 1 must, over a raw RESP3 socket the
  way ADR-0006 did: two clients, one tracking the key under `CLIENT CACHING YES`, the other running
  `EXPIRE`. **If no invalidation fires, ADR-0019 must say so plainly** — liveness does not cover
  another client's TTL change, the countdown can be confidently wrong while the header reads
  `● live`, and `r` is the only correction. That is exactly the ambiguity ADR-0006 exists to
  prevent. Nothing in this task fixes it; the decision is to find out and record it.
- **Both D5 scripts must be run end to end**, not just their constituent commands: gone key,
  no-expiry key, would-expire-now, happy path, and a persist on a key that already has no expiry.

## Decisions (drafted — confirm at checkpoint 1)

**D1. `t` edits the Open key's TTL and reads nothing about its value.**
Every prior value edit branches on `Value` and refuses types it cannot handle. A TTL does not: a
String, a Hash, a Stream and a binary blob all have one TTL, edited identically. So the refusal
ladder in `open_ttl_editor` (mirroring `open_editor`'s at `update/editor.rs:20-34`) is **shorter**
than every sibling's — value-pane focused, not `gone`, not `is_editing`, and a value has been read
at all — with no `cursor_active` prerequisite and no per-type arm. A binary value does **not** refuse
`t`, for the same reason ADR-0018 D5 let a binary member's score be edited: the bytes never travel.

The "a value has been read at all" clause stays even though a TTL edit does not need the value:
acting on a key whose read has not landed means acting on a key that may not exist. Notices:
`"gone — no ttl to edit"`, `"still saving the last edit"`, `"nothing open to edit"`.

**D2. `t` is focus-dependent, exactly like `d`.**
Keys pane → `ToggleTree` (unchanged), value pane → edit TTL. `Keymap::action_for`
(`keymap/mod.rs:424`) is a flat first-match lookup with no focus awareness, deliberately: focus
splitting happens *after* resolution, inside one `Action`, the way `Action::Delete` does at
`update/mod.rs:348-349`. Three sites change and no more:

- `update/mod.rs`: `Action::ToggleTree if state.keys_pane_focused() => toggle_tree(state)` /
  `Action::ToggleTree => open_ttl_editor(state)`.
- `keymap/mod.rs` `pane_is_on_screen` (106-175): `ToggleTree` moves into the focus-dependent group
  beside `Delete`/`Edit`/`Add`.
- `keymap/mod.rs` `label_in` (220-228): `"tree"` when the keys pane is focused, else `"ttl"`, so the
  hint bar names the half in force — the mechanism `Refetch` already uses for `rescan`/`refetch`.
  `label()` (the help overlay, where no pane is focused) becomes `"tree / edit ttl"`, mirroring
  `Refetch`'s `"refetch / rescan"`.

**The `Action::ToggleTree` name now under-describes what it does. Settle this at checkpoint 1.**
A second `Action::EditTtl` bound to `t` would be *unreachable* — `action_for` is first-match, so it
would compile, appear in help, and never fire. A rename needs a name covering both halves, and the
two halves of `t` are not two shades of one verb the way `d`'s two deletes are; `TreeOrTtl` is worse
than the problem. Default: keep the name, and give it a doc comment naming both halves and pointing
at `label_in`. This is the one decision here a reviewer might reasonably want the other way, so it
is flagged rather than made quietly.

**D3. One field, one grammar, three writes.**
`t` opens a single-line capture in the value pane, seeded with the current TTL as a duration
expression. The operation is read off what is typed:

| Typed | Operation | Write |
|---|---|---|
| `5m`, `90`, `2h30m` | set | `EXPIRE` |
| `+30m` | extend | script |
| `-10m` | shorten | script |
| *(empty)*, `never` | persist | script |

Under the field sits a live resolution line, `··`-prefixed and muted like the add form's placeholder
already is (`render/mod.rs:465-480`'s `·· Enter to write the value`):

```
·· set · 42m → 5m
·· extend · 42m → 1h 12m
·· shorten · 42m → 32m
·· persist · 42m → never
```

so the grammar disambiguates **while typing**, not at the confirm dialog. Extend and shorten resolve
exactly, not approximately, because the core never applies the arithmetic itself — the script does,
at the server (D7).

**D4. The duration grammar, and what it refuses.**
Follows ADR-0018 D4 exactly: **validation blocks `⌃S`, with a live indicator, and never fails at the
confirm dialog.** R4.4 is about knowing before it runs.

*Accepted.* Case-insensitive, surrounding whitespace trimmed.

- An optional leading `+` or `-`.
- Then either a bare non-negative integer (seconds), or one or more `<integer><unit>` segments with
  units `s`/`m`/`h`/`d` **in strictly descending unit order**: `2h30m` yes, `1d2h30m10s` yes,
  `30m2h` no, `1h1h` no. Descending-order-only is what makes the parse unambiguous and the preview
  trustworthy; accepting `30m2h` would mean silently summing something nobody writes on purpose.
- `never`, or the empty string → persist.

*Rejected*, each with its own live line and its own `⌃S` block:

| Input | Line under the field |
|---|---|
| unparseable (`abc`, `1.5h`, `5 m`, `30m2h`) | `·· can't read that — try 5m, +30m, or never` |
| `0`, or a set resolving to `0` | `·· 0 deletes the key — use d` |
| `+30m`/`-10m` on a key with `ttl == -1` | `·· no expiry to change — type 30m to set one` |
| a shorten resolving to `≤ 0` | `·· that would expire it now — use d to delete` |
| a set above the ceiling | `·· too long — the most is about 68 years` |
| `never`/empty on a key already at `∞` | `·· already never expires` |

Four deserve their reasoning on the record:

- **`0` is the important one.** `EXPIRE key 0` deletes the key. A TTL field that deletes a key when
  you type one character is a delete wearing a TTL's clothes, and `d` is how this app deletes keys —
  with a preview that says `DEL`, and confirmation that scales on `prod`. Typing `0` must never
  route around that. This is not a parse failure, so it gets its own wording naming `d`.
- **`+30m` on a key with no TTL is refused, not a silent no-op.** A silent no-op is the failure mode
  ADR-0006 was written about: the reader presses a thing, nothing happens, and they cannot tell
  which of two reasons it was. The wording names the fix.
- **A shorten below zero is refused, not clamped.** Clamping to `1s` writes a number nobody typed —
  the same objection that settled ADR-0018's staging fallback.
- **The ceiling is the tighter of two limits**: Redis's own (phase 1 measures it) and this app's,
  which is `i32::MAX` seconds, because `OpenKey::ttl_seconds`, `LoadedSet::ttls` and `format_ttl` are
  all `i32`. Parsing must **saturate rather than overflow**, and the saturated value is then refused
  by the ceiling check — no wrapping, no release-mode surprise.

**The `⌃S` block reads `open.ttl_seconds` — the raw read TTL — not the counted-down one, deliberately.**
`editor_key` and `stage_editor` have no clock (ADR-0011). The two checks needing a current TTL split
cleanly: the `ttl == -1` check does not change with time and is exact; the shorten-below-zero check
is approximate and is the **looser** of the two on purpose. **The script's `n <= 0` guard is the
tight one**, running atomically at the server against the real TTL and refusing with
`NotWritten::WouldExpireNow`. The local check is a courtesy that catches the obvious case at the
keyboard; the server is the authority. The live preview line, which renders and therefore *does*
have a clock, shows the counted-down figure.

**D5. One bare command, two guarded scripts.**

*Set* is a plain `EXPIRE key <seconds>`. No script: `EXPIRE` already refuses to create a missing key
and its `0` means exactly that and nothing else — unlike `PERSIST`'s `0` and unlike `ZADD XX CH`'s.
ADR-0018 D2 kept a redundant `EXISTS` only to separate two refusals; here there is only one.

*Persist* needs a script, for exactly the reason ADR-0018 D2's score edit did:

```lua
if redis.call('EXISTS', KEYS[1]) == 0 then return -1 end
return redis.call('PERSIST', KEYS[1])
```

`-1` key gone, `0` the key already had no expiry, `1` the expiry was removed. A bare `PERSIST`
answers `0` to both of the first two, and "the key you were looking at is gone" and "it already never
expired" are completely different things to tell a reader who just asked for persist.

*Extend/shorten* needs a script because the delta must apply to the TTL **as the server sees it at
write time**, and because `EXPIRE … GT/LT` is 7.0+ and the floor is 6.0:

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
signed delta in seconds.

`EVAL` on every call, never `EVALSHA` (ADR-0015, ADR-0016, ADR-0017, ADR-0018) — no script cache to
manage and no fallback path for a cache miss on a fresh connection.

**The script returns `1`, not the new TTL, and that is a decision.** Returning `n` would let the core
show the server's own arithmetic. It is refused because `MutationOutcome` is a closed enum
(`Done`/`NotWritten`/`NothingToRemove`) and widening it to carry data off a write's reply is exactly
what `Command::Execute`'s doc comment forbids (`command.rs:81-92`) — the core/shell seam would start
carrying server state back through the write path, one number at first. Under D7 there is no cost to
refusing: the refetch brings the real figure a round trip later, by the one legitimate path.

**D6. Three `Mutation` variants, two new `NotWritten` variants.**

`Mutation::{SetTtl { key, seconds }, PersistTtl { key }, ShiftTtl { key, delta_seconds }}`.

One `SetTtl` carrying resolved seconds cannot express extend without the core resolving the delta
itself, which loses the script's atomicity and reintroduces the read-then-write race. Both ends of
the seam need the three apart:

- *The shell sends three different things*: a bare command and two scripts with different return
  codings. One variant would be re-discriminated at `execute` by inspecting a payload, which is the
  shape task 8's D8 spent a phase removing.
- *The dialog shows three different things*: three command lines, three guard lines, three
  resolution lines.
- *`command_label` must differ*: `EXPIRE key` / `PERSIST key` / `EXPIRE key` — the third identical to
  the first on the wire, and unacceptable in an error notification that must not say "set" when the
  reader asked to extend.

`PendingMutation::{SetTtl { name, old_ttl, new_ttl }, PersistTtl { name, old_ttl }, ShiftTtl { name,
old_ttl, delta_seconds }}`, carrying `old_ttl` so the dialog's `old → new` line need not re-derive it
from a `State` it is not given.

**Two new `NotWritten` variants**, both real refusals with nothing written:

- `NotWritten::NoExpiry` — a `ShiftTtl` found the key had no expiry to change. Reason: `"key has no
  expiry to change"`. Not `KeyGone` (the key is fine) and not `FieldGone`/`MemberGone` (a TTL is
  neither).
- `NotWritten::WouldExpireNow` — a `ShiftTtl`'s arithmetic landed at or below zero at the server.
  Reason, in ADR-0017 `ElementMoved`'s longer "look again" register, because like that one it is a
  *race* refusal a reader meets on a key under churn rather than a broken write: `"that would expire
  it now — the ttl moved underneath it, look again"`.

`PersistTtl` returning `0` is **not** a `NotWritten` — nothing was refused and nothing is wrong. It
settles as `MutationOutcome::NothingToRemove`, whose handler (`update/confirm.rs:143-201`) is already
exhaustive over `Mutation` and will force a noun for all three new variants. `PersistTtl`'s noun is
`"expiry"`: `PERSIST k: expiry already gone`.

**D7. The refetch is the only path by which a new TTL reaches the Viewer.**

No local apply. `write_landed` (`update/confirm.rs:114-131`) already issues a refetch after every
write and sets `own_write` so the reply applies immediately rather than being held. The new TTL
arrives through `Msg::ValueLoaded` like every other fact about the key, and `ttl_now` counts down
locally from there.

**What PLAN row 10's "no round trip" means.** It is M1.11's existing property — the countdown ticks
locally rather than polling (R3.9) — not a demand that the header update before the network. Phase 1
amends the row to say so, since its ambiguity is what needed deciding.

**Why not apply optimistically.** It would carve a TTL-shaped exception into the invariant
`write_landed`'s own doc comment states: *"the reply is what reaches the Viewer, never the bytes this
session already knew it sent (ADR-0006: no value cache, not even a one-message-long one)."* The gain
is one round trip against a countdown that ticks once a second — imperceptible. The cost is real:
extend would display `staged_old + delta` where the server applied `ttl_at_write + delta`, a
knowingly-wrong number off by however long the confirm dialog was open. Waiting makes extend exact.

**A trap worth recording even though we are not walking into it.** `OpenKey::read_at_ms` (open.rs:187)
anchors *both* `ttl_now`'s countdown and `currency`'s `read {ago}` line. Any future local TTL apply
routed through it would silently tell the reader their value had just been re-read when nothing of
the sort happened — a lie of exactly the class ADR-0006 was written about, in the header ADR-0006
added. If optimistic TTL updates are ever revisited, `OpenKey` needs a separate `ttl_read_at_ms`
first, the way `LoadedSet` already keeps `ttls` and `ttl_read_at` apart (`loaded.rs:258-279`). Record
this in ADR-0019 so the next person finds the reason rather than the bug.

**D8. Guard lines**, each naming only what is actually checked — ADR-0018's ruling, which refused
both a false clause and a true-but-unearned one:

- **set** — `only if the key still exists`. Earned: `EXPIRE` genuinely refuses on a missing key and
  reports it unambiguously.
- **persist** — `only if the key still exists`. Earned by the script's `EXISTS`, which exists
  precisely so a gone key can be reported apart from a key that already had no expiry.
- **extend / shorten** — `only if the key still has an expiry · never expires it immediately`. Both
  clauses are literally the script's `t == -1` and `n <= 0` branches.

**The tempting third clause is "never deletes the key" and it must not be added** — that is Redis
behaving normally, not work the script does, and it would invite the reader to see protection where
there is only weather. Same ruling, same reason, one type over.

**D9. The confirm dialog, and one new warning.**
Reuses the ZSet score arm's shape (`render/mod.rs:963-991`) — command line, muted guard line, a
scalar `old → new`, then warnings — because that is the arm closest to this: a scalar changing on an
unchanged identity, not a byte diff.

```
EXPIRE session:9f3a 300            PERSIST session:9f3a
only if the key still exists       only if the key still exists
ttl 42m → 5m                       ttl 42m → never
```

```
EXPIRE session:9f3a +30m
only if the key still has an expiry · never expires it immediately
ttl 42m → 1h 12m
```

The extend dialog's command line is not a literal command, which is in keeping rather than a
departure: `PendingMutation::command_text`'s own doc comment establishes that a guarded write shows
"the effective command it performs", never the `EVAL "<script>" 1 …` it is transported as.

**One new warning line**, `Token::Warn`, beside the existing last-field/last-member/last-element
family:

- `⚠ this key had no expiry` — shown on a `SetTtl` where `old_ttl == -1`. This is the risky
  direction: a permanent key becoming a disposable one, and on `prod` that is how data goes missing
  at 3am. It is the TTL analogue of "last member — the key will be deleted".

Deliberately **not** added: a warning for a short resulting TTL. `5m` is a normal thing to ask for,
and warning about it would train the reader to ignore the warn token — the one thing the last-member
warning needs them not to do.

**D10. The preview needs a finer formatter than `format_ttl`, and gets one.**
`format_ttl` (`render/keys.rs:529-538`) renders at the coarsest useful precision — `1m` for 90
seconds — which is right for a column and a header where the figure is glanceable and always moving.
It is wrong for the one moment the exact number matters, which is while the reader decides what to
write: `·· 42m → 1m` for `90` is actively misleading.

So add `format_duration(seconds) -> String` beside it: `45s`, `1m 30s`, `1h 12m`, `2d 3h` — two
most-significant units, never more. The preview line and the dialog's `old → new` use it; the column
and the header keep `format_ttl` unchanged. Doc-comment each pointing at the other so the next person
does not "unify" them.

**D11. The capture is the existing one-line hand-painted one, not a `TextArea`.**
The Hash/ZSet add form's *name* half is already a one-line capture backed by `EditBuffer` via
`name_push`/`name_pop`/`name_push_str` (`state/editor.rs`), painted by hand at
`render/mod.rs:465-480`, with input routed by `name_part_key` (`update/editor.rs`) ahead of
everything else in `editor_key`. That is what `EditTarget::Ttl { text: String }` uses.

**The mechanical payoff is real: the TTL field does not inherit the seeded-cursor-at-position-0
defect** recorded in task 9's phase-3 notes. `EditBuffer::zset_score`/`for_hash_field`/`list_element`
all seed a `TextArea` whose cursor starts at `(0,0)`, so typing prepends and a bare `Backspace` does
nothing. The name-half capture has no cursor to be in the wrong place: characters append, `Backspace`
removes the last one. Seeding it with `42m` therefore behaves the way a reader expects a seeded short
scalar to behave. Say this in the ADR — it is the first target that gets this right, and knowing
*why* is what will eventually fix the other three.

Two routing consequences, both small:

- `active_part()` returns `None` for `Ttl`, honestly — there is no `FIELD`/`VALUE` split to be on. So
  `editor_key`'s routing predicate cannot be `active_part() == Some(FieldPart::Name)` any more.
  Replace it with `EditBuffer::is_single_line_capture()`, true for a `Name`-part add form and for
  `Ttl`. That is the property both call sites actually want, and naming it removes a `FieldPart` from
  a routing decision it was only incidentally good at.
- In `name_part_key`, `Enter`/`↓` currently advance to the value part. For `Ttl` there is nothing to
  advance to, so `Enter` stages — consistent with ADR-0014's 2026-09-22 amendment, under which
  `Enter` stages everything that is not a multi-line String.

`field_name()` must **not** return the TTL text: it feeds the shown-duplicate checks. It gains a
`Ttl => None` arm with a comment saying so, and `EditBuffer::ttl_text() -> Option<&str>` is what the
parser, preview and stage path read.

**Seeding uses raw `open.ttl_seconds`, not `ttl_now`.** `open_ttl_editor` runs in `update`, which has
no clock (ADR-0011). The seed is therefore at most a few seconds stale, and the live resolution line
directly beneath it — which *does* render with a clock — shows the true current figure immediately. A
key with no expiry seeds empty, and the placeholder carries the grammar: `·· 5m · +30m · never`.

**D12. One parser, two callers, no drift.**
The `⌃S` block runs in `update` against the raw TTL; the preview line runs in `render` against the
counted-down TTL. Both call one pure, clock-free helper in a new `crates/core/src/state/ttl.rs`:

```rust
enum TtlEdit { Set(i32), Persist, Shift(i32) }
enum TtlEditRefusal { Unreadable, ZeroDeletes, NoExpiry, WouldExpireNow, TooLong, AlreadyNever }
fn parse_ttl_edit(text: &str) -> Result<TtlEdit, TtlEditRefusal>
fn resolve_ttl_edit(edit: TtlEdit, current: i32) -> Result<TtlOutcome, TtlEditRefusal>
fn format_duration(seconds: i32) -> String
```

This is where the weight of phase 2's tests sits. `TtlEditRefusal` owns its own wording the way
`NotWritten::reason` does, so a new refusal forces a wording decision once rather than inheriting a
neighbour's.

**D13. The live indicator is the resolution line, not the hint bar.**
ADR-0018 D4 made the hint bar the live invalid-score indicator, and that was right there: a score has
nothing to show but a valid/invalid bit. A TTL edit has a *resolved value* to show, and a resolved
value belongs next to the thing being typed, not in the bar at the bottom of the screen. So the `··`
line under the field carries both the resolution and every refusal from D4, and the hint bar stays
constant: `⌃S apply · never persists · Esc cancel`. State the divergence from ADR-0018 in the ADR so
it does not read as an oversight.

## New and changed types

| Where | What |
|---|---|
| `core/src/state/ttl.rs` **(new)** | `TtlEdit`, `TtlEditRefusal`, `parse_ttl_edit`, `resolve_ttl_edit`, `format_duration` (D4, D10, D12) |
| `core/src/mutation.rs` | `Mutation::{SetTtl, PersistTtl, ShiftTtl}`; labels `EXPIRE key` / `PERSIST key` / `EXPIRE key`; `NotWritten::{NoExpiry, WouldExpireNow}` with `reason()` wording (D6) |
| `core/src/state/mod.rs` | `PendingMutation::{SetTtl, PersistTtl, ShiftTtl}` with `command_text()`, `guard_text()` (D8), `json_warning()` → `None`, `into_command()` |
| `core/src/state/editor.rs` | `EditTarget::Ttl { text }`; `EditBuffer::{ttl, ttl_text, is_single_line_capture}`; arms in `field_name`, `active_part`, `name_push`, `name_push_str`, `name_pop`, `advance_to_value`, `return_to_name` (D11) |
| `core/src/state/open.rs` | an `edit_verb` arm (`✎ editing ttl`) |
| `core/src/keymap/mod.rs` | `ToggleTree` arms in `pane_is_on_screen`, `label`, `label_in` (D2) |
| `core/src/update/mod.rs` | the `Action::ToggleTree` focus split (D2) |
| `core/src/update/editor.rs` | `open_ttl_editor`; the TTL `⌃S` block; `is_single_line_capture` routing; `name_part_key`'s `Enter`-stages arm; `stage_editor`'s three arms; `staged_edit_found_key_gone`'s `dialog_up` check |
| `core/src/update/confirm.rs` | `nothing_to_remove`'s `"expiry"` noun; `not_written`'s two new arms (D6) |
| `core/src/render/keys.rs` | `format_duration` (D10) |
| `core/src/render/mod.rs` | the `Ttl` capture body + resolution line; three `confirm_overlay` arms; the `had no expiry` warning; hint-bar arm (D3, D9, D13) |
| `app/src/redis/mutate.rs` | `TTL_PERSIST_SCRIPT`, `TTL_SHIFT_SCRIPT`; `set_ttl`, `persist_ttl`, `shift_ttl`; three arms in `execute` (D5) |

## Phases

Each phase is one commit, ends at a checkpoint, and must be reported before the next starts.

**Read task 9's "Found while building" first.** Its phase-2 note applies identically: the moment a new
`Mutation`/`PendingMutation`/`NotWritten`/`EditTarget` variant exists, every exhaustive match over
those types across both crates must handle it or the workspace does not build. `Mutation` is the
core/shell seam, so **phase 2 necessarily includes the real `crates/app/src/redis/mutate.rs` arms** —
a `todo!()` there would violate CLAUDE.md. That is planned, not a deviation.

**Expect a wider blast radius than task 9 had.** This adds variants to the same four enums *and*
changes `editor_key`'s routing predicate and an `Action`'s pane scoping. **Report every site that
forced a compile error**, and flag any of task 8's D8 exhaustiveness sites that did *not* — that
would be a finding about task 8, not about TTL.

### Phase 1 — Verify the Redis facts, then write the decisions down

Docs only, no code.

1. **Confirm the baseline test counts** on `main`: core 545, golden 148, app 38, integration 81.
   Re-run and report if they differ.
2. Verify every bullet under "Redis facts that shape the write" against redis.io and a real server,
   and both D5 scripts end to end. Report anything that differs. Measure `EXPIRE`'s own upper bound
   and record the exact error text.
3. **Run the `CLIENT TRACKING` experiment** over a raw RESP3 socket the way ADR-0006 did. Report the
   answer either way — this is the one fact here that changes what the product *claims*, not just
   what it sends.
4. Settle D2's `ToggleTree`-keeps-its-name question and D4's ceiling with the main agent.
5. Write **ADR-0019 — A TTL is edited as a duration**. Subjects are D1, D4 and D7. Record the
   verified facts including the tracking answer, all three writes and both scripts, D4's complete
   grammar with every rejection and its wording, D7's argument and the `read_at_ms` trap, D11's
   cursor-seeding note, and D8's guard lines against ADR-0018's "names only what it checks" ruling.
   Rejected alternatives: **applying the TTL locally** (D7 — carves an exception into ADR-0006 for an
   imperceptible gain, and makes extend approximate); **returning the new TTL from the script** (D5 —
   widens `MutationOutcome` into what `Command::Execute` forbids); **one `Mutation` variant carrying
   resolved seconds** (D6 — cannot express extend atomically); **a separate keystroke per operation**
   (`t`/`T`/`⌃T` — three bindings for one field, against CLAUDE.md's "only frequent actions earn a
   key", and it moves disambiguation from typing time to binding-recall time); **an overlay menu**
   (a third overlay and a fourth `Mode`, against "screen space is a budget"); **`EXPIRE … GT/LT`**
   (7.0+, below the floor — note it as the future simplification it is if the floor ever rises);
   **clamping a shorten to `1s`** (writes a number nobody typed). Follow ADR-0018's structure and
   length.
6. Update `PLAN.md` M2 row 10's "Proves" column to name what will actually be tested, **and amend the
   row's own wording** so "no round trip" says what it means (D7). Amend `docs/DESIGN.md:126`'s `t`
   row to "Edit TTL (set / persist / extend)" and scope it "value pane, focused" to match `e`/`a`.

**Checkpoint 1.** Report the fact-check results, the tracking answer, and the ADR. **Stop here** — do
not start phase 2 until the main agent confirms the decisions.

### Phase 2 — Core types, the parser, and the shell arms exhaustiveness forces

`state/ttl.rs`, `mutation.rs`, `state/mod.rs`, `state/editor.rs`, `render/keys.rs` per the table, plus
the real `redis/mutate.rs` implementations, plus the minimal arms every other exhaustive match needs,
each commented as unreachable-for-now citing ADR-0019.

Unit tests, with the weight on the parser: every accepted shape (`5m`, `90`, `2h30m`, `1d2h30m10s`,
`+30m`, `-10m`, `never`, `NEVER`, empty, whitespace-padded), every rejection by name (`abc`, `1.5h`,
`5 m`, `30m2h`, `1h1h`, `0`, `0s`, a shorten past zero, a shift on `ttl == -1`, the ceiling, an
overflowing accumulation, `never` on `∞`), the descending-unit rule specifically, `format_duration`'s
two-unit rule at every boundary, `command_label`, `command_text`, `guard_text`, the `had no expiry`
warning flag, both new `NotWritten` wordings, and the `EditBuffer::ttl` constructor including that
`field_name()` does not return its text.

No wiring into `update/`'s dispatch yet — `t` still toggles the tree everywhere.

**Checkpoint 2.** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`, and the boundary check. Report counts, and the list of sites that
forced a compile error.

### Phase 3 — Core wiring

1. **`keymap/mod.rs`, `update/mod.rs`**: D2's three sites. Tests: `t` in the keys pane still toggles
   the tree with a key open (the shape the existing focus-dependent `d` test has); `t` in the value
   pane opens the TTL editor; `pane_is_on_screen` follows focus; `label_in` says `tree` and `ttl` in
   the right places and `label()` says both.
2. **`update/editor.rs`, `update/confirm.rs`**: `open_ttl_editor` with D1's ladder;
   `is_single_line_capture` routing; `Enter` stages from the TTL capture; `stage_editor`'s three
   arms; D4's `⌃S` block; `nothing_to_remove`'s `"expiry"`; `not_written`'s two new arms.

Tests: D1's ladder including that `t` works on every value type and on a binary value; every `⌃S`
block by name; all three confirm dialogs; read-only refused at confirm; R3.8 held-while-editing across
a TTL capture; and that a settled TTL write issues a refetch rather than applying anything locally
(D7 — assert the `Command::ReadKey`, and that `ttl_seconds` is untouched until the reply lands).

**Checkpoint 3.** Same commands. Report counts.

### Phase 4 — Docker-backed proof of the shell

Integration tests in `crates/app/tests/integration.rs`, every one `#[ignore]`d, under a new
`// ── PLAN M2 task 10 — TTL set, persist, extend/shorten (ADR-0019) ──` section following the ZSet
section's structure and naming. Proving: a set lands and `TTL` reads it back; a set on a gone key
returns `KeyGone` and does not recreate it; a persist clears the expiry and reads back `-1`; a persist
on a key that already had none settles `NothingToRemove` without error; a persist on a gone key
returns `KeyGone`; **an extend adds to the server's TTL, not the staged one** — set a TTL, change it
from a second real connection, then run the staged extend and assert against the second value, the
analogue of task 9's reorder test and the test this row exists for; a shift on a key with no expiry
returns `NoExpiry` and writes nothing; a shift past zero returns `WouldExpireNow` and **the key still
exists afterwards**; a shift on a gone key returns `KeyGone`; a set changes neither the value nor the
type nor the member count; and — D1 made concrete — `EXPIRE` works identically on a String, a Hash and
a ZSet, since unlike every sibling task it is type-agnostic.

**Checkpoint 4.** The three commands plus `cargo test -p redis-pane -- --ignored --test-threads=1`
(needs Docker). Report both counts.

### Phase 5 — Render, golden frames, docs

1. The TTL capture body and its `··` resolution line; three `confirm_overlay` arms; the `⚠ this key
   had no expiry` warning; the hint-bar arm. Golden frames: the capture seeded on a key with a TTL and
   empty on one without (with its placeholder); the resolution line for set, extend, shorten and
   persist; at least three of D4's refusal lines including `0 deletes the key`; all three confirm
   dialogs; the `had no expiry` warning. Pin that the extend dialog visibly differs from the set one.
2. **`CONTEXT.md` has no TTL entry at all.** Add the terms this task makes load-bearing: a **TTL**
   (the server's fact), the **countdown** (the local projection of it, R3.9), and a **duration
   expression** (what the reader types), plus set/persist/extend/shorten as named operations.
3. `PLAN.md` M2 row 10 to **done**; `docs/DESIGN.md` §6 if the capture needs describing; ADR-0019's
   Consequences to match what was built, including the tracking finding's practical consequence.
4. `docs/reviews/2026-09-13-codebase-design-review.md` §9: task 9's phase 5 revisited the M3 trait
   question with four types. **This task is the first edit that is not per-type at all**, which is new
   evidence for that question — state whether a metadata-shaped mutation belongs inside whatever shape
   the four types suggested, or sits beside it. Do **not** implement it.

**Checkpoint 5.** Full local verification. Do not open the PR — the main agent does that.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'   # must print nothing
cargo test -p redis-pane -- --ignored --test-threads=1                      # phase 4 on; needs Docker
```

Baseline on `main` before any change: **core 545, golden 148, app 38**, integration **81**. Counts
only go up.

## Manual test plan

```bash
./scripts/redis-up.sh
python3 - <<'EOF'
import sys; sys.path.insert(0, "scripts")
from resp import Resp
r = Resp()
r.call("DEL", "ttl:str", "ttl:none", "ttl:short", "ttl:hash", "ttl:bin")
r.call("SET", "ttl:str", "hello");     r.call("EXPIRE", "ttl:str", "2520")
r.call("SET", "ttl:none", "forever")
r.call("SET", "ttl:short", "x");       r.call("EXPIRE", "ttl:short", "45")
r.call("HSET", "ttl:hash", "f", "v");  r.call("EXPIRE", "ttl:hash", "600")
r.call("SET", "ttl:bin", b"\xff\x80")
EOF
cargo run -p redis-pane -- --url redis://127.0.0.1:6379
```

- [ ] **`t` follows focus.** In the keys pane, `t` still toggles tree view. `Tab` to the value pane on
      `ttl:str`, and `t` opens the TTL field. The hint bar says `tree` in one pane and `ttl` in the
      other.
- [ ] **Set.** Field seeded `42m`. Clear it, type `5m` — the line reads `·· set · 42m → 5m`. `⌃S`,
      then `y`. The header reads `5m` once the refetch lands, and the keys-pane row agrees.
- [ ] **Extend and shorten.** `t`, `+30m` → `·· extend · 5m → 35m`. `-2m` → `·· shorten`. Both land.
- [ ] **Persist.** `t`, clear the field → `·· persist · 35m → never`. Also try `never`. Header reads
      `∞` and the countdown stops.
- [ ] **Every rejection.** On `ttl:str`: `0`, `abc`, `1.5h`, `30m2h`, `-99h`, and a ten-digit number.
      Each shows its own line and `⌃S` does nothing. On `ttl:none`: `+30m` refuses with `no expiry to
      change`, and `never` refuses with `already never expires`.
- [ ] **Every type.** `t` works on `ttl:hash` with no cursor active, and on `ttl:bin` — neither
      refuses (D1).
- [ ] **Gone under the dialog.** Stage a set on `ttl:short`, `DEL ttl:short` from a second terminal,
      `y`. Refused with `key no longer exists`, nothing recreated.
- [ ] **Moved under the dialog.** Stage `-40s` on `ttl:short`, then `EXPIRE ttl:short 5` elsewhere,
      `y`. Refused with `WouldExpireNow`, and the key is still there.
- [ ] **Another client's TTL change.** `EXPIRE ttl:str 60` from a second terminal and watch the
      header. **This is phase 1's tracking finding made visible** — if no invalidation fires, the
      countdown stays wrong until `r`, and that must match what the ADR says.
- [ ] **Read-only.** `⌃R`, then stage any of the above: refused at the dialog, nothing sent.
- [ ] **Held while editing.** With the TTL field open, change the value from a second terminal. The
      header reads held, and the field is untouched.

## Out of scope

- **`EXPIREAT` and absolute times.** The grammar is durations; an absolute timestamp is a different
  input problem (timezones, formats) and PLAN row 10 says set/persist/extend.
- **`EXPIRE … NX|XX|GT|LT`.** Below the server floor (ADR-0007). If the floor ever rises, the
  `ShiftTtl` script collapses into `EXPIRE … GT`/`LT` — note that in the ADR as the future
  simplification it is.
- **Per-field TTLs** (`HEXPIRE`, 7.4+). ADR-0015 already touches `HPEXPIRETIME` for preservation;
  editing one is its own row.
- **Bulk TTL editing.** Task 13's multi-select feeds the same chokepoint; this row is one key.
- **Fixing the seeded-cursor-at-position-0 defect** in `for_hash_field`/`list_element`/`zset_score`.
  D11 explains why the TTL field does not have it; fixing the other three is still the cross-type
  change task 9 declined to make unilaterally.

## Found while building

_(Executor: append anything noticed but deliberately not fixed, with `file:line`.)_

**Phase 1.** The `CLIENT TRACKING` experiment came back the opposite of what the plan's Context
section was prepared for: an `EXPIRE`-only and a `PERSIST`-only change on an armed, re-armed key
both produced an ordinary `invalidate` push (see ADR-0019's Context and Consequences). This is
good news, not a defect, but it does mean the "another client's TTL change" line in this plan's
manual test plan should be read as an ordinary liveness check, not a demonstration of a known gap —
the countdown is expected to catch up promptly, not stay wrong until `r`. Nothing was fixed because
nothing was broken; flagging it here only so phase 5 does not go looking for a gap that measurement
closed. Also not fixed, because out of scope for phase 1: `docs/PLAN.md` row 10 previously read
"TTL editing: set / persist / extend" with no mention of shorten in the Task column despite the
plan's own D3 table listing shorten throughout — the amended wording (this phase) folds shorten in
explicitly rather than leaving the asymmetry for a future reader to puzzle over.

**Phase 2.** All types built per the table: `crates/core/src/state/ttl.rs` (new) — `TtlEdit`,
`TtlOutcome`, `TtlEditRefusal`, `parse_ttl_edit`, `resolve_ttl_edit`, `format_duration`, pure and
clock-free; `Mutation::{SetTtl, PersistTtl, ShiftTtl}` and `NotWritten::{NoExpiry,
WouldExpireNow}` (`crates/core/src/mutation.rs`); `PendingMutation::{SetTtl, PersistTtl,
ShiftTtl}` with `command_text()`/`guard_text()`/`into_command()` (`crates/core/src/state/mod.rs`);
`EditTarget::Ttl { text }` and `EditBuffer::{ttl, ttl_text, is_single_line_capture}`
(`crates/core/src/state/editor.rs`); the real `TTL_PERSIST_SCRIPT`/`TTL_SHIFT_SCRIPT` and
`set_ttl`/`persist_ttl`/`shift_ttl`, plus three arms in `execute`
(`crates/app/src/redis/mutate.rs`).

**`PendingMutation::ShiftTtl::command_text` is not a literal command, on purpose.** D6's table
says the label is "EXPIRE key" for both Set and Shift (matching what actually crosses the wire),
but that reading conflicts with D6's own worry about an error notification misleadingly reading
"set" for an extend — resolved by noting `Mutation::command_label` (the short form used only in
error notifications, which never spells out an operation word at all — it never says "set")
literally returns `EXPIRE {key}` for both, satisfying D6's concern by construction, while
`PendingMutation::command_text` (the confirm dialog's fuller preview, per D9's own worked
examples) shows a signed duration like `EXPIRE k +30m` instead — not literal Redis syntax, but
`command_text`'s own doc comment already licenses a guarded write's "effective command" to diverge
from the wire form, and this is the form that actually answers "what did I ask for," which a raw
`delta_seconds` in the dialog would not.

**Sites forced to compile beyond the plan's declared four files**, matching task 9 phase 2's
precedent exactly: `crates/core/src/state/open.rs`'s `edit_verb` (one new arm, `✎ editing ttl`);
`crates/core/src/update/editor.rs`'s `stage_editor` (one new arm resolving `EditTarget::Ttl`'s text
via `parse_ttl_edit`, parsed *before* `open.stage_edit()` is called, mirroring the ZSet score
block's own precedent for "no honest fallback exists, so refuse to stage" — the same shape task 9
phase 2 records for its own `nan`-score fallback) and `staged_edit_found_key_gone`'s `dialog_up`
match (three new or-pattern arms); `crates/core/src/update/confirm.rs`'s `nothing_to_remove` (one
new arm, `PersistTtl => "expiry"`, plus `SetTtl`/`ShiftTtl` folded into the existing `"entry"`
catch-all since neither ever settles that way) and `not_written` (two new arms joining the
existing `FieldGone`/`FieldExists`/`MemberExists`/`ElementMoved`/`MemberGone` group, since
`NoExpiry`/`WouldExpireNow` behave identically at this level: the key is fine, the buffer comes
back, the Viewer re-reads); `crates/core/src/render/mod.rs`'s `confirm_overlay` (three new arms
with real preview lines per D9, including the `⚠ this key had no expiry` warning gated on `old_ttl
== TTL_NONE`). All given real arms, not stubs, and all unreachable today since nothing in
`update/`'s dispatch constructs a TTL `EditTarget`/`PendingMutation` yet — `stage_editor`'s Ttl arm
and the `is_new_field`-style dirty-check bypass just above it are exercised directly by phase 2's
own unit tests (calling `stage_editor` on hand-built state), the same way task 9 phase 2 could not
exercise its ZSet arms via `update()` dispatch either.

**The seeded-cursor-at-position-0 defect genuinely does not recur here**, as D11 promised:
`EditBuffer::ttl` seeds `EditTarget::Ttl { text }` directly rather than a `TextArea`, and typing
routes through `name_push`/`name_pop`/`name_push_str` exactly as the Hash/ZSet add forms' name
half already does — confirmed by a dedicated test
(`ttl_typing_mutates_the_hand_painted_text_not_the_text_area`) that pops the seeded `"42m"` down to
empty and retypes, and separately asserts the buffer's `TextArea`-backed `text()` stays `b""`
throughout.

**Not built, deliberately out of this phase's scope, and not a gap:** `open_ttl_editor`, the `⌃S`
duration-grammar block, `editor_key`'s `is_single_line_capture()`-based routing, and `t`'s
keys-pane/value-pane split in `keymap/mod.rs`/`update/mod.rs` — all phase 3. `t` still toggles the
tree in both panes; no keypress opens a TTL editor. Golden frames for the resolution line and the
three confirm dialogs are phase 5, once phase 3 can actually stage one of these three variants
through the real dispatch path.

**Phase 4.** All eleven proofs landed in `crates/app/tests/integration.rs` under
`// ── PLAN M2 task 10 — TTL set, persist, extend/shorten (ADR-0019) ──`, every one `#[ignore]`d,
following the ZSet section's structure and naming exactly. No implementation defect was found:
`crates/app/src/redis/mutate.rs`'s phase-2 `set_ttl`/`persist_ttl`/`shift_ttl` and both scripts
worked as documented on every path exercised — nothing there needed extending or rewriting.

Test 6 (`extending_a_ttl_adds_to_the_servers_ttl_not_the_staged_one`) is the row this phase exists
for. It genuinely exercises the race, not a simulation of it: a key is `EXPIRE`d to 100s (standing
in for "the dialog" opening against that TTL and staging `+50s`), then a second, independent
connection really runs `EXPIRE key 500` on the live server and is `await`ed to completion —
strictly before the staged `shift_ttl(client, key, 50)` executes. `shift_ttl`'s own signature
(`client, name, delta_seconds`) carries no "old TTL" parameter at all, so there is no way for the
call site to smuggle in the stale value even by accident — the atomicity is structural, not just
asserted. The two candidate outcomes are numerically unambiguous (≈150 if it had used the staged
100, ≈550 if it used the server's live 500), and the test asserts the resulting TTL directly
(`final_ttl > 400` and `(530..=550).contains(&final_ttl)`), not merely the `ShiftTtlWrite::Written`
return value — the same shape task 9's reorder test asserts the whole set's final state rather than
just the outcome.

Nothing was noticed and left deliberately unfixed this phase — every proof the plan asked for
passed on the first run against `mutate.rs` exactly as phase 2 built it, and the one pre-existing
flake (`a_freshly_added_entry_reads_as_just_added`) passed cleanly in this run's
`--test-threads=1` pass, so there was nothing to isolate or re-run.

**Phase 3.** Built per the plan's own scope list. `keymap/mod.rs`: `ToggleTree` moved out of the
key-list `pane_is_on_screen` group into the `Delete`/`Edit`/`Add` focus-dependent one; `label_in`
gained a `ToggleTree` arm (`"tree"`/`"ttl"`); `label()` became `"tree / edit ttl"`; the `Action`
variant itself gained a doc comment naming both halves, per checkpoint 1's settled decision.
`update/mod.rs`: the two-arm focus split dispatching to `toggle_tree`/`open_ttl_editor`.
`update/editor.rs`: `open_ttl_editor` (D1's ladder, mirroring `open_editor`'s structure exactly
including the keys-pane-focused branch that is unreachable through the real keymap today but
keeps the function independently testable — the same reasoning `open_editor`'s own copy of that
branch has); `ttl_edit_blocked` (D4's `⌃S` gate, reusing `parse_ttl_edit`/`resolve_ttl_edit`
against `open.ttl_seconds`, the raw read figure); `typing_is_ttl`; `name_part_key` gained an
`is_ttl` branch so `Enter` stages directly (D11 — there is no value part to advance to) and
`Action::EditorStage` from the name part uses `ttl_edit_blocked` instead of
`add_form_name_blocked` for a TTL buffer; `editor_key`'s top routing predicate changed from
`active_part() == Some(FieldPart::Name)` to `EditBuffer::is_single_line_capture`.

**One site the plan's declared list did not name, found while wiring: `paste` in
`update/mod.rs`.** It routes a bracketed paste the same way `editor_key`'s old top check did —
`active_part() == Some(FieldPart::Name)` — which is `None` for a TTL capture (D11: one field,
always active, no `FieldPart` to be on). Left unfixed, pasting a duration into the TTL field would
have gone through `editor.insert_str(&text)` into the buffer's unused internal `TextArea` instead
of `name_push_str` into `EditTarget::Ttl`'s own `text` — silently absorbing the paste into a field
nothing reads. Fixed the same way `editor_key`'s predicate was: `is_single_line_capture()` in
place of the `FieldPart` check. Not covered by a dedicated unit test this phase (paste routing for
the Hash/ZSet name half has none either, and the TTL wiring tests exercise typed input via
`Backspace`/`Char`, not `Msg::Paste`) — worth a golden or unit test in phase 5 if the resolution
line's own tests do not already exercise it incidentally.

**One existing golden frame needed updating, not creating:** `crates/core/tests/golden/help_overlay.txt`,
whose `t` row read `tree` and now reads `tree / edit ttl` — a consequence of `label()`'s D2 change,
not a new TTL-specific frame (those are phase 5's job per the plan). Regenerated with
`UPDATE_GOLDEN=1`.

Every test the plan asked for landed in a new `crates/core/src/update/editor.rs` module,
`ttl_editor_wiring_tests` (21 tests), plus three in `keymap::tests`/`keymap::tests` and one in
`update::tests` for the focus split itself. Notably: the D1 ladder is driven both through the real
keymap dispatch (`open_ttl(..)`, which presses `t`) and via direct `open_ttl_editor(state)` calls
for the branches `update/mod.rs`'s dispatch makes structurally unreachable from a real keypress
(the keys-pane-focused checks) — the same shape `open_editor`'s own tests already use for the
mirrored branch. D7 is pinned by asserting `open.ttl_seconds` is unchanged both immediately after
`y` (before the reply) and after `Msg::MutationSettled` lands (still only `Command::ReadKey`
emitted, never a local apply) — covered for both `SetTtl` and `ShiftTtl`.
