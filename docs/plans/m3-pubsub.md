# M3 task 5: Pub/Sub (`g p`)

Status: **planning — not started.** No code, no ADR. DESIGN §6.7 gives Pub/Sub the same two
sentences as Monitor and no distinct layout description: "Live tail with a filter box,
pause/resume, and a persistent warning banner on `MONITOR` explaining its cost. Buffers are
bounded with a visible cap." The warning-banner sentence is `MONITOR`-specific and does not carry
over as written — see below. This doc designs Pub/Sub's own shape.

Depends on `docs/plans/m3-feed-connection.md` (the dedicated second connection) and the `View`
enum introduced in `docs/plans/m3-slowlog.md`. Shares structure with
`docs/plans/m3-monitor.md` (both are live tails over a feed connection) but PLAN.md's own "Proves"
column for this row is explicit that the two must not collapse into one: "Distinct from Monitor's
layout (not just 'Monitor with a different source')." This doc exists to say concretely what is
different, not just to assert that it must be.

## Context

PLAN.md M3 row 5: "Pub/Sub (`g p`): subscribe to channels and patterns, live tail · Proves:
distinct from Monitor's layout (not just 'Monitor with a different source'); unsubscribing on
view-close leaves no orphaned subscription." PRD R6.2: "Pub/Sub subscribe view (channels and
patterns)."

## Why Pub/Sub is not "Monitor with a different source"

Monitor is passive and undifferentiated: one feed, no configuration beyond pause/filter, the
reader never chooses what appears — it is everything, tailed. Pub/Sub is opt-in and structured:

- **The reader chooses what to subscribe to** — one or more literal channels (`SUBSCRIBE`) and/or
  glob patterns (`PSUBSCRIBE`), entered before anything streams. Monitor has no equivalent
  "configure before you see anything" step; Pub/Sub's view opens to an empty subscription list and
  an input, not an already-running tail.
