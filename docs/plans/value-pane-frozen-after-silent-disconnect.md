# Fix: value pane wedges after a silent disconnect (laptop sleep / idle drop)

## Context

Reported symptom: open a key, navigate into its value, step away from the
terminal (the scenario strongly suggests laptop sleep/resume, an SSH
session going idle, or a NAT/LB silently dropping an idle TCP connection),
come back to find the app says reconnected, but the value pane's cursor is
frozen. `Esc` does nothing. Only `q` (killing the whole process) recovers
it.

## Root cause

Two independent gaps combine to produce this. Both are shell-side
(`crates/app`), not core — the core/shell split (ADR-0011) means the
functional core (`crates/core`) already has correct, tested behaviour here
(`crates/core/src/update/link.rs`'s `liveness_invariants` tests prove the
re-arm-on-reconnect invariant at the state-machine level). The bug is that
the shell never gets to tell the core what actually happened, because the
shell itself hangs.

**1. No connection timeout/heartbeat anywhere (`crates/app/src/redis/mod.rs:109-157`, `connect_with`).**
The `fred::Client` is built from `Builder::from_config(config).build()` with
every timing default left untouched — no `ConnectionConfig` override, no
`CLIENT` keepalive, no periodic `PING`. Detecting a dead connection relies
entirely on the OS/fred surfacing an IO error, which only happens on an
actual read/write attempt hitting a closed/reset socket. A connection that
is silently black-holed (laptop sleep, idle NAT/LB timeout with no RST) is
never proactively noticed — the first thing to touch that socket after
resume is whatever command the app happens to send, and it can block
indefinitely with no error and no timeout.

**2. That hang wedges *every future read*, forever (`crates/app/src/redis/read.rs`'s `ReadGate`/`ReadPermit`).**
Every `Command::ReadKey` (opening a key, `Enter` into a value, a manual
`r` Refetch, and the auto-Refetch tracking invalidations trigger) goes
through `ReadGate::begin()` → `ReadPermit::run()`
(`crates/app/src/redis/read.rs:88-124`). `run()` takes a shared
`tokio::sync::Mutex<()>` guard (`self.lock.lock()`) and only then, in a
documented and deliberate design choice, refuses to cancel the read once it
starts (comment at read.rs:108-109: *"the read completes — interrupting it
mid-way is what would leave the connection armed for a key nobody is
looking at"*). If `read.await` never resolves (case 1), the `_guard` is
never dropped. Every subsequent call to `ReadPermit::run()` — including the
reconnect's own `refetch()` (`crates/core/src/update/link.rs:7-30`'s
`connected()`, which is otherwise exactly the mechanism that is supposed to
restore the Viewer) — blocks forever on `self.lock.lock()` too. The read
pipeline is permanently deadlocked by one stuck read.

This explains every observed detail:
- **Esc does nothing to the value pane**: `Action::Cancel` → `cancel()`
  (`crates/core/src/update/mod.rs:434-461`) only ever emits
  `Command::CancelScan` for an in-flight `SCAN`; there is no
  `Command::CancelRead` at all (confirmed: `Command` in
  `crates/core/src/command.rs` has no such variant). Nothing in the core's
  vocabulary can even ask the shell to abandon a stuck read — and the core
  doesn't know one is stuck; the core already cleared `open_pending` in
  `connection_lost()` (`link.rs:32-47`) if `Msg::ConnectionLost` ever fired.
  So the "stuck" state is invisible to the core. Esc's own logic (pop focus
  back to Keys, exit cursor mode) still *runs* — it's just that no read will
  ever land again, so nothing the reader does moves the Viewer forward.
- **Reconnect appears to succeed but nothing refreshes**: if the dead
  socket is noticed at all (e.g. a separate pubsub/tracking connection
  errors first), `Msg::Connected` fires and the core dutifully issues a
  Refetch (`Command::ReadKey`) — which then queues behind the permanently
  held `ReadGate` mutex and never runs.
- **Only `q` recovers it**: quitting drops the whole tokio runtime, freeing
  the wedged task and mutex. Nothing short of process exit can release it.
