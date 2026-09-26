# M3 task 6: server Dashboard (`g d`)

Status: **planning — not started.** No code, no ADR.

## Decision point — read this before anything else in this doc

**DESIGN.md §9 already flags the Dashboard as "the largest remaining scope risk"** and names a
concrete alternative:

> Does the dashboard belong in v1 at all, or is the slowlog plus a memory figure in the status bar
> the whole of what triage actually needs?

This plan exists to design the *full* Dashboard (per the user's decision — see
`docs/plans/m3-planning.md`), but **the alternative is still live and should be explicitly
re-decided before Dashboard work starts, not assumed away by the existence of this document.**
Concretely, the two options on the table at that decision point are:

1. **Build the Dashboard as designed below** — `INFO`-based tiles: memory used/peak/maxmemory
   bar, hit ratio, ops/sec sparkline, connected/blocked clients, replication role/lag, and
   eviction/expiry counters, refreshing on an interval, single-node (ADR-0008).
2. **Descope**: skip the Dashboard entirely. Add a single memory-used figure to the status bar
   (the title bar already reads `● staging · from profile` — the same readout is where "342MB /
   1GB" would live, no new screen). Rely on the Slowlog (`docs/plans/m3-slowlog.md`, PLAN M3 task
   3, which ships regardless) for triage. This is materially less work — no new polling loop, no
   sparkline rendering, no per-tile expand-to-raw-`INFO` interaction — and DESIGN §9's framing
   ("is the slowlog plus a memory figure the *whole* of what triage actually needs?") suggests its
   authors were not confident the full build earns its cost.

**Why this doc still designs option 1 in full.** The user's call was to plan the complete
Dashboard so the decision is informed rather than made by default, and so that *if* it is chosen,
building does not start from zero. But nothing below should be read as having already settled the
question — the moment M3 implementation actually reaches this row, re-ask it, ideally with
whatever real-world signal M3's earlier tasks (Slowlog in particular, since it ships first and
covers some of the same triage use case) have produced by then about whether the gap it would fill
is real.

## Context

PLAN.md M3 row 6: "Dashboard (`g d`): `INFO`-based tiles ... single-node (ADR-0008) · Proves:
alarming values are colored; every tile expands to its raw `INFO` section; refreshes on an
interval, not static." PRD R6.3: "Server dashboard: `INFO` sections, memory breakdown, connected
clients, keyspace hit ratio, replication state — refreshing, not static." DESIGN §6.6 (four
sentences, the fullest prose any single M3 screen gets in DESIGN today): "Triage-first: memory
used vs. peak vs. maxmemory as a bar, hit ratio, ops/sec sparkline, connected/blocked clients,
replication role and lag, and eviction/expiry counters. Anything alarming is colored, and every
tile can be expanded into the raw `INFO` section behind it."

ADR-0008 fixes this to single-node scope for the reason already given there: `INFO`, `DBSIZE` and
`SLOWLOG` are per-node, so a Cluster-scoped Dashboard would need a node selector this app's premise
has no room for. Since Cluster is out for v1 (ADR-0008), the Dashboard describes exactly one
server for the whole of M3/v1 — no design work needed here to accommodate a future node picker;
that is explicitly ADR-0008's problem to solve when Cluster lands, not this task's.

## Architecture (if option 1 is chosen)

### Data source: `INFO`, polled on an interval

Unlike Monitor and Pub/Sub, the Dashboard is **not** a push feed and needs no
`docs/plans/m3-feed-connection.md` connection — `INFO` (with sections: `server`, `memory`,
`clients`, `stats`, `replication`, `keyspace`) is an ordinary request/response command, issuable on
the existing main connection exactly like `SLOWLOG GET` is in `m3-slowlog.md`. "Refreshing, not
static" (R6.3, and PLAN's "Proves" column) means polling it on a timer, which is a genuinely new
shape for this codebase: every other periodic-looking thing in the app so far is either
event-driven (liveness via `CLIENT TRACKING` push, ADR-0006) or a local per-frame projection with
no round trip (the TTL countdown, M1 task 11). The Dashboard is the first screen whose data is
neither — it must actually re-issue `INFO` on a timer while open.

- `Command::FetchServerInfo` / `Msg::ServerInfoLoaded { sections: ... }`, shaped like
  `Command::FetchSlowlog`/`Msg::SlowlogLoaded` in `m3-slowlog.md`.
