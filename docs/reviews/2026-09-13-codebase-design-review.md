# Codebase design review — 2026-09-13

**Commit reviewed:** `7ba6c76` (0.1.0-alpha.12, after M2 task 6)
**Lens:** *codebase-design*: deep modules, interfaces, seams, adapters. Judged as a Rust
reviewer would judge a TUI meant to ship as a single static binary.
**Status:** a review, not a spec. It proposes no decision on its own. Any fix that changes connection,
safety or liveness behaviour needs an ADR update in the same change, as CLAUDE.md requires.

---

## 1. Scope and method

**Read:** both crates' public interfaces (`Msg`, `Command`, `State`, `OpenKey`, `PendingMutation`,
`Viewer`, `Clock`); all of `update()` and the editor/confirm paths; the terminal shell
(`terminal.rs::run`); the Redis shell (`redis/mod.rs`, `redis/read.rs`, `redis/scan.rs`);
`main.rs`; ADR-0011; CONTEXT.md; the test layout and CI. For the binary-safety question I also read
fred 10.1.0's source (`src/types/args.rs`, `src/modules/response.rs`).

**Not run:** no build, test or live repro was run for this review. Each finding is marked:

- **CONFIRMED**: the failing path was traced end to end in source.
- **PLAUSIBLE**: the defect is in the code, but the path to it is dormant or depends on the environment.

**Vocabulary** (from the skill, used strictly):

| Term | Meaning here |
|---|---|
| **Module** | Anything with an interface and an implementation: a fn, a type, a crate |
| **Interface** | Everything a caller must know: types, invariants, ordering, error modes |
| **Depth** | Behaviour a caller gets per unit of interface they must learn |
| **Seam** | Where an interface lives; where behaviour can change without editing the caller |
| **Adapter** | A concrete thing filling a seam. One adapter is a hypothetical seam; two is a real one |
| **Leverage / Locality** | What callers gain from depth / what maintainers gain from depth |
| **Deletion test** | Delete the module. If complexity vanishes it was a pass-through; if it reappears in N callers it was earning its keep |

**Severity scale:**

| Severity | Meaning |
|---|---|
| **Critical** | Wrong behaviour today that breaks a founding invariant (ADR-0006 liveness, R7.4 honesty) |
| **High** | A design flaw that makes the next feature expensive, or that keeps an invariant alive only through comments |
| **Medium** | Local shallowness, swallowed errors, a missing gate |
| **Low** | Hygiene |

---

## 2. Summary

| ID | Sev | Claim | Anchor | Verdict |
|---|---|---|---|---|
| C1 | Critical | Key identity is lossy UTF-8 once it crosses into `Msg`/`State`; Refetch and writes target the wrong key | `terminal.rs:340`, `update.rs:755`, `update.rs:1323` | CONFIRMED |
| C2 | Critical | Collection reads decode members as `String`; one non-UTF-8 element fails the whole read | `redis/read.rs:177–327` | CONFIRMED |
| H1 | High | The mutation pipeline is shallow: one new mutation touches six places | `terminal.rs:392–599`, `state/mod.rs:347` | CONFIRMED |
| H2 | High | `State` invariants live in doc comments; every field is `pub` across the crate boundary | `state/mod.rs:403`, `command.rs:15` | CONFIRMED |
| H3 | High | Whether to arm tracking is decided in two places that can disagree | `terminal.rs:183, 757–783` | PLAUSIBLE (latent) |
| H4 | High | The shell bypasses the injected clock; `at_ms: 0` survives; `Clock`'s contract is wrong | `terminal.rs:318, 798`, `clock.rs:10` | CONFIRMED |
| M1 | Medium | `server_conditions` fails open: an unreadable `INFO` loses the `replica` guard | `redis/mod.rs:180` | CONFIRMED |
| M2 | Medium | `update.rs` has poor locality; mode precedence is implicit | `update.rs:561–995` | CONFIRMED |
| M3 | Medium | Type knowledge leaks past the `Viewer` trait into update/editor/copy | `update.rs`, `state/editor.rs` | CONFIRMED |
| M4 | Medium | Wrap width is computed by the shell at read time and goes stale | `terminal.rs:721` | CONFIRMED |
| M5 | Medium | A second, test-only read path contradicts "one read path" | `redis/mod.rs:257` | CONFIRMED |
| M6 | Medium | `read_value` makes 4–6 serial round trips and swallows errors | `redis/read.rs:158–327` | CONFIRMED |
| M7 | Medium | CI never runs the 52-test integration suite that guards the re-arm invariants | `.github/workflows/ci.yml` | CONFIRMED |
| M8 | Medium | Startup flow is duplicated between `probe()` and `main()` | `main.rs:87–205` | CONFIRMED |
| L1–L5 | Low | Misplaced doc, empty module, widget in `State`, argument sprawl, empty version | §6 | CONFIRMED |

