# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state

**Greenfield — no code yet.** The repository currently contains only planning documents. The
product and design intent live in:

- [README.md](README.md) — what this is, and the five decisions that shape it
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

## Stack (confirmed 2026-08-24)

Rust + [`ratatui`](https://ratatui.rs) + `crossterm`, with `tokio` for async I/O and
[`fred`](https://docs.rs/fred) for the Redis protocol. `fred` is settled rather than preferred:
liveness (ADR-0006) needs RESP3 `CLIENT TRACKING` invalidation as a raw event stream, which
`fred`'s `TrackingInterface` provides across centralized, clustered and sentinel deployments.
`redis-rs` supports client-side caching too, but its `caching` module maintains a cache, which is
the one thing ADR-0006 forbids. **The server floor is RESP3 and Redis 6.0** (ADR-0007) — RESP2 is
not spoken, so there is one reply shape per command and one code path per Viewer. Rationale for
the rest: a single static binary with no runtime (PRD R7.6), the strongest TUI widget
ecosystem, and the headroom to hold a million-key keyspace in bounded memory.

A TypeScript TUI (Ink, or a hand-written `react-reconciler` host) was considered and rejected on
two of the success metrics, not on ergonomics: the binary lands at 55–100MB against a <20MB
target, and a million JS objects of key metadata will not fit in 250MB RSS. Those two metrics
are the reason the project exists — they are exactly where RedisInsight fails. Everything else
favoured TypeScript, and ratatui's immediate-mode model is a real ergonomic cost to pay for
them: there are no components and no hooks, so widget state is hand-managed. Budget for that.

Once `Cargo.toml` exists, fill in the commands below.

## Commands

Not yet applicable — no build system exists. When scaffolding lands, this section must list:
build, run against a local Redis, test (including how to run a *single* test), lint, and format.
Do not leave it as prose.

Two suites are planned (ADR-0011) and the distinction belongs here when they exist: the default
`cargo test` run is the functional core plus golden-frame snapshots and needs no Docker; the
integration suite uses `testcontainers` against real Redis and covers `SCAN` streaming, tracking
invalidation across a reconnect, capability probing where `CLIENT TRACKING` is refused, and error
mapping for `-LOADING`, `-OOM`, `-MISCONF` and `-READONLY`.

## Architecture guidance

The following constraints come out of the PRD and should shape the code from the first commit —
they are expensive to retrofit:

- **Functional core, imperative shells.** The core takes a message and returns new state plus
  commands; the terminal and the Redis connection are shells around it and are unreachable from
  the core. **The clock is injected**, along with randomness, terminal size, and capability
  detection — TTL countdowns and read ages make a frame a function of *when* it was rendered, and
  golden-frame tests need it to be a function of state alone (ADR-0011). Trivial on day one,
  invasive later.
- **The render loop never does I/O.** All Redis work happens on async tasks that send messages
  into the UI; the UI reads state and draws. A keystroke must be answerable in one frame (16ms)
  regardless of what the network is doing. Every in-flight operation must be cancellable (`Esc`).
- **`SCAN` only, never `KEYS`.** Keyspace traversal is cursor-based, streaming, and resumable.
  Results render as they arrive.
- **The key list is columnar and capped.** Key names go in one byte arena addressed by
  `(offset, len)`; metadata lives in parallel arrays, never a `Vec` of per-key structs. Sorting
  permutes an index vector. Every scanned key is retained up to a documented cap, because `SCAN`
  has no random access and no stable order, which makes a sliding window harder *and* worse
  (ADR-0010). This shapes every access to the key list — it belongs in the first commit that
  stores a key.
- **The keyspace source abstracts over a stream of keys**, not over a cursor. v1 has exactly one
  cursor behind it, but Cluster will have N (ADR-0008) and the browser above must not know.
- **Lists are virtualized.** Render cost is a function of viewport size, not keyspace size.
  Metadata (type, memory, TTL — four columns, no element count; PRD R2.4) is fetched lazily
  and fills in without shifting layout.
- **Type-awareness is a first-class abstraction**, not a `match` scattered through the UI.
  Every Redis type gets a viewer behind one shared trait/interface so the frame (header, body,
  footer) and navigation are identical across types.
- **The Viewer never caches a value.** Reads always hit the server; liveness is push-driven via
  `CLIENT TRACKING ON OPTIN` armed for the open key alone. Any memo keyed by key-name reintroduces
  the exact RedisInsight bug this project was started over — see ADR-0006 before adding one, and
  note that "just for the first frame" is how it starts.
- **Mutations flow through one path** that produces a command preview before executing. Read-only
  mode and confirmation-scaling are enforced at that chokepoint, not at each call site.
- **Colors are semantic tokens, never literals.** Themes remap tokens; widgets ask for
  `border-focus` or `type.hash`, never a hex value. Same for icons (Nerd Font vs. ASCII must be
  width-identical).
- **Terminal capability degrades gracefully.** Truecolor → 256 → monochrome; Nerd Font → ASCII;
  ≥140 cols → 80 cols → single-pane. Layout breakpoints are in DESIGN.md §2.
- **Keybindings are data.** The keymap, the palette, and the on-screen hint bar all read from one
  source, so hints always show the *effective* binding after user overrides.
- **Screen space is a budget, not a canvas.** The layout is two panes (PRD R7.7, G7). A new
  surface either displaces something or lives in the Palette or a dismissible overlay. "It's only
  a few columns" is how the sidebar happened; it was removed for exactly that reason.

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
- **A reconnect re-arms tracking before anything claims to be live.** This is the invariant most
  likely to rot silently, and it puts the product back where RedisInsight was if it does. It has
  a test (ADR-0009, ADR-0011).
- **Read-only Mode carries a reason** — `environment`, `replica`, or `user` — and shows it. The
  `replica` reason is not user-liftable; never offer `⌃R` where the server will refuse anyway.
- **There is no refresh button.** The open key is live by default and the Viewer holds no value
  cache; `r` is a scoped Refetch, not a global refresh. Degradation to manual is always visible
  in the header, never silent (ADR-0006).
- **There are four Environments, not three** — `unknown` is a real one. Ad-hoc Connections to
  anything that isn't loopback or a unix socket get it, and start in Read-only Mode.
- **One Connection per process, one database, fixed at launch.** No switcher, no `SELECT`, no
  tabs, no sidebar. A second target is a second terminal. This is the constraint the rest of the
  design leans on to stay small — check ADR-0005 before adding anything that implies a second
  target, including "just" a database dropdown.

## Conventions

- Use the glossary's words in code, comments, and UI strings. A Profile is not a Connection;
  code that blurs them will blur them in the interface too.
- Environment (`local` / `staging` / `prod` / `unknown`) is a safety feature, not decoration —
  any code path touching mutations must be aware of it (PRD R4.5, R4.6, R1.9).
- Prefer adding to the command palette over adding a keybinding; every action must be reachable
  from the palette (R5.1), and only frequent actions earn a key.
- Errors surface as non-blocking notifications carrying the failing command (R7.4) — never
  `panic!` on a Redis error, and never swallow one silently.