- **Error classification makes it worse even when an error does surface**
  (`crates/app/src/terminal.rs:599-621`, `Shell::watch_link`'s `error_rx`
  handling): only `fred::error::ErrorKind::IO` is mapped to
  `Msg::ConnectionLost`; every other error kind becomes a plain
  `Msg::Failed` notification and the link state stays `Up`/`Armed`. If a
  future fix makes fred surface an unresponsive-connection error under a
  different `ErrorKind` (e.g. `Timeout`), it would silently fail to trigger
  reconnect/re-arm unless this match is widened too — the header would go on
  claiming `● live` over a connection that just proved itself dead.

## Fix

Both parts are shell-only; no core changes are needed (the core's
reconnect/re-arm state machine is already correct and already tested).

1. **Give the client a bounded responsiveness timeout**, in
   `crates/app/src/redis/mod.rs`'s `connect_with`, via fred's
   `ConnectionConfig` (`Builder::from_config(config).with_connection_config(...)`
   or set on `Config` before building) — fred's `unresponsive` detection
   (periodic internal ping with a `max_timeout`) and/or
   `internal_command_timeout`. Pick values generous enough not to false-positive
   on a slow WAN/bastion link (the product's own stated environment) — on the
   order of a few seconds to ~10s, not sub-second. This makes a silently dead
   socket surface an error within a bounded time instead of hanging the
   in-flight read forever. Check fred's docs.rs for the exact config shape
   and the `ErrorKind` it produces on this path before finalizing values —
   don't guess the enum variant name.

2. **Make sure that error is classified as a connection loss**, in
   `crates/app/src/terminal.rs:599-621` (`Shell::watch_link`'s `error_rx`
   task). Widen the `matches!(error.kind(), fred::error::ErrorKind::IO)`
   check to also catch whatever kind fred emits for its
   unresponsive/timeout detection (confirm the exact variant from fred's
   source/docs — likely `ErrorKind::Timeout` or `ErrorKind::Canceled`,
   verify rather than assume). This is what lets the existing, already-correct
   `connection_lost()` → `Link::Reconnecting` → `Command::Reconnect` →
   `Msg::Connected` → `refetch()` → re-arm path in `crates/core` actually run.

Together: (1) guarantees `read.await` inside `ReadPermit::run()`
eventually *resolves* (as an `Err`, once the timeout fires) rather than
hanging forever — which by itself already fixes the deadlock, since
`_guard` is dropped the moment `run()` returns regardless of Ok/Err. (2)
guarantees the app correctly reports the connection as dead and runs the
reconnect/re-arm flow instead of just logging a stray notification while
still claiming `● live`.

No change is needed to `ReadPermit`'s "don't cancel mid-read" rule — that
rule exists to avoid abandoning a `CLIENT CACHING` arm on a *live*
connection when a newer read supersedes an older one. It's irrelevant once
the connection itself is being torn down and reconnected: a fresh
connection tracks nothing regardless (already asserted by
`a_reconnect_drops_liveness_and_does_not_get_it_back_for_free` in
`crates/core/src/update/link.rs`), so there's nothing to "leave armed" once
the socket is gone.

## Files to touch

- `crates/app/src/redis/mod.rs` — add the `ConnectionConfig` timeout in
  `connect_with`.
- `crates/app/src/terminal.rs` — widen the `error_rx` classification in
  `Shell::watch_link` (~line 607).

## Verification

- `cargo test --workspace` should stay green (no core changes).
- Add a `#[ignore]`d integration test to `crates/app/tests/integration.rs`
  (alongside the existing 54) that: opens a key, then makes the connection
  go silent without a clean close — pausing the `testcontainers` Redis
  container (`docker pause`) is the most direct way to reproduce "socket
  goes silent, no RST" without needing a proxy — then asserts the app
  transitions to `Link::Reconnecting` within the configured timeout and,
  once the container is unpaused, successfully re-arms and refetches the
  open key. This is exactly the kind of liveness-across-a-reconnect proof
  the suite's docstring says it exists for.
- Manual check per `scripts/README.md`: `./scripts/redis-up.sh`, open a key,
  `./scripts/redis-up.sh` restart or a firewall rule to black-hole the port
  briefly, confirm the header drops to `Reconnecting`/`manual` within the
  configured timeout and the Viewer resumes updating once the port opens
  again — without ever needing `q`.
