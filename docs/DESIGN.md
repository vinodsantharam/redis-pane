# redis-pane — UX & UI Design

**Status:** Draft v0.4 · **Companion to:** [PRD.md](PRD.md) · **Last updated:** 2026-08-26

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

Two columns, one status bar, one hint bar. The split is resizable; nothing else is permanent.

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
│   ● 91af3c9d…       zset    1.1 KB  12m │ locale         fr-FR             │
│   ● 91af41e0…       zset     380 B  58m │ exp_bucket     B                 │
│ ▸ feed:                           2,088 │ cart_rev       18                │
├─────────────────────────────────────────┼──────────────────────────────────┤
│ ↑↓ move  → open  / filter  d d delete   │ e edit  y copy  t ttl            │
└─────────────────────────────────────────┴──────────────────────────────────┘
                                                                              
 Esc back   ⌃K palette   : console   ? help     SCAN 23% ▓▓▓░░░░░  Esc cancel 
```

**There is no sidebar.** An earlier draft gave one to Profiles, live Connections and databases.
All three turned out to be launch-time concerns: the target is chosen by flag or Profile before
the process starts, and a second target means a second terminal
([ADR-0005](adr/0005-one-connection-per-process.md)). Sixteen permanent columns were showing
information the title bar already carried and offering switches nobody makes mid-session. They
now belong to the keyspace.

The title bar still carries all four things it must always carry: Environment dot, target,
database, and **Source** (`from profile` / `from --url` / `from REDIS_URL` / `default`). Per
[ADR-0001](adr/0001-connection-resolution-order.md) the app resolves silently, and this readout
is the entire mitigation for doing so — it is not optional chrome.

**Responsive behavior**

| Width | Layout |
|---|---|
| ≥ 120 cols | Two columns as above; the value pane takes the larger share |
| 90–119 | Two columns; the keys pane sheds `SIZE`, keeping `TYPE` and `TTL` |
| 70–89 | Two columns, tight; the title bar truncates the target from the left, never the Environment or Source |
| < 70 | Single pane, stack-navigated; breadcrumb replaces columns |
| Height < 24 | Hint bar collapses into the status bar |

`TTL` is the last metadata column to go because it is the field people are hunting when the
terminal is small and the situation is urgent.

## 3. Navigation model

- **`Tab` toggles the two panes**; focus is shown by border color *and* a
  brightened title — never by border alone (colorblind and monochrome safety).
- **Within a pane**, `↑↓` / `j k` move, `→` / `Enter` descends, `←` / `Esc` ascends.
- **Global jumps** use a `g`-prefixed chord: `g k` keys, `g d` dashboard, `g m` monitor,
  `g p` pub/sub, `g s` slowlog. There is no `g c` — there is only ever one Connection.
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
| `Tab` | Move focus between the panes | global |
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

**Focus is one concept at every width.** Below 70 columns it decides which pane is *drawn*
(§2's stack navigation); at or above it, both panes are drawn and focus decides only which one a
pane-scoped key acts on. Opening a key moves focus to it, `Esc` moves it back, and `Tab` moves it
without closing the key. Because `r` rescans in one pane and Refetches in the other, focus is
information rather than decoration: the focused pane's header is drawn in full-strength text and
the unfocused one muted (dim, not merely grey, so the distinction survives monochrome), and the
hint bar names the half in force — `r rescan` or `r refetch`. A pane-scoped key whose target the
reader cannot see is a key that does the wrong thing silently, which is exactly what happened
when focus was inferred from "is a key open" instead of tracked.

## 5. Visual language

**Color roles** (semantic tokens, not literal colors — themes remap them):

| Token | Use |
|---|---|
| `surface` / `surface-alt` | Pane background; `surface-alt` washes the Viewer while it holds a key that is not the Selected key (§6.4) |
| `border` / `border-focus` | Pane edges; focused pane gets `border-focus` + bold title |
| `text` / `text-muted` | Primary content vs. metadata (TTL, sizes, counts) |
| `accent` | Selection, cursor row, active tab |
| `type.*` | One hue per Redis type — consistent everywhere a type appears |
| `env.local` / `env.staging` / `env.prod` / `env.unknown` | Title bar band and confirmation dialogs |
| `danger` / `warn` / `ok` | Destructive actions, expiring TTLs, success toasts |

**Environment signaling.** `local` is neutral, `staging` is amber, `prod` is red, and `unknown`
is a distinct fourth treatment — deliberately not a shade of the others, because it means "nobody
told us," not "somewhere between staging and prod." Applied to the title bar band and to
confirmation dialogs. A prod Connection is recognizable from across a desk. `prod` and
`unknown` both start in Read-only Mode ([ADR-0004](adr/0004-untagged-connections-are-read-only.md)).

**Typography and density.** Bold for headers and focused titles, dim for metadata, no
underlining except links. One blank line between logical groups; padding of one column inside
every pane border. Tables get a header row that stays pinned while the body scrolls.

**Iconography.** Nerd Font glyphs when detected, ASCII fallbacks otherwise, chosen so the
layout width does not change between the two. Codepoints with emoji presentation are banned
outright — they render double-width in most terminals and shear the column grid rather than
degrading quietly. An Ad-hoc Connection is therefore marked `~`, not `⚡`.

**Key names in hint bars are words, not glyphs.** `Esc`, `Enter` and `Tab` are spelled out; the
single-glyph forms are the least reliably present characters in a monospace font, and a missing
glyph breaks alignment instead of falling back. Chords keep their compact form (`⌃K`, `d d`).

**Motion.** Used sparingly and only to explain state: progress bar during scan, a 120ms fade on
toasts, a subtle pulse on a value that just changed under a live view. No decorative animation.

**Themes.** Ship a dark default and a light default, both truecolor, both contrast-checked.
Themes are data (a token → color map in config), so users can add their own.

## 6. Key screens

### 6.1 Launch, and the first-run picker
The normal path has no screen at all. The target resolves deterministically
([ADR-0001](adr/0001-connection-resolution-order.md)) and the app opens straight into the
keyspace browser with the target and Source in the title bar. Bare launch with nothing
configured connects to local Redis; there is no setup step to get to a keyspace.

The picker appears in exactly one case: nothing is configured *and* nothing was detected. It
offers detected local instances (default ports, common socket paths), a "connect to URL" field,
and — because the app never writes the config file
([ADR-0003](adr/0003-app-never-writes-config.md)) — the config path with a ready-to-paste
example Profile. That example is the only teaching moment we get, so it earns real space.

Config parse failures render here too, naming the file with line and column and showing the
offending fragment. A broken config never degrades into a silent fallback to localhost.

### 6.2 Keyspace browser
The centerpiece. Streams `SCAN` results into a virtualized list that stays interactive while
loading, with progress and a cancel affordance in the status bar. Tree mode folds on the
configured separator and shows child counts on collapsed nodes; flat mode is one keypress away.
Metadata columns fill in asynchronously — the key appears immediately, its size arrives when it
arrives, and a pending cell shows a placeholder rather than shifting the layout.

The columns are `KEY`, `TYPE`, `SIZE`, `TTL`. **Element count is not among them** (R2.4). For
strings it prints the same number twice; for collections it is genuinely diagnostic — three
members occupying 1.1 MB means somebody stored blobs as members — but that is capacity forensics,
not the find-a-key-and-read-it loop this pane exists for. It also costs a fourth pipelined
command per key (`HLEN`/`LLEN`/`SCARD`/`ZCARD`/`XLEN`), which competes with `SCAN` for the
connection while the list is still streaming. It stays reachable two ways: the Viewer header
states it on open, and sorting by count surfaces it as a temporary column.

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

### 6.4 Liveness

**There is no refresh button, because there is nothing to refresh.** The Viewer holds what the
server last said, never a memo keyed by the key's name, so a value cannot go stale behind a
control that claims to update it. The open key is tracked by the server, and when it changes the
server says so and the Viewer refetches once.

What arrives depends on where the user is. At rest, the new value simply lands, with changed
fields briefly highlighted so the change is legible rather than merely present. Scrolled into a
large hash or stream, nothing moves — the header announces it and waits, because pulling a row
out from under a reader's cursor is its own kind of broken. Mid-edit, the update is held
entirely; an unsaved buffer is never touched.

The header carries the state at all times. Ambiguity is the actual defect being designed
against: the failure users learn to distrust is not a wrong value, it is being unable to tell
"the update did nothing" from "nothing changed."

```
┌─ value pane header · liveness readout ───────────────────┐
│                                                          │
│ live and current                                  ● live │
│ an update just landed               ● live · updated now │
│ changed, you are scrolled    ● live · changed 2s ago   r │
│ mid-edit, held back              ● live · changed · held │
│ key deleted on the server               ✕ deleted 3s ago │
│ tracking unavailable         ○ manual · read 14s ago   r │
│ refetch found no change             ○ manual · unchanged │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

