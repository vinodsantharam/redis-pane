# ADR-0011 — Functional core, golden-frame tests

**Status:** Accepted · **Date:** 2026-08-26

## Context

A TUI's layout is not decoration here — it is substantially the product. The column alignment,
the responsive breakpoints, the monochrome fallback and the header states carry the design's
promises, and all of them are the kind of thing that rots silently because nobody looks at 80
columns until they are stuck in 80 columns.

`ratatui` provides `TestBackend`, which renders a frame into a `Buffer` that can be asserted on
or snapshotted. That is only useful if `render(state) → frame` is genuinely a function of state.
Two things threaten that: I/O in the render path, which CLAUDE.md already forbids, and *time* —
TTL renders as a local countdown and the degraded header shows Read age, so a frame rendered at
14:22:07 never matches itself again.

The design mockups produced during planning are, structurally, golden frames already: character
grids generated deterministically from fixed state.

## Decision

**A functional core with imperative shells.** The core takes a message and returns new state plus
commands to execute. The terminal and the Redis connection are shells around it; neither is
reachable from the core.

**The clock is injected**, from the first commit.

**Golden-frame snapshots are the primary UI test.** Frames are rendered through `TestBackend` and
compared against committed fixtures, covering each responsive breakpoint, the monochrome and
light themes, and the header states — liveness, disconnected, Read-only reason, deleted key.

**Integration tests run against real Redis via `testcontainers`**, scoped to what only a real
server can prove: `SCAN` streaming and resumption, tracking invalidation and re-arming across a
reconnect, capability probing where `CLIENT TRACKING` is refused, and error mapping for
`-LOADING`, `-OOM`, `-MISCONF` and `-READONLY`.

## Alternatives considered

**Integration-first, end to end.** Rejected as the primary surface. It tests what ships and makes
error states genuinely reachable — `maxmemory 1mb` produces a real `-OOM` — but it needs Docker
for nearly every test, feedback is slow, and nothing pressures the codebase toward a clean seam.
Retained as the secondary suite for exactly the cases above.

**State-only testing, no frame assertions.** Rejected. It is the fastest suite and immune to
snapshot churn, but it would leave the layout — the part of this product that took the most
design effort — with no test at all.

**Snapshot everything, including integration output.** Rejected as brittle for a different
reason: snapshots of live server output encode incidental values and fail for reasons unrelated
to the change under review.

## Consequences

- Purity of the render path and injectability of the clock are architectural commitments, not
  testing preferences. Both are trivial on day one and invasive later.
- Snapshot churn on intentional layout changes is real, and is the point: a diff of a rendered
  terminal is the most reviewable artifact this project can produce. Tooling should make
  accepting a reviewed change cheap.
- The design mockups and the test fixtures converge on one representation, so the spec and the
  suite cannot drift apart without someone noticing.
- Randomness, terminal size and capability detection are injected on the same grounds as the
  clock.
- Docker is required for the integration suite but not for the default `cargo test` run, so the
  fast loop stays fast.
