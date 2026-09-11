# ADR-0015 — Hash field writes are guarded Lua scripts, not plain `HSET`/`HSETNX`

**Status:** Accepted · **Date:** 2026-09-11

## Context

PLAN.md's M2 row 6 asks for inline edit of a Hash field's value, plus add and remove one field,
using the same preview/diff machinery ADR-0014 built for Strings (PRD R4.1, R4.4, R3.8).

Three Redis facts, verified against redis.io docs on 2026-09-11, shape the write:

- **`HSET` and `HSETNX` create the key when it doesn't exist.** Editing or adding a field on a hash
  that expired or was deleted under the confirm dialog would bring the key back with no TTL — the
  same hazard ADR-0014 closed for Strings with `SET … XX`, and there is no `HSET … XX` to reuse.
- **A key's own TTL is untouched by `HSET`/`HSETNX`/`HDEL`** — nothing to guard there.
- **Field-level TTLs (Redis 7.4+, `HEXPIRE`) are cleared by `HSET` overwriting that field.** A plain
  edit would silently drop a field's own expiry, which the reader neither asked for nor was told
  about.
- `HSETEX key FXX KEEPTTL FIELDS 1 field value` would do the edit atomically and exactly — write
  only if the field exists, keep the field's TTL — but it is Redis 8.0+ only. The project's floor
  is Redis 6.0 (ADR-0007), and `HSETEX … FNX` would still create a missing key, so it does not
  guard the add case either way.
- `HDEL` never creates anything, and deleting a Hash's last field deletes the key — Redis's own
  behaviour, needing no guard.
- fred 10.1 exposes `eval` only behind its `i-scripts` feature, not enabled by default in this
  project's `Cargo.toml` before this change.

## Decision

**Edit and add are each written as one guarded Lua script, sent with `EVAL` on every server
version this project supports (6.0–8.x) — never `HSETEX`, never a version-branched code path.**

The edit script (`HASH_FIELD_EDIT_SCRIPT`, `crates/app/src/redis/mod.rs`):

```lua
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then
  return 0
end
local ttl = redis.pcall('HPEXPIRETIME', KEYS[1], 'FIELDS', 1, ARGV[1])
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
if type(ttl) == 'table' and not ttl.err and ttl[1] and tonumber(ttl[1]) and tonumber(ttl[1]) > 0 then
  redis.call('HPEXPIREAT', KEYS[1], ttl[1], 'FIELDS', 1, ARGV[1])
end
return 1
```

Returns `-1` if the key is already gone (never recreated), `0` if the field is already gone
(nothing written), `1` on success. `HPEXPIRETIME` is 7.4+ only; `redis.pcall`, not `redis.call`, is
what lets this exact script still run to completion on 6.0–7.0, where the unknown subcommand comes
back as a Lua error table rather than aborting the script — verified against `redis:6.2-alpine` in
the integration suite.

The add script (`HASH_FIELD_ADD_SCRIPT`):

```lua
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[2])
```

Returns `-1` if the key is gone; otherwise `HSETNX`'s own `0` (an existing field, including one
outside the 500-field read window, was left untouched) or `1` (added).

Delete has no script: `HDEL` alone is enough (PLAN M2 task 6, D3, D4) — it creates nothing, and a
`false` return (nothing removed) is reported exactly like [`crate::redis::delete_key`]'s own
"already true" shape for `DEL`.

Both scripts are sent with `EVAL`, never `EVALSHA` — there is no script cache to manage, and a
cache miss on a fresh connection would otherwise need a fallback path. An ACL that denies
`@scripting` refuses the call outright; that surfaces as an ordinary error notification carrying
the command (R7.4), the same as any other Redis error this app maps.

**D2 — the command preview shows the effective command, plus one muted guard line, never the
literal `EVAL`.** The dialog's first line is what the script actually does — `HSET user:1 token`,
`HSETNX user:1 token` — with the stacked diff below it, plus one `Token::Muted` line naming the
guard the script runs under:

- edit: `only if the field still exists · keeps its TTL`
- add: `only if the key still exists · never overwrites a field`

`EVAL "<script>" 1 key field value` is unreadable in a confirmation dialog and would defeat R4.4's
purpose — showing the reader what is about to happen, not how it is transported. R4.4 and
CONTEXT.md's *Command preview* both gain one clause for this: **for a guarded write, the command it
performs and the guard it runs under**.

**D3 — scope is edit a field's value, add a field, remove a field.** Renaming a field (an atomic
`HSETNX new` + `HDEL old` in one script) is a natural follow-up but is not built here; it is a new
row in PLAN.md.