TTL is a special case worth stating: it counts down locally from the value read at fetch time,
so the most time-sensitive figure on screen is live at no network cost.

A key deleted, expired, or evicted while open keeps its last read value, badged
`✕ deleted 3s ago`, with mutating actions disabled. During an incident the question is almost
always *what was in it*, and that is precisely the moment the answer becomes unrecoverable.

Where the server cannot support tracking — Redis before 6, or no RESP3 — the readout says
`○ manual`, Read age replaces it, and `r` does the work. This is the same rule as the Source
readout in the title bar: the app may choose for you, but it never lets you assume wrongly.
Silent degradation here would recreate the exact frustration this screen exists to remove.

**Whose value is this?** The Viewer holds the **Open key**, and the cursor sits on the
**Selected key**; they are frequently not the same, because opening is explicit and arrowing the
list deliberately does not fire a read and a `CLIENT TRACKING` re-arm per keystroke. Nothing
about that is a freshness problem — the value is live and tracked either way — but left unsaid it
reads as the value pane showing the wrong key, which is the complaint this whole section exists
to answer, one level up.

So the state is stated on both sides of the divider, and the division of labour is: **the Viewer
says what, the divider says where.**

```
│ KEY                    TYPE   TTL ┊ user:8812:session   ⊘ not the selected key │
│ ██user:8812:cart███████zset███12m ┊ hash · 5 fields · 2.1 KB                   │
│ ● user:8812:profile    json    ∞  ┊ ttl 42m                            ● live  │
│ ● user:8812:session    hash   42m ├   ← the Open key's row, underlined         │
│ ● user:8813:session    hash   56m ┊ FIELD        VALUE                         │
```

