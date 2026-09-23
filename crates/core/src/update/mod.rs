//! The single entry point into the core (PLAN M0.4).

use ratatui_textarea::CursorMove;

use crate::command::ReadToken;
use crate::key::KeyName;
use crate::keymap::Action;
use crate::msg::KeyCode;
use crate::msg::KeyPress;
use crate::msg::MouseAction;
use crate::mutation::{Mutation, MutationOutcome, NotWritten};
use crate::render::layout::{self, Pane};
use crate::state::copy::{CopyWhat, redis_cli_command, value_text};
use crate::state::value::Value;
use crate::state::{
    Attachment, EditBuffer, EditTarget, FieldPart, Link, OpenKey, PendingMutation, PendingRead,
    ReadOnlyReason, ScanState, Tracking, is_valid_zset_score,
};
use crate::{Command, Msg, State};

mod confirm;
mod editor;
mod keys;
mod link;
mod mouse;
mod scan;
mod viewer;

// Every submodule is `pub(super)`, never `pub`: `update()` stays the only way
// into this module from outside it (the split plan's invariant 1). These
// globs exist so a test module's `use super::*` keeps resolving every helper
// it used before the split — see `docs/plans/review-m2-update-module-split.md`,
// "The trick that makes this low-risk". A child module (a `#[cfg(test)]` mod
// below) can see a private `use` in its parent, so nothing here needs `pub`.
use self::confirm::*;
use self::editor::*;
use self::keys::*;
use self::link::*;
use self::mouse::*;
use self::scan::*;
use self::viewer::*;

/// Mint the token for a read about to be issued, superseding any in flight.
///
/// Every read goes through here, for the same reason every read goes through
/// one `Command`: a read issued without bumping the token would be answered by
/// a reply the core could not tell apart from a stale one.
fn issue_read(state: &mut State) -> ReadToken {
    state.read_token = state.read_token.next();
    state.read_token
}

/// Issue a Refetch of the Open key: mint its token, record it as the pending
/// read so the frame can say a read is in flight until the reply lands, and
/// return the one [`Command::ReadKey`] that asks for it.
///
/// With nothing open there is nothing to refetch: no command, and no token
/// spent superseding a read nobody issued.
fn refetch(state: &mut State) -> Vec<Command> {
    let Some(open) = &state.open else {
        return Vec::new();
    };
    let (key, index) = (open.name.clone(), open.index);
    let token = issue_read(state);
    state.open_pending = Some(PendingRead {
        name: key.clone(),
        token,
        index,
        issued_at_ms: None,
        activate_cursor: false,
        own_write: false,
    });
    vec![read_key(state, key, index, token)]
}

/// The one read command, with the arming decision filled in from the core's
/// own view of the link (ADR-0006, review H3). Every read is built here.
fn read_key(state: &State, key: KeyName, index: Option<usize>, token: ReadToken) -> Command {
    Command::ReadKey {
        key,
        index,
        token,
        arm: state.read_arms_tracking(),
    }
}

/// The epoch-seconds reading [`crate::state::loaded::LoadedSet::set_ttl`]
/// stores, from the epoch-milliseconds a `Msg` carries. One place to do this
/// conversion rather than two, so both cannot quietly disagree about it.
fn epoch_secs(at_ms: u64) -> u32 {
    (at_ms / 1000) as u32
}

/// `Msg::ReadIssued`: timestamp the pending read for the loading indicator's
/// delay gate. A token that no longer names the outstanding read — already
/// superseded, or already answered — is ignored, the same discipline
/// `ValueLoaded`/`ValueGone` already apply to a stale token.
fn read_issued(mut state: State, token: ReadToken, at_ms: u64) -> (State, Vec<Command>) {
    if let Some(pending) = &mut state.open_pending
        && pending.token == token
    {
        pending.issued_at_ms = Some(at_ms);
    }
    (state, Vec::new())
}

/// `Msg::Paste`: a bracketed paste (ADR-0014). Routed by which capture mode,
/// if any, is active — the inline editor's name part, its body, or the
/// filter — rather than owned by either `editor` or `keys`, since it is the
/// one message that can land in either.
fn paste(mut state: State, text: String) -> (State, Vec<Command>) {
    if let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut) {
        // `is_single_line_capture` in place of `active_part() ==
        // Some(FieldPart::Name)` (PLAN M2 task 10, D11, ADR-0019): a TTL
        // capture has no `FieldPart` to be on, so the old predicate would
        // have sent a pasted duration into the buffer's unused `TextArea`
        // instead of its own `text` — the same routing gap `editor_key`'s
        // top check had, one call site over.
        if editor.is_single_line_capture() {
            let stripped: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
            editor.name_push_str(&stripped);
        } else {
            editor.insert_str(&text);
        }
        (state, Vec::new())
    } else if state.filtering {
        let stripped: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        state.list.filter.push_str(&stripped);
        state.rebuild_list();
        after_move(state)
    } else {
        (state, Vec::new())
    }
}