---

## 3. What is already deep — keep it

This section is here so the fixes below don't break what already works. Several parts of this
codebase are better than most Rust TUIs.

- **The core/shell seam is real and enforced mechanically.** `redis-pane-core` cannot reach
  `tokio`, `fred` or `crossterm`, and the check is a CI job rather than a review convention. This is
  the most valuable property in the repository. Every fix below keeps it.
- **`update(State, Msg) -> (State, Vec<Command>)` is a deep module.** Its interface is tiny: one
  function, two enums. Behind it sits the whole product, and the ~200 unit tests cross exactly that
  seam instead of reaching into helpers. *The interface is the test surface* holds here.
- **`Liveness` is derived and cannot be set.** `State::liveness()` is a pure function of `Link` and
  `Tracking`. `Live` is only reachable through `Msg::TrackingArmed`. That turns ADR-0009 into a type
  property.
- **`ReadToken` supersession.** One monotonic identity per read defeats both "slow reply lands
  last" and "open A, B, A again". The doc comment on `ReadToken` explains why a name is not enough.
- **`LoadedSet`** has a byte arena, parallel metadata arrays, a permutation index, and the cap
  enforced in exactly one place (`scan_batch`). Callers get `push`/`name`/`kind` and never see the
  columnar layout, so this is deep.
- **`Command` is deliberately not `#[non_exhaustive]`**, so a new command fails the shell's `match`
  at compile time. That is the correct use of exhaustiveness in a workspace that owns both sides.
- **`ReadGate` plus pipelining `CLIENT CACHING YES` with `TYPE`.** It is a subtle concurrency
  invariant, verified against a real server and explained where it lives.
- **The fred traps are written down** (`Options { caching }` is inert; `del(Vec<u8>)` is
  elementwise), and each has an integration test. That is how to use a library that compiles clean
  and lies.
- **The keyspace source abstracts over a stream, not a cursor** (`redis/scan.rs`). Cluster can
  replace the implementation without changing `Msg::ScanBatch`.

---

## 4. Critical

### C1 — Key identity is lossy once it crosses into `Msg` and `State`

**Evidence.** The same key is represented by two types, depending on which side of the seam you
are on:

| Where | Type |
|---|---|
| `LoadedSet::name`, `Command::OpenKey.name`, all mutation `Command`s | `&[u8]` / `Vec<u8>` (exact) |
| `Msg::ValueLoaded.name`, `Msg::ValueGone.name`, `Msg::KeyDeleted.name`, `Msg::ValueSet.name`, `Msg::NotWritten.name` | `String` |
| `OpenKey.name` (`state/open.rs:151`), `PendingRead.name` (`state/open.rs:78`) | `String` |

The shell builds those `String`s with `String::from_utf8_lossy` (`terminal.rs:692, 701`), and the
core fills `PendingRead.name` from `LoadedSet::name_str`, which is lossy too (`update.rs:841, 904`).
Bytes are then **re-derived from the lossy string** and sent back to the server:

- `terminal.rs:340`: `Command::RefetchOpenKey` reads `open.name.as_bytes().to_vec()`. That is every
  `r`, every invalidation-driven Refetch, and every Refetch after a write.
- `update.rs:755`: `DeleteHashField { name: open.name.clone().into_bytes(), .. }`.
- `update.rs:1323`: `stage_editor` builds `SetString`/`SetHashField`/`AddHashField` from
  `open.name.clone().into_bytes()`.

**Failure scenario** (any key that is not valid UTF-8, e.g. `\xff\xfe:session`, which is common for
binary-prefixed or packed keys):

1. `→` opens it. `Command::OpenKey` carries the exact bytes, so the first read is correct.
2. `OpenKey.name` becomes `"\u{FFFD}\u{FFFD}:session"`.
3. The next Refetch (`r`, or the invalidation push) reads `EF BF BD EF BF BD 3A …`. That key does
   not exist, so `TYPE` returns `none` and the core receives `Msg::ValueGone`. The Viewer badges a
   live key as **`✕ deleted`**.