- The Viewer is **washed** (`surface-alt`), the divider goes **dashed**, and the header carries
  `⊘ not the selected key` — or `⊘ not in the list` when the Open key has no row at all, because
  it is filtered out, folded inside a collapsed group, or waiting to be re-resolved after a
  rescan. The chip is dropped before the key name is: the name is the pane's identity.
- The keys pane **underlines** the Open key's name, and the divider cell on that row becomes `├`.
  Underline is ranked deliberately below the cursor's full-bar highlight — two marks in one list
  only work if one is obviously the junior — and it is the one modifier still free in monochrome
  once the selection has taken reverse video. When the Open key has scrolled out of the window the
  divider carries `▲`/`▼` at its edge instead.
- **When the two agree, none of this is on screen** and the tie glyph is the only trace. That
  coincidence is the point rather than redundancy: it teaches the relationship in the ordinary
  case, so the moment the panes separate reads as a change and not as a puzzle.

The wash is hue and nothing else, so monochrome loses it entirely — the dashed divider, the chip
and the underline are what carry the state there. That is why the wash is never the only signal,
and it is the same rule as everywhere else: losing colour must lose emphasis, never information.

### 6.5 Editing and confirmation
Editing opens an inline editor in the value pane, not a modal. Committing shows a **command
preview**: the literal command(s) that will be sent, plus a red/green diff for value changes.
Confirmation friction scales with blast radius — a single-key `y` for one non-prod delete, a
typed key-count for a bulk prod delete. Read-only Mode refuses at the preview, not at the keypress: the dialog composes the real
command and its blast radius first, and only then says you cannot run it. You learn what you
were about to do before you learn that you are not allowed to.

### 6.6 Dashboard
Triage-first: memory used vs. peak vs. maxmemory as a bar, hit ratio, ops/sec sparkline,
connected/blocked clients, replication role and lag, and eviction/expiry counters. Anything
alarming is colored, and every tile can be expanded into the raw `INFO` section behind it.

### 6.7 Monitor / Pub-Sub
Live tail with a filter box, pause/resume, and a persistent warning banner on `MONITOR`
explaining its cost. Buffers are bounded with a visible cap.

### 6.8 Connection states and degradation

The title bar already answers *what am I connected to, and why*. It also has to answer *is that
still true*, and *can I write*. Both are chrome the user reads without looking for it, so both
live in the same place.

