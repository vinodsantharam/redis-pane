# redis-pane — Product Requirements

**Status:** Draft v0.4 · **Owner:** Vinod Santharam · **Last updated:** 2026-08-26

## 1. Problem

Working with Redis day-to-day means choosing between two bad options.

**`redis-cli`** is a REPL, not a workspace. You cannot see a keyspace — you can only guess at
it. `KEYS *` blocks the server, `SCAN` requires you to hand-manage cursors, and every value
comes back as an untyped blob. TTLs, memory usage, and type are three separate round trips.
There is no undo, no confirmation, and no visual distinction between a scratch container and
production. Non-experts avoid it; experts tolerate it.

**RedisInsight** is a desktop GUI that solves discoverability and then introduces its own
friction: it is a heavyweight Electron app, slow to launch, sluggish on keyspaces past a few
hundred thousand keys, and unusable in the place engineers most often need it — inside an SSH
session on a bastion host. Connection setup is a form-filling exercise, and the mouse-first
interaction model breaks the flow of people who live in a terminal.

The gap: **there is no fast, keyboard-driven, visually excellent Redis client that runs where
engineers already are.**

## 2. Vision

A Redis client so well designed that you forget it is a terminal application. It launches
instantly, works over SSH, navigates by keyboard, renders structured data as structured data,
and makes dangerous operations feel dangerous. The terminal is the delivery mechanism, not the
excuse.

**One-line pitch:** the keyspace browser Redis should have shipped with.

## 3. Users

| Persona | Context | Primary need |
|---|---|---|
| **Backend engineer (primary)** | Debugging a service locally or in staging; lives in a terminal | Find a key, read it, understand why it looks wrong |
| **On-call / SRE** | SSH'd into a bastion at 3am, incident in progress | Fast triage: memory, slowlog, hot keys, blocked clients — without touching prod state |
| **Data / platform engineer** | Bulk inspection, migrations, cache-shape audits | Scan patterns, compare, export, run scripted commands |
| **Redis newcomer (secondary)** | Knows the product, not the commands | Discoverability — learn Redis by seeing it |

## 4. Goals

- **G1 — Sub-second to useful.** Cold start to a browsable keyspace in under one second.
- **G2 — Never block the server.** Every keyspace traversal is cursor-based and incremental.
- **G3 — Type-aware by default.** Every value renders in a view that matches its type.
- **G4 — Keyboard-complete.** Every action reachable without the mouse; mouse supported anyway.
- **G5 — Safe by construction.** Destructive operations require intent, and production
  connections are visually unmistakable.
- **G6 — Discoverable.** A new user finds the feature without reading documentation.
- **G7 — Nothing on screen you will not act on.** Chrome earns its columns or it is removed.
  Anything that is not the keyspace or the current value is reached on demand, never resident.

## 5. Non-goals (v1)

- Not a Redis server manager (no config editing, no failover orchestration, no provisioning).
- Not a replacement for `redis-cli` scripting/piping in shell pipelines.
- Not a monitoring/alerting product — observability views are for triage, not retention.
- No plugin system, no embedded scripting language, no team/collaboration features.
- **Not a multi-target workspace.** One Connection per process, one database per Connection.
  A second target means a second terminal. See
  [ADR-0005](adr/0005-one-connection-per-process.md).
- **No Cluster support in v1.** Sentinel ships; Cluster is deferred to M4 because per-node
  `SCAN`, `INFO` and tracking reintroduce a node selector into a UI built on having nothing to
  select. See [ADR-0008](adr/0008-sentinel-in-v1-cluster-deferred.md).
- **Nothing older than Redis 6.0**, and RESP2 is not spoken at all
  ([ADR-0007](adr/0007-server-compatibility-floor.md)).
- No export/import in v1. `y` yields a paste-ready `redis-cli` command, which is what actually
  gets pasted into a ticket or a shell.
- No Windows-native terminal work beyond whatever the TUI toolkit gives us for free.

## 6. Requirements

### 6.1 Connections and Profiles

Vocabulary is defined in [CONTEXT.md](../CONTEXT.md): a **Profile** is a saved description in the
config file; a **Connection** is a live session, which may be **ad-hoc** (no Profile behind it).