4. The Refetch armed `CLIENT CACHING YES` on the *wrong* key. The real key is no longer tracked, so
   the Viewer is dark. After the wrong-key read the header shows `● live`: this is the exact
   RedisInsight failure ADR-0006 exists to rule out.
5. The name-guarded writebacks (`state.keys.name(index) == Some(name.as_bytes())`, at
   `update.rs:296, 352, 392, 1534`) never match, so the row never gets its metadata or tombstone.
6. Editing it stages `SET <wrong bytes> … XX`. That fails safe thanks to `XX`, but it is reported as
   "key no longer exists".

**Why it matters, in design terms.** Identity is part of the `Msg` interface, and the interface
disagrees with itself. The core's `Command` side is exact and its `Msg` side is lossy, so every
handler that compares the two is correct only for UTF-8 keys. Nothing documents that restriction,
and no test covers it: every fixture key is ASCII.

**Recommendation.** Make identity one type, decode lossily in exactly one module (render), and
never re-derive bytes from a display string.

```rust
// crates/core/src/key.rs
/// A Redis key, exactly as the server knows it. Never lossy.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct KeyName(Box<[u8]>);

impl KeyName {
    pub fn as_bytes(&self) -> &[u8] { &self.0 }
    /// For display only. The one place a key becomes text.
    pub fn display(&self) -> std::borrow::Cow<'_, str> { String::from_utf8_lossy(&self.0) }
}
impl std::fmt::Debug for KeyName { /* lossy + escaped */ }
```

Use `KeyName` in every `Msg`, every `Command`, `OpenKey`, `PendingRead` and `PendingMutation`.
Delete `LoadedSet::name_str` outside render. Hash *field* names get the same treatment once C2 is
fixed.

**Prove it.** A unit test at the `update()` seam: open a key named `b"\xff\xfe"`, then send
`Msg::Invalidated`. Assert the resulting `Command::RefetchOpenKey` resolves to `b"\xff\xfe"`.
Today that is impossible to assert, because the command carries no name, which is H3's point too.
Add an integration test that opens, invalidates and re-reads a non-UTF-8 key and expects `● live`,
not `✕ deleted`. For a manual repro, run `./scripts/redis-up.sh cli` then `SET "\xff\xfe" hello`,
open the key, and press `r`.

---

### C2 — Collection reads are not binary-safe

**Evidence.** `redis/read.rs` decodes every collection member as `String`:

- `lrange` → `Vec<String>` (`:177`)
- `zrange` → `Vec<(String, f64)>` (`:193`)
- `HSCAN` → `Vec<String>` (`:237`)
- `SSCAN` → `Vec<String>` (`:276`)
- `xrevrange` → `Vec<(String, Vec<(String, String)>)>` (`:324`)

In core, `PairValue`, `IndexedValue`, `MemberValue`, `ScoredValue` and `StreamValue` store `String`.

fred 10.1.0 converts through `Value::into_string`, and for `Value::Bytes` that is
`String::from_utf8(b.to_vec()).ok()` (`fred/src/types/args.rs:885`). `FromValue for String` turns
the `None` into `Error::new_parse("Could not convert to string.")` (`response.rs:249`).

**Failure scenario.** A Hash with one msgpack- or protobuf-encoded field (routine for session
stores and job queues). `read_value` returns `Err`, the core shows `reading <key>: Could not convert
to string.`, and the Viewer shows nothing: the whole value is refused because of one cell. Strings
get this right already (`string_value` falls back to `Value::Binary`), so the gap is specific to
collections.

In a stream, `xrevrange(...).unwrap_or_default()` (`:327`) makes it worse: the parse error is
**swallowed** and the stream renders as **empty**. That breaks R7.4.

**Why it matters.** The `Viewer` trait is deep: one frame and one navigation for every type. But
the data behind it has a narrower domain than Redis itself, and neither the interface nor the error
says so.

**Recommendation.**

```rust
pub struct PairValue   { pub pairs:   Vec<(Bytes, Bytes)>, pub total: usize }
pub struct IndexedValue { pub items:  Vec<Bytes>,          pub total: usize }
// ...and so on for every collection; `Bytes` = `Box<[u8]>` or `bytes::Bytes` (already in fred's tree).

/// One cell rule for every type: UTF-8 shown as text, anything else as escaped hex.
fn cell(b: &[u8]) -> String { /* ... */ }
```

Decode with `Vec<Vec<u8>>` / `Vec<(Vec<u8>, f64)>` on the fred side, which is binary-safe. Keep
`looks_like_json` and the edit path UTF-8-only, and refuse editing a binary field with a notice, the
way `Value::Binary` is refused today. Replace `stream_value`'s `unwrap_or_default` with `?`.

