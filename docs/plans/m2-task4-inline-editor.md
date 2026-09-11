# M2 task 4 (rework): inline value editor first, `$EDITOR` as an escape hatch

Status: approved by the user 2026-09-11. Executed by a subagent. Phases 0 and 1 are in scope for
this run; Phase 2 is a separate later change.

## Context

M2 task 4 (String edit) was first built by shelling out to `$VISUAL`/`$EDITOR`/`vi`. At the time
this plan was written, that work was **staged in the git index on `main` but not committed**.
Manual testing found a bug: after `:wq` in vi, the confirm dialog vanished and the keys-pane filter
line read `/ cccc/cccc`.

**Root cause (verified against crossterm 0.29 source, vim `term.c`, and ratatui's own recipe):**
- vim queries the terminal's colours at startup (`ESC]10;?` / `ESC]11;?`). The terminal replies on
  stdin, e.g. `ESC]10;rgb:cccc/cccc/cccc ESC\`.
- Our input thread (`crates/app/src/terminal.rs`) only checks its `paused` flag between 50ms
  `event::poll` calls. crossterm reads the tty in 1024-byte chunks and keeps already-parsed events
  in process-wide queues (`Parser.internal_events`, global `INTERNAL_EVENT_READER`). So the reply
  was captured by us, not vim, and delivered after resume.
- crossterm has no OSC parser: `ESC ]` becomes Alt+`]`, and each following byte becomes a `Char` key.
- Replayed through the keymap:
  - The first key discarded the confirm dialog: `confirm_key` discards on any non-`y` key.
  - `r` triggered Refetch.
  - `c` ×4 ran Copy four times, overwriting the clipboard.
  - `/` opened the filter, and `cccc/cccc` was captured as filter text.
- This is a known class of bug (helix #15284, yazi #1026), and it is worse over SSH.
- Windows is a release target (`dist-workspace.toml`: `x86_64-pc-windows-msvc`). There `$EDITOR`
  is usually unset, `vi` is not on PATH, `code` is `code.cmd`, and k9s has open Windows hang bugs
  from the same approach.

**Decision** (brainstorm with the user, after surveying lazygit, gitui, k9s, posting, ATAC,
harlequin, RedisInsight and two Redis TUIs):
- **Hybrid, embedded editor first.** `e` opens an editor *inside the value pane*, built on
  `ratatui-textarea`. `$EDITOR` survives only as an opt-in escape hatch (`E`), hardened, in a later
  change (Phase 2). Why:
  - It removes the terminal-handoff bug class from the default path.
  - It works on Windows with zero config.
  - It keeps the `✎ editing · changed · held` liveness header visible while editing (R3.8,
    DESIGN §6.4).
  - It fits the field-level edits of tasks 5–8.
  - It matches how every Redis tool surveyed, and most peer TUIs, edit.
- **Confirm dialog: only `Esc` dismisses.** Every other key (except `y`) is ignored rather than
  discarding, so a stray or leaked keystroke can't silently throw away a staged mutation.
- **Rejected alternatives:**
  - `$EDITOR` as the default: residual late-reply race, hides liveness, heavy for scalars, poor on
    Windows.
  - A hand-rolled editor: realistically 4–8 weeks.
  - An embedded PTY (`tui-term` + `vt100` + `portable-pty`): vt100 doesn't answer DA/OSC queries,
    and ConPTY can hang with inherit-cursor.
  - Single-line prompts only: can't edit JSON.

## Ground rules for the executor

- Read `CLAUDE.md` and `CONTEXT.md` first. Use the glossary's words. The core/shell boundary is
  enforced: `crates/core` must not depend on crossterm, tokio or fred.
- Cargo lives at `~/.cargo/bin`; run `source ~/.cargo/env` if `cargo` isn't found.
- **Git:**
  - Local branches and local commits only as described in Phase 1 step 0.
  - **Never push, never open a PR, never touch `main`'s history.**
  - Do not use `git reset --hard`, `git checkout -- .`, `git clean`, or anything destructive.
- Stop and report at the checkpoints below; don't continue past a checkpoint on your own.
- Default to no comments in code. Where a comment is needed, one short line explaining *why*,
  matching the repo's existing tone.

## Phase 0: spike (throwaway; decides the size policy) ⛳ CHECKPOINT AFTER THIS PHASE

Goal: prove `ratatui-textarea` 0.9.2 holds the 16ms frame budget on realistic values.

1. Add `ratatui-textarea = { version = "0.9.2", default-features = false }` to
   `crates/core/Cargo.toml`. Its only deps (`ratatui-core`, `ratatui-widgets`,
   `unicode-segmentation`, `unicode-width`) are already in `Cargo.lock`. Confirm the boundary still
   holds: `cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'` must print
   nothing.
