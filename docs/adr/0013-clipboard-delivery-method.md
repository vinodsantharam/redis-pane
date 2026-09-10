# ADR-0013 — Clipboard delivery: SSH-detected, not fixed to one method

**Status:** Accepted · **Date:** 2026-09-10

## Context

`crates/app/src/clipboard.rs` copied to the clipboard exclusively via the OSC 52 terminal
escape sequence, on the reasoning that `redis-pane` is meant to be used inside an SSH session on
a bastion host, where a native clipboard call would copy to the *remote* machine's clipboard —
which is nobody's. That reasoning is correct for the SSH case, but it was applied unconditionally,
including to the far more common case of running `redis-pane` locally on a laptop.

OSC 52 support is inconsistent across local terminal emulators: some don't implement it at all,
some gate it behind a preference the user has to find and enable, and there is no reply to read —
a terminal that ignores the sequence does so silently. In practice this meant a local user could
press copy, see a "copied" confirmation, and find their system clipboard completely unchanged,
with no error and no way to tell why from inside the app. This was reported directly: a local
user (no SSH, no tmux) copying a key or value got the confirmation but the paste target kept
showing old, unrelated clipboard content.

No ADR previously covered clipboard delivery at all — the OSC-52-only decision lived only as a
doc comment in `clipboard.rs` and one bullet in `CLAUDE.md`.

## Decision

**The method is chosen per-process by detecting whether the session is over SSH**, using the
same signal every SSH-aware tool relies on: the presence of any of `SSH_TTY`, `SSH_CONNECTION`,
or `SSH_CLIENT` in the environment.

- **Over SSH:** OSC 52, unconditionally, on every OS. A native call here would write to the
  *server's* clipboard, which is exactly the wrong target regardless of platform — this is the
  one case the original decision was protecting, and it is unchanged.
- **Not over SSH, on macOS:** a native call (`pbcopy`, invoked as a subprocess with the payload
  piped to its stdin — no new dependency, matching this file's existing "no dependency" stance).
  There is no remote host to get wrong, and it sidesteps OSC 52's inconsistent local support
  entirely.
- **Not over SSH, elsewhere:** OSC 52, same as before. No native path is implemented yet for
  Linux or Windows.

The two code paths already differ in whether they touch the app's own stdout — OSC 52 does (it
shares the terminal ratatui draws on, so the caller still needs to `term.clear()` afterward);
native writes to `pbcopy`'s own stdin pipe and never touches it, so no redraw is needed on that
path. `MAX_BYTES` truncation (terminals commonly cap OSC 52 payloads) is likewise an OSC-52-only
concern — native has no comparable limit.

## Alternatives considered

**Keep OSC 52 unconditionally.** Rejected — this is the status quo that produced the bug report.
It is the *correct* choice over SSH, which is exactly why the fix is conditional rather than a
flat switch to native.

**Switch to a native clipboard crate/API unconditionally.** Rejected outright — this is the
scenario the original decision exists to prevent. `redis-pane` is explicitly meant to run over
SSH on a bastion host; copying there always to the *server's* clipboard would be silently useless
at best, or copy sensitive key/value data somewhere the user didn't intend at worst.

**Detect terminal support (e.g. a terminfo/capability query) instead of SSH.** Rejected for now:
OSC 52 has no synchronous "did this work" reply to probe, so detecting *terminal* support
reliably would need a round-trip query-and-timeout scheme with its own failure modes. SSH
presence is a much simpler, already-correct signal for the one case that actually matters (don't
send a native call over the wire), and it costs nothing to check.

## Consequences

- macOS locally is now reliable; Linux and Windows locally are unchanged (still OSC 52-only,
  with the same silent-failure risk this ADR describes) until a native path is added for them —
  `xclip`/`wl-copy` on Linux, a Win32 clipboard call on Windows.
- `resolve_method` follows the same shape as `terminal::resolve_color_depth`: a thin
  `std::env::var`-reading wrapper (`detect_method`) around a pure, unit-testable decision function
  that takes `Option<String>`s — this workspace forbids `unsafe`, which `std::env::set_var` needs,
  so environment-dependent decisions are always split this way to stay testable.
- `Command::CopyToClipboard`'s handling in `crates/app/src/terminal.rs` now decides whether to
  `term.clear()` based on which method ran, rather than doing it unconditionally.
