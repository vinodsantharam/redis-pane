# UI task tracker

Working list of UI/functionality gaps, audited against DESIGN.md and PRD.md on 2026-09-03.
Ordered by severity. This is a tracking doc, not a spec — see PRD.md/DESIGN.md for the actual
requirements and ADRs for decisions already made. `[~]` marks an item partially closed on
purpose — what was done, and what was cut and why, are in its own note rather than a new item.
`[–]` marks an item cut from scope, with the reasoning kept so the decision survives.

## Severity 1 — the current UI actively misleads or breaks

*Re-audited 2026-09-05 by reading the code against the docs, after the tracker showed severity 1
clear and the app still had seven of them. The original audit was written on 2026-09-03; **every
severity-1 item since has been found by using the app or auditing it afresh, never by the
checkboxes**. Treat this section's emptiness as a prompt to look again, not as a result.*

- [x] **The disconnect header promised a retry that would never come — 2026-09-05.** With the
  server taken away the title bar read `✕ disconnected · retry 0s   r refetch`, permanently.
  `Command::Reconnect` is a no-op until M2 and fred is built with no retry policy, so nothing was
  retrying and nothing ever would; `r` read a dead client and produced an error. Worse, the
  countdown *displaced* the Read age, so the one moment the age matters most was the one state
  not showing it (ADR-0009 specifies `✕ disconnected · last read …`). `retry_in_ms` is now
  `Option<u64>` so the countdown cannot be rendered for a retry nobody arranged, and the Refetch
  key is offered only where it can work. The golden fixture had been asserting `retry 4s` from a
  state the running app cannot reach — a test holding a promise the code never made.
- [x] **Keys acted on panes that were not on screen — 2026-09-05.** Below 70 columns one pane is
  drawn at a time, and nothing stopped a key scoped to the other one from running anyway. `/` in
  the Viewer there started a filter capture inside a zero-width pane: every keypress after it
  vanished, `q` typed a `q`, and the app was indistinguishable from hung — two keypresses from
  launch. Quieter versions: `↓` moved the key cursor invisibly, so `Esc` returned to a list that
  had wandered; and on the list screen `⌃↓` scrolled a value nobody could see. Actions now declare
  their pane and are dropped when it is not drawn. **Not** a change to what focus means — the
  two-pane keymap is modeless on purpose and is untouched.
- [x] **A Refetch that found nothing rendered a byte-identical frame — 2026-09-05.** ADR-0006's
  founding complaint, reproduced: the reader could not tell "the refresh did nothing" from "the
  value did not change" — nor from the reply being dropped as superseded, nor from a failure whose
  notice was missed. DESIGN §6.4 had specified `● live · updated now` and `○ manual · unchanged`
  from the start; neither existed, and `absorb()` discarded the comparison needed to choose. Now
  recorded and stated, in all four liveness/outcome corners, fading after a couple of seconds
  because it is an account of an event rather than a description of the key.
- [x] **The clipboard reported success it had not had — 2026-09-05.** Three holes: `y v` copied
  the ≤500-row window and said `copied value` (the Viewer header two lines above was truthfully
  saying `500 shown`); a failed clipboard write produced nothing at all, so failure looked like
  success (R7.4); and `nothing open to copy` was built with `at_ms: 0` against a wall clock, so it
  could never appear — its test passed by reading the notice at clock 0, the one reading where the
  defect is invisible. Copying the whole value would need an unbounded re-read (see severity 2),
  so the confirmation states its window instead: `copied value (500 of 12000 items)`.
- [x] **`r` in the keys pane did not rescan with an update waiting — 2026-09-05.** The held-update
  branch returned before the focus check, so `r` applied a value in the *other* pane while the
  hint bar said `r rescan` and the list did not move. The same defect 6d665a3 fixed, surviving in
  the one branch above the check it added.
- [x] **The Open key disclosure did not exist below 70 columns — 2026-09-05.** A gap in 48a1102,
  three commits old. The chip was drawn only in the two-pane branch, and at single-pane width the
  divider, tie glyph and row underline are absent by construction — leaving the wash, which is
  nothing in monochrome. Every fixture written for that feature was 130 or 80 columns, so the
  monochrome tests that looked like proof only ever covered the two-pane case.
- [x] **A key deleted and then recreated left the panes disagreeing — 2026-09-05.** The direction
  958b311 did not cover: a tombstoned row has no type, so the next cursor move refetches it and
  the row returns to `● string 64 B ∞` while the Viewer still reads `✕ deleted 40s ago`; with
  tracking unavailable nothing corrected it. Both directions now resolve by asking the server
  rather than copying one pane's opinion onto the other.
