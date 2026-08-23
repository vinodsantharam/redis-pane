# redis-pane — Product Requirements

**Status:** Draft v0.1 · **Owner:** Vinod Santharam · **Last updated:** 2026-08-23

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

## 5. Non-goals (v1)

- Not a Redis server manager (no config editing, no failover orchestration, no provisioning).
- Not a replacement for `redis-cli` scripting/piping in shell pipelines.
- Not a monitoring/alerting product — observability views are for triage, not retention.
- No plugin system, no embedded scripting language, no team/collaboration features.
- No Windows-native terminal work beyond whatever the TUI toolkit gives us for free.

## 6. Requirements

### 6.1 Connections
- **R1.1** Connect via URL (`redis://`, `rediss://`), host/port, or Unix socket.
- **R1.2** Named connection profiles in a config file; no GUI-form requirement to get started.
- **R1.3** Credentials resolved from env vars, config, or an external command (so secrets are
  never stored in plaintext by us).
- **R1.4** TLS, ACL usernames, and per-profile default database.
- **R1.5** Profile carries an **environment tag** (`local` / `staging` / `prod`) that drives
  chrome color and safety defaults.
- **R1.6** Cluster and Sentinel topology discovery; `MOVED`/`ASK` handled transparently.
- **R1.7** Multiple simultaneous connections, switchable without losing view state.

### 6.2 Keyspace browsing
- **R2.1** Incremental `SCAN` with live streaming results; never `KEYS`.
- **R2.2** Glob filter plus fuzzy filter over already-loaded keys.
- **R2.3** Optional hierarchical grouping by separator (`:` default) — `user:1:session` folds
  into a tree — toggleable with flat view.
- **R2.4** Per-key metadata inline: type, TTL, size/length, memory usage (lazily fetched).
- **R2.5** Sort by name, TTL, or size. Multi-select for bulk operations.
- **R2.6** Handle 1M+ key keyspaces without UI stall (virtualized rendering, bounded memory).

### 6.3 Value inspection
- **R3.1** Dedicated viewer per type: string, hash, list, set, sorted set, stream, bitmap,
  HyperLogLog, and JSON/`ReJSON` when the module is present.
- **R3.2** String auto-detection and pretty-printing: JSON, MessagePack, protobuf-ish binary
  fallback to hex+ASCII dump, gzip/snappy transparent decode where detectable.
- **R3.3** Collections render as paginated, sortable, searchable tables — not as a wall of text.
- **R3.4** Streams get an entry timeline with consumer-group state.
- **R3.5** Search within a value; copy key, value, or a ready-to-paste `redis-cli` command.

### 6.4 Mutation
- **R4.1** In-place edit for scalar values and collection members, with a diff-style confirm.
- **R4.2** TTL editing (set/persist/extend) as a first-class action.
- **R4.3** Rename, copy, move-across-db, delete — single and bulk.
- **R4.4** Every mutation shows the exact command it will run before it runs.
- **R4.5** **Read-only mode**, default-on for `prod`-tagged profiles, toggled explicitly.
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

## 7. Success metrics

| Metric | Target |
|---|---|
| Cold start → interactive keyspace | < 1s on a 100k-key database |
| First render of scan results | < 150ms after connect |
| Keystroke → visible response | < 16ms (one frame) for local interactions |
| Memory at 1M keys browsed | < 250MB RSS |
| Time to find a known key (new user, no docs) | < 30s |
| Binary size | < 20MB |

## 8. Risks

| Risk | Mitigation |
|---|---|
| Destructive action against prod | Env tagging, read-only default, scaled confirmation, command preview |
| Large-value rendering hangs the UI | Hard fetch caps with explicit "load more"; render off the input thread |
| Cluster semantics leak into UX | Topology-aware routing behind the scenes; surface node only where it matters |
| Feature sprawl reproduces RedisInsight's bloat | Non-goals are enforced; every feature must survive the "would an on-call use this?" test |
| Terminal capability fragmentation | Capability detection with graceful degradation; test matrix across common terminals |

## 9. Milestones

- **M0 — Skeleton.** App shell, event loop, theming, connection to a single instance, help overlay.
- **M1 — Browse.** Scan-based keyspace browser, tree/flat views, all core type viewers. *This is
  the milestone that already beats `redis-cli` for daily use.*
- **M2 — Mutate.** Editing, TTL management, delete/rename/copy, read-only mode, safety rails.
- **M3 — Power.** Command palette + console, monitor, pub/sub, server dashboard, slowlog.
- **M4 — Scale & polish.** Cluster/Sentinel, multi-connection, million-key performance work,
  themes, packaging and distribution.

## 10. Open questions

- Do we ship an export/import path (JSON, RDB-ish dump) in v1, or defer it?
- Is a session-scoped undo buffer for mutations feasible, or is command preview enough?
- Should profiles be shareable across a team (checked-in config), and what does that mean for secrets?
