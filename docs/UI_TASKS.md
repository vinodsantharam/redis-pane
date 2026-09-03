# UI task tracker

Working list of UI/functionality gaps, audited against DESIGN.md and PRD.md on 2026-09-03.
Ordered by severity. This is a tracking doc, not a spec — see PRD.md/DESIGN.md for the actual
requirements and ADRs for decisions already made.

## Severity 1 — the current UI actively misleads or breaks

- [ ] **Search within a large value is missing (R3.3).** A 500-entry zset or a 12,000-row list
  is currently a wall of unfilterable, unsortable rows. `Viewer::row_count()`/`row()` exist, but
  there is no `/`-style filter *inside* the value pane the way there is for the key list.
- [ ] **Keys-pane liveness is unresolved (DESIGN §9).** A value updates live once opened, but a
  key being deleted or appearing in the browsed list is invisible until the next manual scan —
  the same "quietly stale" problem ADR-0006 exists to prevent, one level up.

## Severity 2 — real functional gaps in what's shipped

- [ ] **Streams have no timeline (R3.4).** Renders as generic `ID | FIELDS` rows; no
  time-relative view, no consumer-group state (`XINFO GROUPS`/`XPENDING`).
- [ ] **Mouse support is entirely absent (R7.3).** No click-to-focus, scroll, or drag-to-resize.
- [ ] **The split ratio is fixed, not resizable** (hardcoded 45/55). Blocks mouse
  drag-to-resize from being useful once built.

## Severity 3 — parked design questions, now answerable from real use

- [ ] **Tree vs. flat as the default view** (DESIGN §9) — decidable now from real usage.
- [ ] **The scan cap has no dedicated visual surfacing** beyond a status-bar line.
- [ ] **Sub-70-column behavior is minimal** — single-pane works, no breadcrumb/stack nav.

## Severity 4 — polish

- [x] **Binary/hex viewer navigation.** `⌃PgDn`/`⌃PgUp` page by 20 rows, `⌃Home`/`⌃End` jump
  to start/end — shared by every viewer, not just binary. Search-by-byte was scope-cut into
  severity 1's general search-within-value rather than built as a one-off.
- [x] **Visual distinction for a key currently mid-edit.** Header now reads `✎ editing` (or
  `✎ editing · changed · held` with a pending update) instead of a plain `● live`. Inert until
  M2 builds an actual editor — nothing sets `OpenKey::editing` yet — but correct on day one.
- [x] **256-color/monochrome hardened and unit-tested.** Extracted a pure `resolve_color_depth`
  so the TERM/COLORTERM decision is tested without mutating the environment; `NO_COLOR`
  (no-color.org) is now honoured. A real terminal glance is still worth doing — see below.

---

*Update this file as items are picked up or closed. Check the box and leave a one-line note with
the commit that addressed it.*
