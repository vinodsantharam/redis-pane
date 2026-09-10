# redis-pane

A terminal UI for browsing Redis — the keyspace viewer `redis-cli` never gave you.

`redis-cli` is great for running commands, but it can't show you what's actually in your
keyspace. RedisInsight can, but it's a heavy Electron app that struggles with large keyspaces and
doesn't work over SSH. `redis-pane` is a single small binary that runs right in your terminal,
keyboard-driven, and stays fast even with huge keyspaces.

![Browsing a keyspace in redis-pane: filtering to a key, then watching it update on screen the moment another client changes it, with no keypress or refresh](demo.gif)

*Filter to a key, open it, and watch it update live the moment it changes on the server — no
refresh needed.*

## Status

This is an early alpha. It handles browsing a keyspace, viewing every Redis type, and live updates
when a value changes on the server. Mutation is arriving type by type: you can delete a key today
(`d` to stage, `y` to confirm — every mutation previews the exact command first, and Read-only Mode
still refuses it where it should), with editing values, TTLs, rename and copy coming next.

Requires Redis 6.0 or newer (Valkey works too).

## Install

Download the binary for your OS from the [Releases page](https://github.com/vinodsantharam/redis-pane/releases) — macOS, Linux, and Windows are all covered. Or run the install script:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/vinodsantharam/redis-pane/releases/download/v0.1.0-alpha.4/redis-pane-installer.sh | sh
```

On Windows, from PowerShell:

```powershell
irm https://github.com/vinodsantharam/redis-pane/releases/download/v0.1.0-alpha.4/redis-pane-installer.ps1 | iex
```

These alpha builds are unsigned, so your OS may flag them on first run — on macOS, right-click the
binary → Open → confirm; on Windows, click "More info" → "Run anyway" in the SmartScreen prompt.

Prefer to build it yourself? You'll need [Rust](https://rustup.rs):

```bash
git clone https://github.com/vinodsantharam/redis-pane.git
cd redis-pane
cargo build --release
```

The binary lands at `./target/release/redis-pane`.

## Using it

Point it at a Redis server with a URL:

```bash
redis-pane --url redis://localhost:6379
```

Or with individual flags:

```bash
redis-pane --host your-host --port 6379 --user default --password your-password --tls
```

With no arguments, it connects to `127.0.0.1:6379`. For anything you connect to often, save it as
a Profile in `~/.config/redis-pane/config.json` — see [ALPHA.md](ALPHA.md) for a full walkthrough,
including how to keep passwords out of your shell history.

Once you're in:

- `↑↓` or `j`/`k` to move, `→` or `l` to open a key (or expand/descend a tree group), `←` or `h`
  to collapse a group or jump to its parent
- `Enter` to open the selected key and move a cursor inside its value, `Esc` to leave it
- `/` to filter, `Esc` to clear
- `t` to toggle tree/flat view, `s` to cycle sort order
- `c` to copy the key name or the value, whichever pane is focused; `C` to copy a `redis-cli`
  command for the open key (locally this uses the system clipboard directly; over SSH it relies
  on OSC 52 — see [ALPHA.md](ALPHA.md#copying) if a paste comes back empty)
- `?` for help with your actual keybindings

![Folding a tree group with Left/Right, filtering down to one key, and moving a real cursor through its value with Enter and the arrow keys](navigation-demo.gif)

*`←`/`→` fold and step through the tree; `Enter` drops a cursor into the open value so you can move
through a long list without the mouse, and `Esc` takes you back to the key list.*

## Trying it out

See [ALPHA.md](ALPHA.md) for a fuller tour — connecting with a password, what to try, and what's
not built yet.

## Contributing

Working on the code? [CLAUDE.md](CLAUDE.md) has the architecture notes and conventions, and
[CONTEXT.md](CONTEXT.md) is a short glossary of terms used throughout the project. Deeper design
docs live under `docs/` if you want the full history behind a decision.

## Not planning to build

Not a server manager, not a monitoring tool, and not a replacement for `redis-cli` in scripts. No
multi-server workspace, no plugins, no export/import — one connection at a time, kept simple.
