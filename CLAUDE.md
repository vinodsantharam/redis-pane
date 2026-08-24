# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state

**Greenfield — no code yet.** The repository currently contains only planning documents. The
product and design intent live in:

- [docs/PRD.md](docs/PRD.md) — problem, users, requirements (R1.x–R7.x), milestones M0–M4
- [docs/DESIGN.md](docs/DESIGN.md) — layout, navigation model, keymap, visual language, screens
- [CONTEXT.md](CONTEXT.md) — the glossary. Read it before naming anything; several of these terms
  are deliberately distinguished and the distinctions are load-bearing
- [docs/adr/](docs/adr/) — decisions with their rejected alternatives. Check for a relevant ADR
  before changing connection, config, or safety behaviour

Requirements are numbered so code and commits can cite them (e.g. "implements R2.1"). When a
feature diverges from these docs, update the doc in the same change — the docs are the spec, not
a historical artifact.

## What this is

`redis-pane` is a terminal UI for Redis, built because `redis-cli` gives no way to *see* a
keyspace and RedisInsight is too heavy and too mouse-bound to use where engineers actually work
(SSH sessions, bastion hosts). The bar is a TUI good enough that the user forgets it is a
terminal application.

## Stack (proposed, not yet locked)

Rust + [`ratatui`](https://ratatui.rs) + `crossterm`, with `tokio` for async I/O and
[`fred`](https://docs.rs/fred) or `redis-rs` for the Redis protocol. Rationale: single static
binary with no runtime (PRD R7.6), the strongest TUI widget ecosystem, and the headroom to hold
a million-key keyspace in bounded memory. The credible alternative is Go + Bubble Tea — faster
to write, weaker on large-list rendering performance.

**Confirm the stack with the user before scaffolding.** Once `Cargo.toml` exists, replace this
section with the real toolchain and fill in the commands below.

## Commands

Not yet applicable — no build system exists. When scaffolding lands, this section must list:
build, run against a local Redis, test (including how to run a *single* test), lint, and format.
Do not leave it as prose.

## Architecture guidance

The following constraints come out of the PRD and should shape the code from the first commit —
they are expensive to retrofit:

- **The render loop never does I/O.** All Redis work happens on async tasks that send messages
  into the UI; the UI reads state and draws. A keystroke must be answerable in one frame (16ms)
  regardless of what the network is doing. Every in-flight operation must be cancellable (`Esc`).
- **`SCAN` only, never `KEYS`.** Keyspace traversal is cursor-based, streaming, and resumable.
  Results render as they arrive.
- **Lists are virtualized.** Render cost is a function of viewport size, not keyspace size.
  Metadata (size, memory, TTL) is fetched lazily and fills in without shifting layout.
- **Type-awareness is a first-class abstraction**, not a `match` scattered through the UI.
  Every Redis type gets a viewer behind one shared trait/interface so the frame (header, body,
  footer) and navigation are identical across types.
- **Mutations flow through one path** that produces a command preview before executing. Read-only
  mode and confirmation-scaling are enforced at that chokepoint, not at each call site.
- **Colors are semantic tokens, never literals.** Themes remap tokens; widgets ask for
  `border-focus` or `type.hash`, never a hex value. Same for icons (Nerd Font vs. ASCII must be
  width-identical).
- **Terminal capability degrades gracefully.** Truecolor → 256 → monochrome; Nerd Font → ASCII;
  ≥140 cols → 80 cols → single-pane. Layout breakpoints are in DESIGN.md §2.
- **Keybindings are data.** The keymap, the palette, and the on-screen hint bar all read from one
  source, so hints always show the *effective* binding after user overrides.

## Decisions already made (see ADRs before revisiting)

- **Connection resolution** is flags → default Profile → environment → `127.0.0.1:6379`, with
  `REDIS_URL` used wholesale and never merged with the discrete `REDIS_HOST`/`PORT`/`USER`/
  `PASSWORD` variables. It never prompts. The title bar shows target *and* Source at all times,
  and that readout is the mitigation for resolving silently — do not treat it as optional chrome.
- **Config is `~/.config/redis-pane/config.json`, and the app only ever reads it.** There is no
  in-app "save profile". Anything the app persists (last Connection, pane sizes, filter, scroll)
  goes to a separate state file under `$XDG_STATE_HOME/redis-pane/`.
- **Secrets are references** (`passwordEnv`, `passwordCommand`). Literal passwords work but the
  file is refused when group- or world-readable.
- **There are four Environments, not three** — `unknown` is a real one. Ad-hoc Connections to
  anything that isn't loopback or a unix socket get it, and start in Read-only Mode.

## Conventions

- Use the glossary's words in code, comments, and UI strings. A Profile is not a Connection;
  code that blurs them will blur them in the interface too.
- Environment (`local` / `staging` / `prod` / `unknown`) is a safety feature, not decoration —
  any code path touching mutations must be aware of it (PRD R4.5, R4.6, R1.9).
- Prefer adding to the command palette over adding a keybinding; every action must be reachable
  from the palette (R5.1), and only frequent actions earn a key.
- Errors surface as non-blocking notifications carrying the failing command (R7.4) — never
  `panic!` on a Redis error, and never swallow one silently.