**Prove it.** Integration tests: `HSET h f "\x80"`, `RPUSH l "\xff"`, `XADD s * f "\xfe"`. Each must
open and render a hex cell.

---

## 5. High

### H1 — The mutation pipeline is shallow and spread across six places

**Evidence.** Adding one mutation, as M2 task 6 did three times, means editing:

1. `PendingMutation`: a variant plus arms in `command_text`, `guard_text`, `json_warning` and
   `into_commands` (`state/mod.rs:215–372`)
2. `Command`: a variant (`command.rs`)
3. `terminal.rs::run`: a ~50-line spawn arm (`:392–599` holds five of them, almost line for line)
4. `redis/mod.rs`: an `async fn` and its outcome enum (`FieldWrite`, `FieldAdd`)
5. `Msg`: one or more outcome variants (`ValueSet`, `NotWritten`, `KeyDeleted`, `HashFieldAlreadyGone`)
6. `update.rs`: the handler(s)

**Deletion test on `PendingMutation::into_commands`.** Delete it and nothing gets harder: it maps
each variant 1:1 onto a `Command` variant with the same fields. It is a pass-through, and the
`Command` mutation variants are a second copy of `PendingMutation`.

**Deletion test on the five `run` arms.** Delete them and the same complexity (clone client, clone
tx, stamp time, lossy name, match outcome → `Msg`, format the failed command) reappears five
times. So the behaviour is real, but it lives in callers rather than in a module.

Their failure text (`format!("HSET {name_str} {field_str}")`) duplicates
`PendingMutation::command_text`. Two sources for "the command we ran" will drift apart, and R7.4
depends on that text being right.

`run` itself is ~500 lines with eight locals of shell state (`client`, `tracking`, `arming`,
`scan_cancel`, `read_gate`, `reconnect_cancel`, `reconnect_attempt`, `reconnected_rx`). Hence
`#[allow(clippy::too_many_arguments)]` on `open_key` and `spawn_reconnect_attempt`.

**Why it matters.** The chokepoint promised in CLAUDE.md ("mutations flow through one path") exists
in the core. It does not exist in the shell, where each mutation has its own path. M3 and M4 will
add list, set, zset and TTL mutations, so the six-place cost grows linearly.

**Recommendation.** Deepen the shell side into one module with a small interface. Make the core
send *what* to do, and let the outcome come back through one message.

```rust
// core: the staged thing and the executed thing are the same value.
pub enum Mutation {
    DeleteKey { key: KeyName },
    SetString { key: KeyName, new: Bytes },
    SetHashField { key: KeyName, field: Bytes, value: Bytes },
    AddHashField { key: KeyName, field: Bytes, value: Bytes },
    DeleteHashField { key: KeyName, field: Bytes },
}
impl Mutation { pub fn command_text(&self) -> String { /* the one source */ } }

pub struct PendingMutation { pub mutation: Mutation, pub preview: Preview /* old, was_json, last_field, row */ }

pub enum Command { /* … */ Execute(Mutation) }

pub enum MutationOutcome { Done, KeyGone, FieldGone, FieldExists, NothingToRemove }
pub enum Msg { /* … */ MutationSettled { mutation: Mutation, result: Result<MutationOutcome, String>, at_ms: u64 } }
```

```rust
// app: crates/app/src/redis/mutate.rs, one deep module.
pub async fn execute(client: &Client, m: &Mutation) -> Result<MutationOutcome, fred::error::Error>;
```

The `run` arm becomes one line: spawn `execute`, then send `MutationSettled`. Keep the Lua scripts
and fred-trap notes inside `mutate.rs`, where they are an internal seam.

For the rest of `run`, move its locals into `struct Shell { client, tx, arming, read_gate,
scan_cancel, reconnect: Reconnector, clock }` with `fn handle(&mut self, cmd: Command)`. That makes
the loop testable without a terminal and removes both `too_many_arguments` allows.

**Prove it.** Integration tests call `mutate::execute` directly (they already drive
`redis::set_value` and friends). The core's tests replace ~6 outcome-message tests with one
`MutationSettled` handler matrix. Replace-don't-layer: delete the per-variant ones.

---

### H2 — `State` invariants are enforced by doc comments

**Evidence.**

- `State` has 26 `pub` fields (`state/mod.rs:403–482`) and `OpenKey` has 16. `redis-pane` is a
  *different crate*, so every one is writable from the shell. `main.rs:217` builds
  `State { connection, read_only, condition, tree_mode: true, ..State::default() }` directly.
