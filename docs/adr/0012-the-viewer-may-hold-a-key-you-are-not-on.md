# ADR-0012 — The Viewer may hold a key you are not on, and says so

**Status:** Accepted · **Date:** 2026-09-05 · **Verified against Redis 8.4.0**

## Context

Reported from real use: the cursor on one key, the value pane showing another. It reads as the
classic bug — the wrong value — and it is worth being precise that it is not, because the
correct diagnosis picks a different fix.

The value pane holds the **Open key**. The cursor sits on the **Selected key**. They differ
whenever the reader moves in the list without pressing `→`, which is most of the time, and that
divergence is deliberate: [ADR-0006](0006-liveness-without-a-refresh-button.md) makes every open
a read *plus* a `CLIENT TRACKING` re-arm, so a Viewer that followed the cursor would fire one of
each per keystroke. It is also useful — reading one key while hunting for another is a real
workflow, and it is the thing a two-pane layout is for.

So the value on screen was never wrong. It was a correct, live, currently-tracked read of a
different key, and its `● live` badge was telling the truth. **The problem is identity, not
freshness.** Nothing on screen related the two panes: `render/keys.rs` never read `state.open`,
and the Viewer's only back-reference was the key name in its header — the hardest possible
comparison to make by eye across `user:8812:session`, `user:8813:session`, `user:8812:profile`.

Investigating it turned up two genuine bugs underneath, in the same family, which had to be
fixed first — an indicator built on top of them would have faithfully reported a hijacked pane.

## Decision

**Divergence stays legal, and is disclosed rather than prevented.** This is the same trade
ADR-0006 made one level down: *"ambiguity is the actual defect being designed against."*

1. **Both panes carry the signal.** The Viewer is washed (`surface-alt`), its divider goes
   dashed, and its header states `⊘ not the selected key`. The keys pane underlines the Open
   key's name and ties that row to the pane with `├` in the divider cell. The Viewer says *what*;
   the divider says *where*.
2. **When the panes agree, nothing extra is drawn.** The two marks coincide, which is how the
   relationship is taught in the ordinary case, so the split reads as a change rather than a
   puzzle.
3. **Colour is never the only carrier.** The wash is hue and nothing else, so monochrome loses it
   and the dashed divider, the chip and the underline carry the state alone. The loud treatment is
   never the sole treatment.
4. **The liveness badge is not weakened while detached.** The value *is* live. Making `● live`
   equivocate here would trade a legible truth for a hedge.
5. **Every read is identified.** A `ReadToken` is minted by the core for each read and echoed on
   the reply; anything else is dropped. `Msg::Failed` deliberately carries none — a read that
   failed, failed, and suppressing it because the reader moved on would be swallowing an error on
   a technicality (R7.4).
6. **Reads are serialised in the shell.** `CLIENT CACHING YES` arms *the next command on the
   connection*, so two concurrent reads can interleave and leave the server tracking a key the
   Viewer is not showing. Serialising reads is not enough by itself: keys-pane metadata, `SCAN`
   pages and writes share the connection, so the arming and the first read of the key also go out
   as one pipeline (ADR-0006).
7. **`OpenKey::index` does not survive a rescan.** `SCAN` has no stable order, so the number
   addresses a different key once the set refills. It is `None` until the name is scanned again;
   `OpenKey::name` is the identity that never goes stale.

## Rejected alternatives

**The Viewer follows the cursor.** The obvious fix, and it removes the class of problem outright:
if the panes cannot diverge, they cannot disagree. Rejected on ADR-0006 — every open is a read
plus a re-arm, so following would issue both per keystroke, thrash the arming the whole design
leans on, and put real load on a production server for a cursor that is only passing through. A
debounce reduces the traffic without changing the shape: it still fires a read for a key nobody
asked to read. ADR-0006 already says there is no follow mode; this extends that reasoning from
the mechanism to the affordance.

**A pin mode.** Follow by default, with an explicit key to detach and hold one key while
browsing. This preserves the useful workflow and gets zero divergence the rest of the time.
Rejected because it answers a question nobody asked: divergence here is not a *mode* the reader
enters, it is the ordinary consequence of moving a cursor, and giving it a mode with a verb, a
keybinding, a state and a badge is a lot of apparatus for something the reader already does by
accident. The disclosure is the whole of what was missing.

**Clearing the Viewer when the cursor moves.** Honest, and free. Rejected: it destroys the pane's
usefulness to solve a labelling problem, and it makes the common case — glancing down the list
with a key open — flicker.

**A border box around the detached Viewer.** Rejected on mechanics: adding and removing a box
changes the pane's inner width and reflows the body. The project already forbids exactly this for
the focus signal (*"emphasis, not content — no glyph, no reflow, no width change"*). Any
treatment here has to swap in place.

**Dimming the detached Viewer.** Rejected on collision: the Viewer's name row is *already* muted
whenever the keys pane has focus (DESIGN §4), which is precisely the state divergence happens in.
Intensity is spoken for.

**Naming a stale value.** Rejected on accuracy, and CONTEXT.md bans the word: the value is not
stale, and saying so would teach the reader to distrust a badge that is correct — the exact
damage ADR-0006 catalogues.

## Consequences

- `render/keys.rs` reads `state.open` for the first time, which is a new coupling in the file with
  the most edge cases. Three of them needed decided answers rather than emergent ones: the Open
  key filtered out, folded inside a collapsed group, and un-indexed after a rescan. All three land
  on `Attachment::DetachedOffList`, where the keys pane has nothing to mark and the Viewer carries
  the whole signal with `⊘ not in the list`.
- The wash cannot be painted underneath the pane and written over: `put` writes
  `Style::reset().patch(style)`, so a background pre-fill is wiped by the next character drawn.
  The wash rides on every style inside `value_pane` instead. Anything added to that function has
  to go through its `sty` helper or it will punch a hole in the wash.
- The keys pane still has no liveness (DESIGN §9), so a row can be marked as the Open key while
  its metadata is older than the Viewer's. That was already true of every row and is unchanged.
- CONTEXT.md gains **Selected key** and **Open key**. The distinction existed only in code
  (`State::selected_key()` vs `State::open`) and was about to be rendered on screen, which is
  where an unnamed distinction starts drifting between the UI and the source.
- The loudness of the wash was chosen deliberately, against the argument that a state present
  during ordinary browsing should be calm. If it proves fatiguing in use, the wash is the piece to
  turn down: the dashed divider, the chip and the underline already carry the meaning without it,
  and monochrome has been running on exactly those three from the start.