2. Write a throwaway example, `crates/core/examples/editor_spike.rs` (same pattern as
   `crates/core/examples/memreport.rs`). Using `--release`, render `&TextArea` into a ratatui
   `Buffer` at 120×40 and 80×24 with `WrapMode::Word`.
   - Inputs:
     - (a) a ~300KB single-line minified JSON;
     - (b) a ~5,000-line pretty JSON;
     - (c) a 1KB string.
   - For each input, measure:
     - load time;
     - p50/p99 of one `insert_char` + full render;
     - one `move_cursor(CursorMove::Down)` + render;
     - a re-render at a different size (simulating a resize);
     - one `insert_str` of ~300KB (a paste).
3. Pass criterion: p99 keystroke + render < 16ms. Record the largest size that passes.
4. **CHECKPOINT:** stop. Report the timing table and whether each input passes. Don't start Phase 1.
   The example may stay uncommitted.

Phase 0 outcome feeds Phase 1: if (a) passes, no size limit. Otherwise, `e` refuses values above
the measured threshold with a notice pointing to the future escape hatch.

## Phase 1: inline editor (reworks task 4)

### Step 0: preserve the staged `$EDITOR` work

1. `git status` to confirm the staged `$EDITOR` work is present on `main`.
2. `git checkout -b m2-editor-escape-hatch` and commit the staged work there as-is: a local commit
   that snapshots the `$EDITOR` implementation for Phase 2. Leave uncommitted any spike files from
   Phase 0 (don't add them to this commit).
3. `git checkout -b m2-inline-editor` from that commit. All Phase 1 work happens on
   `m2-inline-editor`.

### Keep from the staged work (already correct and reused)

- `PendingMutation::SetString`, including `json_warning()` (`crates/core/src/state/mod.rs`).
- `Command::SetValue` (`crates/core/src/command.rs`), and `Msg::ValueSet` with its handler, which
  clears `editing` then calls `issue_refetch`.
- `crate::redis::set_value` plus its 3 Docker integration tests (`crates/app/src/redis/mod.rs`,
  `crates/app/tests/integration.rs`).
- `StringValue::raw` (`crates/core/src/state/value.rs`).
- The generic `confirm_overlay` with the stacked red/green diff and reserved hint row
  (`crates/core/src/render/mod.rs`).
- The `Msg::Failed` handler clearing `editing`, and `clear_editing()`.
- JSON fixtures in `scripts/fixtures.py` and `scripts/churn.py`.

### Remove on `m2-inline-editor` (it lives on `m2-editor-escape-hatch` for Phase 2)

- `run_editor` / `run_editor_with` / `resolve_editor` / `TempFileGuard` / `write_temp_file` /
  `EditorOutcome` and the `editor_tests` module in `crates/app/src/terminal.rs`.
- The paused-poll input thread. Restore the original blocking `event::read()` loop, and drop the
  `paused` `Arc<AtomicBool>`.
- `Command::EditInEditor` and its handler.
- `Msg::EditCommitted` / `Msg::EditDiscarded` and their handlers.
- The tokio `process` feature in `crates/app/Cargo.toml`.
- The core unit tests that only exercise the removed messages. Keep and adapt the ones about
  `SetString`, `ValueSet`, and Read-only refusal.

### Core changes (`crates/core`)

**Edit buffer.**
- New module `state/editor.rs`: `pub struct EditBuffer { area: TextArea<'static>, original:
  Vec<u8>, was_json: bool }`.
- Manual `PartialEq`/`Eq` comparing `area.lines()`, `area.cursor()`, `original`, `was_json`.
  `TextArea` is `Clone`/`Debug`/`Default` but not `PartialEq`/`Send`. `State` is only ever held on
  the main loop via `runtime.block_on` in `crates/app/src/main.rs`, so `!Send` is fine.
- Methods:
  - `from_value(&Value) -> Result<EditBuffer, &'static str>`:
    - `Value::Str` → `StringValue::raw`;
    - `Value::Json` → the pretty lines joined with `\n`, with `was_json = true`;
    - binary → "binary values aren't editable here yet";
    - collections → "only string values are editable so far";
    - enforce the Phase 0 size threshold if any.
  - `text() -> Vec<u8>`: lines joined with `\n`.
  - `json_valid() -> Option<bool>`: `Some(..)` only when `was_json`.
  - Small op methods (insert char/newline/tab, backspace, delete, move via `CursorMove`, undo, redo,
    `insert_str`).
- `WrapMode::Word`. Disable any textarea default shortcuts; feed input via
  `input_without_shortcuts` or explicit op calls so our keymap stays the single source of truth.

**`OpenKey`** (`state/open.rs`). Add `pub editor: Option<EditBuffer>`. Keep `editing: bool` as the
R3.8 guard: it spans buffer, confirm dialog and `SET` in flight. `may_apply()` and `currency()`
already use it, so set `editing = true` when the editor opens.

**Keymap** (`keymap/mod.rs`, keybindings stay data):
- `Action::Edit` (`e`): opens the inline editor.
- New editor-scoped actions, each with a label and a default binding:
  - `EditorStage` (Ctrl+S);
  - `EditorUndo` (Ctrl+Z);
  - `EditorRedo` (Ctrl+Y).
- Cancel reuses `Action::Cancel` (Esc).
- Add the new actions to `every_action_has_a_default_binding`.
- Ctrl+S is reliable in raw mode (IXON cleared on Unix, processed input off on Windows).
  Ctrl+Enter / Shift+Enter are not, so don't use them.

**Routing** (`update.rs::key_press`). Priority: confirm dialog → **editor** → filter → keymap. Add
`editor_key(state, key)`, shaped like `filter_key`/`confirm_key`:
- Keymap-resolved `EditorStage` / `EditorUndo` / `EditorRedo` first.
- Esc discards the buffer and clears `editing` (always a full discard).
- Arrows → `CursorMove::Up/Down/Back/Forward`.
- Home/End → `Head`/`End`.
- PgUp/PgDn → move by a page of lines.
- Enter → newline.
- Tab → tab character.
- Backspace / Delete → delete.
- Plain chars → insert.
- Ctrl/Alt chars not bound to an action → ignored.

**Opening.** `stage_edit` becomes "open editor":
- Builds the `EditBuffer` via `from_value` and shows its notices on refusal.
- Sets `open.editor` and `editing = true`.
- Emits no command.
- Read-only Mode does not block opening; refusal stays at `y`.

**Stage (Ctrl+S).**
- If `text() == original`: close the editor, clear `editing`, stage nothing.
- Otherwise:
  - set `state.confirm = Some(PendingMutation::SetString { name, old: original, new: text(), was_json })`;
  - close the buffer;
  - keep `editing = true`.
- From there the chokepoint is unchanged: `y` → `Command::SetValue` → `Msg::ValueSet` → Refetch.

**Confirm dialog** (`confirm_key`):
- `y` confirms, as today; the Read-only refusal is unchanged.
- `Esc` dismisses and calls `clear_editing`.
- **Any other key puts `pending` back into `state.confirm` unchanged.** This is needed because
  `key_press` `take()`s it.

**Paste.** New `Msg::Paste(String)`:
- Editor open: one `insert_str`, a single undo step.
- Filter capturing: append the text (strip newlines).
- Otherwise: ignored.

**Key types** (`msg.rs`). Add `KeyCode::Delete`. No shift/selection in this change.

**Render** (`render/mod.rs::value_pane`):
- When `open.editor` is `Some`, draw the editor's `&TextArea` in place of the body row loop (the
  loop around the `cursor_row` / `viewer.row(i, now)` code).
