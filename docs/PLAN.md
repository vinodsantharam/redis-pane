# redis-pane — Implementation plan, M0 and M1

**Status:** Draft v0.1 · **Companion to:** [PRD.md](PRD.md), [DESIGN.md](DESIGN.md) ·
**Last updated:** 2026-08-26

This plan covers the two milestones that produce a usable product. M0 is an internal checkpoint —
it connects and browses nothing, so it has a test suite rather than users. M1 is the milestone
that already beats `redis-cli` for daily work.

Every task names what it *proves*. A task without a proof is a task that will be re-done.

## 1. Approach

**One vertical slice first.** M0 builds a single thin path end to end — resolve, connect, probe,
render one frame — before any breadth. The architecture is unusual enough (functional core,
injected clock, columnar arena, push liveness) that proving it end to end early is worth more
than completing any horizontal layer.

**Liveness transport lands in M0, not M1.** It is the riskiest assumption in the project:
capability probing, push handling, and re-arming. The transport was validated against Redis 8.4.0
before this plan was written — see the Verification table in
[ADR-0006](adr/0006-liveness-without-a-refresh-button.md), which confirmed the mechanism and
turned up one behaviour the design had not accounted for: tracking is consumed by its own
invalidation, so every Refetch must re-arm. M0 builds the connection layer anyway, so the
transport is proven there and M1 only adds the interface on top.

**Golden frames from the first screen.** The design mockups already produced during planning are
character grids generated deterministically from fixed state — structurally the same artifact as
a `TestBackend` snapshot. The suite starts as soon as anything renders.

## 2. Workspace layout

The core/shell boundary is enforced by the compiler, not by review
([ADR-0011](adr/0011-functional-core-golden-frames.md)). `redis-pane-core` does not depend on
`tokio`, `fred` or `crossterm`, so I/O in the render path is a compile error.

```
redis-pane/
├── Cargo.toml                 workspace
├── crates/
│   ├── core/                  redis-pane-core  — pure
│   │   ├── state/             App state, Loaded set arena, Viewer state, Connection state
│   │   ├── msg.rs             Msg — everything that can happen
│   │   ├── command.rs         Command — everything the shells must do
│   │   ├── update.rs          update(State, Msg) -> (State, Vec<Command>)
│   │   ├── render/            panes, Viewers, title bar, hint bar
│   │   ├── theme/             semantic tokens, palettes, capability degradation
│   │   ├── keymap/            bindings as data
│   │   ├── config/            schema types + validation
│   │   ├── resolve.rs          flags -> Profile -> environment -> localhost
│   │   └── clock.rs           Clock trait
│   └── app/                   redis-pane — shells
│       ├── main.rs            args, resolution entry, startup diagnostics, exit codes
│       ├── terminal.rs        crossterm, raw mode, event loop, resize
│       ├── redis/             fred client, executor, capability probe, tracking
│       ├── config_io.rs       file read, permission refusal
│       └── state_file.rs      $XDG_STATE_HOME persistence
└── tests/                     integration, testcontainers
```

`ratatui` belongs in core: it draws into a buffer and performs no I/O. Config *types and
validation* are core and pure; config *reading* is a shell.

## 3. M0 — Skeleton

Proves: the architecture holds, and the app can be trusted about what it is connected to.

**Progress: complete.** The boundary is enforced by CI, the clock is injected, resolution and
config parsing carry their full test tables, the Redis shell connects over RESP3 and probes for
`CLIENT TRACKING`, both re-arm invariants are asserted, and every readout in DESIGN §6.8 is a
recorded golden frame.

