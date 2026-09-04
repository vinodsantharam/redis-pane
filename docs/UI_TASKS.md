# UI task tracker

Working list of UI/functionality gaps, audited against DESIGN.md and PRD.md on 2026-09-03.
Ordered by severity. This is a tracking doc, not a spec — see PRD.md/DESIGN.md for the actual
requirements and ADRs for decisions already made. `[~]` marks an item partially closed on
purpose — what was done, and what was cut and why, are in its own note rather than a new item.
`[–]` marks an item cut from scope, with the reasoning kept so the decision survives.

## Severity 1 — the current UI actively misleads or breaks

- [–] **Search within a large value (R3.3) — cut from scope 2026-09-04.** Not deferred for time;
  it does not decompose cleanly, and the research is worth keeping:
  - **Redis offers no uniform mechanism.** `HSCAN`/`SSCAN`/`ZSCAN` take `MATCH` (this is what
    RedisInsight uses), but there is no `LSCAN`, and streams are ID-ranged rather than
    content-searchable. Three of eight types can be done server-side; the rest cannot.
  - **The types are asymmetric in a way that inverts the payoff.** Hash and Set are fetched
    *whole* today (`HGETALL`/`SMEMBERS`), so a client-side filter there would be completely
    correct — and they are also the types you can simply scroll. List, ZSet and Stream are
    windowed at 500, so a filter would search a slice and report a verdict on the whole. The
    types where search is easiest to get right are the ones that need it least.
  - **`/` already means "filter the key list".** A second meaning inside the value pane is a
    keymap ambiguity on top of the above.
  - What *was* built instead is the honesty half — see the value-window disclosure below, which
    was the part that actively misled and is not a search feature.
- [x] **Keys-pane liveness (DESIGN §9) — resolved 2026-09-04.** Decided rather than deferred:
  the keys pane is **not** push-live, matching RedisInsight, which declines to auto-refresh its
  key list on purpose to avoid loading production instances. Viewport-scoped `CLIENT TRACKING`
  was rejected on tracking-table churn during scroll and would need its own ADR — ADR-0006
  weighed only "one key" against "everything read" and never this middle ground. Two things were
  built in its place:
  - **Free deletion detection.** `fetch_metadata` already issued `TYPE` for every visible row and
    already saw `"none"` for a vanished key — and dropped it on the floor. It now reports those
    rows, so a key deleted underneath the reader is badged `✕ … gone` at **zero extra round
    trips and no tracking table**. The tombstone rides in the existing `kinds` byte array, so it
    costs nothing across a 2M-key set. The row keeps its position (removing it would renumber
    everything below the cursor) and its last-known size, but drops its TTL to `—`: size is
    retrospective and stays true, a countdown is a claim about a key that is not there to expire.
  - **R2.7's rescan, which had never been wired up.** The PRD has always said `r` rescans in the
    keys pane and Refetches in the Viewer; `Action::Refetch` returned `RefetchOpenKey`
    unconditionally and the core never once constructed `Command::StartScan`, leaving its shell
    handler unreachable. `r` now acts on the focused pane, and the hint bar says which
    (`r rescan` vs `r refetch`) rather than naming one half of it at all times.
- [x] **The value header passed a window off as the whole value.** The viewer header printed
  `LLEN`/`ZCARD`/`XLEN` — the real length — over a body capped at 500 fetched rows, with nothing
  on screen admitting the gap: a 12,000-item list read "12,000 items" above 500 rows. This is the
  scan-cap defect one level down, and it is why a naive search over those rows would have
  compounded into "no matches" being indistinguishable from "past the window". The header now
  reads `12,000 items · 500 shown`, in Warn rather than Muted for the same reason the cap banner
  is. Types fetched whole say nothing extra.

## Severity 2 — real functional gaps in what's shipped

- [~] **Streams have no timeline (R3.4).** Partially closed — scoped down to what could be
  done properly in one pass; two real pieces of DESIGN §6.3's target are explicitly not built.
  **Done:** reverse-chronological order (`XREVRANGE`, not `XRANGE` — this was not just an
  ordering preference: `XRANGE("-", "+", COUNT)` takes the *oldest* `COUNT` entries, so a stream
  past the window was silently showing ancient history instead of recent activity, verified
  against a real container). A live **AGE** column, computed from the millisecond timestamp Redis
  embeds in every entry ID — no fetch, no stored state, ticking between frames exactly like the
  TTL countdown (R3.9). **Not done, cut deliberately rather than half-built:** expandable fields
  (an interactive drill-down, its own feature) and the consumer-group panel (`XINFO GROUPS`/
  `XPENDING` — a separate fetch and a separate view, not a per-entry field).