- `ReadToken(pub u64)` (`command.rs:15`). `State::read_token`'s doc says: *"the core is the only
  thing that mints one — a shell that could invent a token could resurrect a superseded read."*
  The shell can: `ReadToken(n)`.
- `OpenKey.offset`: *"never written to directly outside of that"*. It is `pub`.
- `OpenKey.editing: bool` and `OpenKey.editor: Option<EditBuffer>` form one state machine spread
  over two fields and `EditBuffer::is_staged()`. The allowed combinations (not editing; typing;
  staged under the dialog; `SET` in flight with the buffer on screen; `SET` in flight with no
  buffer) are spelled out in prose (`open.rs:180–193`). Keeping them consistent is why there are
  `drop_staged_buffer`, `unstage_buffer` and `clear_editing`, plus seven `expect(..)` calls in
  `update.rs`'s production code (six of them `"checked above"`).
- `OpenKey.cursor` is *"meaningless otherwise"* when `cursor_active` is false.

**Why it matters.** This project's standard is "make the wrong claim unrepresentable": `Liveness`
has no setter and `Link::Reconnecting.retry_in_ms` is an `Option` for that reason. The same
standard isn't applied to the editor or the token. Each invariant above has already produced a fix
(the doc comments are incident reports). A type would have caught them at compile time.

**Recommendation.**

```rust
// Tokens: minted only in core.
pub struct ReadToken(u64);            // private field
impl ReadToken { pub fn get(self) -> u64 { self.0 } }   // for logs, if needed

// One enum instead of two flags that must agree.
pub enum EditPhase {
    Idle,
    Typing(EditBuffer),
    Staged(EditBuffer),        // under the confirm dialog
    Saving { shown: Option<EditBuffer> }, // SET in flight
}
pub struct OpenKey { /* … */ pub(crate) edit: EditPhase, pub(crate) cursor: Option<usize> }

// Construction the shell can't get wrong.
pub struct Startup { pub connection: Connection, pub read_only: Option<ReadOnlyReason>,
                     pub condition: Option<ServerCondition>, pub tree_mode: bool }
impl State { pub fn new(s: Startup) -> Self; }
```

Make fields `pub(crate)` and give render read accessors. Render lives in core, so it keeps full
access. The shell needs only `State::new`, `keys().name(i)` (for `FetchMetadata`) and `open()`. If
H3 is fixed, it needs even less.

**Prove it.** It is mostly compile-time: any shell code that mints a token, or writes a field, stops
compiling. `R3.8` tests (the "held while editing" family) keep passing unchanged, because they go
through `update()`.

---

### H3 — Whether to arm tracking is decided in two places

**Evidence.** The core owns `Link::Up { tracking: Tracking }` and derives `Liveness` from it. The
shell separately owns `tracking: bool` and `arming: Arming` (`terminal.rs:106, 183`). It updates
them only in the `reconnected_rx` branch (`:251–260`). The fred-level reconnect watcher
(`spawn_link_watchers`, `:757–783`) re-probes tracking and sends `Msg::Connected { tracking_supported }`
to the core, **but cannot update the shell's `arming`**: it is a spawned task that owns neither
local.