- **R1.1** Connect via URL (`redis://`, `rediss://`), host/port, or Unix socket.
- **R1.2** A Connection's target resolves in strict precedence: flags (`--url`, `--host`,
  `--profile`, or a bare Profile name — `redis-pane staging`) → the config file's default
  Profile → the environment → `127.0.0.1:6379`. In the
  environment, `REDIS_URL` is used wholesale if set, otherwise the target is assembled from
  `REDIS_HOST` / `REDIS_PORT` / `REDIS_USER` / `REDIS_PASSWORD`. The two forms are never merged.
  See [ADR-0001](adr/0001-connection-resolution-order.md).
- **R1.3** Resolution never prompts. The title bar permanently shows the resolved target **and**
  its Source (flag / Profile / environment). Zero configuration is a supported way to run: with
  nothing set up at all, the app connects to a local Redis and starts browsing.
- **R1.4** Profiles live in a JSON file at `$XDG_CONFIG_HOME/redis-pane/config.json` (falling
  back to `~/.config/redis-pane/config.json`), carrying address, credential reference,
  Environment, and an optional `note` rendered in the UI.
  See [ADR-0002](adr/0002-json-config-file.md).
- **R1.5** Credentials are referenced, not embedded: `passwordEnv` or `passwordCommand`. A
  literal `password` is honoured, but the file is refused if group- or world-readable, and such
  Profiles are badged.
- **R1.6** The app never writes the config file — Profiles are hand-authored. Session state is
  persisted separately under `$XDG_STATE_HOME/redis-pane/`.
  See [ADR-0003](adr/0003-app-never-writes-config.md).
- **R1.7** The config file must be discoverable without the app ever writing it: first run prints
  its path with a ready-to-paste example, `--config-path` reports it, and parse errors name the
  file with line and column.
- **R1.8** Every Profile and Connection carries an **Environment** — `local`, `staging`, `prod`,
  or `unknown` — driving chrome colour and safety defaults.
- **R1.9** Ad-hoc Connections infer their Environment: loopback, `::1`, or a unix socket →
  `local`; anything else → `unknown`, which starts in Read-only Mode and is lifted with one
  keypress. See [ADR-0004](adr/0004-untagged-connections-are-read-only.md).
- **R1.10** TLS, ACL usernames, and per-Profile default database.
- **R1.11** Sentinel topology discovery, with failover surfacing as a reconnect to the new
  address. Cluster is **not** in v1 — see [ADR-0008](adr/0008-sentinel-in-v1-cluster-deferred.md)
  and §5.
- **R1.12** Exactly one Connection per process, against exactly one database, fixed at launch.
  There is no in-app connection switcher and no `SELECT`.
- **R1.13** The server floor is **RESP3 and Redis 6.0**, or an API-compatible fork. Older servers
  are refused at launch, naming the version found. Liveness is gated by *capability* — the app
  attempts `CLIENT TRACKING` and degrades on error — never by version number, because managed
  platforms restrict it independently of the version they report.
  See [ADR-0007](adr/0007-server-compatibility-floor.md).
- **R1.14** A target that cannot be reached at launch prints a diagnostic naming the target, its
  Source, and the failure, and exits non-zero. A Connection lost mid-session does **not** exit:
  reconnection runs in the background with visible backoff, the UI stays interactive, and the
  Viewer retains its last read value badged as disconnected. Every successful reconnect re-arms
  tracking and refetches the open key. See [ADR-0009](adr/0009-connection-lifecycle.md).
- **R1.15** Server conditions that will reject writes are detected, not merely reported:
  `role:slave` turns on Read-only Mode with reason `replica`; `-OOM` and `-MISCONF` raise a
  banner explaining why writes fail; `-LOADING` renders as a connection state with progress.

### 6.2 Keyspace browsing
- **R2.1** Incremental `SCAN` with live streaming results; never `KEYS`.
- **R2.2** Glob filter plus fuzzy filter over already-loaded keys.
- **R2.3** Hierarchical grouping by separator (`:` default) — `user:1:session` folds into a
  tree — is the default view; `t` is one keypress from flat. Tree shows fewer rows at rest, which
  matters more than the one extra keypress to reach a leaf, once there is a real keyspace to look
  at rather than a mockup.
