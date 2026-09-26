# M3 task 1: the command palette (`Ctrl-K`)

Status: **superseded.** This plan shipped (#43, 0.1.0-alpha.14), and the Palette it built was then
withdrawn — see [`m3-palette-withdrawn.md`](m3-palette-withdrawn.md) and
[ADR-0020](../adr/0020-no-command-palette.md).

Status: **planning — not started.** No code, no ADR needed (the Palette is already named and
scoped by CONTEXT.md and DESIGN.md — see below; this doc plans the build, not a new decision).

## Context

PLAN.md M3 row 1: "Palette (`Ctrl-K`): fuzzy list over every app action, reading the same
keymap-as-data source the hint bar uses · Proves: every bound action is reachable via the
Palette; hint bar and Palette never disagree on the effective binding."

Unlike the other four M3 features, the Palette is not a blank page:

- **CONTEXT.md** already has a full glossary entry: "The fuzzy launcher for actions belonging to
  *the application* — navigation, settings, view switching. Every action is reachable here,"
  explicitly distinguished from the (now-cut, see `docs/plans/m3-*.md` siblings and PRD.md §10)
  Console.
- **DESIGN.md §3** ("Navigation model") already places it: "`Ctrl-K` (or `Cmd-K` where the
  terminal forwards it) is the escape hatch for everything: actions, keys, profiles, commands,
  help topics — one fuzzy list."
- **DESIGN.md §4** ("Core keymap") already states the discipline this task exists to enforce:
  "Every one of these is also listed in the palette with its binding shown, so the keymap teaches
  itself."
- **CLAUDE.md**'s "Conventions" section: "Prefer adding to the command palette over adding a
  keybinding; every action must be reachable from the palette (R5.1), and only frequent actions
  earn a key" — this is the requirement the Palette exists to satisfy, and it is retroactive:
  once the Palette exists, every `Action` already defined in `crates/core/src/keymap/mod.rs`
  (`Quit`, `Refetch`, `ToggleReadOnly`, `Delete`, `Copy`, … the full non-exhaustive list) must
  appear in it, not just the ones a future task adds.

So this task is mostly wiring an already-designed surface onto an already-existing data model,
not designing a new one.

## What already exists to build on

`crates/core/src/keymap/mod.rs` defines `Action` as a `#[non_exhaustive]` enum and a `Keymap` that
maps `KeyPress -> Action`, with the explicit stated purpose: "The keymap, the command palette and
the on-screen hint bar all read from this one source, so a hint always shows the **effective**
binding after user overrides." The hint bar (`crates/core/src/render/mod.rs`) already reads
`state.keymap` to label keys. The Palette is the third reader of that same source, not a new one
— it must not maintain its own list of actions or labels.

What is missing today: a reverse index (`Action -> label, description`) for the fuzzy list to
search over, and the fuzzy-match/selection state machine itself, plus everything Palette-specific
that is not "app action" — key search and Profile listing per DESIGN §3's "actions, keys,
profiles, commands, help topics." **Scope for M3**: app actions (R5.1's literal text) are in
scope; fuzzy key search and Profile listing are stretch — see Out of scope below. Command search
is out entirely, since the Console it would have searched is cut.

## Architecture

### Core (`crates/core/src`)

- `crates/core/src/keymap/mod.rs`: each `Action` variant needs a human label and a short
  description for the Palette row (`"Toggle read-only" / "Lift or impose Read-only Mode"`, etc.).
  Adding this as a `fn label(&self) -> &'static str` / `fn description(&self) -> &'static str` on
  `Action` (or a parallel `match` in a new `palette.rs`) is the natural extension point — and the
  one CLAUDE.md's "screen space is a budget" rule indirectly enforces: a description too long to
  fit one row in the narrowest supported width (DESIGN §2, 80 cols) is a description that needs
  rewriting, not a wider Palette.
- New `crates/core/src/state/palette.rs`: `PaletteState { query: String, matches: Vec<Action>,
  selected: usize }`, opened by `Action::OpenPalette` (a new keymap `Action`, bound to `Ctrl-K`)
  and closed by `Esc` or by selecting a row. Lives on `State` as `pub palette: Option<PaletteState>`
  — `Option`, matching the existing shape of every other overlay (`state.confirm`,
  `state.help_open`), so "is the Palette open" is one field, not a derived condition.
  Fuzzy matching itself is a pure function over `&str` query and the static action list — no
  external crate strictly required (a simple subsequence-match-with-scoring is enough for the
  action count this app has, likely under 40), but a small pure crate (`fuzzy-matcher` or similar)
  is fine as a `redis-pane-core` dependency since matching is CPU-only, no I/O (the boundary check
  — `cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'` — is unaffected by
  a pure matching crate).