- [ ] **Mouse support is entirely absent (R7.3).** No click-to-focus, scroll, or drag-to-resize.
- [ ] **The split ratio is fixed, not resizable** (hardcoded 45/55). Blocks mouse
  drag-to-resize from being useful once built.
- [ ] **Hash and Set reads are unbounded.** `HGETALL`/`SMEMBERS` pull the entire collection,
  where List/ZSet/Stream are windowed at 500 (`read.rs`, `WINDOW`). A million-field hash comes
  down whole, into a 250MB budget (PRD §7), on the same connection the scan is using. Noticed
  while scoping value search (above), where the same asymmetry is what made the feature not
  decompose. The fix is `HSCAN`/`SSCAN` with a cursor, which would also be the server-side half
  of a future search — so these two are worth doing together or not at all.

## Severity 3 — parked design questions, now answerable from real use

- [x] **Tree vs. flat as the default view** — tree, decided by the user from real usage.
  `tree_mode: true` set at session start in `main.rs` (not on `State::default()`, which many
  tests rely on as a blank slate). DESIGN §9 and PRD R2.3 updated.
- [x] **The scan cap gets a persistent banner** above the key list — `⚠ 2,000,000 key limit
  reached — narrow the filter` — reserved only while capped (G7), and immune to being displaced
  by a copy confirmation or sort readout the way the status-bar line alone was. Correctly
  survives filtering/sorting/tree-toggle, since none of those re-scan.
- [x] **Sub-70-column stack navigation.** `Open` pushes into a full-width value pane with a
  breadcrumb header (`Esc back · key-name`, following the effective binding per R7.5) replacing
  the column headers there is no room for; `Esc` pops back — after closing help or dismissing an
  error, ahead of cancelling an unrelated scan. `SinglePaneView` is inert at any wider density
  (tested directly). Existing Density::Single flat/tree browsing was already built and is
  untouched — all its golden fixtures pass byte-for-byte unmodified.

**Severity 3 fully closed (2026-09-04).**

## Severity 0 — spec/code drift (found 2026-09-04, closed same day)

- [x] **`type.*` and `accent` (selection) tokens were specified but never implemented.**
  CLAUDE.md's own example of a semantic token was `Token::TypeHash`, and it did not exist —
  every key rendered in plain text colour regardless of type, and "selected" was a foreground
  change, not the highlight bar DESIGN §5 describes. Added a colour dot before every key name
  (● known type, · pending — same convention as every other lazily-fetched cell), one hue per
  Redis type (`type.*`, consistent in the keys pane and the value pane header), and a full-row
  `Token::Selected` highlight (dark-on-amber in colour, reverse video in monochrome). Verified at
  the buffer-cell level that the highlight has no gaps and that distinct types render with
  distinct hues — not just that the code compiles.

## Severity 4 — polish

- [x] **Binary/hex viewer navigation.** `⌃PgDn`/`⌃PgUp` page by 20 rows, `⌃Home`/`⌃End` jump
  to start/end — shared by every viewer, not just binary. Search-by-byte was scope-cut into
  severity 1's general search-within-value rather than built as a one-off.
- [x] **Visual distinction for a key currently mid-edit.** Header now reads `✎ editing` (or
  `✎ editing · changed · held` with a pending update) instead of a plain `● live`. Inert until
  M2 builds an actual editor — nothing sets `OpenKey::editing` yet — but correct on day one.
- [x] **256-color/monochrome hardened and unit-tested.** Extracted a pure `resolve_color_depth`
  so the TERM/COLORTERM decision is tested without mutating the environment; `NO_COLOR`
  (no-color.org) is now honoured. Confirmed sane by eye against a real terminal at both
  `TERM=xterm-256color` and `TERM=dumb` (2026-09-04).

**Severity 4 fully closed (2026-09-04).**

---

*Update this file as items are picked up or closed. Check the box and leave a one-line note with
the commit that addressed it.*
