# M3 task 3: Slowlog viewer (`g s`)

Status: **planning — not started.** No code, no ADR. DESIGN.md has no prose for this screen at
all today — only the `g s` chord in §3's jump list and ADR-0008's single-node caveat. This doc is
the first design pass.

## Context

PLAN.md M3 row 3: "Slowlog viewer (`g s`): `SLOWLOG GET`/`RESET`, sort, single-node (ADR-0008) ·
Proves: entries render in a type-aware-consistent frame; `RESET` is a real mutation (confirm
dialog, read-only refusal)." PRD R6.4: "Slowlog viewer with sort and reset."

Redis's slowlog is a ring buffer the server keeps of commands that took longer than
`slowlog-log-slower-than` microseconds, capped at `slowlog-max-len` entries, readable with
`SLOWLOG GET [count]` and clearable with `SLOWLOG RESET`. Each entry is `(id, timestamp,
duration_us, args, client_addr, client_name)`. This is a request/response read against the
existing single connection — unlike Monitor and Pub/Sub, Slowlog needs no feed connection
(`docs/plans/m3-feed-connection.md` does not apply here) — which is exactly why it is a
reasonable place to start M3: it exercises the "new full-screen view reached by `g`" and
"a mutation that is not shaped like M2's key-scoped ones" machinery M3 needs, on the cheapest
possible data source.

**ADR-0008's single-node caveat applies as written**: `SLOWLOG` is per-node, so a Cluster target
would need a node selector this app's one-Connection premise does not have room for. Since Cluster
is out for v1 entirely (ADR-0008), this is inherited scope, not a new decision — noted here only
because Slowlog is the first M3 feature where the caveat is directly load-bearing rather than
background context.

## Architecture

### A new full-screen view, not a third pane

This is the first M3 feature to raise a question M0–M2 never had to answer: **the app currently
has exactly one screen** — two panes (keys, value), `Pane` (`crates/core/src/render/layout.rs`) is
`Keys | Value` and nothing else, and `State` has no notion of "which top-level view is showing."
DESIGN §3 already answers what the *navigation* looks like (`g`-prefixed chords: `g k` keys,
`g d` dashboard, `g m` monitor, `g p` pub/sub, `g s` slowlog), but the state machine behind it does
not exist yet. This task (being first alphabetically and simplest data-wise, though PLAN.md orders
it after task 2's feed plumbing since Monitor/Pub-Sub need that too) is a reasonable place to build
that machine once, since Dashboard/Monitor/Pub-Sub all need the same shape:

- `crates/core/src/state/mod.rs`: a `pub view: View` field on `State`, `View` being an enum
  (`Keys` (the existing two-pane browser — the default and only view through M0–M2), `Slowlog`,
  and, as those tasks land, `Dashboard`, `Monitor`, `PubSub`). This is additive to, not a
  replacement for, the existing `Pane` focus concept — `Pane` still decides keys-vs-value focus
  *within* the `Keys` view; `View` decides which full-screen surface is showing at all. `g s` sets
  `state.view = View::Slowlog`; `Esc` (DESIGN's "always back") returns to `View::Keys`, never
  closing the app itself the way DESIGN's `Esc` rule for the two-pane browser only pops one level.
  This `View` enum is worth flagging explicitly in DESIGN.md §3 once built, since it formalizes
  something the design doc currently only implies through the keymap table.
- `crates/core/src/state/slowlog.rs` (new): `SlowlogState { entries: Vec<SlowlogEntry>, sort: ...,
  loading: bool }`. `SlowlogEntry` is a plain columnar struct (id, timestamp, duration_us, command
  text, client addr/name) — small enough (`slowlog-max-len` defaults to 128) that this does *not*
  need the Loaded set's byte-arena-plus-parallel-arrays treatment (ADR-0010); that discipline
  exists because the keyspace can be a million keys, and a slowlog ring buffer structurally cannot
  be more than `slowlog-max-len` entries. A plain `Vec<SlowlogEntry>` is the right amount of
  machinery here, not a small copy of the keys-pane's arena for its own sake.
- Sort permutes an index the same way the keys pane's sort does (M1 task 7's discipline: "Sorting
  ... issues no mass fetch") — trivial here since the whole set is already in memory, but the
  *pattern* (an index vector, not re-fetching or re-ordering the backing data) carries over so a
  reader of this code recognizes the same idiom as `keys.rs`'s sort.

### Reading (`crates/app/src/redis/`)

- `crates/app/src/redis/read.rs` gains `fetch_slowlog(client: &Client, count: i64) ->
  Result<Vec<SlowlogEntry>, ...>`, issuing `SLOWLOG GET <count>` and parsing fred's reply into the
  core's `MetadataEntry`-shaped `SlowlogEntry` (a shell-side parse, core-side plain-old-data type
  — the same split `fetch_metadata` already uses). `Command::FetchSlowlog { count }` /
  `Msg::SlowlogLoaded { entries }` are the new `Command`/`Msg` pair, following the exact shape of
  `Command::FetchMetadata`/`Msg::MetadataLoaded`.
- Opening the view (`g s`) issues `Command::FetchSlowlog`; there is no liveness here (`SLOWLOG` has
  no `CLIENT TRACKING` equivalent) — `r` is a plain rescan/refetch, matching DESIGN's existing
  "there is no refresh button ... `r` is a scoped Refetch" everywhere else, applied to a view
  rather than a key.

### `RESET` as a mutation, through the existing chokepoint

R6.4 and PLAN's "Proves" column are explicit: `RESET` is "a real mutation (confirm dialog,
read-only refusal)," not a bare keypress that clears the server's ring buffer. This is the second
thing this task has to get right, and CLAUDE.md is direct about it: "Mutations flow through one
path that produces a command preview before executing. Read-only mode and confirmation-scaling
are enforced at that chokepoint, not at each call site." Concretely:

- `crates/core/src/mutation.rs`: `Mutation::ResetSlowlog`, alongside the existing per-key
  mutations. `crates/core/src/state/mod.rs`: `PendingMutation::ResetSlowlog` with
  `command_text()` returning the literal `SLOWLOG RESET` (there is no guarded-script indirection
  here — unlike the Hash/Set/List/ZSet edits, there is no recreate-a-gone-key hazard to guard
  against, since there is no key at all) — same `command_text()`/`guard_text()` shape task 6-10's
  `PendingMutation` variants already established, so `render/mod.rs`'s existing `confirm_overlay`
  needs only a new arm, not new machinery.
  - **This exercises the chokepoint on a mutation with no target key**, which every `PendingMutation`
    variant before it has had. Worth confirming during phase 1 that `State::confirm` and
    `update/confirm.rs`'s dispatch genuinely have no hidden assumption that a `PendingMutation`
    names a `KeyName` — if one exists, this is where it surfaces, not a design decision to make
    from scratch.
- Read-only Mode refusal is free: `update/confirm.rs`'s existing "refuse at confirm, never at the
  keypress that staged it" logic (R4.4, already proven by M2 task 2's tests) applies to
  `ResetSlowlog` the moment it is a `PendingMutation` variant, with zero new logic.

## CLAUDE.md rules this binds

- **Mutations flow through one path.** `RESET` is the point of this task's second half; see above.
- **Screen space is a budget, not a canvas.** Slowlog is a `View`, reached by `g s`, never a
  third permanent pane (Palette withdrawn, ADR-0020).
- **The render loop never does I/O.** `Command::FetchSlowlog`/`Command::ResetSlowlog` (issued only
  once confirmed) are the only I/O triggers; `update()` never calls into `redis/` directly.
- **Type-awareness is a first-class abstraction** doesn't apply literally (a Slowlog entry is not
  a Redis value type with a Viewer), but the "shared frame" idea does: Slowlog's screen should use
  the same header/body/footer/hint-bar chrome shape every other view uses, not a bespoke layout —
  this is what PLAN's "Proves: entries render in a type-aware-consistent frame" is asking for.
- **Colors are semantic tokens.** A slow-duration entry likely wants a warning color at some
  threshold — reuse whatever token the Dashboard's "alarming values are colored" rule ends up
  defining (see `docs/plans/m3-dashboard.md`) rather than inventing a second one.

## Files touched

| File | Change |
|---|---|
| `crates/core/src/state/mod.rs` | `View` enum, `pub view: View` on `State` |
| `crates/core/src/state/slowlog.rs` (new) | `SlowlogState`, `SlowlogEntry`, sort |
| `crates/core/src/command.rs` | `Command::FetchSlowlog { count }` |
| `crates/core/src/msg.rs` | `Msg::SlowlogLoaded { entries }` |
| `crates/core/src/mutation.rs` | `Mutation::ResetSlowlog` |
| `crates/core/src/state/mod.rs` | `PendingMutation::ResetSlowlog` |
| `crates/core/src/update/slowlog.rs` (new) | `g s` entry, sort keypress, stage/confirm `RESET` |
| `crates/core/src/render/mod.rs` | Slowlog screen render, `confirm_overlay` arm for `ResetSlowlog` |
| `crates/core/src/keymap/mod.rs` | `Action::OpenSlowlog`, `Action::ResetSlowlog` (or reuse `Delete`'s shape — decide in phase 1) |
| `crates/app/src/redis/read.rs` | `fetch_slowlog` |
| `crates/app/src/redis/mutate.rs` | `reset_slowlog` |
| `crates/app/src/terminal.rs` | dispatch arms for the two new `Command`s |

## Testing

- **Core unit tests**: `g s` sets `View::Slowlog` and issues `Command::FetchSlowlog`; `Esc` returns
  to `View::Keys`; sort permutes the index, not the backing `Vec`; staging `RESET` produces
  `PendingMutation::ResetSlowlog` with the right `command_text()`; read-only refusal at confirm
  (a state-transition test in the same shape as every M2 mutation's refusal test).
- **Golden frames**: the Slowlog screen with entries, empty (`slowlog-max-len` reached zero
  entries, a real state after a fresh `RESET`), and the confirm dialog for `RESET`.
- **Docker-backed integration test**: seed a slow command against a live container
  (`CONFIG SET slowlog-log-slower-than 0` makes every command qualify — a standard technique for
  testing this reliably rather than trying to time a real slow command), read it back via
  `fetch_slowlog`, then `RESET` and prove `SLOWLOG GET` returns empty afterward.

## CONTEXT.md

No glossary entry exists yet for **Slowlog**. Proposed, to be added when this feature's
implementation actually starts:

**Slowlog**:
The server's own ring buffer of commands that took longer than its configured threshold to run —
what `SLOWLOG GET` answers. It is the server's fact, not something this app measures or infers;
the app only reads and, on request, clears it (`SLOWLOG RESET`).
_Avoid_: Query log, slow query list, performance log

## Out of scope

- **Per-entry drill-down** (e.g. jumping from a slow `HSET user:1` entry to that key in the
  browser) — a real nicety, not asked for by R6.4, and it implies a `View` transition back into
  `Keys` with a key pre-selected, which is its own small design question. Revisit once the `View`
  enum exists and this is cheap to add.
- **Configuring `slowlog-log-slower-than`/`slowlog-max-len` from the app** — R6.4 is read-and-reset
  only; changing server config is a different, larger surface this project has no requirement for.
