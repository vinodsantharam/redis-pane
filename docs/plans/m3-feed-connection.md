# M3 task 2: dedicated-connection plumbing for push/poll feeds

Status: **planning — not started.** No code, no ADR. This is infrastructure PLAN.md M3 tasks 4
(Monitor) and 5 (Pub/Sub) both depend on and neither should reinvent — see those two docs for how
they use what this one builds. Slowlog and Dashboard do not need this; they are request/response
(`SLOWLOG GET`, `INFO`) against the existing connection, same as every M1/M2 read.

## Context

PLAN.md M3 row 2: "Dedicated-connection plumbing for push/poll feeds: a second `fred::Client` (or
equivalent) the shell can hand to Monitor/Pub-Sub without starving the main read/write path ·
Proves: the main connection keeps answering ordinary reads/writes while a feed connection is open;
closing the feed view tears down its connection cleanly."

Every read and write built through M0–M2 shares one `fred::Client` (`crates/app/src/redis/mod.rs`,
`connect_with`), matching ADR-0005's one-Connection-per-process rule and the `CLIENT TRACKING`
arming that connection carries for the Open key (ADR-0006). `MONITOR` and `SUBSCRIBE`/`PSUBSCRIBE`
do not fit on that connection:

- **`MONITOR`** puts a connection into a special mode where it streams every command the server
  executes and accepts no further commands of its own until the connection is closed or reset.
  Issuing it on the main connection would freeze every read and write for as long as Monitor stays
  open — the value pane, the key list, TTL refetches, everything.
- **`SUBSCRIBE`/`PSUBSCRIBE`** similarly puts RESP2 clients into a restricted mode; RESP3 (this
  app's floor, ADR-0007) relaxes that to allow ordinary commands to interleave with push messages
  on the same connection, but a subscribed connection still owns whatever channels it subscribed
  to for its own lifetime, and mixing that lifecycle with the Open key's `CLIENT TRACKING` arming
  on the same connection is a needless coupling between two features that have nothing to do with
  each other.

So both features want a second connection, opened only while their view is open, closed when it
is not. This task is that plumbing, built once, generic over "a feed connection that streams
`Msg`s into the app," so Monitor and Pub/Sub each add only what is specific to their own source.

**This does not change ADR-0005.** One Connection per process still means one *logical* target,
resolved once at launch, with no switcher and no second `--profile`. A feed connection is a second
TCP socket to the *same* resolved target, invisible to the reader as a "connection" in the
CONTEXT.md sense — it is transport plumbing behind Monitor/Pub-Sub's Viewer, not a second
Connection the title bar would need to describe. Worth a line in ADR-0005's Consequences when this
lands, so a future reader does not mistake it for the thing that ADR rejected.

## Architecture

### What lives in the shell (`crates/app/src/redis/`)

A new module, `crates/app/src/redis/feed.rs`, alongside `read.rs`, `scan.rs` and `mutate.rs`:

- `pub async fn open_feed(url: &str, kind: FeedKind) -> Result<FeedHandle, ConnectError>` —
  resolves credentials and dials a second `fred::Client` against the same target
  `connect_with` (`crates/app/src/redis/mod.rs`) already resolves, reusing `resolve()`'s output
  rather than re-parsing flags. `FeedKind` is `Monitor` or `Subscribe { channels, patterns }` for
  the first call; Pub/Sub's own task can widen it (`Subscribe`/`Unsubscribe`/`PSubscribe` as the
  reader edits the subscription list) without touching Monitor's arm.
- The handle owns a `tokio::task` that reads the feed connection in a loop and sends translated
  `Msg`s (`Msg::MonitorLine { .. }` / `Msg::PubSubMessage { .. }`, see each feature's own doc for
  the exact shape) into the same `mpsc::Sender<Msg>` `terminal.rs`'s event loop already uses to
  push `Msg`s into `update()`. One channel, one place the core's inbox is fed from — a feed
  connection is not a second source of truth about the app's own state, only another producer of
  events into the loop that already exists.
- `FeedHandle` carries a `tokio_util::sync::CancellationToken` (the same primitive
  `terminal.rs`/`crate::redis::scan` already use for `Esc`-cancellable scans) so closing the view
  tears the task and the socket down deterministically — `Command::CancelScan`'s sibling is
  `Command::CloseFeed` (see below), and the shell's handler is `token.cancel(); drop(client)`,
  nothing cleverer.
- Reconnect is **not** this task's job to build generically. Monitor and Pub/Sub each decide their
  own story for "the feed connection dropped" (a banner + manual re-open is enough for M3; neither
  needs the main connection's exponential-backoff machinery, ADR-0009, reused wholesale) — this
  doc only guarantees that opening and closing a feed connection is cheap and clean enough that
  either feature can build that on top without inventing socket-lifecycle code twice.

### What lives in the core (`crates/core/src`)

The core never opens a socket (CLAUDE.md: "the render loop never does I/O" — a feed connection is
exactly the kind of I/O that rule exists to keep out of `update()`). What it needs:

- `Command::OpenFeed { kind: FeedKindMsg }` and `Command::CloseFeed` in `crates/core/src/command.rs`
  — new variants on the existing non-exhaustive-by-design `Command` enum (it is deliberately *not*
  `#[non_exhaustive]`, per the doc comment already on it, so every shell match over `Command` fails
  to compile until it handles these). `FeedKindMsg` is a core-only description (`Monitor` /
  `Subscribe(Vec<ChannelPattern>)`) with no `fred` types in it, mirroring how `Command::ReadKey`
  names a key by bytes rather than by a shell-side handle.
- `Msg::FeedOpened` / `Msg::FeedClosed { reason: Option<String> }` in `crates/core/src/msg.rs`, so
  the core can render "connecting…" / "live" / "feed closed: `<reason>`" without inferring
  connection state from the presence or absence of other messages.
- A small piece of `State` both Monitor and Pub/Sub read: not a `Vec<FeedHandle>` (the core holds
  no handles — it cannot, they contain `fred` types) but a `FeedStatus` enum
  (`Idle` / `Connecting` / `Open` / `Closed { reason }`) that each feature's own state
  (`state.monitor`, `state.pubsub` — see their own docs) embeds. `update/mod.rs` gains a small
  `feed.rs` file, matching the existing one-topic-per-file shape of `update/link.rs`,
  `update/scan.rs` etc., holding the `Msg::FeedOpened`/`Msg::FeedClosed` handlers shared by both.

### Sequence

1. Reader presses `g m` (or `g p`) with no feed open. `update()` emits `Command::OpenFeed`.
2. Shell's dispatch loop (`terminal.rs`, the same `match` that already turns `Command::ReadKey`
   into a `redis::read` call) calls `redis::feed::open_feed`, stores the returned `FeedHandle` in
   shell-local state (never in `State` — the core owns no `fred::Client`), and sends
   `Msg::FeedOpened` back in.
