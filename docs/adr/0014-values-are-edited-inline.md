# ADR-0014 — Values are edited inline, with `$EDITOR` as an escape hatch

**Status:** Accepted · **Date:** 2026-09-11

## Context

M2 task 4 (String edit, R3.2, R4.1) was first built by shelling out to `$VISUAL`/`$EDITOR`/`vi` on
a temp file: `e` wrote the value out, handed the terminal to the reader's editor, and read the
result back on exit. That implementation was staged in the git index — never committed — when
manual testing over a real terminal found a live bug: after `:wq` in vim, the confirm dialog
vanished and the keys-pane filter line read `/ cccc/cccc`, from nothing the reader had typed.

**Root cause, verified against crossterm 0.29 source, vim's `term.c`, and ratatui's own recipe for
spawning an external editor:**

- vim queries the terminal's colours at startup (`ESC]10;?` / `ESC]11;?`, patch 8.0.1194). The
  terminal replies on stdin — e.g. `ESC]10;rgb:cccc/cccc/cccc ESC\` — whenever it gets around to
  it, which is not guaranteed to be before the child process exits.
- This app's input thread (`crates/app/src/terminal.rs`) paused between 50ms `event::poll` calls
  while the editor had the terminal, but crossterm reads the tty in whatever chunks arrive and
  keeps already-parsed events in process-wide queues (`Parser.internal_events`, the global
  `INTERNAL_EVENT_READER`). A reply that lands after the editor's own read loop has moved on, but
  before our thread resumes polling, is captured by *us*, not by vim — and delivered once we
  resume, indistinguishable from real keystrokes.
- crossterm has no OSC parser: `ESC ]` decodes as Alt+`]`, and every following byte becomes a
  plain `Char` key press.
- Replayed through the keymap, that burst did real damage: the first key discarded the confirm
  dialog (any non-`y` key dismissed it — a defect this ADR also fixes, see below); `r` triggered a
  Refetch; `c` × 4 ran Copy four times, overwriting the clipboard; and `/` opened the filter, with
  `cccc/cccc` captured as filter text.
- This is a known class of bug elsewhere in the terminal-app ecosystem (helix #15284, yazi #1026,
  #2177), and it is worse over SSH, where the round trip to the terminal emulator is slower and
  the reply is more likely to arrive after the editor has already exited.
- Windows is a release target (`dist-workspace.toml` builds `x86_64-pc-windows-msvc`). There,
  `$EDITOR` is usually unset, `vi` is not on `PATH`, a configured `code` resolves to `code.cmd`
  (needing a shell to invoke), and k9s has open Windows hang bugs from the same
  hand-terminal-to-a-child-process approach (derailed/k9s#2087, #1913).

**A Phase 0 spike (`crates/core/examples/editor_spike.rs`, throwaway, not committed) measured
whether an embedded, in-process editor could hold the 16ms frame budget instead** — using
`ratatui-textarea` 0.9.2 in release, one `insert_char` plus a full render, at 120×40 and 80×24 with
`WrapMode::Word`:

| Input | p50 | p99 | Result |
|---|---|---|---|
| ~300KB single-line minified JSON | 7.8ms | 8.3–18ms across runs | borderline |
| ~5,000-line pretty JSON (77KB) | 1.9ms | 2–3ms (one 18ms outlier) | pass |
| 1KB string | 0.1ms | 0.2ms | pass |

Bisecting a single unwrapped line found: 10KB p99 ≤1.3ms, 30KB ≤1.5ms, 100KB ≤5.7ms, 200KB
≤8.5ms — every one of those clean on every run — while 300KB was noisy run to run. The cost is
dominated by re-wrapping one long logical line at the pane's width, not by the edit operation
itself; outliers also appeared at 77KB, so they most likely reflect scheduler jitter on the
machine the spike ran on rather than a size effect.

## Decision

**`e` opens an embedded editor (`ratatui-textarea`) directly in the value pane**, replacing the
body rows with an editable buffer. `Ctrl-S` stages it for the existing command-preview confirm
dialog; `Esc` discards it outright. `$EDITOR` survives only as an opt-in escape hatch (`E`, seeding
`$VISUAL`/`$EDITOR`/`vi` with the buffer), planned for a later, separate, hardened change and not
part of this one. Reasons, weighed against the survey below:

- It removes the terminal-handoff bug class — and the race inside it — from the default path
  entirely. There is no child process, so there is no second reader of the tty to race.
- It works on Windows with zero configuration: no `$EDITOR` to resolve, no shell quoting, no
  `.cmd` shims.
- It keeps the `✎ editing · changed · held` liveness header visible while editing (R3.8, DESIGN
  §6.4) — a child process owning the whole terminal cannot show it at all.
- It fits the field-level edits planned for tasks 5–8 (hash fields, list/set/zset members):
  a shell-out per field would be one process launch per edit.
- It matches how most peer tools handle this: lazygit, gitui and k9s embed simple editing rather
  than shelling out for their own inline fields, and the two Redis TUIs surveyed
  (redis-tui, alissonviana/redis-tui) do not offer a full external-editor path at all. Where an
  external editor *is* offered (posting, harlequin, ATAC — none Redis-specific), it is explicitly
  the power-user escape hatch, not the default, which is the shape this ADR keeps.

**The size policy: `EditBuffer::from_value` refuses values whose raw byte length exceeds 200KB
(`state::editor::MAX_EDIT_BYTES`, `200 * 1024`), with a notice.** 200KB is the largest size the
Phase 0 bisection found clean on every run (≤8.5ms p99); 300KB was the size that first turned
noisy. The notice says the value is too large to edit inline and mentions that an
external-editor escape hatch is planned — it does not name a keybinding, because none exists in
this change.

**The confirm dialog now discards only on `y` (confirm) or `Esc` (dismiss); every other key is
ignored and the pending mutation is put back unchanged.** This is the fix for the actual reported
bug: the OSC-reply-replay above found the confirm dialog live for exactly one keystroke, and the
old rule — *any* non-`y` key dismisses — discarded the whole preview on the very first stray key
in the burst. A confirm dialog is not a place where "do nothing" should be reachable by an
unrecognized key; only its two real actions should be.

## Alternatives considered

**Keep `$EDITOR` as the default, harden the handoff instead.** Rejected for this change. Hardening
is possible — a synchronous pause/resume handshake with the input thread, a settle delay plus an
input flush before returning to raw mode, `tcflush(TCIFLUSH)` on Unix — and is exactly what Phase 2
(`m2-editor-escape-hatch`) will do once the escape hatch is opt-in rather than the default path.
Even hardened, a terminal reply later than the settle window can still leak; accepting that
residual risk is reasonable for an opt-in path a reader chooses, not for the default one.

**A hand-rolled editor widget.** Rejected as disproportionate: realistically 4–8 weeks to reach
parity with an existing crate on cursor movement, wrapping, undo/redo and Unicode handling, none
of which is this project's differentiator.

**An embedded PTY** (`tui-term` + `vt100` + `portable-pty`), giving the reader their actual
`$EDITOR` inside a pane instead of handing over the whole terminal. Rejected: `vt100`'s own docs
say it does not answer terminal queries (DA, OSC) at all
(<https://docs.rs/vt100/latest/vt100/trait.Callbacks.html>), which reproduces this ADR's bug
inside the pane instead of removing it; and Windows' ConPTY has documented hangs with
inherit-cursor mode (<https://learn.microsoft.com/en-us/windows/console/createpseudoconsole>),
which is exactly the platform this decision most needs to work on.

**Single-line prompts only, no multi-line buffer.** Rejected: cannot edit JSON, which is a named
requirement (R3.2) and the value shape this project's users hit constantly (cached API responses,
session blobs).

## Consequences

- `crates/core/src/state/editor.rs` adds `EditBuffer`, wrapping a `ratatui-textarea` `TextArea` —
  the Edit buffer is the reader's *unsaved* text, distinct from the Open key's read value (see
  CONTEXT.md). `TextArea` is not `Send`; this is fine because `State` lives only on the main loop
  (`runtime.block_on` in `crates/app/src/main.rs`), never crossing a thread boundary.
- `OpenKey::editor: Option<EditBuffer>` is new state; `OpenKey::editing: bool` keeps its existing
  meaning as the R3.8 guard spanning the buffer, the confirm dialog, and the `SET` in flight — a
  live update is held throughout, not just while the buffer is open.
- The keymap gains three actions with default bindings — `EditorStage` (`Ctrl-S`), `EditorUndo`
  (`Ctrl-Z`), `EditorRedo` (`Ctrl-Y`) — chosen because `Ctrl-S` is reliable in raw mode on every
  platform this ships for (IXON cleared on Unix, processed input off on Windows), where
  `Ctrl-Enter`/`Shift-Enter` are not, so they are not used for commit.
- `crates/app/src/terminal.rs` loses `run_editor`/`run_editor_with`/`resolve_editor`/
  `TempFileGuard`/`write_temp_file`, the `paused: Arc<AtomicBool>` the input thread checked, and
  its bounded-poll loop — restored to a plain blocking `event::read()`, since nothing here hands
  the terminal to a child process any more. `EnableBracketedPaste`/`DisableBracketedPaste` are
  added at startup/teardown, and `Event::Paste` now forwards as `Msg::Paste` — one `insert_str`
  into the editor when it is open, appended (newlines stripped) into an open filter otherwise.
- **Windows has no bracketed paste support** (crossterm-rs/crossterm#737): pasted text there
  arrives as a burst of individual key events instead of one `Msg::Paste`. This is safe rather
  than silently broken — `Enter` inserts a newline in the editor (commit is `Ctrl-S`, not `Enter`)
  — but it costs one undo step per character instead of one for the whole paste. Accepted as a
  documented platform limitation rather than solved here.
- The `$EDITOR` implementation this ADR replaces is preserved, not deleted: it lives as a snapshot
  commit on `m2-editor-escape-hatch`, to be hardened and reworked into the `E` escape hatch in a
  separate later change.
- `tokio`'s `process` feature is dropped from `crates/app/Cargo.toml` — nothing left in this crate
  spawns a child process.

## Sources

- ratatui recipe, spawn vim: <https://ratatui.rs/recipes/apps/spawn-vim/>
- vim `term.c` (OSC 10/11 queries): <https://github.com/vim/vim/blob/master/src/term.c>; the
  patch that introduced them:
  <https://github.com/vim/vim/commit/65e4c4f6868882a380c319632a1728a5e7d274ad>
- neovim OSC 11 query discussion: <https://github.com/neovim/neovim/issues/32109>
- helix, late terminal replies: <https://github.com/helix-editor/helix/issues/15284>; yazi:
  <https://github.com/sxyazi/yazi/issues/1026>, <https://github.com/sxyazi/yazi/issues/2177>
- crossterm, split-reply parsing: <https://github.com/crossterm-rs/crossterm/issues/993>; Windows
  bracketed paste: <https://github.com/crossterm-rs/crossterm/issues/737>,
  <https://github.com/crossterm-rs/crossterm/pull/1030>
- bubbletea, reader-cancellation race: <https://dr-knz.net/bubbletea-control-inversion.html>,
  <https://github.com/charmbracelet/bubbletea/issues/616>
- k9s Windows editor hangs: <https://github.com/derailed/k9s/issues/2087>,
  <https://github.com/derailed/k9s/issues/1913>
- kubectl editor defaults: <https://github.com/kubernetes/kubectl/blob/master/pkg/cmd/util/editor/editor.go>
- lazygit config: <https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md>
- gitui: <https://github.com/gitui-org/gitui/blob/master/src/keys/key_list.rs>,
  <https://github.com/gitui-org/gitui/blob/master/src/popups/externaleditor.rs>
- posting external tools: <https://posting.sh/guide/external_tools/>; harlequin:
  <https://github.com/tconbeer/harlequin>; ATAC: <https://github.com/Julien-cpsn/ATAC>
- Redis TUIs surveyed: <https://github.com/davidbudnick/redis-tui>,
  <https://github.com/alissonviana/redis-tui>
- ratatui-textarea: <https://github.com/ratatui/ratatui-textarea>; docs:
  <https://docs.rs/ratatui-textarea/latest/ratatui_textarea/>
- vt100 unhandled terminal queries: <https://docs.rs/vt100/latest/vt100/trait.Callbacks.html>;
  ConPTY inherit-cursor warning:
  <https://learn.microsoft.com/en-us/windows/console/createpseudoconsole>
- kitty keyboard protocol (considered, not adopted, for future keybinding reliability):
  <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>