- **R2.4** Per-key metadata inline, lazily fetched: type, memory usage, and TTL. Element count
  is deliberately **not** a column — it is noise for strings, it costs a fourth round trip per
  key, and the Viewer header states it the moment a key is opened.
- **R2.5** Sort by name, TTL, size, or element count, across the whole **Loaded set**. Sorting
  by a lazily-fetched column orders the values that have arrived, parks the rest at the end, and
  states the count — it never triggers a mass metadata fetch. Sorting by element count surfaces
  it as a temporary column, which is how it stays reachable without being resident (R2.4).
  Multi-select for bulk operations.
- **R2.6** Handle 1M+ key keyspaces without UI stall: virtualized rendering, and every scanned
  key retained in a columnar arena so sort and filter span the Loaded set. Memory is bounded by
  a documented **cap**; on reaching it, scanning stops and says so rather than growing until the
  process is killed. See [ADR-0010](adr/0010-keyspace-memory-model.md).
- **R2.7** `r` acts on the focused pane and nothing else: a rescan in the keys pane, a Refetch
  in the Viewer. There is no global refresh, because there is no global staleness.

### 6.3 Value inspection
- **R3.1** Dedicated viewer per type: string, hash, list, set, sorted set, stream, bitmap,
  HyperLogLog, and JSON/`ReJSON` when the module is present.
- **R3.2** String auto-detection and pretty-printing: JSON, MessagePack, protobuf-ish binary
  fallback to hex+ASCII dump, gzip/snappy transparent decode where detectable.
- **R3.3** Collections render as paginated, sortable, searchable tables — not as a wall of text.
- **R3.4** Streams get an entry timeline with consumer-group state.
- **R3.5** Search within a value; copy key, value, or a ready-to-paste `redis-cli` command.
- **R3.6** **There is no value cache.** Every read of the open key issues real commands; the
  Viewer never serves a value from memory. A stale value is therefore not a state the
  application can reach. See [ADR-0006](adr/0006-liveness-without-a-refresh-button.md).
- **R3.7** The open key is **live by default**: `CLIENT TRACKING ON OPTIN` is armed for exactly
  that one key, and a Refetch is triggered by the server's invalidation push. No polling, no
  idle traffic, one tracked key per session. **Every Refetch re-arms**, because tracking is
  consumed by the invalidation it produces — a Refetch that does not re-arm goes silently dark
  while still claiming to be live (verified; ADR-0006).
- **R3.8** An arriving update applies immediately when the Viewer is at rest, and is announced
  (`changed 2s ago · r to load`) when the user has scrolled. An open editor is never touched.
- **R3.9** TTL renders as a countdown computed locally. It never costs a round trip.
- **R3.10** A key deleted, expired, or evicted while open retains its last read value, badged
  with the deletion and the read time, with mutating actions disabled.
- **R3.11** Liveness state is always visible in the Viewer header. Where the server cannot
  support it — Redis < 6, or no RESP3 — the app degrades to Read age plus explicit Refetch and
  **says so**. It never silently behaves differently from what the user would assume.

### 6.4 Mutation
- **R4.1** In-place edit for scalar values and collection members, with a diff-style confirm.
- **R4.2** TTL editing (set/persist/extend) as a first-class action.
- **R4.3** Rename, copy, move-across-db, delete — single and bulk.
- **R4.4** Every mutation shows the exact command it will run before it runs.
- **R4.5** **Read-only Mode**, default-on for `prod` and `unknown` Environments, toggled
  explicitly (`Ctrl-R`). It carries a **reason** — `environment`, `replica`, or `user` — which is
  displayed. The `replica` reason cannot be lifted, because the server will refuse regardless
  (R1.15).
- **R4.6** Confirmation friction scales with blast radius: single delete = one keypress;
  bulk delete on prod = typed confirmation.

### 6.5 Command surface
- **R5.1** Command palette (fuzzy, single keystroke) for every app action.
- **R5.2** Embedded command console with history, completion, and inline documentation for the
  command under the cursor.
- **R5.3** Results of console commands render in the same type-aware viewers.
- **R5.4** Command history persisted per profile, searchable.

### 6.6 Live views
- **R6.1** `MONITOR` tail with filtering — clearly flagged as expensive.
- **R6.2** Pub/Sub subscribe view (channels and patterns).
- **R6.3** Server dashboard: `INFO` sections, memory breakdown, connected clients, keyspace hit
  ratio, replication state — refreshing, not static.
