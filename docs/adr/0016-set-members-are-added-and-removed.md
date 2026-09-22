# ADR-0016 — Set members are added and removed, never edited in place

**Status:** Accepted · **Date:** 2026-09-21

## Context

PLAN.md's M2 row 7 asks for add/remove of a Set member, using the same preview/diff machinery
ADR-0014 built for Strings and ADR-0015 built for Hash fields (PRD R4.1, R4.4, R3.8). Task 6
(Hash) is the template: one `Mutation` enum, one `Msg::MutationSettled`, one `redis::mutate::execute`
(review M2, H1), and the guard shape ADR-0015 established for a write that could recreate a key
deleted under the confirm dialog.

Seven facts about `SADD`, `SREM` and set member TTLs shape the write. All were re-verified for
this ADR — against the redis.io command docs and against a real server (`redis:8.4-alpine`, via
`./scripts/redis-up.sh`) — rather than carried over from the planning agent's reading, and all
seven held exactly as expected:

- **`SADD` creates the key when it does not exist.** `EXISTS newset` returned `0`; `SADD newset
  alpha` returned `1`; `EXISTS newset` then returned `1`. Adding a member to a set that expired or
  was deleted under the confirm dialog would bring the key back — the same hazard ADR-0014 closed
  for Strings and ADR-0015 closed for Hash fields. There is no `SADD … XX`: the command's own docs
  list no conditional variant, and `COMMAND DOCS SADD` confirms the summary is exactly "Adds one or
  more members to a set. Creates the key if it doesn't exist."
- **`SADD` on a member already present is a no-op returning 0, and writes nothing.** Against a live
  server, `SADD guardset alpha` (already a member) returned `0` and `SMEMBERS guardset` was
  unchanged; `SADD guardset beta` (new) returned `1` and `beta` appeared. This is the exact shape
  `HSETNX` gave the Hash add in ADR-0015, and the redis.io docs confirm it: "Specified members that
  are already a member of this set are ignored."
- **`SREM` never creates anything, and removing the last member deletes the key.** `SADD newset
  alpha` then `SREM newset alpha` returned `1`, and `EXISTS newset` then returned `0` — Redis's own
  behaviour for an emptied collection, identical to `HDEL` on a last field. The docs confirm
  `SREM` on a missing key is treated as an empty set and returns `0`, never an error and never a
  creation.
- **There is no per-member TTL for Sets.** `COMMAND INFO SEXPIRE` returned nothing — the command
  does not exist — while `COMMAND INFO HEXPIRE` returned a full spec. The `HEXPIRE` docs list its
  `group` as `hash` and its `since` as `7.4.0`; there is no Set counterpart at any version. The Set
  add script therefore has no TTL-preservation branch and is strictly simpler than
  `HASH_FIELD_EDIT_SCRIPT` — there is nothing here for `ADD` to preserve, only `EXISTS` to guard.