| # | Task | Proves |
|---|---|---|
| 1 | Workspace scaffold, CI running `fmt`, `clippy -D warnings`, `test` | The boundary compiles; core has no I/O deps |
| 2 | `Clock` trait, injected everywhere | Two renders at different wall-clock times produce identical frames |
| 3 | Theme tokens + truecolor / 256 / monochrome degradation | Golden frames of one screen in all three modes |
| 4 | `Msg`, `State`, `Command`, `update` skeleton | A state transition test with no runtime attached |
| 5 | Terminal shell: raw mode, event loop, resize, quit | Synthetic events drive `update` without a terminal |
| 6 | Connection resolution chain, flags → Profile → env → localhost, carrying **Source** | Table-driven tests over the whole precedence matrix (R1.2, ADR-0001) |
| 7 | Config schema, strict parse rejecting unknown fields, permission refusal | Line/column errors; a typo'd `passwordEnv` fails loudly; group-readable file refused (R1.5, R1.7) |
| 8 | Redis shell: `fred`, RESP3, version floor, **capability probe** | Connects to 6.2 and 7.x; Redis 5 is refused with a diagnostic rather than a protocol error; a server refusing `CLIENT TRACKING` degrades to `○ manual` (R1.13, ADR-0007) |
| 9 | Startup diagnostics and exit codes | Unreachable target exits non-zero with target, Source and cause on stderr (R1.14) |
| 10 | Reconnect with visible backoff, **both re-arm invariants** | Server killed mid-session: the header never reads `● live` until tracking is re-armed. A second write after an invalidation still produces a push, proving the Refetch re-armed (ADR-0006, ADR-0009) |
| 11 | Title bar: Environment dot, target, db, Source, Read-only reason | Golden frames of every readout in DESIGN §6.8, including `replica … locked` |
| 12 | Help overlay, keymap as data, hint bar | An overridden binding changes the on-screen hint (R7.5) |

**Done when** `redis-pane staging` opens against a real server, shows exactly what it is
connected to and why, survives the server being restarted underneath it, and exits usefully when
it cannot connect. No keyspace. — **Met.**

## 4. M1 — Browse

Proves: the keyspace is legible, the values keep their shape, and the screen is never lying about
how current it is.

**Progress: 1–4 complete.** The Loaded set holds a million keys in 36MB, the keyspace source
streams, filters and cancels against a real server, and the browser renders at every breakpoint
with metadata filling in behind placeholders that hold their column.

| # | Task | Proves |
|---|---|---|
| 1 | Columnar Loaded set: byte arena + parallel metadata arrays, hard cap | 1M synthetic keys inside the memory budget; cap stops scanning and says so (R2.6, ADR-0010) |
| 2 | Keyspace source: a stream of keys with progress, `SCAN` driver behind it | 100k-key scan streams, resumes, and cancels on `Esc`; the abstraction hides the cursor count (R2.1, ADR-0008) |
| 3 | Virtualized key list, four columns, responsive breakpoints | A golden frame in each band of DESIGN §2 — 130, 119, 100, 80, 70, 60 — and a frame drawn from a 200k-key Loaded set in under 16ms (R2.4, R2.6, R7.1) |
| 4 | Lazy metadata fetch with placeholders | Pending cells render without shifting layout, and only the visible window is ever fetched — three pipelined commands per row, not per keyspace (R2.4) |
| 5 | Filter: glob and fuzzy over the Loaded set | Unit tests plus a golden frame |
| 6 | Tree / flat toggle, prefix index over the same arena | No second copy of the key names (R2.3) |
| 7 | Sort across the Loaded set | Sorting a lazily-fetched column orders what arrived, parks the rest, states the count — and issues no mass fetch (R2.5) |
| 8 | `Viewer` trait and shared frame — header, body, footer | Navigation transfers between types because only the body changes (R3.1) |
| 9 | Type Viewers: string, hash, list, set, zset, stream, JSON, binary | A golden frame per type against a testcontainer fixture |
| 10 | Liveness UI on M0's transport: arm on open, apply / announce / held, deleted retention | Golden frames of all seven header states; a key modified externally lands without a keypress (R3.6–R3.11) |
| 11 | Local TTL countdown | Injected-clock test; no round trip |
| 12 | `y` copy — key, value, `redis-cli` command | Clipboard payloads are exact |

**Done when** a 100k-key keyspace is browsable in under a second, every type renders as itself,
and a key changing on the server updates on screen without anyone pressing anything.

## 5. Explicitly not in M0 or M1

Mutation, editing and confirmation (M2). Palette, Console, dashboard, monitor, pub/sub, slowlog
(M3). Cluster, themes beyond the two defaults, packaging (M4). Keys-pane liveness, and the other
open questions in [DESIGN §9](DESIGN.md) — all decidable later without rework, which is why they
are still open.

## 6. Risk order

The tasks most likely to invalidate something already decided, earliest first:

1. **M0.8 and M0.10 — capability probe and re-arm.** If tracking does not behave as ADR-0006
   assumes, the product's headline feature changes shape. Deliberately in M0.
2. **M1.1 — the columnar arena.** It shapes every access to the key list and is the most
   expensive thing here to retrofit.
3. **M1.2 — the keyspace source abstraction.** Cheap to build now, a rewrite once Cluster needs
   N cursors.
4. **M0.2 — the injected clock.** Trivial on day one; invasive after a hundred call sites render
   a TTL.