/// Takes a message, returns new state plus commands for a shell to execute.
///
/// Pure: no I/O, no clock, no randomness. Time arrives inside the message
/// (see [`Msg::ReadCompleted`]) rather than being read here, which is what
/// keeps a frame a function of state alone (ADR-0011).
pub fn update(mut state: State, msg: Msg) -> (State, Vec<Command>) {
    match msg {
        Msg::Key(key) => key_press(state, key),
        Msg::Mouse(action) => mouse_action(state, action),
        Msg::Resized { cols, rows } => {
            state.cols = cols;
            state.rows = rows;
            state.rewrap_open();
            // A filter being typed must stay where it can be seen. Narrowing
            // the terminal past two panes with the Viewer focused would
            // otherwise leave the capture running inside a pane that is no
            // longer drawn — the same invisible keystroke sink `/` used to open
            // directly, arrived at by dragging a window edge instead.
            if state.filtering && !state.keys_pane_visible() {
                state.focus = Pane::Keys;
            }
            (state, Vec::new())
        }
        Msg::ReadCompleted { at_ms } => {
            state.last_read_ms = Some(at_ms);
            (state, Vec::new())
        }
        Msg::ReadIssued { token, at_ms } => read_issued(state, token, at_ms),
        Msg::Connected {
            version,
            tracking_supported,
        } => connected(state, version, tracking_supported),
        Msg::ConnectionLost => connection_lost(state),
        Msg::ReconnectScheduled {
            attempt,
            retry_in_ms,
        } => reconnect_scheduled(state, attempt, retry_in_ms),
        Msg::TrackingArmed => tracking_armed(state),
        Msg::Invalidated => invalidated(state),
        Msg::ScanStarted { estimated_total } => scan_started(state, estimated_total),
        Msg::ScanBatch { keys } => scan_batch(state, keys),
        Msg::MetadataBatch {
            entries,
            gone,
            at_ms,
        } => metadata_batch(state, entries, gone, at_ms),
        Msg::ScanComplete => scan_complete(state),
        Msg::ScanCancelled => scan_cancelled(state),
        Msg::ScanFailed { error } => scan_failed(state, error),
        Msg::ValueLoaded {
            token,
            index,
            name,
            value,
            ttl_seconds,
            size_bytes,
            at_ms,
        } => value_loaded(
            state,
            ValueRead {
                token,
                index,
                name,
                value,
                ttl_seconds,
                size_bytes,
                at_ms,
            },
        ),
        Msg::ValueGone {
            token,
            index,
            name,
            at_ms,
        } => value_gone(state, token, index, name, at_ms),
        Msg::Copied { label, at_ms } => {
            state.notice = Some((format!("copied {label}"), at_ms));
            (state, Vec::new())
        }
        Msg::Noticed { text, at_ms } => {
            state.notice = Some((text, at_ms));
            (state, Vec::new())
        }
        Msg::ServerState {
            read_only,
            condition,
        } => server_state(state, read_only, condition),
        Msg::Failed {
            command,
            detail,
            at_ms,
        } => failed(state, command, detail, at_ms),
        Msg::Paste(text) => paste(state, text),
        Msg::MutationSettled {
            mutation,
            index,
            result,
            at_ms,
        } => mutation_settled(state, mutation, index, result, at_ms),
        Msg::Quit => quit(state),
    }
}

/// Which input mode is active. Derived from `State`, never stored: two
/// fields that must agree are two fields that can disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Confirm,
    Editing,
    Filtering,
    Normal,
}

/// The one place mode precedence is decided — confirm dialog, then editor,
/// then filter, then Normal.
pub(crate) fn mode(state: &State) -> Mode {
    // A staged mutation is a modal dialog: it is the only thing on screen
    // that can act on the keypress until it is confirmed or dismissed,
    // exactly as `state.filtering`, below, is the only thing capturing text.
    // Read-only Mode is decided *here*, at confirm — never at the keypress
    // that staged the mutation — so the preview always shows the real
    // command and its blast radius before the reader learns whether they
    // are allowed to run it (R4.4, DESIGN §6.5).
    if state.confirm.is_some() {
        return Mode::Confirm;
    }
    // The inline editor is a mode of its own too (ADR-0014), ranked above the
    // filter: both capture ordinary characters as text, and a key open for
    // editing outranks a filter that could only have been left running in
    // the background.
    if state.open.as_ref().and_then(OpenKey::typing).is_some() {
        return Mode::Editing;
    }
    // While the filter is capturing, ordinary characters are text rather than
    // commands. Only Esc and Enter mean anything else.
    if state.filtering {
        return Mode::Filtering;
    }
    Mode::Normal
}