```
┌─ title bar · connection and safety readout ──────────────────┐
│                                                              │
│ healthy, writes allowed             ● staging · from profile │
│ read-only by Environment          READ-ONLY environment   ⌃R │
│ read-only: target is a replica    READ-ONLY replica   locked │
│ read-only by choice                      READ-ONLY user   ⌃R │
│ maxmemory reached                    ✕ OOM · writes rejected │
│ RDB save failing                 ✕ MISCONF · writes rejected │
│ server restarting                              ⟳ loading 43% │
│ connection lost                ✕ disconnected · retry 4s   r │
│ reconnected                              ● tracking re-armed │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

**Losing the Connection is not an error screen.** Reconnection runs in the background with a
visible backoff countdown, the app stays interactive, and the Viewer keeps its last read value
badged rather than clearing — the same promise as a deleted key (§6.4). `r` retries immediately
instead of waiting out the timer, because a silent wait is a freeze wearing a different name.

**A reconnect re-arms tracking before it claims to be live.** Tracking is per-connection state,
so a transparent reconnect leaves the server no longer watching the open key. The header must
not read `● live` until it does. This is the one invariant in the design that, if it rots, puts
the product back where RedisInsight was.

**Read-only Mode shows its reason.** It can be on because the Environment is `prod` or
`unknown`, because the server reported `role:slave`, or because the user asked. Only the first
and third are liftable; a replica will refuse writes whatever the app believes. Offering `⌃R`
where it cannot work would be a toggle that lies, so the hint reads `locked` instead.

**Failing at launch is not this screen.** A target that cannot be reached exits to the shell with
a diagnostic naming the target, its Source, and the failure
([ADR-0009](adr/0009-connection-lifecycle.md)). Mid-session there is data worth keeping on
screen; at startup there is nothing to show, and an app that opens to an empty error box wastes
the reader's time.

## 7. Interaction details that carry the product

- **Optimistic focus.** Opening a key renders header and metadata instantly from what the list
  already knows, then fills the body when the fetch lands. No blank frame. What the list "knows"
  is a hint for the first frame only — it is never the value (§6.4).
- **Cancellation everywhere.** `Esc` aborts an in-flight scan, fetch, or command and says so.
- **Toasts, not dialogs, for outcomes.** Errors include the failing command and a copy action.
- **Persistent session state.** Pane split, filter, and scroll position restore on relaunch,
  keyed by target — reopening `redis-pane staging` feels like never having left, and it does not
  drag staging's filter into a prod session. This lives in a state file under
  `$XDG_STATE_HOME/redis-pane/`, never in the user's config file.
- **Copy that fits the terminal.** `y` offers key / value / `redis-cli` command / permalink-style
  reference — because the next step is usually pasting into a ticket or a shell.

## 8. Accessibility

- Never encode meaning in color alone — pair every color signal with a glyph, label, or weight.
- WCAG AA contrast for both shipped themes; a high-contrast theme as a third option.
- Full monochrome fallback that remains navigable. The type name stays in the key list when
  color is gone, so a hash is still distinguishable from a sorted set.
- Screen-reader-friendly mode: linearized rendering, no box-drawing, announced focus changes.
- No timing-dependent interactions; every chord has a non-chord equivalent in the palette.

## 9. Open design questions

- Does the dashboard belong in v1 at all, or is the slowlog plus a memory figure in the status
  bar the whole of what triage actually needs? This is now the largest remaining scope risk.
- With two panes, is the split fixed at a ratio, or does it default to whichever pane has focus?
- Does the keys pane need a permanent column header row, or can the columns be implied by the
  data and explained once in help?

**Resolved since v0.5** — the keys pane does not get liveness, and the open key remains the only
tracked thing. Deliberate, not deferred: RedisInsight declines to auto-refresh its key list for
the same reason, and re-walking a million keys on a timer is what `SCAN`-not-`KEYS` exists to
avoid. Viewport-scoped `CLIENT TRACKING` — arming only the ~30 visible rows — is the middle
ground ADR-0006 never considered, and it is rejected here on tracking-table churn during scroll;
revisiting it needs its own ADR. What the pane gets instead is the half that was already on the
wire and being discarded: `fetch_metadata` issues `TYPE` for every visible row and sees `"none"`
for a key that has been deleted, expired or evicted, so those rows are now badged `✕ … gone` at
no extra round trip. A gone row **keeps its position** — removing it would renumber everything
below the reader's cursor between one frame and the next — and keeps its last-known size, which
is usually the only answer left about a key during an incident; its TTL becomes `—`, because a
countdown is a claim about a key that is no longer there to expire. Anything beyond deletion
needs the keyspace walked again, which is what `r` in the keys pane now does (R2.7 — documented
from the start, and unimplemented until now). The value header states its own window the same
way: `12,000 items · 500 shown`, since `LLEN`/`ZCARD`/`XLEN` and the 500-row read window are
different numbers and printing only the first turns a slice into the whole.

**Resolved since v0.4** — the scan cap gets a persistent banner row above the key list, not just
a status-bar line: a copy confirmation or a sort readout could otherwise displace the one signal
that what is on screen is a prefix of the keyspace, not the whole of it — a wrong "no matches"
looking identical to "never scanned that far" was the actual risk. Reserved only while capped
(G7), and correctly survives filtering, sorting and tree/flat toggling, none of which re-scan.
Stack navigation below 70 columns is built: `Open` pushes from the key
list to a full-width value pane with a breadcrumb header (`Esc back · key-name`) in place of the
column headers there is no room for; `Esc` pops back, ahead of an unrelated in-flight scan but
behind closing help or dismissing an error. Tree is the default key view (R2.3): fewer rows at rest outweighs the
one extra keypress to reach a leaf, decided from real use against seeded keyspaces on Upstash and
Redis Cloud rather than from the mockup alone.

**Resolved since v0.2** — the sidebar (removed; [ADR-0005](adr/0005-one-connection-per-process.md)),
tabs vs. sidebar for multiple Connections (dissolved with it), the Console's shape (an overlay;
a persistent split is exactly the resident chrome G7 forbids), and the element-count column
(dropped; see §6.2).