- The *timer* itself is a shell concern, not a core one — CLAUDE.md's injected-clock discipline
  ("The clock is injected... golden-frame tests need it to be a function of state alone") means
  `update()` must not start its own interval; the shell (`terminal.rs`'s event loop, which already
  runs a `tokio::select!` over input, resize, and Redis replies) adds a `tokio::time::interval`
  arm, alive only while `state.view == View::Dashboard`, that produces `Command::FetchServerInfo`
  on each tick — mirroring how the shell, not the core, currently owns the reconnect backoff timer
  (M0 task 10). Propose a 2–5 second interval; `INFO` is cheap enough on a single node that this is
  not the kind of cost `MONITOR`'s warning banner exists for.
- Opening (`g d`) issues one `FetchServerInfo` immediately (no blank tile waiting for the first
  tick — same "optimistic focus, no blank frame" principle DESIGN §7 states for opening a key) and
  the shell starts the interval; closing (`Esc`/`g` elsewhere) stops it. This "timer alive only
  while a view is open" shape is the polling analogue of `m3-feed-connection.md`'s "feed connection
  alive only while its view is open" — a different mechanism (interval vs. socket) solving the same
  problem (don't do background work nobody is looking at).

### Tiles, alarming values, and the raw `INFO` drill-down

- `crates/core/src/state/dashboard.rs` (new): `DashboardState { raw: RawInfo, memory: MemoryTile,
  hit_ratio: f64, ops_history: VecDeque<u64> (bounded, for the sparkline — same cap discipline as
  Monitor's/Pub-Sub's buffers, sized to however many points the sparkline actually renders, not
  open-ended), clients: ClientsTile, replication: ReplicationTile, eviction: EvictionTile,
  expanded_tile: Option<TileId> }`. `RawInfo` keeps the parsed-but-unprocessed `INFO` reply
  (section -> key -> value) so "every tile expands to its raw `INFO` section" (PLAN's proof
  clause) is a lookup into data already held, not a second fetch.
- Each tile is a pure function of `DashboardState` (and, where "alarming" needs a threshold —
  e.g. memory used approaching `maxmemory`, replication lag past some seconds — a small set of
  constants, not user-configurable for M3 unless a requirement says otherwise). "Anything alarming
  is colored" (PRD/DESIGN/PLAN all say this) routes through the theme's semantic tokens
  (`crates/core/src/theme/mod.rs`) — a new token or two (`status.warning`, `status.critical`, if
  the theme does not already have general-purpose ones from other overlays) rather than a literal
  color chosen inside the Dashboard's own render code, per CLAUDE.md's "colors are semantic
  tokens, never literals."
- Expanding a tile (a keypress or `Enter` on the focused tile) sets `expanded_tile` and renders
  that section's raw `INFO` key/value pairs as an overlay or an in-place expansion — DESIGN gives
  no specifics here; propose an overlay (consistent with the confirm dialog's existing overlay
  pattern) since a tile expanding in place would reflow the whole grid under the
  reader's cursor, which is exactly the kind of layout shift M1's lazy-metadata design went out of
  its way to avoid for the keys pane ("Pending cells render without shifting layout").

### Layout

A grid of tiles is a genuinely new layout shape — `crates/core/src/render/layout.rs` today only
knows the two-pane `Keys`/`Value` split plus its Density breakpoints. The Dashboard needs its own
layout function (not reusing `layout()`), likely a simple row/column grid whose tile count per row
degrades with terminal width the same way the keys pane's column count degrades with Density
(CLAUDE.md: "Terminal capability degrades gracefully... Layout breakpoints are in DESIGN.md §2") —
DESIGN §2's specific breakpoints are written for the two-pane browser and do not obviously carry
over to a tile grid; this task should propose its own breakpoints (e.g. tiles per row at Full vs.
Tight vs. the 80-column floor) rather than force-fitting the keys-pane numbers, and record them in
DESIGN.md when built, since DESIGN §2 currently has no grid-layout section at all.

## CLAUDE.md rules this binds

- **The clock is injected.** The polling interval lives in the shell, never inside `update()`;
  golden frames of the Dashboard are a function of a `DashboardState` snapshot, not of when the
  test runs.
- **The render loop never does I/O.** The shell's timer produces `Command`s; `update()` only reacts
  to `Msg::ServerInfoLoaded`.
- **Colors are semantic tokens, never literals.** Directly load-bearing here — "alarming values are
  colored" is a requirement this rule exists to keep consistent with the rest of the app's chrome.
- **Terminal capability degrades gracefully.** The tile grid needs its own breakpoints; see Layout
  above.
- **Screen space is a budget, not a canvas.** The Dashboard is a `View` (introduced in
  `m3-slowlog.md`), reached by `g d` (Palette withdrawn, ADR-0020), never a persistent
  status-bar-adjacent panel — which is exactly what option 2 above proposes *instead of* this
  screen, worth remembering while building option 1: the two are genuinely alternatives, not
  layers.
- **Every in-flight operation must be cancellable.** Less literally applicable here (a poll isn't
  "in flight" the way a scan is) but the interval must stop the instant the view closes, the same
  discipline as a cancelled scan or a closed feed connection.

## Files touched (option 1)

| File | Change |
|---|---|
| `crates/core/src/state/dashboard.rs` (new) | `DashboardState`, per-tile structs, `RawInfo` |
| `crates/core/src/state/mod.rs` | `View::Dashboard`; `pub dashboard: DashboardState` on `State` |
| `crates/core/src/command.rs` | `Command::FetchServerInfo` |
| `crates/core/src/msg.rs` | `Msg::ServerInfoLoaded { sections }` |
| `crates/core/src/update/dashboard.rs` (new) | `g d` entry, tile-expand toggling, ops-history push (bounded) |
| `crates/core/src/render/dashboard.rs` (new) | grid layout + breakpoints, tile rendering, raw-`INFO` overlay |
| `crates/core/src/theme/mod.rs` | `status.warning`/`status.critical` tokens, if not already present |
| `crates/app/src/redis/read.rs` | `fetch_server_info` (parses `INFO`'s section format) |
| `crates/app/src/terminal.rs` | `tokio::time::interval` arm, alive only while `View::Dashboard`; dispatch for `Command::FetchServerInfo` |
| `docs/DESIGN.md` §2 | new grid-layout breakpoints, once decided |

## Testing (option 1)

- **Core unit tests**: `Msg::ServerInfoLoaded` populates every tile correctly from a fixed parsed
  `INFO` fixture; alarming-threshold logic (memory-near-maxmemory, replication lag) returns the
  right color token at the boundary and just past it; `ops_history` stays bounded across many
  pushes (same cap-discipline test shape as Monitor's/Pub-Sub's buffers); expanding/collapsing a
  tile toggles `expanded_tile` and does not touch anything else.
- **Golden frames**: the tile grid at more than one width/Density (proving the new breakpoints
  hold), a tile in its alarming color, the raw-`INFO` overlay for one tile, and the empty/loading
  state right after `g d` before the first `Msg::ServerInfoLoaded` lands (the "no blank frame"
  requirement).
- **Docker-backed integration test**: `fetch_server_info` against a real container parses a real
  `INFO` reply into every tile's fields without panicking (this is the test most likely to catch a
  real-world `INFO` format surprise — section headers, missing fields on a fresh container with no
  replication configured, etc.); a test that drives two polls in sequence (via two `fetch_server_info`
  calls, not a real timer, since the interval itself is shell plumbing this suite does not need to
  wait out) and proves the ops/sec figure changes between them, which is what "refreshing, not
  static" actually claims.

## CONTEXT.md

No glossary entry exists yet for **Dashboard**. Proposed, to be added when this feature's
implementation actually starts (if option 1 is chosen; moot under option 2):

**Dashboard**:
The triage-first view of one server's own vitals — memory, hit ratio, ops/sec, clients,
replication, eviction — built from `INFO` and polled on an interval rather than pushed. Scoped to
exactly one node (ADR-0008); there is no cross-node aggregation because Cluster is out for v1.
_Avoid_: Overview, stats page, metrics (too generic — this is specifically the server's own `INFO`
data, not application-level metrics)

## Out of scope

- **Historical retention beyond the in-session sparkline buffer** — no persistence across
  relaunch; the ops/sec sparkline is a bounded in-memory window, not a time-series store.
  `ADR-0003`'s "app never writes config, and persists only session state" boundary applies if this
  is ever reconsidered — a metrics history is a different kind of persistence than pane sizes or
  scroll position.
- **Alerting/thresholds configurable by the reader** — the coloring thresholds are fixed constants
  for M3; making them configurable is a real feature but not asked for by R6.3.
- **Cluster-wide aggregation** — ADR-0008, inherited, not re-litigated here.