**Failure scenario.** fred reconnects internally to a server whose capability differs, e.g. a
failover to a managed replica that refuses `CLIENT TRACKING`. The core says `Unsupported` → manual.
The shell still sends `CLIENT CACHING YES` on every read. Upstash-style servers reject that, so
every read fails (the bug `read.rs`'s module doc describes fixing). The opposite direction leaves
reads unarmed while the core waits for a `TrackingArmed` that never comes.

**Dormant today.** The comment at `:751` notes no `ReconnectPolicy` is set, so this watcher does not
fire. It will as soon as someone sets one, and nothing would warn them.

**Why it matters.** The re-arm invariants are *the* invariants of this project. The claim side
lives in the core and is unit-tested. The act side, which is "arm or not", is decided by shell
locals that no unit test reaches. There are two sources of truth with no seam between them.

**Recommendation.** The core decides and the command carries the decision. Then the shell has
nothing to remember, and the core's test suite covers the whole invariant:

```rust
pub enum Command {
    ReadKey { key: KeyName, index: Option<usize>, token: ReadToken, arm: bool },  // replaces OpenKey + RefetchOpenKey
    /* … */
}
```

`arm` is `matches!(state.link, Link::Up { tracking: Tracking::Available | Tracking::Armed | Tracking::Consumed, .. })`.
Merging `OpenKey` and `RefetchOpenKey` also fixes C1's refetch half: the command carries the key
bytes, so the shell no longer rebuilds them from `state.open`. It strengthens ADR-0006's "one read
path" to one read *command*.

**Prove it.** A property-style unit test: for every `Msg` sequence that ends in `Link::Up { tracking:
Available|Armed|Consumed }`, every emitted `ReadKey` has `arm == true`, and for `Unsupported`,
`arm == false`. Also delete the shell's `tracking`/`arming` locals.

---

### H4 — The shell bypasses the injected clock, and the clock's contract is wrong

**Evidence.**

- `run` receives `clock: &dyn Clock`, yet `terminal.rs` computes
  `SystemTime::now().duration_since(UNIX_EPOCH)` inline **nine times** (`:298, 396, 427, 466, 515,
  564, 657, 667, 872`). `SystemClock` in `lib.rs` is a tenth copy.
- `at_ms: 0` survives at `terminal.rs:318` (a metadata fetch failure) and `:798` (connection-level
  errors). `Msg::Noticed`'s doc (`msg.rs:240–251`) records that `at_ms: 0` once made notices
  invisible "for the life of the process" and says the class was fixed. It was fixed for notices.
  Errors don't fade, so the symptom here is smaller: a wrong timestamp, not an invisible message.
  But it is the same defect, and the reason it recurs is that stamping is not a module.
- The `Clock` doc says *"Monotonic, never wall time"* (`clock.rs:10`). Every consumer needs **epoch**
  time: `LoadedSet::set_ttl` stores epoch seconds, and `stream_entry_age` subtracts the epoch-ms
  prefix of a stream ID. `SystemClock` returns wall time, so the implementation is right and the
  interface is wrong. A test that believes the doc and injects a small monotonic reading
  (`FixedClock(1_000)`) against a real stream ID gets `now < id` and `saturating_sub`, so every
  entry reads "just now" and nothing fails loudly.

**Why it matters.** ADR-0011 makes the clock an architectural commitment. As things stand it is
injected into render and nowhere else, so the integration suite cannot pin time in shell-produced
messages. The interface also states the opposite of what callers depend on.

**Recommendation.** Fix the contract (`fn now_epoch_ms(&self) -> u64`; "wall-clock milliseconds
since the Unix epoch; tests pin it"). Store `Arc<dyn Clock + Send + Sync>` in the `Shell` struct
from H1, and make every spawned task stamp through it. Remove the two `at_ms: 0`. With H1 in place
there is one stamping site per message kind rather than nine.

---

## 6. Medium

### M1 — `server_conditions` fails open

`redis/mod.rs:180` runs `client.info(Some(InfoKind::Default)).await.unwrap_or_default()`. If `INFO`
is refused (ACL `-info`, which some managed platforms restrict), `role:` is absent,
`read_only = None`, and **a replica is treated as a primary**. The `replica` Read-only reason is a
safety feature (R1.15, ADR-0009). It should fail *closed*, or at least visibly. Return
`Result<(Option<ReadOnlyReason>, Option<ServerCondition>), Error>`, and on failure surface a
`Msg::Failed { command: "INFO", .. }` so the chrome doesn't imply a primary. Also consider
`ROLE`, which may be allowed where `INFO` isn't.

### M2 — `update.rs` has poor locality

It is 6,583 lines: ~1,850 of production code and ~4,700 of tests in eight `#[cfg(test)]` modules.
`key_press` is ~435 lines. **Mode precedence** (confirm dialog > editor > filter > keymap) is
implemented as a sequence of early returns on `Option` fields (`update.rs:569–588`). It is only
discoverable by reading, and the rule for "which mode is active" is re-derived in render and in
`keymap::pane_is_on_screen`.

Ratatui's own TEA guidance names this cost: a single `update` "grows into a giant match block", and
the remedy is composable sub-updates over state slices, not abandoning TEA. Keep the external seam
(`update()` stays the only public entry, and tests keep crossing it), and deepen the inside:

```text
update/mod.rs      pub fn update(); fn mode(&State) -> Mode   // one precedence rule
update/link.rs     Connected, ConnectionLost, Reconnect*, TrackingArmed, Invalidated
update/scan.rs     Scan*, MetadataBatch, scan_batch (cap stays here)
update/keys.rs     selection, filter, tree fold, sort
update/viewer.rs   ValueLoaded/Gone, cursor, copy
update/editor.rs   open_editor, begin_add_field, editor_key, stage_editor
update/confirm.rs  confirm_key, MutationSettled (after H1)
```

`Mode` should be derived, not stored: `enum Mode { Confirm, Editing, Filtering, Normal }`. Do not
switch to ratatui's component architecture (trait objects with their own state and handlers): it
would break the single `(State, Msg)` seam that makes golden frames and the liveness tests
possible.

### M3 — Type knowledge leaks past the `Viewer` trait

Render respects the abstraction: one `Value::` pattern in `render/mod.rs`. Behaviour does not.
There are 43 `Value::` references in `update.rs`, 22 in `state/editor.rs` and 11 in `state/copy.rs`.
"Is this editable, and what is the edit target" is `if let Value::Hash(pairs) = value` in
`open_editor`, `begin_add_field` and the `Action::Delete` arm.

Today there is one editable collection type, and *one adapter is a hypothetical seam*, so don't add
a trait yet. But the moment a second collection type becomes editable (List `LSET`, ZSet `ZADD`),
add the capability to the value rather than another `match` in update:

```rust
impl Value {
    fn edit_target(&self, cursor: Option<usize>) -> Result<EditTarget, &'static str>;
    fn remove_target(&self, cursor: Option<usize>) -> Result<Mutation, &'static str>;
}
```

Record this as a note in PLAN M3 so the refactor lands with the second adapter, not after the third.

### M4 — Display wrapping is computed by the shell at read time

`terminal.rs:721` has `area_width` = `(term.width / 2).max(20)`, passed to `read_value` and then to
`StringValue::new(text, width)`, which stores pre-wrapped `lines`. This width ignores
`split_adjust`, density (a single pane is full width), borders and gutters. It is fixed at read
time, so resizing or dragging the divider leaves stale wrapping until the next read. `StringValue`
has to keep `raw` alongside `lines` because of this (`value.rs:113–127`).

Wrapping is a render concern. Store only the text in `Value::Str`, and wrap in render against the
actual `Rect`. Cache per `(width, generation)` if profiling shows a need. That removes a terminal
dimension from the Redis shell's interface.

### M5 — A second, test-only read path

`redis::refetch_and_rearm(client, key: &str) -> Result<Option<String>, Error>`
(`redis/mod.rs:257`) is used **only** by `tests/integration.rs` (5 call sites). Its own doc says
*"There is no sibling function that reads without arming, and there should never be one"*. It isn't
the production path (`read::read_value`), it takes `&str` (not binary-safe), and it reads with `GET`
only. The tracking tests therefore verify arming on a function the app never calls.

Point the tests at `read::read_value(.., Arming::Enabled)` and delete `refetch_and_rearm`. Also
delete `probe_tracking_public`: it wraps a private fn one-for-one, so make `probe_tracking` `pub`.

### M6 — `read_value` makes 4–6 serial round trips and swallows errors

After the `CLIENT CACHING` + `TYPE` pipeline, `read_value` awaits `TTL`, `MEMORY USAGE`, the
length command (`HLEN`/`LLEN`/`SCARD`/`ZCARD`/`XLEN`) and the data command **one at a time**. On a
bastion-hop link at 40ms RTT, that is ~200ms per open on a path that must feel instant, sitting
right at the `APPEAR_DELAY_MS = 200` loading gate.

Four of those calls use `.unwrap_or(..)`: TTL defaults to `-1` ("no expiry"), memory to `0`, length
to `0`, and stream entries to empty. A failure there becomes a confident wrong value. That is what
CLAUDE.md calls a swallowed error ("indistinguishable from the app deciding to do nothing").

Pipeline `TTL` + `MEMORY USAGE` + length + data after the arming pipeline. That is safe: the arming
already landed on `TYPE`. Map any per-reply error to `Msg::Failed`, or to an explicit "unknown" cell
where a partial result is still worth showing.

### M7 — CI never runs the integration suite

All 52 integration tests in `crates/app/tests/integration.rs` are `#[ignore]`, and CI runs
`cargo test --workspace`. CLAUDE.md still says "24 tests", which is stale and should be corrected in
the same change. CLAUDE.md calls the
two re-arm invariants "the invariant most likely to rot silently" and says *"only these prove the
shell actually arms"*. Nothing proves it automatically, and C1 and H3 are both shell-side liveness
defects that unit tests cannot see.

GitHub-hosted Ubuntu runners have Docker. Add a job that runs `cargo test -p redis-pane -- --ignored
--test-threads=1` on `schedule` (nightly) and on PRs labelled `shell`, or that touch
`crates/app/src/redis/**` or `terminal.rs`.

### M8 — Startup flow is duplicated

`main.rs` has `probe()` (`:87–131`) and the main path (`:187–205`), which each build a runtime,
call `connect_with`, and map `BelowFloor|NoResp3 → UNSUPPORTED_SERVER`, else `CONNECTION`. The
`target · env · source` line is formatted three times (`:99, :172`, and `startup_failure`). Extract
`fn connect_or_exit(&Runtime, &Resolution) -> (Client, Established)` and a `Connection::readout()`.
It is small, but this is the one place where exit codes, which are an interface, are defined twice.

---

## 7. Low

- **L1 — Misplaced doc comment.** The paragraph *"There is deliberately no setter for this. Liveness
  is derived…"* sits on `read_only_liftable` (`state/mod.rs:535–545`) instead of on `liveness()`
  (`:705`). Rustdoc attaches it to the wrong item, and the most important invariant in the type is
  undocumented where it is enforced.
- **L2 — Empty public module.** `crates/app/src/state_file.rs` is a doc comment and nothing else,
  exported as `pub mod state_file`. It is a hypothetical seam. Either implement it (ADR-0003) or
  remove it, and let `split_adjust`'s doc point at the ADR instead.
- **L3 — A widget inside `State`.** `EditBuffer` holds `ratatui_textarea::TextArea<'static>`, so
  `State: Clone + PartialEq` depends on a third-party widget's `Clone`/`PartialEq` semantics,
  including cursor, undo history and style. That is allowed under ADR-0011 (render lives in core),
  and any `assert_eq!` on a `State` with an open editor compares undo stacks and cursor styling.
  Keep an eye on it when `ratatui-textarea` is bumped.
- **L4 — Argument sprawl.** Five `#[allow(clippy::too_many_arguments)]`: two in `terminal.rs`
  (fixed by H1's `Shell`), three in render (`value_pane`, and two in `render/keys.rs`). A
  `struct RenderCtx<'a> { state: &'a State, theme: &'a Theme, now_ms: u64 }` passed alongside
  `(area, buf)` fixes the render side. Reading the clock once per frame is also more correct than
  each widget reading it separately.
- **L5 — Empty version.** Startup (`terminal.rs:173`) and the reconnect watcher (`:773`) send
  `Msg::Connected { version: String::new() }`, so `Link::Up.version` is empty for the whole
  session unless a full reconnect happens. `Established.version` is already known at startup, so
  pass it through, or remove `version` from `Link` if nothing renders it.

---

## 8. Suggested order of work

Ordered by blast radius and by what each fix unblocks, not strictly by severity:

| Step | Findings | Why this order | Docs to touch |
|---|---|---|---|
| 1 | **C1 + H3** together | `ReadKey { key: KeyName, arm }` fixes the lossy refetch *and* moves arming into the core in one change; both are liveness | ADR-0006 (one read *command*), CONTEXT.md (key identity) |
| 2 | **C2** | Independent of 1; changes `Value` shapes, so do it before more Viewers land | PRD R3.x note on binary cells |
| 3 | **M7** | Gate the shell before the refactors below, so they are caught if they regress arming | CLAUDE.md "Commands" |
| 4 | **H1** (+ L4 shell half, H4) | Needs `KeyName` from step 1; lands before M3/M4 add mutations | CLAUDE.md "Mutations flow through one path" |
| 5 | **H2** | Mechanical once H1 has shrunk the shell's reach into `State` | none (types only) |
| 6 | **M1, M6, M5, M8** | Local, independent, small | ADR-0009 (INFO failure mode) |
| 7 | **M2, M4** | Internal restructure; only safe once the above have settled the seams | PLAN §2 module layout |
| — | **M3** | Deferred until a second editable collection type exists | PLAN M3 note |

---

## Sources

- Ratatui, *The Elm Architecture (TEA)*: https://ratatui.rs/concepts/application-patterns/the-elm-architecture/
- Ratatui, *Component Architecture*: https://ratatui.rs/concepts/application-patterns/component-architecture/
- fred 10.1.0 source: `src/types/args.rs` (`Value::into_string`), `src/modules/response.rs`
  (`impl FromValue for String`)
- Michael Feathers, *Working Effectively with Legacy Code* (seams); John Ousterhout, *A Philosophy of
  Software Design* (deep modules). The skill's glossary deliberately uses depth-as-leverage rather
  than Ousterhout's line ratio.