/// Resolve a keypress through the keymap, never against hard-coded keys.
///
/// The hint bar and help overlay read the same map, so what is shown is always
/// the effective binding after user overrides (R7.5).
fn key_press(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    match mode(&state) {
        Mode::Confirm => {
            // `confirm_key` needs the `PendingMutation` by value: `mode` only
            // answers which mode is active, so the take() still happens here.
            let pending = state.confirm.take().expect("Mode::Confirm implies confirm");
            return confirm_key(state, pending, key);
        }
        Mode::Editing => return editor_key(state, key),
        Mode::Filtering => return filter_key(state, key),
        Mode::Normal => {}
    }
    let Some(action) = state.keymap.action_for(&key) else {
        return (state, Vec::new());
    };
    // An action whose pane is off screen does nothing. Below 70 columns only
    // one pane is drawn, so without this `↓` moves a cursor nobody can see and
    // `/` starts capturing into a filter line of zero width — every keypress
    // after it vanishing, `q` typing a `q`, the app indistinguishable from
    // hung. Stack navigation says the other pane is simply not there right now
    // (DESIGN §2), and the honest reading of "not there" is "does nothing",
    // not "does something invisible". `Esc` is the way back and it is in the
    // hint bar. Above 70 columns both panes are drawn and nothing changes.
    if !action.pane_is_on_screen(&state) {
        return (state, Vec::new());
    }
    match action {
        Action::Quit => quit(state),
        Action::Help => {
            state.help_open = !state.help_open;
            (state, Vec::new())
        }
        Action::Cancel => cancel(state),
        Action::Refetch => refetch_action(state),
        Action::CyclePane => {
            // Focus only ever moves to a pane there is something to focus.
            // With no key open the Viewer has nothing in it, and below 70
            // columns moving focus is what draws the other pane — so focusing
            // an empty Viewer there would black out the screen.
            state.focus = match state.focus {
                Pane::Keys if state.open.is_some() => Pane::Value,
                Pane::Keys => Pane::Keys,
                Pane::Value => Pane::Keys,
            };
            (state, Vec::new())
        }
        Action::WidenKeysPane => {
            // The clamp lives in `layout()`, which is the only place that
            // knows the terminal's actual width; here the offset is a plain
            // number, so a session that never touches this key costs nothing
            // and the core stays geometry-free (ADR-0011).
            if state.split_is_adjustable() {
                state.split_adjust = state
                    .split_adjust
                    .saturating_add(crate::render::layout::SPLIT_STEP as i16);
                state.rewrap_open();
            }
            (state, Vec::new())
        }
        Action::NarrowKeysPane => {
            if state.split_is_adjustable() {
                state.split_adjust = state
                    .split_adjust
                    .saturating_sub(crate::render::layout::SPLIT_STEP as i16);
                state.rewrap_open();
            }
            (state, Vec::new())
        }
        // `d` is focus-dependent (D4, PLAN M2 task 6), like `c`: the keys
        // pane's Selected-key delete below is unchanged; the Viewer's
        // Hash-field/Set-member/List-element delete (D5, PLAN M2 task 7,
        // ADR-0016; PLAN M2 task 8, ADR-0017) is `delete_value_row`'s job,
        // which tells the three apart itself.
        Action::Delete if state.keys_pane_focused() => delete_selected_key(state),
        Action::Delete => delete_value_row(state),
        // Nothing is staged — `key_press` intercepts every keypress before
        // this match while `state.confirm` is `Some`, so `y` only ever
        // reaches here with nothing to confirm.
        Action::ConfirmMutation => (state, Vec::new()),
        // Each of these six moves the value cursor instead of the key list
        // while cursor mode is active (`Enter`/`Action::EnterValueCursor`) —
        // the same keys, aimed at whichever thing the reader is currently
        // working in, never a silent redirect from merely looking at a pane.
        Action::MoveDown if cursor_active(&state) => move_cursor(state, 1),
        Action::MoveUp if cursor_active(&state) => move_cursor(state, -1),
        Action::PageDown if cursor_active(&state) => move_cursor(state, VALUE_PAGE_ROWS as isize),
        Action::PageUp if cursor_active(&state) => move_cursor(state, -(VALUE_PAGE_ROWS as isize)),
        Action::Top if cursor_active(&state) => cursor_to(state, 0),
        Action::Bottom if cursor_active(&state) => cursor_to(state, usize::MAX),
        Action::MoveDown => move_selection(state, 1),
        Action::MoveUp => move_selection(state, -1),
        Action::PageDown => {
            let page = state.visible_rows() as isize;
            move_selection(state, page)
        }
        Action::PageUp => {
            let page = state.visible_rows() as isize;
            move_selection(state, -page)
        }
        Action::Top => {
            state.view.selected = 0;
            after_move(state)
        }
        Action::Bottom => {
            state.view.selected = state.row_count().saturating_sub(1);
            after_move(state)
        }
        Action::Open => open_selected(state),
        Action::EnterValueCursor => enter_value_cursor(state),
        Action::Copy => {
            // Which pane the reader is looking at decides what `y` copies —
            // no mnemonic to remember, no chord to get half right (DESIGN §4).
            let what = if state.keys_pane_focused() {
                CopyWhat::Key
            } else {
                CopyWhat::Value
            };
            build_copy(state, what)
        }
        Action::CopyCommand => build_copy(state, CopyWhat::Command),
        Action::Edit => open_editor(state),
        Action::Add => begin_add_entry(state),
        // Nothing is open to edit: `key_press` intercepts every keypress
        // before this match while an editor buffer or a field-name capture
        // exists, so these only ever reach here with neither to act on.
        Action::EditorStage | Action::EditorUndo | Action::EditorRedo => (state, Vec::new()),
        Action::Filter => {
            state.filtering = true;
            (state, Vec::new())
        }
        Action::Sort => cycle_sort(state),
        // `t` is focus-dependent, exactly like `d` above (PLAN M2 task 10,
        // D2, ADR-0019): the keys pane's tree toggle is unchanged; the
        // Viewer's TTL editor is `open_ttl_editor`'s job.
        Action::ToggleTree if state.keys_pane_focused() => toggle_tree(state),
        Action::ToggleTree => open_ttl_editor(state),
        Action::CollapseGroup => collapse_group(state),
        Action::ToggleReadOnly => {
            // A replica will refuse writes whatever we believe, so this is not
            // a toggle the user gets to win (ADR-0009).
            match state.read_only {
                Some(ReadOnlyReason::Replica) => {}
                Some(_) => state.read_only = None,
                None => state.read_only = Some(ReadOnlyReason::User),
            }
            (state, Vec::new())
        }
    }
}