- **A key's own TTL is untouched by `SADD`/`SREM`.** Verified live: `SADD ttlset a b c`, `EXPIRE
  ttlset 1000`, then `SADD ttlset d` and `SREM ttlset a` — `TTL ttlset` read `1000` before and after
  both writes.
- **`SADD`/`SREM` against a key of a different type return `WRONGTYPE`.** Verified live:
  `SET strkey hello` then `SADD strkey x` and `SREM strkey x` both returned `WRONGTYPE Operation
  against a key holding the wrong kind of value`. The `EXISTS` guard below does not check type, so
  a key deleted and recreated as another type under the dialog surfaces an ordinary error
  notification carrying the command (R7.4) — the same accepted behaviour ADR-0015's scripts have.
- Sets are unordered and `SSCAN` gives no stable order — unchanged from the read path already
  built (`sscan_window`, `crates/app/src/redis/read.rs`, `MemberValue { members: Vec<Vec<u8>>,
  total }`, review C2).

There is a genuinely new decision here, though, that these facts alone don't settle: what does
"editing" a Set member even mean?

## Decision

**D1 — scope is add and remove; a Set member is never edited in place.** A Hash field has a name
that survives an edit and a value that changes under it — `HSET key field newvalue` is unambiguously
an edit of that field. A Set member has no such split: its bytes *are* its identity. There is
nothing to hold onto while "the value changes," because the value is the whole of what the member
is. Changing `"alpha"` to `"alpha2"` is not modifying a member, it is removing one member and
adding a different one — a **rename**, exactly the operation ADR-0015 D3 already deferred for Hash
fields (`HSETNX new` + `HDEL old` in one script, PLAN M2 task 14). Building "edit" for Sets here
would mean building that rename twice, once per type, with no shared shape between them. `e` on a
Set row gives a notice rather than opening a buffer; the rename, when it is built, is task 14's,
and Hash and Set can share its script and its confirm-dialog shape then.

**D2 — add is a guarded Lua script; remove is a plain `SREM`.** Mirrors ADR-0015 exactly, down to
`EVAL`-not-`EVALSHA`, for the same reasons: no script cache to manage, no fallback path for a cache
miss on a fresh connection, and atomicity that doesn't need a connection reserved for `WATCH`.

```lua
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('SADD', KEYS[1], ARGV[1])
```

Returns `-1` if the key is already gone (nothing written, nothing recreated), otherwise `SADD`'s
own `0` (the member was already there, including one outside the 500-member read window) or `1`
(added). Verified against a live server with the exact script above via `redis-cli --eval`: a
missing key returned `-1` and stayed missing; an existing member returned `0` and wrote nothing; a
new member returned `1` and appeared in `SMEMBERS`.

The command preview follows ADR-0015 D2: the dialog's first line is the effective command
(`SADD key`), not the literal `EVAL "<script>" …`, plus one muted guard line — "only if the key
still exists · never duplicates a member" — naming what the script actually guards.

`SREM` needs no script: it creates nothing, and a `0` return (nothing removed) is reported exactly
as `HDEL`'s "already gone" case is, and as `crate::redis::delete_key`'s own "already true" shape is
for `DEL`.

**D3 — the add form is one part, not two.** The Hash add form is a two-part `FIELD`/`VALUE` form
because a field has a name *and* a value (ADR-0015's follow-up F). A Set member is only a value, so
`EditTarget::NewSetMember` carries no `FieldPart` and the form is a single capture. The
shown-duplicate guard still applies exactly as ADR-0015's does for a Hash field name: exact byte
equality against a member already in the fetched window blocks `Enter`/`Ctrl-S` while typing, and a
duplicate hidden outside the 500-member window is still caught only at write time, by `SADD`
returning `0` — the same division ADR-0015 accepted for a hidden duplicate field.

## Alternatives considered

- **In-place edit as a combined `SREM old` + `SADD new` script.** Rejected: as D1 argues, this is a
  rename, not an edit — there is no field-name/value split to preserve one half of. Building it here
  would either duplicate the rename ADR-0015 D3 already deferred to task 14, or force task 14 to
  retrofit itself onto whatever shape got built first. Building it once, for both types, at task 14
  is strictly less work and gives Hash and Set the same rename UI instead of two that drifted apart.
- **A plain, unguarded `SADD`.** Rejected for the same reason ADR-0015 rejected plain `HSET`/
  `HSETNX`: it recreates a key that expired or was deleted under the confirm dialog, with no TTL and
  with a single member where the reader expected nothing at all. The `EXISTS` guard is the entire
  cost of avoiding that, and it is one line.

## Consequences

- `crates/core/src/mutation.rs` gains `Mutation::{AddSetMember, DeleteSetMember}`, with
  `command_label` → `SADD key` / `SREM key`, and `NotWritten::MemberExists` — a new variant, not a
  reuse of `FieldExists`, because a member and a field are different things in this codebase's
  glossary (CONTEXT.md) and the distinction should survive into the error type.
- `crates/core/src/state/mod.rs` gains `PendingMutation::{AddSetMember, DeleteSetMember {
  last_member } }`, each with `command_text()` and `guard_text()` (D2's muted line); the delete
  variant carries the same `last_field`-shaped warning ADR-0015 D4 gave Hash's last-field delete,
  renamed to what it actually is for a Set.
- `crates/core/src/state/editor.rs` gains `EditTarget::NewSetMember` and
  `EditBuffer::new_set_member()`, a single-part form per D3.
- `crates/core/src/keymap/mod.rs` renames `Action::AddField` to `Action::Add` — it now serves Hash
  fields and Set members both, and the user-facing label was already the neutral `"add"`
  (`keymap/mod.rs:200`), so this is an internal rename with no compatibility surface.
- `e`/`a`/`d` stay focus-gated to the value pane with a value fetched, per ADR-0015 D4; `d` stays
  focus-dependent — keys pane stages `DEL`, value pane on a Set stages `SREM`. `e` on a Set gives
  D1's notice instead of opening a buffer.
- Binary members are refused with a notice, exactly as binary Hash fields are (`editor.rs`:
  "binary fields aren't editable here yet"), worded "binary members aren't editable here yet."
- `crates/app/src/redis/mutate.rs` gains `SET_MEMBER_ADD_SCRIPT`, `add_set_member`,
  `delete_set_member`, and a `MemberAdd` result enum, plumbed through the same
  `Command::Execute`/`Msg::MutationSettled` shape review M2 (H1) already generalized Hash's writes
  onto — no new `Command`/`Msg` variants are needed for this type.
- The hint bar (`crates/core/src/render/mod.rs`) gains a Set-shaped arm alongside the Hash one:
  `a add · d remove`, with no `edit` — D1 means `e` never opens a buffer for a Set, so the hint
  must not promise a mode `open_editor` never gives it.

**Phase boundary corrected while building** (see the plan's "Found while building" for the full
account): planning drew the shell (`crates/app/src/redis/mutate.rs`) as phase 4's work and the
core types as phase 2's. Rust's exhaustive-match checking made that split unbuildable — the moment
`Mutation::{AddSetMember, DeleteSetMember}` existed, every `match` over `Mutation` anywhere in the
workspace, including the shell's `redis::mutate::execute`, had to cover them or the crate failed to
build, whether or not anything could reach the new arms yet. `Mutation` is the seam between core
and shell (review H1), so a phase that adds a variant to it cannot stop at the crate edge. `phase
2`'s commit therefore already contains `SET_MEMBER_ADD_SCRIPT`, `add_set_member`,
`delete_set_member` and the `MemberAdd` enum — a real implementation, not a stub, since a
`todo!()` on an unreachable path is still the `panic!` CLAUDE.md forbids and an `Err(_)` arm would
misreport a future success as a failure. Phase 4 proved this code against a real server rather than
writing it.

**D3 was corrected in phase 3: the shown-duplicate guard blocks `Ctrl-S` alone, not
`Enter`/`Ctrl-S`.** The decision as drafted echoed ADR-0015's wording for the Hash *name* part,
where `Enter` means "advance to the value part." A Set member has no name part —
`EditTarget::NewSetMember` carries no `FieldPart`, so `active_part()` returns `None` and the buffer
never routes through the name-part key handler that gives `Enter` that meaning. On the Set add
form's single `TextArea`, `Enter` keeps the ordinary editor meaning it has everywhere else —
insert a newline — and only `Ctrl-S` (`Action::EditorStage`) is gated by
`crates/core/src/update/editor.rs`'s `set_member_blocked`. A member that picks up a stray newline
this way is still caught before anything runs: the confirm dialog shows it in the `+` side of the
diff, which is what R4.4's preview is for.

**A member/field wording bug in `crates/core/src/update/confirm.rs`'s `nothing_to_remove`, found
and fixed in phase 3.** Before `d` on a Set was wired, every `MutationOutcome::NothingToRemove`
reported "field already gone" — correct for `DeleteHashField`, wrong for the `DeleteSetMember` this
ADR adds. Fixed to decide the noun from the `Mutation` itself, once. The match still ends in a `_
=> "field"` fallback rather than an exhaustive one, so it does not force the same decision on a
future `DeleteZSetMember` (task 9) the way an exhaustive match would — see the M3 inventory
(`docs/reviews/2026-09-13-codebase-design-review.md` §9) for the exact line and why it was left
that way rather than fixed here.

## Sources

- `SADD` (creates the key, ignores existing members, no conditional variant):
  <https://redis.io/docs/latest/commands/sadd/>
- `SREM` (never creates the key, deletes it when the last member goes):
  <https://redis.io/docs/latest/commands/srem/>
- `HEXPIRE` (field-level TTL is Hash-only, `since: 7.4.0`, `group: hash`, no Set equivalent):
  <https://redis.io/docs/latest/commands/hexpire/>
- `EVAL`/scripting: <https://redis.io/docs/latest/commands/eval/>
- Live verification against `redis:8.4-alpine` (`./scripts/redis-up.sh`, `redis-cli`,
  `redis-cli --eval`): `EXISTS`/`SADD`/`SREM`/`TTL`/`EXPIRE`/`SET`/`COMMAND INFO`/`COMMAND DOCS`
  round trips described inline above, run 2026-09-21.
- Sibling decision this one mirrors: `docs/adr/0015-hash-field-writes-are-guarded.md`.
