# ADR-0008 — Sentinel in v1, Cluster deferred

**Status:** Accepted · **Date:** 2026-08-26

## Context

R1.11 promised Cluster and Sentinel together, as though they were one requirement of one size.
They are not.

**Sentinel** is discovery and failover. The client asks Sentinel for the master's address and
reconnects elsewhere when it moves. There is still one logical server, one keyspace, one `SCAN`
cursor, one `INFO`. A failover is a reconnect against a new address — a path
[ADR-0009](0009-connection-lifecycle.md) requires anyway.

**Cluster** is a different shape of problem, and it quietly reintroduces the multi-target
question that [ADR-0005](0005-one-connection-per-process.md) removed — not for Connections, but
for nodes:

- `SCAN` is per-node. A full browse is N concurrent cursors, and "41,203 of ~180,000" becomes a
  sum of per-node estimates.
- `INFO`, `DBSIZE` and `SLOWLOG` are per-node. The dashboard (R6.3) must therefore answer
  *whose* memory and *whose* hit ratio, which means a node selector — a switcher, inside a UI
  whose entire premise is that there is nothing to switch.
- `CLIENT TRACKING` is per-node; liveness must arm on whichever node owns the open key.

## Decision

**Sentinel ships in v1. Cluster is a stated v1 non-goal.**

One architectural obligation follows and is not optional: the keyspace source is an abstraction
over *a stream of keys with progress*, not over a cursor. Whether that stream is fed by one
cursor or by N merged cursors is invisible above the abstraction.

## Alternatives considered

**Both in v1, as R1.11 promised.** Rejected for v1, not on merit. Cluster users are real and are
exactly the people who need the million-key work. But it makes M1 — the milestone that first
beats `redis-cli` — materially harder, and it forces the node-selector question into the
dashboard before the dashboard has proven it belongs in v1 at all.

**Neither in v1.** Rejected. Sentinel is nearly free given the reconnect machinery, and dropping
it saves almost nothing.

**Cluster with the dashboard scoped to a single node.** Rejected as a half-measure that still
pays the full `SCAN` and tracking cost while shipping a dashboard that quietly answers a
different question than the user asked.

## Consequences

- R1.11 splits: Sentinel is a v1 requirement, Cluster moves to M4 and to the non-goals list.
- The keyspace-source abstraction is load-bearing and must exist from the first scan, even
  though v1 only ever has one cursor behind it. It is cheap now and a rewrite later.
- The dashboard stays coherent in v1 because there is exactly one node to describe.
- Sentinel failover surfaces as a reconnect with a changed address; the title bar's target
  readout updates, and the change is visible rather than silent — same principle as Source.
- When Cluster does arrive, this ADR is the record of what it has to solve: per-node dashboards,
  merged progress, and per-node tracking.