/// `Esc`: back out of the nearest thing first — an overlay, then an error,
/// then value-cursor mode, then an in-flight scan. One keypress, one
/// meaning.
fn cancel(mut state: State) -> (State, Vec<Command>) {
    if state.help_open {
        state.help_open = false;
        return (state, Vec::new());
    }
    if state.error.is_some() {
        state.error = None;
        return (state, Vec::new());
    }
    // The "pop" half of stack navigation: back to the list you were
    // just looking at, before an unrelated background scan. Also
    // exits value-cursor mode if it was active — the same keypress,
    // since there is nothing left to pop before it. The value stays
    // open and unchanged either way; only where plain movement is
    // aimed changes.
    if state.focus == Pane::Value {
        state.focus = Pane::Keys;
        if let Some(open) = &mut state.open {
            open.cursor_active = false;
        }
        return (state, Vec::new());
    }
    // Every in-flight operation is cancellable (PRD R7.3).
    if state.scan.is_running() {
        return (state, vec![Command::CancelScan]);
    }
    (state, Vec::new())
}

/// `r`: retry a dropped link, rescan the keys pane, apply a held Viewer
/// update, or Refetch the Open key — whichever applies, in that order.
fn refetch_action(mut state: State) -> (State, Vec<Command>) {
    // A dropped link, not merely `Link::Connecting` (before the first
    // connect): there is nothing to refetch or rescan over a dead
    // connection, only a reconnect to retry. ADR-0009: "`r` retries
    // immediately rather than waiting out the timer." Checked before
    // the pane-focus split below, which presupposes a connection to
    // act over.
    //
    // Deliberately narrower than `Liveness::Disconnected`, which also
    // covers `Link::Connecting` — real startup never renders an
    // interactive frame in that state (the shell connects before the
    // event loop starts), but a great many tests build off
    // `State::default()`, whose `Link` defaults to `Connecting`, and
    // never mean to be testing reconnection at all.
    if matches!(state.link, Link::Reconnecting { .. }) {
        return (state, vec![Command::Reconnect { after_ms: 0 }]);
    }
    // `r` acts on the focused pane and nothing else (R2.7), and the
    // hint bar names which half is in force. The keys pane is not
    // push-live — the deletions it can detect for free arrive with the
    // metadata it was already fetching, and anything else needs the
    // keyspace walked again (DESIGN §9).
    //
    // The held-update branch below used to sit *above* this check, so
    // `r` in the keys pane with an update waiting in the Viewer applied
    // that update and did not rescan — while the hint bar said
    // `r rescan`. The list did not move, no scan readout appeared, and
    // a value in the other pane changed instead: no error, no feedback,
    // nothing to explain it. That is the same defect 6d665a3 fixed,
    // surviving in the one branch that ran before the focus check.
    if state.keys_pane_focused() {
        // `None` is the same traversal the session opened with: the
        // scan has never been server-side filtered, `/` narrows the
        // Loaded set on this side, and so the active filter survives a
        // rescan without being mentioned here.
        return (state, vec![Command::StartScan { pattern: None }]);
    }
    // In the Viewer, a held update is the cheapest possible answer:
    // applying what the server has already sent is the one thing `r`
    // must never re-ask for.
    if let Some(open) = &mut state.open
        && open.pending.is_some()
    {
        open.take_pending();
        open.at_rest = true;
        return (state, Vec::new());
    }
    let commands = refetch(&mut state);
    (state, commands)
}

/// An operation failed, shown with the command that failed (R7.4).
fn failed(mut state: State, command: String, detail: String, at_ms: u64) -> (State, Vec<Command>) {
    // A failed read is still a read that answered — its loading indicator
    // would otherwise read `⟳ fetching…` forever, which is exactly the
    // "operation vanishes, nothing on screen explains it" defect the error
    // toast below exists to prevent.
    state.open_pending = None;
    // A failure while a write is in flight (the `SET` was refused) ends the
    // edit, so R3.8's guard is not held long after there is anything left to
    // protect. A buffer still being typed into is not discarded by a failure
    // that has nothing to do with it — a metadata fetch, a clipboard error —
    // which used to switch the guard off under unsaved text (review H2).
    if let Some(open) = &mut state.open
        && open.typing().is_none()
    {
        open.end_edit();
    }
    state.error = Some((format!("{command}: {detail}"), at_ms));
    (state, Vec::new())
}

