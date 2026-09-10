# Trying redis-pane (alpha)

Thanks for testing this. It's early — expect rough edges, and please say so when you hit one.

## It cannot write anything

This build is **read-only**. There is no delete, no edit, no rename — that work hasn't started
yet (it's milestone M2). Every Redis command the app can issue is a read: `SCAN`, `TYPE`, `TTL`,
`GET`/`HGETALL`/`LRANGE`/`SMEMBERS`/`ZRANGE`/`XRANGE`, `MEMORY USAGE`. Point it at anything —
local, staging, even something you'd hesitate to open RedisInsight against — it cannot change a
byte of it.

## Install

Download the binary for your OS from the [Releases page](https://github.com/vinodsantharam/redis-pane/releases) — macOS (Intel or Apple Silicon), Linux (x86_64), and Windows are all built there. Or, on macOS/Linux, run the installer script from a release page (note: these alpha releases are marked as GitHub prereleases, so the `/latest/` URL alias doesn't resolve to them — use the tagged URL, matching whatever the current alpha tag is):

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/vinodsantharam/redis-pane/releases/download/v0.1.0-alpha.4/redis-pane-installer.sh | sh
```

On Windows, from PowerShell:

```powershell
irm https://github.com/vinodsantharam/redis-pane/releases/download/v0.1.0-alpha.4/redis-pane-installer.ps1 | iex
```

These builds are **unsigned** — expected for an alpha. On first run:

- **macOS** will refuse to open it as "from an unidentified developer." Either right-click the
  binary → Open → confirm, or run `xattr -d com.apple.quarantine ./redis-pane` once.
- **Windows** SmartScreen may show "Windows protected your PC." Click "More info" → "Run anyway."

If your platform isn't covered by the release binaries, or you'd rather build from source:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if you don't have Rust
git clone https://github.com/vinodsantharam/redis-pane.git
cd redis-pane
cargo build --release
```

The binary is at `./target/release/redis-pane`.

## Something to point it at

Any Redis 6.0 or newer works (Valkey too). If you don't have one handy:

```bash
brew install redis && brew services start redis
```

or spin up a free instance on [Upstash](https://upstash.com) or [Redis Cloud](https://redis.io) —
both work fine, TLS included.

## Connecting

Simplest — a URL directly:

```bash
redis-pane --url redis://localhost:6379
```

For anything with a password, quickest is `--user`/`--password`/`--tls` directly:

```bash
redis-pane --host your-host --port 6379 --user default --password your-actual-password --tls
```

`redis-pane` warns on stderr when you do this, because the password is then visible in your shell
history and to other users on the box via `ps`. For anything long-lived, use a Profile instead so
the secret never touches either. Write `~/.config/redis-pane/config.json`:

```json
{
  "defaultProfile": "mine",
  "profiles": {
    "mine": {
      "url": "rediss://default@your-host:6379",
      "env": "staging",
      "passwordEnv": "REDIS_PANE_PW"
    }
  }
}
```

```bash
chmod 600 ~/.config/redis-pane/config.json
export REDIS_PANE_PW=your-actual-password
redis-pane
```

(The file is refused outright if it's group- or world-readable — that's intentional, not a bug.)

Sanity-check a connection without opening the full TUI:

```bash
redis-pane --profile mine --probe
```

## What to try

- **`↑↓`** or **`j`/`k`** — move. **`→`** or **`l`** — open a key.
- **`t`** — toggle tree/flat. Tree is the default.
- **`/`** then type — filter the list. `Esc` clears it.
- **`s`** — cycle sort: scan order → name → ttl → size → type.
- **`c`** — copy the key name or the value, whichever pane is focused. **`C`** — copy a
  ready-to-run `redis-cli` command for the open key.
- **`⌃R`** — toggle Read-only Mode (some environments start locked and say why).
- **`?`** — help, showing your actual keybindings.
- **Resize the terminal.** Columns drop in order above 70 wide; below 70, opening a key pushes
  into a full-width view with a breadcrumb back to the list.
- **If your server supports `CLIENT TRACKING`**, open a key and change it from another terminal
  (`redis-cli HSET the-key field value`) — it should update on screen with no keypress. If it
  doesn't, or the header never says `● live`, that's exactly the kind of thing to report.

## Copying

`c`/`C` reach the clipboard one of two ways, chosen automatically (ADR-0013):

- **Locally, on macOS:** a direct system-clipboard call (`pbcopy`). This is the common case and
  should just work.
- **Over SSH** (detected via `SSH_TTY`/`SSH_CONNECTION`/`SSH_CLIENT`), or locally on any other
  OS: the OSC 52 terminal escape sequence. This hands the text to your *local* terminal emulator
  rather than the SSH server, but not every terminal implements it, and there's no reply to
  confirm it landed. Known-working: iTerm2, kitty, WezTerm, Alacritty, foot, Windows Terminal,
  and tmux (with `set -g set-clipboard on`). If a paste comes back empty or stale, check your
  terminal's OSC 52 / "allow clipboard access" setting first — the app can't tell the difference
  between "the terminal ignored it" and "it worked."

## What's not there yet, on purpose

- No editing, deleting, renaming, or TTL changes (M2).
- No command palette, console, server dashboard, `MONITOR`, or pub/sub (M3).
- No Cluster support — Sentinel works, Cluster is deferred.
- Nothing older than Redis 6.0 / no RESP2 — you'll get a clear message naming the version, not a
  crash.
- A handful of known UI gaps are already tracked in `docs/UI_TASKS.md` if you want to check
  before reporting something as new.

## Reporting something

Open an issue on this repo, or just message me directly. Useful to include: what you were doing,
what you expected, what happened instead, and if relevant, what you were connected to (Redis
version, and whether it's local/Upstash/Redis Cloud/something else).

Genuinely appreciate you trying this out.
