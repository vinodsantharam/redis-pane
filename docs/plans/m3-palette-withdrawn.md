# M3 task 1, withdrawn: remove the Palette

Status: **approved 2026-09-26 — in progress.** Supersedes [`m3-palette.md`](m3-palette.md).
Decision recorded in ADR-0020 (written by this change).

## Context

The Palette shipped in 0.1.0-alpha.14 (#43). Hands-on testing found no use for it: every `Action`
it lists already has a key, so it is only ever a slower route to the same thing. The reasons it
was designed are gone — Console search went with the Console (PRD §10), Profile listing is moot
under ADR-0005, and key search was never built — and the features planned next (Slowlog,
Monitor, Pub/Sub, Dashboard) are reachable by `g`+key without it. Its one surviving argument,
discovery for the occasional on-call user, moves to a contextual help overlay — a separate
change, [`m3-contextual-help.md`](m3-contextual-help.md) (PR 2, planned after this one lands).

Decisions (settled in the 2026-09-26 grilling session):

- Remove the Palette **completely**: `Action::OpenPalette` and its `Ctrl-K` binding,
  `ALL_ACTIONS`, `Action::description()`, `PaletteState` + fuzzy match, the update handlers,
  the overlay, the golden frames.
- **Keep** `update::dispatch_action` (the keymap path is its remaining caller) and
  `Action::label()` (the help overlay and hint bar use it).
- R5.1 is marked **Withdrawn**, not deleted — commits cite requirement numbers, and alpha.14
  shipped `Ctrl-K`. Same pattern the Console cut used for R5.2–R5.4.
- The palette was the only check on keymap growth (CLAUDE.md: "prefer the palette over a
  keybinding"). It is replaced by a written **keymap growth rule** in DESIGN §4, no test:
  top-level single keys only for per-session actions; views under `g`+key; everything else is
  scoped to the focused pane or view. §4 also lists the currently free single keys.

## Code — `crates/core`

1. `keymap/mod.rs`
   - Delete `Action::OpenPalette`, its default `Ctrl-K` binding, `ALL_ACTIONS`, `description()`.
   - Delete the test `every_action_has_a_short_label_and_description` and any test that exists
     only for `OpenPalette`/`Ctrl-K`. Keep `every_action_has_a_default_binding`; if it now uses
     `ALL_ACTIONS`, rewrite it to the form it had before #43 (check `git show e8490a7`).
   - Strip the Palette mention from `pane_is_on_screen`'s trailing comment.
   - The `label()` arm and `label_in` need no Palette arm after the variant is gone.
2. Delete `state/palette.rs` and `update/palette.rs`; remove their `mod`/`pub use` lines,
   `State::palette`, and its initialiser.
3. `update/mod.rs` — remove the Palette input routing (the branch that sends keys to the Palette
   while it is open). Keep `dispatch_action`; reword its doc comment so it no longer names the
   Palette as a caller.
4. `render/mod.rs` — delete `palette_overlay` and its call site; remove any Palette-only theme
   token usage (leave a token in the theme only if something else uses it).
5. `tests/golden.rs` — delete the palette tests; delete the seven `tests/golden/palette_*.txt`;
   regenerate `help_overlay.txt` (it loses its `⌃K palette` line) the same way the other goldens
   are regenerated (see the file's header / existing update mechanism).
6. Anything else `git show e8490a7 --stat` touched in `crates/` — reverse the Palette-specific
   part only. The `dispatch_action` extraction stays.

`crates/app` should need nothing; if it references the Palette, remove that too.

## Docs — same change (CLAUDE.md: the docs are the spec)

- **New `docs/adr/0020-no-command-palette.md`**, in the format of ADR-0019 (Status/Date line,
  Context, Decision, Consequences, Alternatives considered). Content: the context above; decision
  = no Palette, discovery via contextual help (link the PR 2 plan), keymap growth rule;
  alternatives rejected — (a) searchable cheat sheet (duplicates `?`), (b) home for rare
  long-tail actions (none exist that a focus-scoped key or `g`-chord can't take), (c) universal
  "go to anything" (competes with Filter; one Connection leaves little else to jump to),
  (d) keep the code but unbind it (dead code rots; git history keeps it). **Revisit when**
  focus-scoped keys and `g`-chords genuinely run out, or overloaded keys (like `t` = tree/TTL)
  start confusing users.
- **`docs/PRD.md`**: R5.1 → `~~…~~ **Withdrawn** — see ADR-0020.` in the style §10 uses for the
  Console; R7.7 "through the Palette or a dismissible overlay" → "through a dismissible overlay";
  §9 M3 line and the §10 resolved-question sentence ("The Palette (R5.1) ships in M3 on its own")
  updated to say it shipped in alpha.14 and was withdrawn (ADR-0020).
- **`docs/PLAN.md` §6**: the progress paragraph and row 1 → "done in alpha.14, then withdrawn
  (ADR-0020)"; the "Only the Palette (R5.1) ships" sentence; §6.1's link list adds this doc;
  the "Palette, Console, …" deferred line.
- **`docs/DESIGN.md`**: line 45 mock hint bar — drop `⌃K palette` (and `: console`, which was cut
  too); §3 — delete the palette bullet, rewrite the console bullet to say the Console is not
  scheduled (PRD §10) without contrasting it with a palette; §4 — delete the `Ctrl-K` row (and
  the `:` row, same reason), replace the "Every one of these is also listed in the palette…"
  paragraph with the **keymap growth rule** plus a short list of free single keys (derive it
  from the actual default keymap in `keymap/mod.rs`, not from this table); line ~485 "every chord
  has a non-chord equivalent in the palette" → rewrite honestly (e.g. every chord is listed in
  help with its binding).
- **`CONTEXT.md`**: replace the **Palette** entry with a one-line withdrawn note pointing at
  ADR-0020 (keep the word defined so old commits stay readable); remove the Palette contrast from
  the **Console** entry.
- **`CLAUDE.md`**: "Keybindings are data" → the keymap, the help overlay and the hint bar read
  one source; "Screen space…" → "or lives in a dismissible overlay"; replace the
  "Prefer adding to the command palette…" convention with the keymap growth rule (short, point
  at DESIGN §4).
- **`docs/plans/m3-palette.md`**: add a `Status: **superseded**` line at the top pointing here.
- **`docs/plans/m3-{dashboard,pubsub,slowlog,monitor,planning}.md`, `docs/adr/0005-*.md`,
  `docs/adr/0014-*.md`**: edit only sentences that mention the Palette (e.g. "reachable from the
  Palette") — ADRs keep their decisions; add "(Palette withdrawn, ADR-0020)" where deleting the
  phrase would change an ADR's meaning.

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'   # must print nothing
grep -rni palette crates CLAUDE.md CONTEXT.md docs README.md
```

The last grep should hit only: ADR-0020, the Withdrawn R5.1 and related PRD/PLAN history notes,
the CONTEXT.md withdrawn note, `m3-palette.md` (superseded), and this file. By hand:
`cargo run -p redis-pane` — `Ctrl-K` does nothing, `?` no longer lists a palette row.
