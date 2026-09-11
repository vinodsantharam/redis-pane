# ADR-0006 — Liveness without a refresh button

**Status:** Accepted · **Date:** 2026-08-26 · **Verified against Redis 8.4.0 on 2026-08-26**

## Context

The most-cited frustration with RedisInsight is that its refresh button does not refresh the
selected key. You open a key, the value changes on the server, you press refresh, and the same
value stares back. The known workaround is to select a different key and return — which is a
tell: the value is memoized under the key's name, the refresh control refreshes the *key list*,
and navigating away is the only thing that unmounts the memo. It is a cache invalidation bug
wearing a button.

The damage is larger than one stale render. Once a refresh control has lied, the user cannot
distinguish **"the refresh did nothing"** from **"the value genuinely did not change"** — and
that ambiguity persists even after the underlying bug is fixed. Any design here has to answer
both: never be stale, and never be ambiguous about it.

Redis offers a mechanism built for precisely this. `CLIENT TRACKING` makes the server remember
which keys a connection has read and push an `invalidate` message when one is modified, expired,
or evicted. It needs no server-side configuration, only Redis 6+ and RESP3.

## Decision

**No value cache.** Every read of the open key issues real commands. A stale value is not a
state the application can reach, so the bug above is structurally impossible rather than merely
fixed.

**Live by default, push-driven.** `CLIENT TRACKING ON OPTIN` is armed for exactly one key — the
one in the Viewer — using `CLIENT CACHING YES` before its read. An invalidation triggers one
Refetch. There is no polling and no idle traffic, and the server tracks one key per session
rather than the tens of thousands a browse would otherwise touch.

**Every Refetch re-arms.** Tracking is *consumed* by the invalidation it produces, not standing:
once the server has told you a key changed, it stops tracking that key until you read it again
under `CLIENT CACHING YES`. A Refetch that does not re-arm leaves the Viewer permanently dark
while the header still reads `● live` — the original defect, reached by a different route. This
is an invariant, and it is verified below rather than assumed.

**Apply if at rest, announce if scrolled, never while editing.** An open editor is never touched
by an arriving update.

**TTL counts down locally**, with no round trip.

**A key that was loaded and is then deleted while open keeps its last value**, badged, with
mutations disabled — because "what was in it" still has an answer for a key that was actually
read. A key confirmed gone *before* it was ever loaded has no such answer and gets no such badge
over a value: see Consequences for the distinction this decision did not originally draw.

**Degradation is announced.** Without RESP3 tracking the header reads `○ manual`, Read age is
shown, and `r` performs the Refetch.

There is no follow mode and no refresh button. The word "refresh" does not appear in the UI.

## Alternatives considered

**Polling on an interval.** Rejected as the primary mechanism. It forces an interval nobody can
choose correctly — too slow to be live, too fast to be free — and the cost scales with the value,
so tailing a 4 MB string means refetching 4 MB a second. It survives only as the shape of the
pre-RESP3 fallback, where it is manual rather than timed.

**Keyspace notifications (`notify-keyspace-events`).** Rejected. It defaults to off, so it
requires a server configuration change the user frequently cannot make — least of all on the
managed prod instance where they need this most. It is also the wrong granularity: pattern
subscriptions per database rather than tracking per key.

**Manual Refetch with a Read age, and nothing else.** Rejected as the whole answer, kept as the
floor. It fixes correctness and ambiguity but leaves a keystroke on the hot path — the exact
keystroke this decision exists to remove.

**Default (non-`OPTIN`) tracking mode.** Rejected. It tracks every key the connection reads, so
a browse of a 180k-key keyspace makes the server track most of the keyspace on the app's behalf
and push far more invalidation than the UI can use. `OPTIN` costs one extra command per key
opened and bounds the whole feature to one entry.

## Verification

Measured directly against Redis 8.4.0 over a raw RESP3 socket, reading the protocol bytes rather
than trusting a client library. All findings below are reproduced from actual pushes.

| Claim | Result |
|---|---|
| An armed key produces an invalidation push on write | **Confirmed** — `>2 $10 invalidate *1 $11 spike:armed` |
| `OPTIN` genuinely scopes: a read without `CLIENT CACHING YES` is not tracked | **Confirmed** — silence |
| `DEL` produces an invalidation | **Confirmed** |
| Expiry produces an invalidation, both lazy and active | **Confirmed** — fires even when no client touches the key |
| A field write to a hash invalidates the whole key | **Confirmed** — the Viewer's actual case |
| A fresh connection is not tracking anything | **Confirmed** — validates the reconnect invariant in [ADR-0009](0009-connection-lifecycle.md) |
| **Tracking is consumed by its own invalidation** | **Confirmed, and not previously accounted for** — see below |
| `CLIENT TRACKINGINFO` exposes readable state | **Confirmed** — `flags: on, optin` |

The last row is the finding that changed this ADR. Five rapid writes to an armed key produced
**one** push, not five: the server coalesces, because after the first invalidation it is no
longer tracking that key. A subsequent write produced nothing at all until the key was read
again under `CLIENT CACHING YES`.

