# redis-pane

A terminal UI for Redis — the keyspace browser `redis-cli` should have shipped with.

`redis-cli` is a REPL, not a workspace: you cannot *see* a keyspace, only guess at it.
RedisInsight can see one, but it is an Electron app that is slow past a few hundred thousand
keys and unusable where engineers most often need it — inside an SSH session on a bastion host.
`redis-pane` is the third option: a single static binary, keyboard-driven, that renders
structured data as structured data and makes dangerous operations feel dangerous.

## Status

**Requires Redis 6.0+ (or Valkey).** RESP3 only.

**Planning. There is no code yet.** This repository currently holds the specification, and the
specification is the deliverable — it is kept current, not archived. The stack is settled
(Rust + [ratatui](https://ratatui.rs) + tokio); scaffolding has not started.

## What it looks like

```
┌─ redis-pane ─ ● staging · cache-01:6379/0 · from profile ──────────────────┐
│ KEYS   scanning 41,203 of ~180,000      │ user:8812:session                │
│ / user:*:session            3,410 match │ hash · 14 fields · 2.1 KB        │
│ KEY                 TYPE      SIZE  TTL │ ttl 00:42:17                     │
│ ▾ user:                          41,203 │                                  │
│   ▾ 8812:                             6 │ FIELD          VALUE             │
│     ● session       hash    2.1 KB  42m │ id             8812              │
│     ● profile       json     880 B    ∞ │ device         ios/17.2          │
│     ● cart          zset     412 B  12m │ region         eu-west-1         │
│   ▸ 8813:                             6 │ cart_total     4                 │
│ ▾ cart:                           8,120 │ plan           pro               │
├─────────────────────────────────────────┼──────────────────────────────────┤
│ ↑↓ move  → open  / filter  d d delete   │ e edit  y copy  t ttl            │
└─────────────────────────────────────────┴──────────────────────────────────┘
 Esc back   ⌃K palette   : console   ? help     SCAN 23% ▓▓▓░░░░░  Esc cancel
```

## The documents

| File | What it is |
|---|---|
| [docs/PRD.md](docs/PRD.md) | Problem, users, goals, numbered requirements (R1.x–R7.x), milestones M0–M4 |
| [docs/DESIGN.md](docs/DESIGN.md) | Layout, navigation, keymap, visual language, screen-by-screen behaviour |
| [docs/PLAN.md](docs/PLAN.md) | Implementation plan for M0 and M1 — workspace layout, tasks, and what each one proves |
| [CONTEXT.md](CONTEXT.md) | The glossary. Several terms are deliberately distinguished and the distinctions are load-bearing |
| [docs/adr/](docs/adr/) | Decisions, each with the alternatives that were rejected and why |
| [CLAUDE.md](CLAUDE.md) | Working guidance for contributors and coding agents |

Requirements are numbered so commits can cite them (`implements R2.1`). When a change diverges
from these documents, the documents change in the same commit.

## The shape of it, in seven decisions

- **There is no refresh button.** The open key is live: the server pushes an invalidation when
  it changes and the Viewer refetches. Nothing is ever memoized, so a value cannot go stale
  behind a control claiming to update it. ([ADR-0006](docs/adr/0006-liveness-without-a-refresh-button.md))
- **It never claims to be current when it isn't.** A dropped connection keeps your data on
  screen and says it is disconnected; a reconnect re-arms tracking before the header calls itself
  live again; Read-only Mode names the reason it is on, and says `locked` rather than offering a
  toggle that a replica would refuse.
  ([ADR-0009](docs/adr/0009-connection-lifecycle.md))
- **One Connection per process, one database, fixed at launch.** No switcher, no `SELECT`, no
  tabs, no sidebar — a second target is a second terminal. Everything else stays small because
  of this. ([ADR-0005](docs/adr/0005-one-connection-per-process.md))
- **Connection resolution is deterministic and never prompts:** flags → default Profile →
  environment → `127.0.0.1:6379`. Zero configuration is a supported way to run. The title bar
  permanently shows the target *and* where it was resolved from, which is the entire mitigation
  for resolving silently. ([ADR-0001](docs/adr/0001-connection-resolution-order.md))
- **Config is read-only to the app.** Profiles are hand-authored JSON at
  `~/.config/redis-pane/config.json`; anything the app persists goes to a separate state file.
  ([ADR-0002](docs/adr/0002-json-config-file.md), [ADR-0003](docs/adr/0003-app-never-writes-config.md))
- **Secrets are references** — `passwordEnv` or `passwordCommand`. Literal passwords work, but
  the file is refused when group- or world-readable.
- **There are four Environments, not three.** `unknown` is a real one: anything that is not
  loopback or a unix socket and was not tagged gets it, and starts in Read-only Mode.
  ([ADR-0004](docs/adr/0004-untagged-connections-are-read-only.md))

## Non-goals

Not a server manager, not a monitoring product, not a replacement for `redis-cli` in shell
pipelines, and not a multi-target workspace. No plugins, no embedded scripting, no export/import
in v1. Sentinel ships; Cluster is deferred past v1. Nothing older than Redis 6.0. The full list is [PRD §5](docs/PRD.md).