/// Ends the edit a dialog was confirming, if there is one. Harmless (and a
/// no-op) for mutations that never open an edit, such as `DeleteKey` — safe to
/// call unconditionally from both of `confirm_key`'s non-executing branches.
fn clear_editing(state: &mut State) {
    if let Some(open) = &mut state.open {
        open.end_edit();
    }
}

fn quit(mut state: State) -> (State, Vec<Command>) {
    state.quitting = true;
    (state, vec![Command::Quit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::KeyCode;
    use crate::state::{Environment, Source};

    #[test]
    fn resize_updates_dimensions_and_emits_nothing() {
        let (s, cmds) = update(
            State::default(),
            Msg::Resized {
                cols: 118,
                rows: 29,
            },
        );
        assert_eq!((s.cols, s.rows), (118, 29));
        assert!(cmds.is_empty());
    }

    #[test]
    fn quit_sets_the_flag_and_asks_the_shell_to_exit() {
        let (s, cmds) = update(State::default(), Msg::Quit);
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn read_completion_records_the_clock_reading_it_was_given() {
        let (s, _) = update(State::default(), Msg::ReadCompleted { at_ms: 9_000 });
        assert_eq!(s.last_read_ms, Some(9_000));
    }

    #[test]
    fn update_is_pure_same_input_same_output() {
        let a = update(State::default(), Msg::Resized { cols: 80, rows: 24 });
        let b = update(State::default(), Msg::Resized { cols: 80, rows: 24 });
        assert_eq!(a, b);
    }

    // ── M0.5: synthetic events drive the core with no terminal attached ─────

    #[test]
    fn q_quits() {
        let (s, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::plain(KeyCode::Char('q'))),
        );
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn ctrl_c_quits() {
        let (s, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('c'))),
        );
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn ctrl_q_is_not_q() {
        let (s, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('q'))),
        );
        assert!(!s.quitting);
        assert!(cmds.is_empty());
    }

    #[test]
    fn an_unbound_key_changes_nothing() {
        let before = State::default();
        let (after, cmds) = update(
            before.clone(),
            Msg::Key(KeyPress::plain(KeyCode::Char('z'))),
        );
        assert_eq!(before, after);
        assert!(cmds.is_empty());
    }

    #[test]
    fn question_mark_toggles_the_help_overlay_and_esc_closes_it() {
        let (s, _) = update(
            State::default(),
            Msg::Key(KeyPress::plain(KeyCode::Char('?'))),
        );
        assert!(s.help_open);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!s.help_open);
    }

    /// A key open in the Viewer *and focused*, at a two-pane width — the state
    /// that `Action::Open` produces, since opening a key moves focus onto it.
    fn viewing() -> State {
        State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(
                Some(0),
                "k".into(),
                crate::state::value::Value::Str(crate::state::value::StringValue::new("v", 40)),
                -1,
                10,
                0,
            )),
            ..State::default()
        }
    }

    fn press_r(state: State) -> Vec<Command> {
        update(state, Msg::Key(KeyPress::plain(KeyCode::Char('r')))).1
    }

    #[test]
    fn r_in_the_viewer_asks_for_a_refetch_which_is_the_only_read_path() {
        assert!(matches!(
            press_r(viewing()).as_slice(),
            [Command::ReadKey { .. }]
        ));
    }

    /// R2.7: `r` acts on the focused pane and nothing else. This half was
    /// documented from the start and never wired up — the core emitted a
    /// Refetch unconditionally, so `StartScan` was unreachable.
    #[test]
    fn r_in_the_keys_pane_rescans_the_keyspace() {
        let state = State {
            cols: 130,
            rows: 40,
            ..State::default()
        };
        assert_eq!(
            press_r(state),
            vec![Command::StartScan { pattern: None }],
            "with no key open, the keys pane is what `r` acts on"
        );
    }

    /// ADR-0009: "`r` retries immediately rather than waiting out the
    /// timer." Disconnected, there is nothing to refetch or rescan — checked
    /// before the pane-focus split, in both panes, with or without a key open.
    #[test]
    fn r_reconnects_immediately_while_disconnected_regardless_of_pane() {
        fn dropped_link() -> Link {
            Link::Reconnecting {
                attempt: 1,
                retry_in_ms: Some(4_000),
            }
        }
        let keys_pane = State {
            cols: 130,
            rows: 40,
            link: dropped_link(),
            ..State::default()
        };
        assert_eq!(
            press_r(keys_pane),
            vec![Command::Reconnect { after_ms: 0 }],
            "keys pane focused, but there is nothing to rescan without a link"
        );

        let viewer = State {
            link: dropped_link(),
            ..viewing()
        };
        assert_eq!(
            press_r(viewer),
            vec![Command::Reconnect { after_ms: 0 }],
            "Viewer focused, but there is nothing to refetch without a link"
        );
    }

    #[test]
    fn r_follows_focus_at_every_width_not_merely_whether_a_key_is_open() {
        // The bug this replaced: at a two-pane width, focus was inferred from
        // `open.is_some()`. Opening a key then silently handed `r` to the
        // Viewer while the arrow keys still drove the key list, so pressing `r`
        // to rescan quietly refetched the value instead and the list did not
        // move. A key can be open and unfocused; that is what `Tab` is for.
        for cols in [60u16, 130] {
            let focused_value = State { cols, ..viewing() };
            assert!(
                matches!(
                    press_r(focused_value.clone()).as_slice(),
                    [Command::ReadKey { .. }]
                ),
                "at {cols} columns, a focused Viewer owns `r`"
            );

            let focused_keys = State {
                focus: Pane::Keys,
                ..focused_value
            };
            assert_eq!(
                press_r(focused_keys),
                vec![Command::StartScan { pattern: None }],
                "at {cols} columns, an open-but-unfocused key must not own `r`"
            );
        }
    }

    /// PLAN M2 task 10, D2, ADR-0019: `t` splits on focus the same way `r`
    /// does — keys pane still folds/unfolds the tree, value pane opens the
    /// TTL editor. This is the shape the existing focus-dependent `d` test
    /// has, one key over.
    #[test]
    fn t_follows_focus_tree_in_the_keys_pane_ttl_in_the_viewer() {
        let mut before_tree = State {
            focus: Pane::Keys,
            ..viewing()
        };
        assert!(!before_tree.tree_mode);
        let (after_tree, cmds) = update(
            before_tree.clone(),
            Msg::Key(KeyPress::plain(KeyCode::Char('t'))),
        );
        assert!(
            after_tree.tree_mode,
            "t in the keys pane still toggles the tree with a key open"
        );
        assert!(cmds.is_empty());
        assert!(
            after_tree.open.as_ref().unwrap().editor().is_none(),
            "no TTL buffer was opened"
        );
        before_tree.tree_mode = true;

        let (after_ttl, cmds) = update(viewing(), Msg::Key(KeyPress::plain(KeyCode::Char('t'))));
        assert!(
            !after_ttl.tree_mode,
            "t in the value pane must not touch tree mode"
        );
        assert!(cmds.is_empty(), "opening the TTL editor emits no command");
        assert!(
            after_ttl.open.as_ref().unwrap().is_editing(),
            "t in the value pane opens the TTL editor"
        );
        assert!(matches!(
            after_ttl.open.as_ref().unwrap().editor().unwrap().target(),
            crate::state::EditTarget::Ttl { .. }
        ));
    }

    #[test]
    fn tab_moves_focus_between_the_panes_and_r_follows_it() {
        // DESIGN §4 has specified `Tab` since the beginning; nothing
        // implemented it, which left R2.7's "acts on the focused pane" resting
        // on a focus the user could not move.
        let tab = |s: State| update(s, Msg::Key(KeyPress::plain(KeyCode::Tab))).0;

        let s = viewing();
        assert_eq!(s.focus, Pane::Value, "opening a key focuses it");

        let s = tab(s);
        assert_eq!(s.focus, Pane::Keys);
        assert_eq!(
            press_r(s.clone()),
            vec![Command::StartScan { pattern: None }],
            "Tab back to the list must make `r` rescan, without closing the key"
        );
        assert!(s.open.is_some(), "and without closing the key");

        let s = tab(s);
        assert_eq!(s.focus, Pane::Value, "Tab cycles back");
    }

    #[test]
    fn tab_does_not_focus_a_viewer_with_nothing_in_it() {
        // Below 70 columns focusing the Viewer is what *draws* it, so focusing
        // an empty one would blank the screen. Above, it would hand `r` to a
        // pane that has nothing to refetch.
        let s = State {
            cols: 60,
            rows: 40,
            ..State::default()
        };
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        assert_eq!(s.focus, Pane::Keys);
        assert_eq!(press_r(s), vec![Command::StartScan { pattern: None }]);
    }

    // ── the split is resizable (DESIGN §2, UI_TASKS severity 2) ────────────

    #[test]
    fn widen_and_narrow_move_the_split_by_one_step() {
        let state = viewing(); // 130 columns: two panes exist to resize
        let (state, cmds) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Right)));
        assert!(cmds.is_empty(), "a plain state mutation, no command needed");
        assert_eq!(state.split_adjust, crate::render::layout::SPLIT_STEP as i16);

        let (state, _) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Left)));
        assert_eq!(state.split_adjust, 0, "back where it started");

        let (state, _) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Left)));
        assert_eq!(
            state.split_adjust,
            -(crate::render::layout::SPLIT_STEP as i16)
        );
    }

    #[test]
    fn resizing_below_seventy_columns_does_nothing() {
        // There is no divider to move with one pane on screen (DESIGN §2) —
        // changing the number would be exactly the invisible action the
        // pane-visibility gate above exists to rule out for every other key.
        let state = State {
            cols: 60,
            rows: 40,
            ..State::default()
        };
        let (state, _) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Right)));
        assert_eq!(state.split_adjust, 0);
    }

    #[test]
    fn the_adjustment_survives_moving_around_the_app() {
        // Not reset by anything else the reader does in the same session —
        // opening a key, moving the cursor, changing focus.
        let mut state = viewing();
        (state, _) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Right)));
        let widened = state.split_adjust;
        assert_ne!(widened, 0);

        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(state.split_adjust, widened, "untouched by ordinary use");
    }

    #[test]
    fn ctrl_r_toggles_read_only_but_a_replica_cannot_be_lifted() {
        // Off -> user-imposed.
        let (s, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))),
        );
        assert_eq!(s.read_only, Some(ReadOnlyReason::User));
        // And back off again.
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))));
        assert_eq!(s.read_only, None);

        // A replica refuses to be lifted, because the server would refuse the
        // write regardless and a toggle that does nothing is a toggle that lies.
        let replica = State {
            read_only: Some(ReadOnlyReason::Replica),
            ..State::default()
        };
        let (s, _) = update(replica, Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))));
        assert_eq!(s.read_only, Some(ReadOnlyReason::Replica));
    }

    #[test]
    fn a_rebound_key_takes_effect_in_update_not_just_in_the_hint() {
        let mut keymap = crate::keymap::Keymap::default();
        keymap.bind(Action::Quit, KeyPress::ctrl(KeyCode::Char('x')));
        let state = State {
            keymap,
            ..State::default()
        };
        let (s, cmds) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Char('x'))));
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn prod_and_unknown_are_read_only_by_default_local_and_staging_are_not() {
        assert!(Environment::Prod.read_only_by_default());
        assert!(Environment::Unknown.read_only_by_default());
        assert!(!Environment::Local.read_only_by_default());
        assert!(!Environment::Staging.read_only_by_default());
    }

    #[test]
    fn source_labels_name_where_the_target_came_from() {
        assert_eq!(Source::Default.label(), "from default");
        assert_eq!(
            Source::Profile("staging".into()).label(),
            "from profile staging"
        );
    }

    fn open_with_live_editor() -> State {
        let value = crate::state::Value::Hash(crate::state::value::PairValue {
            pairs: vec![("f".into(), "v".into())],
            total: 1,
        });
        let mut open = OpenKey::new(Some(0), "k".into(), value, -1, 0, 0);
        open.edit = crate::state::EditPhase::Typing(EditBuffer::new_hash_field());
        State {
            open: Some(open),
            ..State::default()
        }
    }

    #[test]
    fn confirm_outranks_a_live_editor() {
        let mut s = open_with_live_editor();
        s.confirm = Some(PendingMutation::DeleteKey {
            index: 0,
            name: "k".into(),
        });
        assert_eq!(mode(&s), Mode::Confirm);
    }

    #[test]
    fn editor_outranks_the_filter() {
        let mut s = open_with_live_editor();
        s.filtering = true;
        assert_eq!(mode(&s), Mode::Editing);
    }

    #[test]
    fn filter_alone_is_filtering() {
        let s = State {
            filtering: true,
            ..State::default()
        };
        assert_eq!(mode(&s), Mode::Filtering);
    }

    #[test]
    fn bare_state_is_normal() {
        assert_eq!(mode(&State::default()), Mode::Normal);
    }
}