The coalescing is welcome — no flood from a hot key. The consumption is the trap. It means the
Refetch cycle is `invalidate → read with CLIENT CACHING YES → armed again`, and any Refetch path
that omits the arming step goes silently dark. There are therefore **two** re-arm invariants, not
one: after every invalidation, and after every reconnect.

## Consequences

- **RESP3 is a hard requirement of the Redis client library**, which settles the open choice in
  favour of [`fred`](https://docs.rs/fred): its `TrackingInterface` exposes the raw invalidation
  stream (`invalidation_rx`) across centralized, clustered and sentinel deployments. `redis-rs`
  also supports client-side caching, but its `caching` module *maintains a cache* — the one thing
  this decision forbids.
- Invalidation is best-effort in the safe direction. When the server's tracking table fills, it
  sends invalidations early rather than dropping them, so the failure mode is a redundant
  Refetch, never a stale value.
- Cluster tracking is per-node; R1.11's hidden routing must arm tracking on whichever node owns
  the open key.
- Follow mode is not needed, including for streams — the server reports the change and the
  Viewer refetches. One fewer feature and one fewer binding.
- The Viewer gains one piece of state it did not have: whether the viewport is at rest. That is
  the price of apply-if-idle, and it is the only new state this decision introduces.
- **The shell never told the core that arming succeeded.** `read_value` awaited
  `CLIENT CACHING YES` and it worked, but nothing sent `Msg::TrackingArmed` — the message the core
  requires before it will report `Live` (that guard is the whole point of this ADR). The header
  read `○ manual` on every server, including local Redis with tracking fully working, and no test
  in the suite could catch it: the core's own tests supply `TrackingArmed` directly, and the
  integration suite asserted the *server* pushed an invalidation without checking that the *shell*
  ever reported the arming that made the read live in the first place. Found only by running the
  real binary against Redis Cloud and asking why a server that accepts `CLIENT TRACKING` still
  showed manual. Fixed by sending `TrackingArmed` from the one place both `OpenKey` and
  `RefetchOpenKey` funnel through, whenever `Arming::Enabled` and the read succeeds — which is
  exactly when arming is known to have happened on the wire.
- **There is nothing to arm on a server that refuses tracking.** Arming unconditionally makes
  every read fail on such a server — Upstash rejects `CLIENT CACHING` outright, so the app could
  browse a keyspace and open nothing in it. Arming is driven by the capability probe and by
  nothing else; the `Arming` enum exists so a caller cannot casually pass `false` and go dark.
  This narrows the rule rather than weakening it: *every read arms wherever arming is possible*.
- **`fred`'s `Options { caching: Some(true) }` does not work** (10.1.0). It compiles, and it reads
  as the idiomatic way to send `CLIENT CACHING YES` before a command, but the field is copied onto
  the command struct and never read by the router — nothing reaches the wire. A connection using
  it reports tracking as enabled while arming nothing, which is this project's characteristic bug
  wearing a library's clothes. Arming goes through an explicit `client_caching(true)` call, and
  the integration suite is what catches a regression here.
- **Another command could take the arming.** `CLIENT CACHING YES` arms the next command on the
  connection, whatever it is, and Redis clears it after that command unless a transaction is open.
  `read_value` sent the arming and `TYPE` as two separate commands. Reads took turns with each
  other, but keys-pane metadata, `SCAN` pages and writes share the same client, and any of them
  could land in between: that command was tracked, the open key was not, and the header said
  `● live` over a key nothing would ever invalidate. Reproduced against Redis 7 with 20 concurrent
  reads per round: 25 of 25 reads went untracked. Fixed by sending the arming and the first read
  as one fred pipeline, which fred's router writes with nothing in between. Covered by
  `a_read_armed_while_other_commands_share_the_connection_is_still_tracked`.
- The Refetch path is the *only* place a value is read, precisely so the arming step cannot be
  forgotten on one of several paths. This is the same chokepoint argument as mutations.
- Reversing this means reintroducing a cache, which is where the original bug lives. Treat the
  no-cache rule (R3.6) as the load-bearing half; the transport can change, the absence of a
  cache should not.
- **"A key deleted while open keeps its last value" assumed a value had been read.** `ValueGone`
  originally carried only a token, not the key's index or name — unlike `ValueLoaded`, which
  carries both — so the handler could not tell *which* key had come back gone and simply mutated
  whatever `OpenKey` happened to be in `state.open`. Reported from use: arrow onto an already-
  deleted key while a different, healthy key is open, and the healthy key gets badged `✕ deleted`
  and its row tombstoned in the keys pane — a token check passes (it genuinely is the latest
  read), so nothing here violated the letter of the invariant above, only its spirit. Fixed by
  giving `ValueGone` the same identity `ValueLoaded` has, and by drawing the distinction this ADR
  had not: a key that *was* loaded and is deleted while open keeps its value, badged, exactly as
  decided; a key confirmed gone *before* it was ever loaded has no value to keep, and `OpenKey`
  can now say so (`value: Option<Value>`) instead of being forced to badge someone else's.
