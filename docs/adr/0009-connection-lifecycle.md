# ADR-0009 — Connection lifecycle and failure model

**Status:** Accepted · **Date:** 2026-08-26

## Context

The documents promised that errors surface as non-blocking notifications carrying the failing
command (R7.4) and that the app never freezes (DESIGN principle 6). Neither said what a *dropped
connection* becomes, what happens when the server cannot be reached at launch, or what to do
about the Redis conditions that are not really errors — `-LOADING` during a restart, `-BUSY`
while a script runs, `-OOM` at `maxmemory`, `-MISCONF` when RDB saves are failing, and
`-READONLY` when the target turns out to be a replica.

[ADR-0006](0006-liveness-without-a-refresh-button.md) raised the stakes. `CLIENT TRACKING` is
per-connection state: a transparent reconnect leaves the server no longer tracking the open key
while the header still reads `● live`. That is the original RedisInsight failure — claiming
currency you do not have — reintroduced through the back door.

## Decision

**Startup failure exits.** A target that cannot be reached — refused, DNS failure, auth
rejected, or a server below the floor ([ADR-0007](0007-server-compatibility-floor.md)) — prints a
diagnostic naming the resolved target, its **Source**, and the precise failure, and exits
non-zero. There is no half-open application. The first-run picker remains the different case:
nothing was specified at all.

**Mid-session drops do not exit.** Reconnection runs in the background with backoff. The UI stays
interactive throughout, the Viewer retains its last read value badged
`✕ disconnected · last read 14:22:07`, and `r` retries immediately rather than waiting out the
timer.

**Every successful reconnect re-arms tracking and refetches the open key.** This is an invariant.
Liveness state is connection state; a reconnect that does not restore it must not present itself
as live.

**Server conditions feed the safety chrome rather than only the error stream:**

| Condition | Behaviour |
|---|---|
| `role:slave` | Read-only Mode on, reason `replica`. `⌃R` cannot lift it |
| `-OOM`, `-MISCONF` | Banner stating writes are being rejected, and why |
| `-LOADING` | A connection state with progress, not a wall of errors |
| `-BUSY` | Surfaced as a transient state; operations retry after it clears |

**Read-only Mode therefore carries a reason** — `environment`, `replica`, or `user` — and the
header displays it. A guard whose origin is invisible is a guard the user will misread.

## Alternatives considered

**Open the TUI in a disconnected state at startup.** Rejected, though it is the tidier state
machine: one "not connected" presentation whether it happened at second zero or minute forty. The
asymmetry is real and worth honouring — mid-session there is data on screen worth preserving, at
startup there is nothing to show. An app that opens to an error box with nothing behind it wastes
the user's time and forfeits a useful exit code.

**Exit on hard failures, open on transient ones.** Rejected. It matches intent when it guesses
right, but the user cannot predict which they will get, and "it depends" is a poor answer to
"what does this command do when it fails".

**Reactive error reporting only.** Rejected. R7.4 already guarantees nothing is hidden, so this
is defensible — but it means learning that a server is a replica by having a write rejected,
which is the exact ordering DESIGN principle 5 forbids: danger visible *before* it is possible.
R6.3 already polls `INFO`, so detection adds no new traffic, only wiring.

## Consequences

- Read-only Mode gains a reason field, displayed in the header, and one of its reasons is not
  user-liftable. The `⌃R` hint must reflect that rather than offering a toggle that cannot work.
- Startup is a distinct code path from the running application, with its own diagnostics. That
  diagnostic is the only UI some users will ever see, and it is worth writing well.
- Backoff needs a visible countdown and an immediate-retry key; a silent wait is a freeze wearing
  a different name.
- Sentinel failover is this path with a changed address
  ([ADR-0008](0008-sentinel-in-v1-cluster-deferred.md)).
- The reconnect invariant is the kind of rule that rots quietly. It belongs in a test that
  disconnects mid-session and asserts the header does not read `● live` until tracking is
  re-armed ([ADR-0011](0011-functional-core-golden-frames.md)).
