# ADR-0005 — One Connection per process

**Status:** Accepted · **Date:** 2026-08-24

## Context

The v0.2 PRD promised multiple simultaneous Connections switchable without losing view state
(R1.12), and DESIGN.md gave a permanent sidebar to Profiles, live Connections and databases.
Both assumed that moving between targets is something that happens during a session.

It is not how the tool is used. The target is decided before the process starts — by a flag, a
Profile name, or the environment — and it does not change afterwards. Looking at a second
database or a second server means opening a second terminal, which is already how every other
tool in the terminal behaves.

Multi-target support is not free. It forces a connection registry, per-Connection view state,
a switcher UI, disambiguation in every error message and notification, and a decision about what
`Ctrl-R` and the Environment chrome mean when two Environments are open at once. That last one
is a safety question, not an ergonomic one: a read-only badge is only trustworthy when there is
exactly one thing it can be describing.

## Decision

Exactly one Connection per process, against exactly one database, fixed at launch.

There is no connection switcher, no `SELECT`, no tabs, and no sidebar. The Palette does not
offer a "switch to Profile" action. The database is part of the target, chosen the same way the
host is.

The sidebar is removed entirely; the layout is two panes.

## Alternatives considered

**Multiple Connections behind tabs or a sidebar.** Rejected. It is the single largest source of
incidental state in the application, it costs permanent screen space, and it weakens the
Environment signal — the mechanism the whole safety model rests on
([ADR-0004](0004-untagged-connections-are-read-only.md)).

**One Connection, but an in-app database switcher.** Rejected as a false economy. `SELECT` looks
cheap, but it invalidates the key list, the current value, the filter, the scroll position and
the cached metadata — which is a connection switch wearing a smaller name. `--db` at launch, or
a second terminal, is the same operation with none of the machinery.

**Keeping a secondary read-only Connection for cross-server diffing.** Rejected for v1. It is a
genuinely useful migration workflow, but it reintroduces a second connection lifecycle for a
narrow case, and nothing about this decision makes it harder to add later as an explicit,
purpose-built feature rather than as general multi-connection support.

## Consequences

- R1.12 inverts: it now states the constraint rather than the capability. `SELECT` is not
  implemented, and a Connection's database appears in the title bar as a fact, not a control.
- The sidebar comes out of DESIGN.md §2, and its two open questions dissolve with it.
- Launch ergonomics become load-bearing, since launching is now the only way to change target.
  A bare positional Profile name (`redis-pane staging`) is part of this decision, not a
  convenience.
- Session state persists per target, so `redis-pane staging` restores staging's filter and
  scroll without leaking them into a prod session.
- Cluster support (R1.11) is the one place this strains — a cluster is many nodes behind one
  logical target. Routing stays hidden; if that proves untenable, this ADR is what to revisit.
- Adding multi-connection back is expensive by design. That is the point: the cost is being paid
  once, now, in exchange for everything else staying small.