- **Messages carry a channel identity Monitor's lines do not** — every message is `(channel,
  payload)`, and with multiple channels/patterns subscribed at once, the tail is naturally
  multiplexed. This wants a channel column or a per-channel color/badge in the tail, which Monitor
  has no analogue of (a `MONITOR` line has no comparable second dimension to key the display on).
  **This is the concrete answer to "distinct layout"**: the tail is at minimum two columns
  (channel, payload) where Monitor's is one (raw command text), and the view needs a visible list
  of active subscriptions (with a way to remove one without closing the whole view) that Monitor
  has nothing corresponding to.
- **The cost story is different, so the persistent-warning requirement does not carry over
  verbatim.** `MONITOR`'s cost is server-wide and involuntary from the reader's perspective — every
  command, whether they wanted to see it or not. A Pub/Sub subscription's cost is scoped to
  exactly what the reader asked for; there is no equivalent "you may not have meant to ask for
  this much" hazard, so a persistent cost banner is very likely the wrong chrome for this screen —
  propose no persistent warning here, revisit only if a real cost surfaces (e.g. subscribing to a
  very high-traffic pattern) that DESIGN's authors did not anticipate when they wrote the shared
  two-sentence paragraph. Flag this divergence from the DESIGN prose explicitly when this task's
  ADR-equivalent design note lands, since DESIGN §6.7 currently reads as if both screens share the
  banner.

## Architecture

### State and the feed

- `crates/core/src/state/pubsub.rs` (new): `PubSubState { subscriptions: Vec<Subscription>,
  messages: VecDeque<PubSubMessage>, cap: usize, paused: bool, input: String }`.
  `Subscription` is `Channel(String) | Pattern(String)`. `PubSubMessage` is `{ at_ms: u64, source:
  Subscription, channel: String, payload: Vec<u8> }` — `channel` is the message's actual channel
  even under a pattern subscription (Redis's own `pmessage` reply carries both the matched pattern
  and the concrete channel; keep both, since "which pattern matched" and "which channel this
  actually was" are different facts a reader may want).
- Reuses `m3-feed-connection.md`'s `FeedHandle`/`FeedKind::Subscribe` plumbing. Unlike Monitor,
  which opens its feed once and never reconfigures it, Pub/Sub's subscription list changes while
  the view stays open — adding a channel mid-session must not tear down and reopen the connection
  (that would drop in-flight messages on the channels already subscribed). This is the
  `Command::UpdateSubscription { add: Vec<Subscription>, remove: Vec<Subscription> }` case
  `m3-feed-connection.md`'s Open Questions section names as Pub/Sub-specific — this task is where
  it actually gets designed and built, translating to `SUBSCRIBE`/`PSUBSCRIBE`/`UNSUBSCRIBE`/
  `PUNSUBSCRIBE` on the existing feed connection rather than a new one per change.
- Same bounded-buffer discipline as Monitor (`m3-monitor.md`'s `push_monitor_line` pattern,
  mirrored here as a single `push_pubsub_message` enforcement point) and the same
  paused-means-not-consumed requirement — PLAN's "bounded buffer" language in task 4's row is
  Monitor-specific text, but R6.2 gives no reason Pub/Sub's tail should be allowed to grow
  unbounded either, and CLAUDE.md's cap discipline is general, not Monitor-specific. Treat the cap
  as an M3-wide requirement for every live tail, not something to re-litigate per feature.

### Unsubscribing cleanly — the load-bearing requirement

PLAN's "Proves" column: "unsubscribing on view-close leaves no orphaned subscription." Concretely:
`Esc` (or `g` to anywhere else) from the Pub/Sub view must emit `Command::CloseFeed`
(`m3-feed-connection.md`), and the shell's feed-close handler must actually issue
`UNSUBSCRIBE`/`PUNSUBSCRIBE` (or simply drop the connection — Redis unsubscribes everything
implicitly when a client disconnects, which is arguably the simpler and equally correct
implementation) before or as part of closing the socket. The failure mode this guards against: a
Pub/Sub connection that is dropped from the shell's own bookkeeping (`FeedHandle` gone) while the
underlying TCP connection or server-side subscription state somehow lingers — this is exactly the
shape of bug `docs/plans/value-pane-frozen-after-silent-disconnect.md` fixed for the main
connection, generalized to a feed connection with actual server-side subscription state attached
to it, not just an idle socket. Worth a Docker-backed test that inspects server-side state
(`PUBSUB CHANNELS` from a second connection) after the view closes, not just an assertion about
the app's own `PubSubState`, since "orphaned" is a claim about the server, not about this app's
bookkeeping.

## CLAUDE.md rules this binds

- **Every in-flight operation must be cancellable (`Esc`).** Same as Monitor, plus the
  unsubscribe-on-close requirement above, which is stricter than Monitor's (Monitor has no
  server-side state to leak beyond the connection itself; Pub/Sub does).
- **The cap is enforced in exactly one place.** `push_pubsub_message`, mirroring
  `m3-monitor.md`'s `push_monitor_line`.
- **The render loop never does I/O.** Subscribing/unsubscribing are `Command`s the shell executes;
  `update()` only decides *that* a subscription changed, never sends the `SUBSCRIBE` itself.
- **Screen space is a budget, not a canvas.** Pub/Sub is a `View`; the subscription list and the
  tail share the same screen rather than each claiming a permanent slot elsewhere.
- **Colors are semantic tokens.** Per-channel/pattern color coding in the tail (if built) is
  assigned from the theme's palette, not literal colors.

## Files touched

| File | Change |
|---|---|
| `crates/core/src/state/pubsub.rs` (new) | `PubSubState`, `Subscription`, `PubSubMessage` |
| `crates/core/src/state/mod.rs` | `View::PubSub`; `pub pubsub: PubSubState` on `State` |
| `crates/core/src/command.rs` | `Command::UpdateSubscription { add, remove }` |
| `crates/core/src/msg.rs` | `Msg::PubSubMessage { at_ms, source, channel, payload }` |
| `crates/core/src/update/pubsub.rs` (new) | subscription-list editing, `push_pubsub_message`, pause/resume |
| `crates/core/src/render/mod.rs` | Pub/Sub screen: subscription list, two-column tail, input for adding a channel/pattern |
| `crates/core/src/keymap/mod.rs` | `Action::OpenPubSub`, `Action::Subscribe`, `Action::Unsubscribe` |
| `crates/app/src/redis/feed.rs` | `FeedKind::Subscribe` handling, `Command::UpdateSubscription` execution |

## Testing

- **Core unit tests**: adding/removing a subscription updates `PubSubState.subscriptions` and
  emits the right `Command::UpdateSubscription`; `push_pubsub_message` respects the cap and the
  paused-means-not-consumed rule (mirroring `m3-monitor.md`'s tests); a `pmessage` populates both
  the matched pattern and the concrete channel; `Esc` emits `Command::CloseFeed`.
- **Golden frames**: empty view (no subscriptions yet, input focused), subscribed with messages
  arriving on more than one channel (proving the two-column/channel-badge layout actually reads as
  distinct from Monitor's single-column tail — the direct golden-frame proof of PLAN's "distinct
  from Monitor's layout" clause), and paused.
- **Docker-backed integration test**: subscribe to a channel and a pattern on the feed connection,
  publish from a second ordinary connection, and prove both a direct-channel message and a
  pattern-matched message arrive correctly attributed; add a subscription mid-session and prove
  the first one keeps receiving messages uninterrupted; close the view and, from a second
  connection, run `PUBSUB CHANNELS`/`PUBSUB NUMPAT` and prove nothing this session subscribed to
  is still listed — the test that actually proves "leaves no orphaned subscription" against the
  server's own accounting, not the app's.

## CONTEXT.md

No glossary entry exists yet for **Pub/Sub**. Proposed, to be added when this feature's
implementation actually starts:

**Pub/Sub**:
The view of messages published to channels and patterns the reader has explicitly subscribed to,
over its own feed connection (`m3-feed-connection.md`) — never the main one, and never messages
the reader did not ask for. Distinct from Monitor: Monitor is passive and server-wide; a Pub/Sub
subscription is chosen, scoped, and edited while the view stays open.
_Avoid_: Subscriber, message log, event stream (too generic — this is specifically Redis Pub/Sub,
not a general event feed)

## Out of scope

- **Publishing from the app** — R6.2 says "subscribe view," not a publisher. A `PUBLISH` action
  would be a real mutation (through the existing chokepoint) but is not asked for here; revisit as
  its own row if wanted later.
- **Sharded Pub/Sub** (`SSUBSCRIBE`, Cluster-only) — moot while Cluster itself is deferred
  (ADR-0008).
