# redis-pane — UX & UI Design

**Status:** Draft v0.1 · **Companion to:** [PRD.md](PRD.md) · **Last updated:** 2026-08-23

## 1. Design principles

1. **The terminal is a canvas, not a constraint.** We use space, color, weight, and motion the
   way a well-made desktop app does. If something looks like ASCII-art compromise, it is a bug.
2. **Show, don't make them remember.** Contextual key hints are always on screen. The user
   should never need to recall a command to make progress.
3. **One primary action per pane.** Each pane has an obvious next step. Ambiguous focus is a
   design failure.
4. **Data keeps its shape.** A hash is a table. A stream is a timeline. Nothing is flattened
   into a string just because the terminal is text.
5. **Danger is visible before it is possible.** Environment color, read-only badge, and command
   preview all appear *before* the user commits.
6. **Never freeze.** Every network operation is cancellable and every long list is virtualized.
   A spinner is acceptable; a locked keyboard is not.
7. **Familiar to two tribes.** Vim keys and arrow keys both work, always. No modal purity tests.

## 2. Layout

Three columns, one status bar, one hint bar. Splits are resizable and collapsible.

```
┌ redis-pane ── ● staging · redis://cache-01:6379 · db 0 ────────── READ-ONLY ─┐
│ CONNECTIONS   │ KEYS  scan 41,203 / ~180k        │ VALUE                     │
│               │ ┌ filter: user:*:session ──────┐ │ user:8812:session         │
│ ● local       │ │                              │ │ hash · 14 fields · 2.1KB  │
│ ● staging   ▸ │ │ ▾ user:                      │ │ ttl 00:42:17              │
│ ○ prod        │ │   ▾ 8812:                    │ │                           │
│               │ │     ● session   hash  42m    │ │  FIELD        VALUE       │
│ DATABASES     │ │     ● profile   json   ∞     │ │  id           8812        │
│  db0  128,441 │ │   ▸ 8813:            (3)     │ │  device       ios/17.2    │
│  db1      —   │ │ ▾ cart:                      │ │  cart_total   4          │
│               │ │   ● 91af…       zset  12m    │ │  …                        │
│ VIEWS         │ │                              │ │                           │
│  Dashboard    │ │                              │ │                           │
│  Monitor      │ └──────────────────────────────┘ │                           │
│  Pub/Sub      │  ↑↓ move  → open  / filter  ⌫ del│  e edit  y copy  t ttl    │
└───────────────┴──────────────────────────────────┴───────────────────────────┘
 :cmd  ⌘K palette   ? help   ⇥ next pane            SCAN 23%  ▓▓▓░░░░  cancel ⎋
```

**Responsive behavior**

| Width | Layout |
|---|---|
| ≥ 140 cols | Three columns as above, value pane widest |
| 100–139 | Sidebar collapses to icons + labels on focus |
| 80–99 | Two columns: keys + value; sidebar becomes an overlay (`g c`) |
| < 80 | Single pane, stack-navigated; breadcrumb replaces columns |
| Height < 24 | Hint bar collapses into the status bar |

## 3. Navigation model

- **Panes** are traversed with `Tab` / `Shift-Tab`; focus is shown by border color *and* a
  brightened title — never by border alone (colorblind and monochrome safety).
- **Within a pane**, `↑↓` / `j k` move, `→` / `Enter` descends, `←` / `Esc` ascends.
- **Global jumps** use a `g`-prefixed chord: `g c` connections, `g k` keys, `g d` dashboard,
  `g m` monitor, `g p` pub/sub, `g s` slowlog.
- **The command palette** (`Ctrl-K`, or `Cmd-K` where the terminal forwards it) is the escape
  hatch for everything: actions, keys, profiles, commands, help topics — one fuzzy list.
- **The console** (`:`) is for raw Redis commands. Palette and console are deliberately separate:
  the palette drives the *app*, the console drives the *server*.
- **`Esc` is always "back"** and never destroys unsaved input without asking.

## 4. Core keymap

| Key | Action | Scope |
|---|---|---|
| `?` | Help overlay | global |
| `Ctrl-K` | Command palette | global |
| `:` | Redis console | global |
| `Tab` / `S-Tab` | Cycle pane focus | global |
| `g` + key | Jump to view | global |
| `Ctrl-C` ×2 | Quit (single press = cancel current op) | global |
| `/` | Filter / search in pane | pane |
| `n` / `N` | Next / previous match | pane |
| `Space` | Toggle multi-select | key list |
| `Enter` | Open key in value pane | key list |
| `r` | Refresh / rescan | pane |
| `e` | Edit value | value pane |
| `t` | Edit TTL | value pane |
| `y` | Copy (key / value / command — submenu) | value pane |
| `d d` | Delete selection | key list |
| `Ctrl-R` | Toggle read-only mode | global |

Every one of these is also listed in the palette with its binding shown, so the keymap teaches
itself. Bindings are user-overridable in config; the hint bar renders the *effective* binding.

## 5. Visual language

**Color roles** (semantic tokens, not literal colors — themes remap them):