- Style it only from theme tokens (`set_style`, `set_cursor_style`, `set_cursor_line_style`).
  No colour literals.
- Header: the existing `✎ editing` currency, plus a muted `json ✓` or a `Token::Warn` `json ✗`
  when `was_json`.
- Hint bar while editing, read from the keymap: `⌃S stage   ⌃Z undo   Esc cancel`.

**Docs drift.** The code says `✎ editing · changed · held`, while the DESIGN §6.4 mockup says
`● live · changed · held`. Make the doc match the code.

### Shell changes (`crates/app/src/terminal.rs`)

- `translate()`: map `XKeyCode::Delete` → `KeyCode::Delete`.
- Input thread: forward `Event::Paste(s)` as `Msg::Paste(s)`.
- Enable `event::EnableBracketedPaste` at startup, and `event::DisableBracketedPaste` in
  `Guard::drop`.
- Windows has no bracketed paste (crossterm #737), so pasted text arrives as keys. Enter inserts a
  newline because commit is Ctrl+S, so this is safe, just per-character undo. Accept it and
  document it in the ADR.

### Docs (same change, per CLAUDE.md "the docs are the spec")

- **New ADR:** `docs/adr/0014-values-are-edited-inline.md`, following the format of existing ADRs
  in `docs/adr/`.
  - Decision, context (the vim colour-query leak and its crossterm evidence), Phase 0 numbers.
  - Rejected alternatives: `$EDITOR`-only, hand-rolled, PTY, prompts-only.
  - The Windows paste limitation.
  - Sources: see below.
- **`docs/DESIGN.md`:**
  - §6.5: rewrite for the inline editor.
    - `e` opens it in the value pane.
    - `⌃S` stages → diff preview → `y`.
    - `Esc` discards.
    - Live JSON indicator.
    - The size policy from Phase 0.
    - `E` `$EDITOR` escape hatch planned.
  - §4 keymap: `e` inline edit, `⌃S` stage.
  - §6.4: mockup text.
- **`docs/PLAN.md` M2:**
  - Update the task 4 row and prose to describe the inline editor.
  - Add a new row after task 4 for the `$EDITOR` escape hatch (Phase 2).
  - Fix row 12's incorrect claim that `Space` is already bound: it isn't in the default keymap.
- **`CONTEXT.md`:** add **Edit buffer** — the unsaved text in the value pane, distinct from the
  Open key's read value; a live update never touches it. Include an _Avoid_ line in the glossary's
  format.
- **`README.md` Status:** String editing happens in the value pane (`e`, `⌃S`, `y`).

### Phase 1 verification ⛳ CHECKPOINT AFTER THIS PHASE

- **Core unit tests** (`crates/core/src/update.rs`, `state/editor.rs`):
  - `e` opens a buffer holding `StringValue::raw` exactly, not the wrapped lines.
  - JSON opens with pretty lines and `was_json`.
  - Typing / undo / redo change `text()`.
  - Ctrl+S with no change closes silently.
  - Ctrl+S with a change stages `SetString` and keeps `editing`.
  - Esc discards.
  - While the editor is open, a `Msg::ValueLoaded` for that key is held, not applied (R3.8).
  - A stray key while the confirm dialog is up keeps the dialog.
  - Esc at confirm clears `editing`.
  - `Msg::Paste` inserts once.
  - Read-only Mode still refuses at `y`, not at `e`.
  - Collections and binary refuse with notices.
- **Golden frames** (`crates/core/tests/golden.rs`; regenerate intended changes with
  `UPDATE_GOLDEN=1 cargo test -p redis-pane-core`):
  - editor open on a string;
  - JSON with `json ✗`;
  - a wrapped long line;
  - the hint bar while editing;
  - the confirm diff after staging.
- **All must pass:**
  - `cargo build --workspace`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
  - `cargo tree -p redis-pane-core -e normal | grep -iE 'crossterm|tokio|fred'` (must print
    nothing)
  - If Docker is available, `cargo test -p redis-pane -- --ignored --test-threads=1`.
- Commit the Phase 1 work on `m2-inline-editor` as local commits with clear messages. No push,
  no PR.
- **CHECKPOINT:** stop. Report:
  - what changed, file by file at a high level;
  - test counts;
  - any deviations from this plan and why;
  - anything that couldn't be verified (e.g. a real interactive terminal, Windows).

Manual checks, for the user after the checkpoint:
- `./scripts/redis-up.sh`, `python3 scripts/fixtures.py --flush`, `cargo run -p redis-pane`.
- Edit a JSON `profile:*` key while `python3 scripts/churn.py --focus <key>` runs; the header should
  show `changed · held` while typing.
- Ctrl+S → diff → `y` → value refetches.
- Paste a large JSON.
- Repeat over SSH, and on Windows Terminal if available.

## Phase 2: `$EDITOR` escape hatch (NOT in this run; separate change from `m2-editor-escape-hatch`)

**Keys.** `E` in the value pane, and Ctrl+E inside the inline editor, seeding `$EDITOR` with the
current buffer.

**Hardening:**
1. **Synchronous handoff.** The main loop sets `paused` and waits for an ack from the input thread
   confirming it is parked (not inside `poll`) before leaving raw mode and spawning.
2. **After the editor exits, still in raw mode:**
   - a short settle (~150ms, tuned over SSH);
   - drain crossterm with `poll(Duration::ZERO)` / `read` and discard;
   - `tcflush(TCIFLUSH)` on Unix (via `rustix`), or `FlushConsoleInputBuffer` on Windows.
3. **Windows.** Default to `notepad` when `$VISUAL`/`$EDITOR` are unset, and launch through
   `cmd /C` so `code --wait` and `.cmd` shims work.

**Documented residual risk:** a terminal reply later than the settle window can still leak.
Acceptable only because the path is opt-in.

## Sources

- ratatui recipe, spawn vim: https://ratatui.rs/recipes/apps/spawn-vim/
- vim term.c (OSC 10/11 queries): https://github.com/vim/vim/blob/master/src/term.c ; patch 8.0.1194: https://github.com/vim/vim/commit/65e4c4f6868882a380c319632a1728a5e7d274ad
- neovim OSC 11 query: https://github.com/neovim/neovim/issues/32109
- helix late terminal replies: https://github.com/helix-editor/helix/issues/15284 ; yazi: https://github.com/sxyazi/yazi/issues/1026 , https://github.com/sxyazi/yazi/issues/2177
- crossterm split-reply parsing: https://github.com/crossterm-rs/crossterm/issues/993 ; Windows paste: https://github.com/crossterm-rs/crossterm/issues/737 , https://github.com/crossterm-rs/crossterm/pull/1030
- bubbletea reader-cancellation race: https://dr-knz.net/bubbletea-control-inversion.html , https://github.com/charmbracelet/bubbletea/issues/616
- k9s Windows editor hangs: https://github.com/derailed/k9s/issues/2087 , https://github.com/derailed/k9s/issues/1913
- kubectl editor defaults: https://github.com/kubernetes/kubectl/blob/master/pkg/cmd/util/editor/editor.go
- lazygit: https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md
- gitui: https://github.com/gitui-org/gitui/blob/master/src/keys/key_list.rs , https://github.com/gitui-org/gitui/blob/master/src/popups/externaleditor.rs
- posting: https://posting.sh/guide/external_tools/ ; harlequin: https://github.com/tconbeer/harlequin ; ATAC: https://github.com/Julien-cpsn/ATAC
- Redis TUIs: https://github.com/davidbudnick/redis-tui , https://github.com/alissonviana/redis-tui
- ratatui-textarea: https://github.com/ratatui/ratatui-textarea ; docs: https://docs.rs/ratatui-textarea/latest/ratatui_textarea/
- vt100 unhandled queries: https://docs.rs/vt100/latest/vt100/trait.Callbacks.html ; ConPTY inherit-cursor warning: https://learn.microsoft.com/en-us/windows/console/createpseudoconsole
- kitty keyboard protocol: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