- [x] **A password could reach stderr — 2026-09-05.** `redis/mod.rs` interpolated the dial URL
  into the URL-parse failure message. Reachable only on a malformed URL, which is exactly when
  someone has pasted one by hand with a real secret in it and is about to paste the error into a
  bug report. Redacted, like every other path.

- [x] **The value pane could be showing a key the cursor is not on, with nothing saying so —
  reported from use, fixed 2026-09-05.** Diagnosed rather than patched: the value on screen was
  never wrong. It was a correct, live, tracked read of a *different* key, and `● live` was telling
  the truth — the problem is identity, not freshness. Three separate causes, two of them races
  that would have made any new indicator report a lie:
  - **A late read hijacked the pane.** `Msg::ValueLoaded`'s fallback arm installed a new `OpenKey`
    unconditionally, and `open_key` spawned a detached task per read with no token and no
    ordering. Open a 1.1 MB zset, change your mind, open something small: the small one arrives,
    then the big one lands and takes the pane back. Every read now carries a `ReadToken` minted by
    the core, and a reply that answers a superseded question is dropped.
  - **`ValueGone` carried no identity at all** — `{ at_ms }`, while its sibling carried index and
    name, both of which the shell had in scope and threw away. A stale gone-reply badged whichever
    key was open *and*, since 958b311 keeps the panes in step, tombstoned its row: a healthy key
    marked `✕ deleted` in both panes, with nothing afterwards to correct it. The sharpest seam in
    the area, and invisible until something went looking.
  - **`CLIENT CACHING YES` arms the next command on the connection**, so two concurrent reads
    could interleave and leave the server tracking a key the Viewer was not showing — `● live`
    over a value nothing would ever push an update for, which is ADR-0006's own defect by a third
    route. Reads now take turns; the arm-and-read pair is indivisible.
  - **And then the disclosure**, which is what was actually asked for: the Viewer is washed with a
    dashed divider and `⊘ not the selected key`, the keys pane underlines the Open key's row and
    ties it to the pane with `├`, and `▲`/`▼` point at it when it has scrolled out of view. Nothing
    is drawn when the panes agree. See [ADR-0012](adr/0012-the-viewer-may-hold-a-key-you-are-not-on.md)
    for the rejected alternatives — follow-the-cursor and a pin mode, both turned down — and
    CONTEXT.md for **Selected key** / **Open key**, which had no names until now.
  - Two fixtures were quietly self-contradicting and are now coherent: `opened()` opened index 0
    whatever name it was given, and `viewer_deleted` badged the Viewer while leaving its row
    untouched — the exact disagreement 958b311 removed, pictured in a golden frame.

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
- [x] **The two panes could state different things about one key — found by testing, fixed
  2026-09-04.** With a key open and then deleted, the Viewer badged `✕ deleted just now` while
  that key's row in the list still read `● string 64 B ∞`. `Msg::ValueGone` told the Viewer and
  not the Loaded set, and nothing would ever have corrected it: metadata is refetched only for
  rows whose type is *unknown*, and this row's was known and stale. The list is the more
  believable of the two contradictory claims precisely because it looks untouched. Both
  directions are now kept in step — `ValueGone` tombstones the row, and `ValueLoaded` writes the
  kind, TTL and size back, which un-badges a key that was deleted and then written again.
- [x] **Pane focus did not exist (DESIGN §4) — found by testing, fixed 2026-09-04.** R2.7 says
  `r` "acts on the focused pane", and DESIGN §4 has listed `Tab` as *Cycle pane focus* from the
  start; neither was implemented. The first cut of the rescan above inferred focus from
  `open.is_some()` at two-pane widths, which failed in the first minute of real use: opening a
  key silently handed `r` to the Viewer while the arrow keys still drove the key list, so `r`
  pressed to rescan quietly refetched an unchanged value and the list did not move — no error,
  no feedback, nothing to explain it. Fixed properly rather than patched: `SinglePaneView`
  became `Pane` and `single_pane_view` became `focus`, one concept at every width instead of a
  layout detail that only existed below 70 columns. `Tab` moves focus without closing the key,
  and the focused pane's header is drawn at full strength against the other's muted — DIM in
  monochrome, so the one signal that says what `r` will do survives the loss of colour.
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