#[cfg(test)]
mod honesty_tests {
    //! The three ways the app could quietly claim something untrue, each with
    //! a test so it cannot come back.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::{Environment, Liveness, ServerCondition};

    /// R7.4: an error is never swallowed. A Redis failure that produces no
    /// visible effect is indistinguishable from the app deciding to do nothing.
    #[test]
    fn a_failure_surfaces_with_the_command_that_caused_it() {
        let (state, _) = update(
            State::default(),
            Msg::Failed {
                command: "reading user:8812:session".into(),
                detail: "WRONGTYPE Operation against a key".into(),
                at_ms: 1_000,
            },
        );
        let shown = state.error_text().expect("the failure must be visible");
        assert!(shown.contains("reading user:8812:session"), "{shown}");
        assert!(shown.contains("WRONGTYPE"), "{shown}");
    }

    #[test]
    fn an_error_does_not_fade_the_way_a_confirmation_does() {
        // A confirmation nobody reads has cost nothing. An error nobody reads
        // is an error nobody handled.
        let (state, _) = update(
            State::default(),
            Msg::Failed {
                command: "c".into(),
                detail: "d".into(),
                at_ms: 0,
            },
        );
        assert!(state.error_text().is_some());
        assert!(
            state.notice_now(State::NOTICE_MS * 10).is_none(),
            "a copy notice would be gone by now"
        );
        assert!(state.error_text().is_some(), "the error is not");
    }