| Token | Use |
|---|---|
| `surface` / `surface-alt` | Pane background, zebra striping |
| `border` / `border-focus` | Pane edges; focused pane gets `border-focus` + bold title |
| `text` / `text-muted` | Primary content vs. metadata (TTL, sizes, counts) |
| `accent` | Selection, cursor row, active tab |
| `type.*` | One hue per Redis type — consistent everywhere a type appears |
| `env.local` / `env.staging` / `env.prod` | Title bar and connection chrome |
| `danger` / `warn` / `ok` | Destructive actions, expiring TTLs, success toasts |

**Environment signaling.** `local` is neutral, `staging` is amber, `prod` is red — applied to
the title bar band, the connection dot, and the confirmation dialogs. A prod connection is
recognizable from across a desk.

**Typography and density.** Bold for headers and focused titles, dim for metadata, no
underlining except links. One blank line between logical groups; padding of one column inside
every pane border. Tables get a header row that stays pinned while the body scrolls.

**Iconography.** Nerd Font glyphs when detected, ASCII fallbacks otherwise, chosen so the
layout width does not change between the two.

**Motion.** Used sparingly and only to explain state: progress bar during scan, a 120ms fade on
toasts, a subtle pulse on a value that just changed under a live view. No decorative animation.

**Themes.** Ship a dark default and a light default, both truecolor, both contrast-checked.
Themes are data (a token → color map in config), so users can add their own.

## 6. Key screens

### 6.1 Connection picker (first run)
No empty-terminal cliff. First launch shows detected local instances (default ports, common
socket paths, `REDIS_URL` if set), a "connect to URL" field, and a hint about the config file.
The user is browsing a keyspace within two keystrokes.

### 6.2 Keyspace browser
The centerpiece. Streams `SCAN` results into a virtualized list that stays interactive while
loading, with progress and a cancel affordance in the status bar. Tree mode folds on the
configured separator and shows child counts on collapsed nodes; flat mode is one keypress away.
Metadata columns fill in asynchronously — the key appears immediately, its size arrives when it
arrives, and a pending cell shows a placeholder rather than shifting the layout.

### 6.3 Value viewers
One viewer per type, each with the same frame (header: key, type, size, TTL; body: type-specific;
footer: actions) so navigation muscle memory transfers:

- **String** — syntax-highlighted when JSON/XML/YAML is detected, toggleable raw/pretty/hex.
- **Hash** — two-column table, sortable by field, filterable, inline edit.
- **List** — indexed rows with head/tail jump; push/pop actions surfaced.
- **Set / Sorted set** — member table; zset adds a score column and rank, sortable by either.
- **Stream** — reverse-chronological entry timeline, expandable fields, consumer-group panel.
- **JSON module** — collapsible tree with JSONPath breadcrumb.
- **Binary/unknown** — hex + ASCII dump with offset gutter.

### 6.4 Editing and confirmation
Editing opens an inline editor in the value pane, not a modal. Committing shows a **command
preview**: the literal command(s) that will be sent, plus a red/green diff for value changes.
Confirmation friction scales with blast radius — a single-key `y` for one non-prod delete, a
typed key-count for a bulk prod delete. Read-only mode intercepts before the editor opens and
explains how to disable it.

### 6.5 Dashboard
Triage-first: memory used vs. peak vs. maxmemory as a bar, hit ratio, ops/sec sparkline,
connected/blocked clients, replication role and lag, and eviction/expiry counters. Anything
alarming is colored, and every tile can be expanded into the raw `INFO` section behind it.

### 6.6 Monitor / Pub-Sub
Live tail with a filter box, pause/resume, and a persistent warning banner on `MONITOR`
explaining its cost. Buffers are bounded with a visible cap.

## 7. Interaction details that carry the product

- **Optimistic focus.** Opening a key renders header and metadata instantly from what the list
  already knows, then fills the body when the fetch lands. No blank frame.
- **Cancellation everywhere.** `Esc` aborts an in-flight scan, fetch, or command and says so.
- **Toasts, not dialogs, for outcomes.** Errors include the failing command and a copy action.
- **Persistent session state.** Last profile, pane sizes, filter, and scroll position restore on
  relaunch. Reopening the app feels like never having left.
- **Copy that fits the terminal.** `y` offers key / value / `redis-cli` command / permalink-style
  reference — because the next step is usually pasting into a ticket or a shell.

## 8. Accessibility

- Never encode meaning in color alone — pair every color signal with a glyph, label, or weight.
- WCAG AA contrast for both shipped themes; a high-contrast theme as a third option.
- Full monochrome fallback that remains navigable.
- Screen-reader-friendly mode: linearized rendering, no box-drawing, announced focus changes.
- No timing-dependent interactions; every chord has a non-chord equivalent in the palette.

## 9. Open design questions

- Tree vs. flat as the *default* key view — tree is more legible, flat is more honest about scale.
- Does the console deserve a persistent bottom split (tmux-style) or stay an overlay?
- How much of the dashboard belongs in v1 before it starts pretending to be a monitoring tool?
- Should multi-connection be tabs across the top, or purely sidebar-driven?