3. The feed task streams `Msg`s in; `update()` folds each into the feature's own bounded buffer
   (see Monitor's and Pub/Sub's docs for the cap).
4. Reader leaves the view (`Esc`, or `g` to somewhere else). `update()` emits `Command::CloseFeed`.
   The shell cancels the token, drops the client, drops the handle.
5. A silent drop (server-side close, network blip) surfaces as `Msg::FeedClosed { reason }` from
   the feed task's read loop erroring out, exactly like `Msg::ConnectionLost` does for the main
   connection today (`crates/core/src/update/link.rs`).

## CLAUDE.md rules this binds

- **The render loop never does I/O.** The feed task is a `tokio::task`, not a call inside
  `update()` or the render path; `update()` only ever returns `Command::OpenFeed`/`CloseFeed` and
  reacts to `Msg`s already translated by the shell.
- **Every in-flight operation must be cancellable (`Esc`).** `CancellationToken` per feed,
  reused from the scan-cancellation pattern rather than invented fresh.
- **The Viewer never caches a value; reads always hit the server.** Doesn't directly apply to a
  push/poll feed (there is no "Open key" here), but the same spirit does: a feed connection is
  never memoized across view-opens keyed by anything — every `g m`/`g p` reopens a fresh
  connection rather than resuming a stale one, so a feed that silently died is never mistaken for
  one that is live.
- **One Connection per process** (ADR-0005) is not violated — see the Architecture section's note
  above — but this is exactly the kind of addition ADR-0005 says to check against before building,
  so its Consequences section gets a line when this lands.

## Files touched

| File | Change |
|---|---|
| `crates/app/src/redis/feed.rs` (new) | `open_feed`, `FeedHandle`, `FeedKind` |
| `crates/app/src/terminal.rs` | dispatch arms for `Command::OpenFeed`/`CloseFeed`; shell-local `Option<FeedHandle>` per feature |
| `crates/core/src/command.rs` | `Command::OpenFeed { kind }`, `Command::CloseFeed` |
| `crates/core/src/msg.rs` | `Msg::FeedOpened`, `Msg::FeedClosed { reason }` |
| `crates/core/src/update/feed.rs` (new) | shared handlers for the two new `Msg` variants |
| `crates/core/src/state/mod.rs` | `FeedStatus` enum, reused by Monitor's and Pub/Sub's own state |
| `docs/adr/0005-one-connection-per-process.md` | a Consequences line noting feed connections and why they don't reopen the question |

## Testing

- **Core unit tests** (`crates/core/src/update/feed.rs`): `Msg::FeedOpened`/`Msg::FeedClosed`
  transition `FeedStatus` correctly regardless of which feature embeds it; a `FeedClosed` while
  idle is a no-op, not a panic (the pattern every other "stale reply" handler in this codebase
  already follows, e.g. `open_pending` token mismatches in `update/viewer.rs`).
- **No golden frames here** — this task renders nothing on its own; Monitor's and Pub/Sub's docs
  own the golden frames for `FeedStatus`'s connecting/open/closed chrome.
- **Docker-backed integration test** (`crates/app/tests/integration.rs`, `#[ignore]`d, per the
  established pattern): open a feed connection and a second ordinary read concurrently, and prove
  the ordinary read completes without waiting on the feed — the literal claim in PLAN.md's "Proves"
  column for this row. A second test: drop the feed connection's socket (kill the container, or a
  `fred` client-side close) and prove `Msg::FeedClosed` arrives rather than the app hanging — this
  is the task-2-scoped analogue of the bug fixed in
  `docs/plans/value-pane-frozen-after-silent-disconnect.md`, worth pinning here for the same
  reason it was worth fixing there.

## Open questions to settle before phase 1

- Does `FeedHandle`'s read loop need its own reconnect-with-backoff, or is "closed, reader presses
  `g m` again to reopen" enough for M3? Leaning toward the latter — simpler, and consistent with
  Monitor/Pub-Sub being secondary views rather than the primary always-on Viewer liveness depends
  on (ADR-0006 is about the Open key, not these).
- One `feed.rs` shared by Monitor and Pub/Sub, or does Pub/Sub's dynamic channel-list (subscribe
  to more channels while the view stays open) want its own `resubscribe` command distinct from
  Monitor's fire-and-forget `MONITOR`? Current lean: `FeedKind::Subscribe` carries the channel/
  pattern list and a `Command::UpdateSubscription` variant is added when Pub/Sub's own task needs
  it, rather than designed speculatively here.