    #[test]
    fn esc_dismisses_an_error_before_it_touches_anything_else() {
        let (state, _) = update(
            State::default(),
            Msg::Failed {
                command: "c".into(),
                detail: "d".into(),
                at_ms: 0,
            },
        );
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(state.error_text().is_none());
    }

    /// ADR-0009: the shell now reports reconnects, and the core must drop the
    /// liveness claim when it does.
    #[test]
    fn a_reported_reconnect_drops_the_liveness_claim_until_re_armed() {
        let viewing = State {
            open: Some(OpenKey::new(
                Some(0),
                "k".into(),
                crate::state::value::Value::Str(crate::state::value::StringValue::new("v", 40)),
                -1,
                1,
                0,
            )),
            ..State::default()
        };
        let (state, _) = update(
            viewing,
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        let (state, _) = update(state, Msg::TrackingArmed);
        assert_eq!(state.liveness(), Liveness::Live);

        let (state, _) = update(state, Msg::ConnectionLost);
        assert_eq!(state.liveness(), Liveness::Disconnected);

        // The shell re-probes and re-announces; still not live until armed.
        let (state, cmds) = update(
            state,
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert_ne!(state.liveness(), Liveness::Live);
        assert!(cmds.iter().any(|c| matches!(c, Command::ReadKey { .. })));
    }

    /// R1.15: a replica outranks an Environment guard, and cannot be lifted.
    #[test]
    fn a_replica_guard_outranks_an_environment_guard() {
        let state = State {
            read_only: Some(ReadOnlyReason::Environment),
            ..State::default()
        };
        let (state, _) = update(
            state,
            Msg::ServerState {
                read_only: Some(ReadOnlyReason::Replica),
                condition: None,
            },
        );
        assert_eq!(state.read_only, Some(ReadOnlyReason::Replica));
        assert!(!state.read_only_liftable(), "⌃R must not offer to lift it");
    }

    #[test]
    fn a_failover_away_from_a_replica_falls_back_to_the_environment_guard() {
        // Sentinel promotes the replica we were reading. The `replica` reason
        // no longer applies, but a prod Environment guard still does.
        let mut state = State {
            read_only: Some(ReadOnlyReason::Replica),
            ..State::default()
        };
        state.connection.environment = Environment::Prod;
        let (state, _) = update(
            state,
            Msg::ServerState {
                read_only: None,
                condition: None,
            },
        );
        assert_eq!(state.read_only, Some(ReadOnlyReason::Environment));
    }

    #[test]
    fn the_server_changing_its_mind_never_lifts_a_user_imposed_guard() {
        let state = State {
            read_only: Some(ReadOnlyReason::User),
            ..State::default()
        };
        let (state, _) = update(
            state,
            Msg::ServerState {
                read_only: None,
                condition: None,
            },
        );
        assert_eq!(state.read_only, Some(ReadOnlyReason::User));
    }

    #[test]
    fn a_condition_that_rejects_writes_reaches_the_chrome() {
        let (state, _) = update(
            State::default(),
            Msg::ServerState {
                read_only: None,
                condition: Some(ServerCondition::Misconf),
            },
        );
        assert_eq!(state.condition, Some(ServerCondition::Misconf));
        assert!(
            state
                .condition
                .unwrap()
                .readout()
                .contains("writes rejected")
        );
    }
}

#[cfg(test)]
mod stack_navigation_tests {
    //! Below 70 columns there is one pane at a time (DESIGN §2). Opening a
    //! key pushes into it; Esc pops back — a stack two rungs deep, not a full
    //! navigation history, because that is all this layout ever needs.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::LoadedSet;

    fn browsing() -> State {
        let mut keys = LoadedSet::default();
        keys.push(b"k1");
        keys.push(b"k2");
        let mut state = State {
            keys,
            rows: 30,
            ..State::default()
        };
        state.rebuild_list();
        state
    }

    #[test]
    fn opening_a_key_pushes_into_value_view() {
        let state = browsing();
        assert_eq!(state.focus, Pane::Keys);
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        assert_eq!(state.focus, Pane::Value);
        assert!(matches!(cmds.first(), Some(Command::ReadKey { .. })));
    }

    #[test]
    fn esc_pops_back_to_keys_from_value_view() {
        let state = browsing();
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        assert_eq!(state.focus, Pane::Value);

        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(state.focus, Pane::Keys);
        assert!(
            cmds.is_empty(),
            "popping the stack is not itself a scan cancellation"
        );
    }

    #[test]
    fn esc_closes_help_before_popping_the_value_view() {
        // Nearest thing first: an overlay you opened on top of everything
        // closes before backing out of the navigation underneath it.
        let mut state = browsing();
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        state.help_open = true;

        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!state.help_open);
        assert_eq!(
            state.focus,
            Pane::Value,
            "one Esc closes one thing, not two"
        );
    }

    #[test]
    fn esc_pops_the_value_view_before_cancelling_an_unrelated_scan() {
        let mut state = browsing();
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        (state, _) = update(
            state,
            Msg::ScanStarted {
                estimated_total: 10,
            },
        );
        assert!(state.scan.is_running());

        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(state.focus, Pane::Keys);
        assert!(
            cmds.is_empty(),
            "the scan must still be running, not cancelled by this Esc"
        );
        assert!(state.scan.is_running());
    }

    #[test]
    fn opening_a_key_at_a_wide_terminal_is_harmless() {
        // The flag is set unconditionally on Open; at any density wider than
        // Single, layout() never consults it, so this must change nothing an
        // actual reader can see.
        let state = browsing();
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        assert_eq!(state.focus, Pane::Value);
        // No assertion on rendered output here — render::layout's own test
        // (`focus_is_ignored_at_any_wider_density`) is the proof;
        // this just confirms the state transition still happens uniformly.
    }
}
