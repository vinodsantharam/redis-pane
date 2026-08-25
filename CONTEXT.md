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
property of the running app, not of the Redis user's ACL.
_Avoid_: Safe mode, locked, protected

**Palette**:
The fuzzy launcher for actions belonging to *the application* — navigation, settings, view
switching. Every action is reachable here.
_Avoid_: Command palette (ambiguous against Console), launcher, menu

**Console**:
The input surface for raw commands sent to *the Redis server*. Deliberately separate from the
Palette: the Palette drives the app, the Console drives the server.
_Avoid_: REPL, terminal, prompt, command bar

**Viewer**:
The type-specific rendering of a value — one per Redis type, all sharing a common frame so
navigation transfers between them.
_Avoid_: Renderer, panel, inspector, formatter
