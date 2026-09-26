# M3 — Power: summary and feature plan docs

## Context

M2 (Mutate) is done. The user wants (1) a summary of what M3 covers and (2) a set of
planning documents for M3's features, prepared the way M2's tasks got individual docs
under `docs/plans/` before each was built.

M3 is almost entirely unplanned today — unlike M0/M1/M2, it has no task table in
`docs/PLAN.md`, just a one-line scope mention. Per-feature detail across PRD/DESIGN/ADRs
ranges from a few sentences to nothing at all (see summary below). Two scope questions
were open in the docs themselves; the user has now resolved both:

- **Dashboard** (R6.3): DESIGN.md §9 flags it as "the largest remaining scope risk" and
  suggests an alternative (status-bar memory figure + Slowlog only). **Decision: plan the
  full Dashboard, but the plan doc must flag the descope alternative prominently so it can
  be chosen instead at build time.**
- **Console** (R5.2–R5.4): PRD.md's open questions challenge whether it's worth building
  given `redis-cli` is one keystroke away in the same terminal. **Decision: drop the
  Console from M3. Palette (R5.1) ships (it later shipped and was withdrawn — ADR-0020); Console
  is cut, not deferred-unlabeled — the PRD's open question gets resolved in the docs, not left
  dangling.**

## M3 summary

PRD.md's one-line scope: *"Command palette + console, monitor, pub/sub, server dashboard,
slowlog."* With Console cut, M3 becomes five features:

| Feature | Requirement | What exists in the docs today |
|---|---|---|
| Palette | R5.1 | Full CONTEXT.md glossary entry; nav model in DESIGN §3; "overlay" decided; ADR-0005 forbids a profile-switch action in it |
| Dashboard | R6.3 | 4-sentence prose in DESIGN §6.6, no mockup; ADR-0008 fixes it to single-node scope (Cluster deferred to M4); flagged as the biggest open scope risk |
| Monitor | R6.1 | 2-sentence prose in DESIGN §6.7 (shared with Pub/Sub); "expensive, must warn" |
| Pub/Sub | R6.2 | same 2-sentence prose, no distinct layout described |
| Slowlog | R6.4 | no prose at all — only a keymap chord (`g s`) and ADR-0008's single-node caveat |

Common thread: every one of these needs its screen designed close to from scratch. None
has an ADR of its own. Monitor/Pub-Sub/Dashboard also raise one architecture question none
of M0–M2 faced: **they're push/poll feeds, not request-response reads** — `MONITOR` and
`SUBSCRIBE` occupy a Redis connection exclusively for their duration (a subscribed/
monitoring connection can't run ordinary commands), so each needs its own dedicated
connection, separate from the app's single main `fred::Client` — this wants a decision
before any of the three is built, not per-feature.

## Deliverables

1. **`docs/PLAN.md`** — insert a new `## 6. M3 — Power` section (task table, in the same
   format as M0/M1/M2's), renumbering the current §6 ("Explicitly not in M0, M1 or M2") to
   §7 and "Risk order" to §8. The M3 table:

   | # | Task | Proves |
   |---|---|---|
   | 1 | Palette (`Ctrl-K`): fuzzy list over every app action, reading the same keymap-as-data source the hint bar uses (CLAUDE.md's "Keybindings are data") | Every bound action is reachable via the Palette; hint bar and Palette never disagree on the effective binding |
   | 2 | Dedicated-connection plumbing for push/poll feeds: a second `fred::Client` (or equivalent) the shell can hand to Monitor/Pub-Sub without starving the main read/write path | The main connection keeps answering ordinary reads/writes while a feed connection is open; closing the feed view tears down its connection cleanly |
   | 3 | Slowlog viewer (`g s`): `SLOWLOG GET`/`RESET`, sort, single-node (ADR-0008) | Entries render in a type-aware-consistent frame; `RESET` is a real mutation (confirm dialog, read-only refusal) |
   | 4 | Monitor (`g m`): live tail, filter box, pause/resume, bounded buffer with a visible cap, persistent cost-warning banner | Buffer never grows unbounded; pausing stops consuming the feed, not just hides it; the warning is impossible to miss |
   | 5 | Pub/Sub (`g p`): subscribe to channels and patterns, live tail | Distinct from Monitor's layout (not just "Monitor with a different source"); unsubscribing on view-close leaves no orphaned subscription |
   | 6 | Dashboard (`g d`): `INFO`-based tiles — memory used/peak/maxmemory bar, hit ratio, ops/sec sparkline, clients, replication role/lag, eviction/expiry counters, single-node (ADR-0008) | Alarming values are colored; every tile expands to its raw `INFO` section; refreshes on an interval, not static — **flag at the top of this task's plan doc**: the documented alternative (skip the Dashboard, add a memory figure to the status bar, rely on the Slowlog for triage) is still on the table and should be re-decided before work starts, not assumed away by this plan existing |

   Note under the table: Console (R5.2–R5.4) is explicitly out of scope for M3 — see the
   PRD.md change below for the resolved rationale.

2. **`docs/PRD.md`** — resolve, don't just leave, the open question. In §10 Open
   questions, replace *"Is the Console worth building in v1, given that the terminal it is
   running in already has `redis-cli` one keystroke away?"* with a short **Resolved**
   note: Console cut from M3/v1 for that reason; R5.2–R5.4 stay in the requirements list
   as unbuilt-and-not-currently-planned (not deleted — a real future requirement, just not
   scheduled), with a pointer to this decision. Matches CLAUDE.md's "when a feature
   diverges from these docs, update the doc in the same change."

3. **One `docs/plans/m3-*.md` per feature** (mirroring `docs/plans/m2-task*.md`'s pattern:
   context, approach, files to touch, tests), for the five tasks above:
   - `docs/plans/m3-palette.md`
   - `docs/plans/m3-feed-connection.md` (the dedicated-connection plumbing task 2 — shared
     infrastructure, written as its own doc since Monitor and Pub-Sub both depend on it)
   - `docs/plans/m3-slowlog.md`
   - `docs/plans/m3-monitor.md`
   - `docs/plans/m3-pubsub.md`
   - `docs/plans/m3-dashboard.md` — opens with the descope alternative as a named decision
     point, not buried in prose, per the user's answer above.

   Each doc follows the existing M2 plan docs' shape: what problem it solves and why now,
   the concrete approach (core state additions, shell/`Command`/`Msg` additions, keymap
   entry, golden-frame coverage), and explicit callouts where CLAUDE.md's architecture
   rules bind (functional core / imperative shell, render loop does no I/O, cancellable
   with `Esc`, colors as semantic tokens, screen space as a budget — R7.7 means every one
   of these five is an overlay/view reached via `g`-chord or Palette, never a third
   permanent pane).

   Also note in each doc, where relevant: `CONTEXT.md` has no glossary entries for
   Monitor, Dashboard, Pub/Sub, or Slowlog today (only Palette and Console are defined) —
   each plan doc should propose the term's definition and its "Avoid" list, to be added to
   `CONTEXT.md` when that feature's implementation actually starts (per the
   domain-modeling practice CLAUDE.md points to), not invented ad hoc in code/UI strings
   later.

## Verification

This is a documentation-only change — no code, no tests to run. Verification is
consistency: the new `docs/PLAN.md` M3 table uses the same column format as M0–M2's
tables; the PRD.md edit is a resolution of an existing open question, not a deletion of a
requirement; each `docs/plans/m3-*.md` stays internally consistent with what PLAN.md's
table says that task proves.