**D4 — `e`/`a`/`d` act on the value cursor's row, and `d` becomes focus-dependent.** With the value
pane focused, a Hash open and the value cursor on a row (`Enter` first): `e` edits that field's
value, `a` adds a field (no cursor prerequisite — a new field has no row to have picked), `d`
stages removing the field under the cursor. `d` joins `c` as a focus-dependent key: in the keys
pane it still stages `DEL` of the Selected key; in the Viewer, on a Hash with an active cursor, it
stages `HDEL`. The confirm dialog's first line (`DEL …` vs `HDEL …`) tells the two apart before
anything runs. Without an active cursor on a Hash, `e`/`d` give the notice `Enter to pick a field`;
`a` still works.

## Alternatives considered

- **`HSETEX … FXX KEEPTTL` on 8.0+, the script above on 6.0–7.x.** Rejected: two code paths, and the
  core would have to learn and carry the server's version to choose between them — exactly the
  kind of version-branching ADR-0007 was written to keep out of the read path, now showing up on
  the write side instead.
- **Plain `HSET`/`HSETNX`.** Rejected: recreates a key that expired or was deleted under the
  dialog, and silently clears a field's own TTL as a side effect of the edit.
- **`WATCH`/`MULTI`/`EXEC`.** Rejected: needs a connection dedicated to the transaction — `WATCH`'s
  guarantee only holds for the connection that issued it, and this project's client is
  multiplexed across the whole app (scans, metadata fetches, the tracked read) rather than
  reserved per operation. A script runs atomically on the server with no such requirement.
- **A field-level memo of what the read window last saw**, to decide client-side whether a field
  exists before writing. Rejected outright by ADR-0006: a memo keyed by field name is the exact
  bug this project exists to remove, one level down from key names.

## Consequences

- `crates/app/Cargo.toml` gains fred's `i-scripts` feature.
- `crates/app/src/redis/mod.rs` gains `set_hash_field`, `add_hash_field`, `delete_hash_field`, and
  the `FieldWrite`/`FieldAdd` result enums, built as binary-safe `Key`/`Vec<u8>` `ARGV` entries
  exactly like `delete_key`/`set_value` — see those functions' doc comments for the `Vec<u8>` →
  `Key` elementwise-conversion trap this avoids.
- `crates/core/src/state/editor.rs` gains `EditTarget` (`Value` / `HashField` / `NewHashField`) on
  `EditBuffer`, and `EditBuffer::for_hash_field`/`EditBuffer::new_hash_field` alongside the existing
  `from_value`. A Hash field opens its **raw** value, never reformatted, mirroring `from_value`'s
  String path; `was_json` is still classified from the raw text, so the dialog can warn the same
  way a String edit does — including, correctly, for a field whose value happens to be a bare JSON
  scalar such as a numeric ID, since a lone number is valid JSON syntax.
- `crates/core/src/state/mod.rs` gains `PendingMutation::{SetHashField, AddHashField,
  DeleteHashField}`, each with `command_text()` (D2) and a new `guard_text()` for the muted line.
- `crates/core/src/command.rs` gains `Command::{SetHashField, AddHashField, DeleteHashField}`,
  dispatched by the shell exactly like `Command::SetValue`: a spawned task, never a bare
  `Err(_) => return`.
- `crates/core/src/msg.rs`'s `Msg::ValueSetKeyGone` is generalized into `Msg::NotWritten { name,
  why: NotWritten::{KeyGone, FieldGone, FieldExists}, at_ms }`, with the String path migrated onto
  it — one message and one `update()` handler for every guarded write's refusal, rather than one
  per mutation. A `DeleteHashField` whose `HDEL` finds the field already gone is not an error and
  is not routed through `NotWritten` — there is no buffer to hand back — so it gets its own
  `Msg::HashFieldAlreadyGone`, reported as a notice, then a Refetch.
- `OpenKey` gains `field_capture: Option<String>` for `a`'s one-line field-name capture, and the
  keymap gains `Action::AddField` (default `a`). `Action::Delete`'s `pane_is_on_screen` is now
  focus-dependent, like `Action::Copy`'s existing focus split.
- The command-preview dialog's `PendingMutation` match, and the render layer's `hint_bar`, both
  grow the three new cases without touching the String/`DeleteKey` ones (D2, D4).

## Sources

- Key and field expiration behaviour (`HSET`/`HDEL` clear field TTLs):
  <https://redis.io/docs/latest/develop/ai/search-and-query/advanced-concepts/expiration/>
- Field-level expiration in hashes (Redis KB):
  <https://support.redislabs.com/hc/en-us/articles/30050967065874-Using-Field-Level-Expiration-in-Redis-Hashes>
- `HSETEX` (8.0+, `FNX`/`FXX`, `KEEPTTL`): <https://redis.io/docs/latest/commands/hsetex/>
- `HPERSIST` / field expiration command family (7.4+): <https://redis.io/docs/latest/commands/HPERSIST/>
- `EVAL`/scripting: <https://redis.io/docs/latest/commands/eval/>
- Previous task's decision on never recreating a gone key:
  `docs/adr/0014-values-are-edited-inline.md`
