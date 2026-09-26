# ADR-0020 — No command palette

**Status:** Accepted · **Date:** 2026-09-26

## Context

The Palette shipped in 0.1.0-alpha.14 (#43): `Ctrl-K` opened a fuzzy list over every `Action` in
`crates/core/src/keymap/mod.rs`, reading the same `Keymap` the hint bar and help overlay already
read rather than a second copy of it.

Hands-on testing found no use for it. Every `Action` it listed already had a key, so selecting a
row was never faster than pressing the key directly — it was a slower route to the same thing,
reachable only by first opening the thing that replaces reaching for the key. The reasons it was
designed in the first place are all gone by the time it shipped:

- **Console search** went with the Console. R5.2–R5.4 are not scheduled for M3 (PRD §10), so
  there was nothing left for the Palette to search alongside its own action list.
- **Profile listing** is moot under [ADR-0005](0005-one-connection-per-process.md): one
  Connection per process, fixed at launch, means there is no live Profile to switch to from
  inside a running session.
- **Key search** — the one thing a fuzzy list over the keyspace could have offered that `/`
  filtering does not — was never built. The Palette that shipped searched `Action`s only.
- The features planned next when the Palette was designed — Slowlog, Monitor, Pub/Sub,
  Dashboard — turned out to be reachable by a `g`-prefixed chord without it (DESIGN §3), so they
  never gave it new work either.

Its one surviving argument — discovery for the occasional on-call user who does not have the
keymap memorized — does not need a second input surface to search. It moves to a contextual help
overlay: a separate change, `m3-contextual-help.md`, planned for right after this one lands.

The Palette was also doing quiet duty as the one check against the keymap growing without limit:
CLAUDE.md's convention read "prefer adding to the command palette over adding a keybinding." That
check goes with it and needs replacing, not just deleting — see Decision.

## Decision

**Remove the Palette completely**, not merely unbind it: `Action::OpenPalette` and its default
`Ctrl-K` binding, `ALL_ACTIONS`, `Action::description()`, `PaletteState` and its fuzzy matcher,
the `update` handlers, the overlay, and the golden frames that covered it. `git show e8490a7` is
the complete record of what shipped and is now reversed.

`update::dispatch_action` and `Action::label()` are kept. Neither was Palette-specific:
`dispatch_action` is what a resolved `Action` does, and the keymap path (`key_press`) is its only
remaining caller now that the Palette's `Enter` no longer calls it too; `label()` is read by the
help overlay and the hint bar, which never stopped needing it.

**Discovery moves to a contextual help overlay** — the Palette's one surviving argument, given a
home that does not duplicate a search surface nobody used the general form of.

**A written keymap growth rule replaces the Palette as the check on keymap growth** (DESIGN §4):
a bare top-level single key is spent only on a per-session action; a new view lives under a
`g`-prefixed chord, never a bare key of its own; everything else is scoped to whichever pane or
view already has focus, the way `r` already means refetch or rescan depending on which one is
focused. No test enforces this — it is a rule for whoever adds the next binding to read, the same
way ADR-0009's re-arm invariants are a rule with a test and this one is a rule without.

R5.1 is marked **Withdrawn** in PRD.md, not deleted: commits already cite it, and alpha.14 shipped
`Ctrl-K` under that requirement number. The same pattern the Console cut used for R5.2–R5.4 —
resolve the question in the docs, don't leave it dangling.

## Alternatives considered

- **A searchable cheat sheet** — a Palette that only ever lists bindings, doing nothing. Rejected:
  this is what the help overlay (`?`) already is. Building a second overlay with the same content
  under a different key duplicates chrome for no new capability.
- **A home for rare, long-tail actions** that don't deserve a keybinding of their own. Rejected:
  audited against the current `Action` enum, none exist. Every action added since M0 has fit
  either a focus-scoped key or a `g`-chord; there is no backlog of orphaned actions waiting on a
  Palette to reach them.
- **A universal "go to anything"** — keys, Profiles, commands, and actions in one fuzzy list, the
  shape the original nav-model prose in DESIGN §3 described. Rejected: it competes directly with
  the keys pane's own `/` filter for the one thing worth fuzzy-searching (keys), and
  [ADR-0005](0005-one-connection-per-process.md) leaves little else to jump to — one Connection,
  no Profile switcher, no second target.
- **Keep the code but unbind the default key.** Rejected: dead code rots silently, and it costs
  nothing to bring it back correctly from git history (`e8490a7`) if a future need actually
  reappears. Keeping it unbound is keeping a maintenance burden for a hedge against a need that
  has not shown up once.

**Revisit this decision** when focus-scoped keys and `g`-chords genuinely run out — a new action
with nowhere to go that isn't a bare top-level key — or when an overloaded key (`t` already means
tree in one pane and TTL in the other) starts confusing users rather than merely economizing on
keys.

## Consequences

- `crates/core/src/keymap/mod.rs`: `Action::OpenPalette`, `ALL_ACTIONS`, and `description()` are
  gone; `every_action_has_a_default_binding` goes back to a hand-typed list of actions, the form
  it had before `ALL_ACTIONS` existed.
- `crates/core/src/state/palette.rs` and `crates/core/src/update/palette.rs` are deleted, along
  with `State::palette` and the mode branch that gave the Palette first claim on keystrokes while
  open.
- `crates/core/src/render/mod.rs` loses `palette_overlay` and its call site.
- `crates/core/tests/golden.rs` loses its Palette test module and the seven
  `tests/golden/palette_*.txt` fixtures; `help_overlay.txt` is regenerated without its `⌃K
  palette` line.
- `docs/PRD.md`, `docs/PLAN.md`, `docs/DESIGN.md`, and `CONTEXT.md` are updated in the same
  change, per CLAUDE.md's "the docs are the spec, not a historical artifact" — see
  `docs/plans/m3-palette-withdrawn.md` for the itemized list.
- `docs/plans/m3-palette.md` is marked superseded rather than deleted; its content stays as the
  record of what was built and why, since deleting a superseded plan would just make the next
  reader of `e8490a7`'s history dig it out of git instead.