- **R6.4** Slowlog viewer with sort and reset.

### 6.7 Application quality
- **R7.1** Minimum viable terminal: 80×24. Responsive layout above that.
- **R7.2** Truecolor, 256-color, and no-color/monochrome fallbacks.
- **R7.3** Mouse support: click to focus, scroll, drag to resize splits.
- **R7.4** Errors surface as non-blocking, dismissible notifications with the failing command.
- **R7.5** Full-app help overlay and per-pane contextual key hints always visible.
- **R7.6** Single self-contained binary, no runtime dependency.
- **R7.7** The default layout is two panes — keyspace and current value. Anything else reaches
  the screen through the Palette or a dismissible overlay, and nothing else holds columns
  permanently. New surfaces must displace something or justify their width against G7.

## 7. Success metrics

| Metric | Target |
|---|---|
| Cold start → interactive keyspace | < 1s on a 100k-key database |
| First render of scan results | < 150ms after connect |
| Keystroke → visible response | < 16ms (one frame) for local interactions |
| Memory at 1M keys browsed | < 250MB RSS |
| Time to find a known key (new user, no docs) | < 30s |
| Server change → visible in the Viewer | < 100ms after the invalidation push |
| Binary size | < 20MB |

## 8. Risks

| Risk | Mitigation |
|---|---|
| Destructive action against prod | Environment tagging, read-only default, scaled confirmation, command preview |
| Silently resolving to an unexpected server | Deterministic precedence, target + Source always visible, `unknown` Environment read-only by default |
| Large-value rendering hangs the UI | Hard fetch caps with explicit "load more"; render off the input thread |
| Cluster semantics leak into UX | Deferred from v1; the keyspace source abstracts over *a stream of keys*, so N cursors can replace one without a rewrite |
| Reconnect silently drops liveness | Re-arming tracking is an invariant of reconnect, asserted by test (ADR-0009, ADR-0011) |
| Showing a value the server no longer holds | No value cache (R3.6); liveness is push-driven and its state is always on screen |
| Feature sprawl reproduces RedisInsight's bloat | Non-goals are enforced; every feature must survive the "would an on-call use this?" test |
| Terminal capability fragmentation | Capability detection with graceful degradation; test matrix across common terminals |

## 9. Milestones

- **M0 — Skeleton.** App shell, event loop, theming, help overlay, the full connection
  resolution chain (flags → Profile → env → localhost) with target and Source in the title bar,
  and the connection lifecycle: capability probe, startup diagnostics, background reconnect with
  re-arming (R1.13–R1.15).
- **M1 — Browse.** Scan-based keyspace browser, tree/flat views, all core type viewers, and
  liveness on the open key (R3.6–R3.11). *This is the milestone that already beats `redis-cli`
  for daily use.*
- **M2 — Mutate.** Editing, TTL management, delete/rename/copy, read-only mode, safety rails.
- **M3 — Power.** Command palette + console, monitor, pub/sub, server dashboard, slowlog.
- **M4 — Scale & polish.** Cluster support, million-key performance work, themes, packaging
  and distribution.

## 10. Open questions

- Should Profiles be shareable across a team (checked-in config)? ADR-0002's reference-only
  credential path makes this plausible — is it worth designing for in v1?
- With one Connection per process, is there any in-app Profile surface left to build, or do
  `--profile`, a bare positional name, and shell completion cover it entirely?
- Is the Console worth building in v1, given that the terminal it is running in already has
  `redis-cli` one keystroke away?

**Resolved since v0.3** — the server floor (RESP3, Redis 6.0+; ADR-0007), Cluster vs. Sentinel
(Sentinel in v1, Cluster deferred; ADR-0008), connection lifecycle and startup failure
(ADR-0009), the keyspace memory model and the R2.5/R2.6 contradiction (ADR-0010), and the test
architecture (ADR-0011).

**Resolved since v0.2** — undo buffers (no; the command preview is the mechanism, and an
inverse-operation model per type is a large hidden surface that cannot be correct for every
type), export/import (no; now a stated non-goal), positional Profile names (yes; with one target
per terminal, launching *is* the interaction).
