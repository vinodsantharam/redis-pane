# redis-pane

A terminal UI for browsing and editing Redis. This glossary pins the terms that mean something
specific here — where the project's language differs from Redis's own, or from everyday usage.

## Language

### Connecting

**Profile**:
A named, saved description of how to reach a Redis server, stored in the user's config file. It
carries an address, a credential reference, an Environment, and an optional note.
_Avoid_: Saved connection, bookmark, target, server entry

**Connection**:
A live session against a Redis server. A Connection may originate from a Profile or be Ad-hoc.
Exactly one exists per running process, against one database, fixed at launch.
_Avoid_: Session, link, client

**Ad-hoc Connection**:
A Connection with no Profile behind it — resolved from a command-line flag or from the
environment. It exists only for the life of the session.
_Avoid_: Temporary connection, unsaved connection, quick connect

**Environment**:
The blast-radius classification of a Profile or Connection: `local`, `staging`, `prod`, or
`unknown`. It drives chrome colour and whether Read-only Mode starts on.
_Avoid_: Env tag, stage, tier, severity

**Source**:
Where a Connection's address was resolved from — a flag, a Profile, or the environment. Always
displayed alongside the target so the answer to "what am I connected to, and why" is never
inferred.
_Avoid_: Origin, provenance

### Working

**Read-only Mode**:
An app state in which every mutating operation is refused before it can be composed. It is a
property of the running app, not of the Redis user's ACL. It always carries a **reason** —
`environment`, `replica`, or `user` — and the reason is displayed, because only some of them can
be lifted.
_Avoid_: Safe mode, locked, protected

**Staged mutation**:
A change proposed but not yet sent — the state between pressing a mutating key (`d`, or
committing an edit) and confirming it. Every mutation is staged before it can run: staging composes
the real command, and only confirming decides whether Read-only Mode lets it through.
_Avoid_: Pending action, draft, queued command

**Command preview**:
The literal command a staged mutation shows before it runs, in the confirm dialog. It exists so
the reader learns what they were about to do before they learn whether they are allowed to —
Read-only Mode refuses at this dialog, never at the keypress that staged it. For a mutation sent as
a guarded script rather than the literal command (a Hash field edit or add, ADR-0015), the preview
is the effective command it performs plus the guard it runs under — never the `EVAL "<script>" …`
it is actually sent as, which would defeat the point of showing it at all.
_Avoid_: Confirmation dialog (names the UI, not what it shows), dry run

**Loaded set**:
The keys the current session has scanned and is holding. Sorting, filtering and bulk selection
all operate on the Loaded set, never on the whole keyspace — so the UI must be able to say how
large it is and whether scanning is still adding to it.
_Avoid_: Result set, cache, buffer, page

**Selected key**:
The key on the row under the cursor in the keys pane. It is what `→` would open and what a
key-list action operates on — it is *not*, by itself, the key on screen in the Viewer.
_Avoid_: Highlighted key, current key, cursor key

**Open key**:
The key the Viewer is showing. Opening is explicit, so the Open key is frequently **not** the
Selected key — the user moves the cursor without opening, and the Viewer goes on holding what it
was given. That divergence is legal and useful, and it is the app's job to say when it applies:
both panes state the relationship rather than leaving the two names to be compared by eye.
A key is identified by its exact bytes, which need not be valid UTF-8. The name shown on screen
is for reading only: every read and write names the key by its bytes, never by what is displayed.
_Avoid_: Current key, active key, focused key, previewed key

**Palette** _(withdrawn, see [ADR-0020](docs/adr/0020-no-command-palette.md))_:
Shipped in 0.1.0-alpha.14 as the fuzzy launcher for actions belonging to *the application* —
navigation, settings, view switching, every action reachable from one place. Removed: every
action it listed already had a key of its own, so it was only ever a slower route to the same
thing. Kept defined here so the term stays readable in commits and code written before the
withdrawal.
_Avoid_: Command palette (ambiguous against Console), launcher, menu

**Console**:
The input surface for raw commands sent to *the Redis server*. Not scheduled for M3
(PRD §10).
_Avoid_: REPL, terminal, prompt, command bar

**Refetch**:
A single re-read of the Open key from the server — type, memory usage, TTL, and the value
itself. It always issues real commands; there is no value cache to serve from.
_Avoid_: Refresh, reload, re-query

**Liveness**:
The property that the Open key updates itself when it actually changes, driven by server
invalidation rather than by a timer or a button. When the server cannot provide it, the app says
so rather than falling back quietly.
_Avoid_: Auto-refresh, polling, watch, live mode

**Read age**:
How long ago the value on screen was read. It is displayed whenever Liveness is unavailable,
because a value with no stated age is a value the user has to guess about.
_Avoid_: Staleness, last updated, timestamp

**TTL**:
The server's fact about when a key expires — what `TTL key` answers, in whole seconds, with `-1`
meaning the key has no expiry at all. A TTL only ever changes because a read brought back a new
one or a write set one; the app never infers it.
_Avoid_: Expiry time, lifetime, timeout (each names a different thing to a Redis reader)

**Countdown**:
The local projection of a TTL, ticking down between reads so the most time-sensitive figure on
screen costs no round trip. It is computed per frame from the TTL and the moment it was read —
never polled, never refreshed, and never a second source of truth: a countdown that disagrees with
the server is a countdown waiting for the next read, not a TTL.
_Avoid_: Timer, ticker, live TTL, auto-refresh

**Duration expression**:
What the reader types into the TTL field — `5m`, `2h30m`, `+30m`, `-10m`, or `never`. It is not a
TTL: it is a request that resolves to one of four operations against the key's current TTL, and
the field says which one while it is being typed. **Set** replaces the expiry, **persist** removes
it, and **extend** and **shorten** move it by a signed delta the server applies to its own TTL.
_Avoid_: Timeout string, TTL input, expiry value

**Viewer**:
The type-specific rendering of a value — one per Redis type, all sharing a common frame so
navigation transfers between them.
_Avoid_: Renderer, panel, inspector, formatter

**Edit buffer**:
The reader's unsaved text in the value pane, open while an inline edit is in progress. It is
distinct from the Open key's read value: the buffer is what the reader is typing, never a copy of
what the server last said, and a live update never touches it while it is open. It becomes a
Staged mutation only when the reader asks — `Ctrl-S` — never on its own. Once staged it stays on
screen, taking no more keys, until the write is read back or the edit ends. If the key is gone by
the time it would be written, nothing is written — the key is never recreated — and the buffer is
handed back to be typed into.
_Avoid_: Draft, scratch value, cache