- `crates/core/src/update/mod.rs` (or a new `update/palette.rs`, matching the one-topic-per-file
  layout `update/keys.rs`/`update/viewer.rs`/etc. already use): typing narrows `matches` by
  re-running the fuzzy filter; `↑↓` moves `selected`; `Enter` dispatches the selected `Action`
  through the *same* path a keypress bound to it would take — the Palette must not have a second
  implementation of what each `Action` does, only a second way to name one. This is the load-
  bearing design point: `key_press()` already maps `KeyPress -> Action -> (new State, Vec<Command>)`
  (`crates/core/src/update/mod.rs`); the Palette's `Enter` handler should call the same
  `Action`-dispatch function `key_press` calls internally, refactored out if it is not already a
  separate function, rather than duplicating each `Action`'s effect inline.
- Render: `crates/core/src/render/mod.rs` gains a `palette_overlay` function, matching the shape of
  the existing `confirm_overlay` (a centered box over the two panes, dismissible, DESIGN's overlay
  pattern) — "Screen space is a budget, not a canvas... a new surface either displaces something
  or lives in the Palette or a dismissible overlay" (CLAUDE.md) is the rule that makes the Palette
  an overlay rather than a permanent third pane.

### Shell (`crates/app/src`)

Nothing new. The Palette is pure input-and-render, no I/O of its own — every `Action` it can
dispatch already has whatever `Command`s it needs wired through the existing `update()` ->
`Command` -> shell path. `terminal.rs` needs no new dispatch arm; `Ctrl-K` is just another
`KeyPress` translated the same way every other key already is.

## CLAUDE.md rules this binds

- **Keybindings are data.** This is the task that makes that rule's payoff visible: the Palette,
  the hint bar and a user's remapped `config.json` all read one `Keymap`, so they cannot disagree.
- **Screen space is a budget, not a canvas.** The Palette is an overlay, never a pane; see above.
- **The render loop never does I/O.** Fuzzy matching over a static, in-memory action list is CPU
  work, not I/O — no `Command` is needed to open or filter the Palette, only to execute whatever
  `Action` gets picked (which already goes through the existing Command-producing path).
- **Colors are semantic tokens.** The selected row, matched-character highlight, and any "no
  matches" state use theme tokens, not literals — consistent with every other overlay.

## Files touched

| File | Change |
|---|---|
| `crates/core/src/keymap/mod.rs` | `Action::OpenPalette`; label/description for every `Action` |
| `crates/core/src/state/palette.rs` (new) | `PaletteState`, fuzzy match function |
| `crates/core/src/state/mod.rs` | `pub palette: Option<PaletteState>` on `State` |
| `crates/core/src/update/palette.rs` (new) | open/type/move/select/dismiss handlers |
| `crates/core/src/update/mod.rs` | route `Ctrl-K` and (while open) all typed input to the Palette; factor the `Action`-dispatch step out of `key_press` if not already separate, so the Palette's `Enter` calls it directly |
| `crates/core/src/render/mod.rs` | `palette_overlay` |
| `docs/DESIGN.md` §4 | add `Ctrl-K` binding's `Action` name for consistency with the keymap table's other rows once it exists in code |

## Testing

- **Core unit tests**: fuzzy match scoring/ordering on a fixed action list (deterministic, no
  clock or I/O involved, so plain state-transition tests suffice — no injected clock needed here
  since nothing in the Palette is time-sensitive); opening/closing state transitions; that
  selecting a row dispatches the *same* `Command`s a direct keypress for that `Action` would
  (a test that opens the Palette, fuzzy-matches down to `ToggleReadOnly`, presses `Enter`, and
  asserts the resulting `State`/`Vec<Command>` is identical to pressing `Ctrl-R` directly — this is
  the test that actually proves "hint bar and Palette never disagree," PLAN's stated proof for this
  row, by construction rather than by inspection).
- **Golden frames**: the Palette overlay empty, mid-query with matches, and a "no matches" state,
  at more than one Density (DESIGN §2) since the overlay's width is itself budget-constrained.
- **No Docker-backed integration test needed** — the Palette dispatches through paths M0–M2's
  integration suite already covers for whatever `Action` gets picked; this task adds no new Redis
  interaction of its own.

## Out of scope for this pass

- **Fuzzy key search and Profile listing** (DESIGN §3's "keys, profiles" in the same fuzzy list).
  R5.1's literal text is "for every app action" — the stretch goals are real but each has its own
  design question (searching the Loaded set from the Palette overlaps with the keys pane's own `/`
  filter; there is exactly one Profile per process by ADR-0005, so "listing profiles" in the
  Palette is closer to "show what Source resolved this Connection" than a switcher). Revisit as a
  follow-up row once the action-search core is built and in use.
- **Command search** — cut with the Console (see `docs/plans/m3-*.md` siblings, PRD.md §10).

## CONTEXT.md

Already has an entry (see Context above) — no new glossary work needed for this task.
