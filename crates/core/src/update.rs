//! The single entry point into the core (PLAN M0.4).

use crate::keymap::Action;
use crate::msg::KeyCode;
use crate::msg::KeyPress;
use crate::render::layout::SinglePaneView;
use crate::state::copy::{CopyWhat, redis_cli_command, value_text};
use crate::state::{Link, OpenKey, ReadOnlyReason, ScanState, Tracking};
use crate::{Command, Msg, State};

/// Takes a message, returns new state plus commands for a shell to execute.
///
/// Pure: no I/O, no clock, no randomness. Time arrives inside the message
/// (see [`Msg::ReadCompleted`]) rather than being read here, which is what
/// keeps a frame a function of state alone (ADR-0011).
pub fn update(mut state: State, msg: Msg) -> (State, Vec<Command>) {
    match msg {
        Msg::Key(key) => key_press(state, key),
        Msg::Resized { cols, rows } => {
            state.cols = cols;
            state.rows = rows;
            (state, Vec::new())
        }
        Msg::ReadCompleted { at_ms } => {
            state.last_read_ms = Some(at_ms);
            (state, Vec::new())
        }
        Msg::Connected {
            version,
            tracking_supported,
        } => {
            // Connected is never live on its own. Tracking starts Available at
            // best, and only an accepted arming moves it to Armed — which is
            // ADR-0009's invariant expressed as a state transition rather than
            // as a comment somebody has to remember.
            state.link = Link::Up {
                version,
                tracking: if tracking_supported {
                    Tracking::Available
                } else {
                    Tracking::Unsupported
                },
            };
            let commands = if tracking_supported {
                vec![Command::RefetchOpenKey]
            } else {
                Vec::new()
            };
            (state, commands)
        }
        Msg::ConnectionLost => {
            state.link = Link::Reconnecting {
                attempt: 1,
                retry_in_ms: 0,
            };
            (state, vec![Command::Reconnect { after_ms: 0 }])
        }
        Msg::ReconnectScheduled {
            attempt,
            retry_in_ms,
        } => {
            state.link = Link::Reconnecting {
                attempt,
                retry_in_ms,
            };
            (
                state,
                vec![Command::Reconnect {
                    after_ms: retry_in_ms,
                }],
            )
        }
        Msg::TrackingArmed => {
            if let Link::Up { tracking, .. } = &mut state.link
                && *tracking != Tracking::Unsupported
            {
                *tracking = Tracking::Armed;
            }
            (state, Vec::new())
        }
        Msg::Invalidated => {
            // The push consumed the arming. Refetching is what re-arms, and
            // there is only one command that can do it.
            if let Link::Up { tracking, .. } = &mut state.link
                && *tracking == Tracking::Armed
            {
                *tracking = Tracking::Consumed;
            }
            (state, vec![Command::RefetchOpenKey])
        }
        Msg::ScanStarted { estimated_total } => {
            state.keys.clear();
            state.rebuild_list();
            state.scan = ScanState::Running {
                scanned: 0,
                estimated_total,
            };
            (state, Vec::new())
        }
        Msg::ScanBatch { keys } => scan_batch(state, keys),
        Msg::MetadataBatch { entries, gone } => {
            for e in entries {
                state.keys.set_kind(e.index, e.kind);
                state.keys.set_ttl(e.index, e.ttl_seconds);
                state.keys.set_size(e.index, e.size_bytes);
            }
            // A deleted key keeps its row and its last-known TTL and size; only
            // the type byte gives way to the tombstone. Removing the row would
            // renumber everything below the cursor between one frame and the
            // next, which is a worse lie than a row that says it is gone.
            for index in gone {
                state.keys.set_gone(index);
            }
            (state, Vec::new())
        }
        Msg::ScanComplete => {
            // A cap reached mid-scan already told its own story; completing
            // afterwards must not overwrite it with a smaller truth.
            if !matches!(state.scan, ScanState::Capped { .. }) {
                state.scan = ScanState::Complete {
                    total: state.keys.len() as u64,
                };
            }
            (state, Vec::new())
        }
        Msg::ScanCancelled => {
            if !matches!(state.scan, ScanState::Capped { .. }) {
                state.scan = ScanState::Cancelled {
                    scanned: state.keys.len() as u64,
                };
            }
            (state, Vec::new())
        }
        Msg::ScanFailed { error } => {
            state.scan = ScanState::Failed { error };
            (state, Vec::new())
        }
        Msg::ValueLoaded {
            index,
            name,
            value,
            ttl_seconds,
            size_bytes,
            at_ms,
        } => {
            match &mut state.open {
                // A read of the key already open is an update, and where it
                // lands depends on where the reader is (ADR-0006).
                Some(open) if open.index == index => {
                    open.absorb(value, ttl_seconds, size_bytes, at_ms)
                }
                _ => {
                    state.open = Some(OpenKey::new(
                        index,
                        name,
                        value,
                        ttl_seconds,
                        size_bytes,
                        at_ms,
                    ))
                }
            }
            (state, Vec::new())
        }
        Msg::ValueGone { at_ms } => {
            // The value stays on screen, badged. During an incident the
            // question is almost always what was in it, and this is the moment
            // that answer becomes unrecoverable.
            if let Some(open) = &mut state.open {
                open.deleted_at_ms = Some(at_ms);
                open.pending = None;
            }
            (state, Vec::new())
        }
        Msg::Copied { label, at_ms } => {
            state.notice = Some((format!("copied {label}"), at_ms));
            (state, Vec::new())
        }
        Msg::ServerState {
            read_only,
            condition,
        } => {
            // A replica outranks an Environment guard: that reason cannot be
            // lifted, so claiming the weaker one would offer a toggle the
            // server will refuse anyway (ADR-0009). A user-imposed guard is
            // never taken away by the server changing its mind.
            state.read_only = match (read_only, state.read_only) {
                (Some(reason), _) => Some(reason),
                (None, Some(ReadOnlyReason::Replica)) => state
                    .connection
                    .environment
                    .read_only_by_default()
                    .then_some(ReadOnlyReason::Environment),
                (None, existing) => existing,
            };
            state.condition = condition;
            (state, Vec::new())
        }
        Msg::Failed {
            command,
            detail,
            at_ms,
        } => {
            state.error = Some((format!("{command}: {detail}"), at_ms));
            (state, Vec::new())
        }
        Msg::Quit => quit(state),
    }
}

/// Absorb a page of keys, stopping the traversal if the cap is reached.
///
/// This is the single place the cap is enforced, so there is no path that grows
/// the Loaded set past it (ADR-0010).
fn scan_batch(mut state: State, keys: Vec<Vec<u8>>) -> (State, Vec<Command>) {
    for key in keys {
        if !state.keys.push(&key) {
            state.scan = ScanState::Capped {
                at: state.keys.len(),
            };
            return (state, vec![Command::CancelScan]);
        }
    }
    if let ScanState::Running {
        estimated_total, ..
    } = state.scan
    {
        state.scan = ScanState::Running {
            scanned: state.keys.len() as u64,
            estimated_total,
        };
    }
    state.rebuild_list();
    // Keys render as they arrive; their metadata should follow, but only for
    // the rows a reader can actually see.
    let indices = state.rows_needing_metadata();
    if indices.is_empty() {
        (state, Vec::new())
    } else {
        (state, vec![Command::FetchMetadata { indices }])
    }
}

/// Resolve a keypress through the keymap, never against hard-coded keys.
///
/// The hint bar and help overlay read the same map, so what is shown is always
/// the effective binding after user overrides (R7.5).
fn key_press(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    // While the filter is capturing, ordinary characters are text rather than
    // commands. Only Esc and Enter mean anything else.
    if state.filtering {
        return filter_key(state, key);
    }
    // `y` arms a chord; the next key says what to copy.
    if state.copy_pending {
        state.copy_pending = false;
        return copy_key(state, key);
    }
    let Some(action) = state.keymap.action_for(&key) else {
        return (state, Vec::new());
    };
    match action {
        Action::Quit => quit(state),
        Action::Help => {
            state.help_open = !state.help_open;
            (state, Vec::new())
        }
        Action::Cancel => {
            // Esc backs out of the nearest thing first: an overlay, then an
            // error, then an in-flight scan. One keypress, one meaning.
            if state.help_open {
                state.help_open = false;
                return (state, Vec::new());
            }
            if state.error.is_some() {
                state.error = None;
                return (state, Vec::new());
            }
            // The "pop" half of stack navigation: back to the list you were
            // just looking at, before an unrelated background scan.
            if state.single_pane_view == SinglePaneView::Value {
                state.single_pane_view = SinglePaneView::Keys;
                return (state, Vec::new());
            }
            // Every in-flight operation is cancellable (PRD R7.3).
            if state.scan.is_running() {
                return (state, vec![Command::CancelScan]);
            }
            (state, Vec::new())
        }
        Action::Refetch => {
            // `r` is a scoped Refetch, not a global refresh. When an update is
            // already waiting, it applies that instead of asking the server for
            // something it has already been told. This branch comes first in
            // both panes: a held update is the cheapest possible answer, and
            // re-asking for it would be the one thing `r` must never do.
            if let Some(open) = &mut state.open
                && open.pending.is_some()
            {
                open.take_pending();
                open.at_rest = true;
                return (state, Vec::new());
            }
            // Otherwise `r` acts on the focused pane and nothing else (R2.7).
            // The keys pane is not push-live — the deletions it can detect for
            // free arrive with the metadata it was already fetching, and
            // anything else needs the keyspace walked again (DESIGN §9).
            if state.keys_pane_focused() {
                // `None` is the same traversal the session opened with: the
                // scan has never been server-side filtered, `/` narrows the
                // Loaded set on this side, and so the active filter survives a
                // rescan without being mentioned here.
                return (state, vec![Command::StartScan { pattern: None }]);
            }
            (state, vec![Command::RefetchOpenKey])
        }
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
        Action::Open => {
            let Some(index) = state.selected_key() else {
                return (state, Vec::new());
            };
            let Some(name) = state.keys.name(index).map(|n| n.to_vec()) else {
                return (state, Vec::new());
            };
            // Below 70 columns this is the "push" half of stack navigation
            // (DESIGN §2). Setting it unconditionally is harmless at any wider
            // density, where layout() never consults it — both panes already
            // show, so there is nothing this can visibly change.
            state.single_pane_view = SinglePaneView::Value;
            (state, vec![Command::OpenKey { index, name }])
        }
        Action::ViewerDown => scroll_viewer(state, ViewerMove::By(1)),
        Action::ViewerUp => scroll_viewer(state, ViewerMove::By(-1)),
        // A page is approximated at 20 rows: the viewer does not know its own
        // rendered height here, and a fixed page beats no paging at all for a
        // 500-entry zset or a large hex dump.
        Action::ViewerPageDown => scroll_viewer(state, ViewerMove::By(20)),
        Action::ViewerPageUp => scroll_viewer(state, ViewerMove::By(-20)),
        Action::ViewerTop => scroll_viewer(state, ViewerMove::Absolute(0)),
        Action::ViewerBottom => scroll_viewer(state, ViewerMove::Absolute(usize::MAX)),
        Action::Copy => {
            state.copy_pending = true;
            (state, Vec::new())
        }
        Action::Filter => {
            state.filtering = true;
            (state, Vec::new())
        }
        Action::Sort => {
            state.list.sort = state.list.sort.next();
            state.rebuild_list();
            after_move(state)
        }
        Action::ToggleTree => {
            state.tree_mode = !state.tree_mode;
            state.view.selected = 0;
            state.view.offset = 0;
            state.rebuild_list();
            after_move(state)
        }
        Action::ToggleGroup => {
            if state.tree_mode
                && let Some(prefix) = group_prefix_at(&state, state.view.selected)
            {
                state.tree.toggle(&prefix);
                state.rebuild_list();
            }
            after_move(state)
        }
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

/// The full prefix of the group under the cursor, e.g. `user:8812:`.
fn group_prefix_at(state: &State, row: usize) -> Option<String> {
    use crate::state::tree::Row;
    let Some(Row::Group { depth, .. }) = state.tree.row(row) else {
        return None;
    };
    // Rebuild the prefix from the first key beneath this group, which is the
    // next Key row at greater depth.
    let index = (row + 1..state.tree.len()).find_map(|r| state.tree.key_index(r))?;
    let name = state.keys.name_str(index)?;
    let sep = state.tree.separator;
    let mut out = String::new();
    for (i, segment) in name.split(sep).enumerate() {
        if i > depth as usize {
            break;
        }
        out.push_str(segment);
        out.push(sep);
    }
    Some(out)
}

/// The second half of the `y` chord.
///
/// `y y` copies the key, `y v` the value, `y c` a `redis-cli` command. Anything
/// else cancels — an unrecognised second key should do nothing rather than
/// guess, because the clipboard is somewhere the user cannot see.
fn copy_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    let what = match key.code {
        KeyCode::Char('y') | KeyCode::Char('k') => CopyWhat::Key,
        KeyCode::Char('v') => CopyWhat::Value,
        KeyCode::Char('c') => CopyWhat::Command,
        _ => return (state, Vec::new()),
    };

    // The key name is copyable from the list alone; the other two need an open
    // value, because there is nothing to copy until the server has said what it
    // holds (ADR-0006).
    let text = match what {
        CopyWhat::Key => match state.open.as_ref().map(|o| o.name.clone()) {
            Some(name) => name,
            None => match state.selected_key().and_then(|i| state.keys.name_str(i)) {
                Some(name) => name.into_owned(),
                None => return (state, Vec::new()),
            },
        },
        CopyWhat::Value => match &state.open {
            Some(open) => value_text(&open.value, open.read_at_ms),
            None => {
                state.notice = Some(("nothing open to copy".into(), 0));
                return (state, Vec::new());
            }
        },
        CopyWhat::Command => match &state.open {
            Some(open) => redis_cli_command(&state.connection.target, open),
            None => {
                state.notice = Some(("nothing open to copy".into(), 0));
                return (state, Vec::new());
            }
        },
    };

    (
        state,
        vec![Command::CopyToClipboard {
            text,
            label: what.label(),
        }],
    )
}

/// Keys typed while the filter is capturing.
fn filter_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    match key.code {
        KeyCode::Esc => {
            // Esc abandons the filter entirely rather than keeping a partial
            // pattern nobody typed on purpose.
            state.filtering = false;
            state.list.filter.clear();
            state.rebuild_list();
            after_move(state)
        }
        KeyCode::Enter => {
            state.filtering = false;
            (state, Vec::new())
        }
        KeyCode::Backspace => {
            state.list.filter.pop();
            state.rebuild_list();
            after_move(state)
        }
        KeyCode::Char(c) if !key.ctrl && !key.alt => {
            state.list.filter.push(c);
            state.view.selected = 0;
            state.view.offset = 0;
            state.rebuild_list();
            after_move(state)
        }
        _ => (state, Vec::new()),
    }
}

/// Move the selection, clamped to the Loaded set.
fn move_selection(mut state: State, by: isize) -> (State, Vec<Command>) {
    if state.row_count() == 0 {
        return (state, Vec::new());
    }
    let last = state.row_count() as isize - 1;
    let next = (state.view.selected as isize + by).clamp(0, last);
    state.view.selected = next as usize;
    after_move(state)
}

/// Scrolling reveals rows whose metadata has not been fetched, so every move
/// asks for what is newly visible — and only what is visible (R2.4).
fn after_move(mut state: State) -> (State, Vec<Command>) {
    // Moving in the key list is not scrolling the value, so the Viewer returns
    // to rest: whatever arrives next may land without asking.
    if let Some(open) = &mut state.open
        && open.offset == 0
    {
        open.at_rest = true;
    }
    let height = state.visible_rows();
    state.view = state.view.scrolled_to_selection(height);
    let indices = state.rows_needing_metadata();
    if indices.is_empty() {
        (state, Vec::new())
    } else {
        (state, vec![Command::FetchMetadata { indices }])
    }
}

/// Where a viewer scroll ends up: relative or absolute.
enum ViewerMove {
    By(isize),
    /// Clamped to the last row; `usize::MAX` means "the end".
    Absolute(usize),
}

/// Scroll the open value. Shared by every viewer, because the frame around a
/// value is one abstraction (R3.1) — a hex dump and a hash pane both scroll
/// this way.
fn scroll_viewer(mut state: State, mv: ViewerMove) -> (State, Vec<Command>) {
    if let Some(open) = &mut state.open {
        let last = open.value.viewer().row_count().saturating_sub(1);
        open.offset = match mv {
            ViewerMove::By(by) => (open.offset as isize + by).clamp(0, last as isize) as usize,
            ViewerMove::Absolute(n) => n.min(last),
        };
        // Scrolling away from the top means updates are announced rather than
        // applied; returning to the top does not by itself undo that — the
        // reader chooses when a held update lands.
        if open.offset > 0 {
            open.at_rest = false;
        }
    }
    (state, Vec::new())
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

    /// `r` on a state with a key open in the Viewer, at a two-pane width.
    fn viewing() -> State {
        State {
            cols: 130,
            rows: 40,
            open: Some(OpenKey::new(
                0,
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
        assert_eq!(press_r(viewing()), vec![Command::RefetchOpenKey]);
    }

    /// R2.7: `r` acts on the focused pane and nothing else. This half was
    /// documented from the start and never wired up — the core emitted
    /// `RefetchOpenKey` unconditionally, so `StartScan` was unreachable.
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

    #[test]
    fn below_two_panes_r_follows_whichever_pane_is_on_screen() {
        // Stack navigation: only one pane exists, so which one is showing is
        // the whole of the answer — an open key is not enough on its own.
        let stacked = State {
            cols: 60,
            rows: 40,
            single_pane_view: SinglePaneView::Value,
            ..viewing()
        };
        assert_eq!(press_r(stacked.clone()), vec![Command::RefetchOpenKey]);

        let popped = State {
            single_pane_view: SinglePaneView::Keys,
            ..stacked
        };
        assert_eq!(
            press_r(popped),
            vec![Command::StartScan { pattern: None }],
            "popping back to the list must not leave `r` pointed at the Viewer"
        );
    }

    #[test]
    fn a_held_update_is_applied_before_either_pane_gets_a_say() {
        // The cheapest possible answer, and re-asking the server for something
        // it has already sent is the one thing `r` must never do.
        let mut state = viewing();
        // An update only waits when the reader is not at rest — otherwise it
        // lands straight away and there is nothing for `r` to apply.
        state.open.as_mut().unwrap().at_rest = false;
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                index: 0,
                name: "k".into(),
                value: crate::state::value::Value::Str(crate::state::value::StringValue::new(
                    "v2", 40,
                )),
                ttl_seconds: -1,
                size_bytes: 11,
                at_ms: 1_000,
            },
        );
        assert!(state.open.as_ref().unwrap().pending.is_some(), "held");
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('r'))));
        assert!(cmds.is_empty(), "no read is asked for");
        assert!(state.open.as_ref().unwrap().pending.is_none(), "applied");
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
}

#[cfg(test)]
mod liveness_invariants {
    //! The two re-arm invariants from ADR-0006 and ADR-0009, asserted against
    //! the state machine rather than against a server. A network test can show
    //! that the shell *did* re-arm once; these show that the header *cannot*
    //! claim liveness without it, on any path.

    use super::*;
    use crate::state::Liveness;

    fn connected(tracking_supported: bool) -> State {
        let (s, _) = update(
            State::default(),
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported,
            },
        );
        s
    }

    fn armed() -> State {
        let (s, _) = update(connected(true), Msg::TrackingArmed);
        s
    }

    #[test]
    fn a_fresh_connection_is_not_live_until_tracking_is_armed() {
        let s = connected(true);
        assert_eq!(
            s.liveness(),
            Liveness::Manual,
            "connected is not the same as live"
        );
        assert_eq!(armed().liveness(), Liveness::Live);
    }

    #[test]
    fn connecting_emits_a_refetch_which_is_the_only_thing_that_arms() {
        let (_, cmds) = update(
            State::default(),
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);
    }

    /// ADR-0009: a reconnect that does not re-arm must not present as live.
    #[test]
    fn a_reconnect_drops_liveness_and_does_not_get_it_back_for_free() {
        let live = armed();
        assert_eq!(live.liveness(), Liveness::Live);

        let (dropped, _) = update(live, Msg::ConnectionLost);
        assert_eq!(dropped.liveness(), Liveness::Disconnected);

        // The server comes back. Tracking does not come back with it: a fresh
        // connection tracks nothing, which the spike confirmed directly.
        let (back, cmds) = update(
            dropped,
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert_ne!(
            back.liveness(),
            Liveness::Live,
            "reconnected but not re-armed"
        );
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);

        let (rearmed, _) = update(back, Msg::TrackingArmed);
        assert_eq!(rearmed.liveness(), Liveness::Live);
    }

    /// ADR-0006: tracking is consumed by its own invalidation, so every
    /// invalidation must produce a Refetch — and Refetch is the only read path,
    /// so it always re-arms.
    #[test]
    fn an_invalidation_consumes_the_arming_and_forces_a_refetch() {
        let (after, cmds) = update(armed(), Msg::Invalidated);
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);

        let (rearmed, _) = update(after, Msg::TrackingArmed);
        assert_eq!(rearmed.liveness(), Liveness::Live);
    }

    #[test]
    fn a_server_that_refuses_tracking_degrades_to_manual_and_never_claims_live() {
        let s = connected(false);
        assert_eq!(s.liveness(), Liveness::Manual);

        // Even an erroneous TrackingArmed cannot promote an unsupported server.
        let (s, _) = update(s, Msg::TrackingArmed);
        assert_eq!(s.liveness(), Liveness::Manual);
    }

    #[test]
    fn liveness_is_derived_and_disconnected_always_wins() {
        // Whatever the tracking state was, losing the link shows disconnected.
        for start in [connected(true), armed(), connected(false)] {
            let (s, _) = update(start, Msg::ConnectionLost);
            assert_eq!(s.liveness(), Liveness::Disconnected);
        }
    }

    #[test]
    fn backoff_is_scheduled_visibly_rather_than_waited_out_silently() {
        let (s, cmds) = update(armed(), Msg::ConnectionLost);
        assert!(matches!(s.link, Link::Reconnecting { .. }));
        assert_eq!(cmds, vec![Command::Reconnect { after_ms: 0 }]);

        let (s, cmds) = update(
            s,
            Msg::ReconnectScheduled {
                attempt: 3,
                retry_in_ms: 4_000,
            },
        );
        assert_eq!(
            s.link,
            Link::Reconnecting {
                attempt: 3,
                retry_in_ms: 4_000
            }
        );
        assert_eq!(cmds, vec![Command::Reconnect { after_ms: 4_000 }]);
    }

    /// The exhaustive statement of the rule: across every reachable link and
    /// tracking combination, `live` appears only where arming has happened.
    #[test]
    fn live_appears_only_where_tracking_is_armed_or_mid_refetch() {
        let cases = [
            (Link::Connecting, Liveness::Disconnected),
            (
                Link::Reconnecting {
                    attempt: 1,
                    retry_in_ms: 0,
                },
                Liveness::Disconnected,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Unsupported,
                },
                Liveness::Manual,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Available,
                },
                Liveness::Manual,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Armed,
                },
                Liveness::Live,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Consumed,
                },
                Liveness::Live,
            ),
        ];
        for (link, expected) in cases {
            let state = State {
                link: link.clone(),
                ..State::default()
            };
            assert_eq!(state.liveness(), expected, "for {link:?}");
        }
    }
}

#[cfg(test)]
mod scan_tests {
    //! The keyspace traversal, as seen by the core: batches arrive, the cap is
    //! enforced in one place, and cancellation is always available.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::LoadedSet;

    /// A scan on a realistically sized terminal, so `visible_rows` is sane.
    fn started(estimated_total: u64) -> State {
        let base = State {
            rows: 30,
            ..State::default()
        };
        let (s, _) = update(base, Msg::ScanStarted { estimated_total });
        s
    }

    fn batch(n: usize, from: usize) -> Vec<Vec<u8>> {
        (from..from + n)
            .map(|i| format!("k:{i}").into_bytes())
            .collect()
    }

    #[test]
    fn a_scan_starts_empty_and_reports_progress_against_an_estimate() {
        let s = started(180_000);
        assert!(s.keys.is_empty());
        assert_eq!(
            s.scan,
            ScanState::Running {
                scanned: 0,
                estimated_total: 180_000
            }
        );

        let (s, cmds) = update(
            s,
            Msg::ScanBatch {
                keys: batch(500, 0),
            },
        );
        assert_eq!(s.keys.len(), 500);
        assert_eq!(s.scan.readout(), "scanning 500 of ~180,000");

        // The keys render immediately; their metadata is requested separately,
        // and only for the rows on screen (R2.4).
        let Some(Command::FetchMetadata { indices }) = cmds.first() else {
            panic!("expected a metadata request, got {cmds:?}");
        };
        assert_eq!(
            indices.len(),
            s.visible_rows(),
            "500 keys arrived, but only the visible window is fetched"
        );
    }

    #[test]
    fn batches_accumulate_so_results_render_as_they_arrive() {
        let mut s = started(1_000);
        for i in 0..4 {
            (s, _) = update(
                s,
                Msg::ScanBatch {
                    keys: batch(250, i * 250),
                },
            );
        }
        assert_eq!(s.keys.len(), 1_000);
        assert_eq!(s.keys.name_str(0).unwrap(), "k:0");
        assert_eq!(s.keys.name_str(999).unwrap(), "k:999");
    }

    #[test]
    fn starting_a_new_scan_discards_the_previous_one() {
        let (s, _) = update(started(10), Msg::ScanBatch { keys: batch(5, 0) });
        assert_eq!(s.keys.len(), 5);
        let (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 10,
            },
        );
        assert!(
            s.keys.is_empty(),
            "a filter change must not append to stale results"
        );
    }

    #[test]
    fn reaching_the_cap_stops_the_scan_and_says_so() {
        let mut s = State {
            keys: LoadedSet::with_cap(600),
            ..State::default()
        };
        (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 10_000,
            },
        );
        let (s, cmds) = update(
            s,
            Msg::ScanBatch {
                keys: batch(1_000, 0),
            },
        );

        assert_eq!(
            cmds,
            vec![Command::CancelScan],
            "the shell must be told to stop"
        );
        assert_eq!(s.keys.len(), 600);
        assert!(s.keys.is_capped());
        assert_eq!(s.scan, ScanState::Capped { at: 600 });
        assert!(s.scan.readout().contains("narrow the filter"));
    }

    #[test]
    fn a_completion_arriving_after_the_cap_does_not_erase_it() {
        // The shell's CancelScan will be answered by ScanComplete or
        // ScanCancelled. Neither may overwrite the more important fact.
        let mut s = State {
            keys: LoadedSet::with_cap(10),
            ..State::default()
        };
        (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 100,
            },
        );
        (s, _) = update(s, Msg::ScanBatch { keys: batch(50, 0) });
        assert!(matches!(s.scan, ScanState::Capped { .. }));

        let (s, _) = update(s, Msg::ScanComplete);
        assert!(
            matches!(s.scan, ScanState::Capped { .. }),
            "the cap is the story"
        );

        let mut s2 = State {
            keys: LoadedSet::with_cap(10),
            ..State::default()
        };
        (s2, _) = update(
            s2,
            Msg::ScanStarted {
                estimated_total: 100,
            },
        );
        (s2, _) = update(s2, Msg::ScanBatch { keys: batch(50, 0) });
        let (s2, _) = update(s2, Msg::ScanCancelled);
        assert!(matches!(s2.scan, ScanState::Capped { .. }));
    }

    #[test]
    fn completing_normally_reports_the_total() {
        let (s, _) = update(
            started(700),
            Msg::ScanBatch {
                keys: batch(700, 0),
            },
        );
        let (s, _) = update(s, Msg::ScanComplete);
        assert_eq!(s.scan, ScanState::Complete { total: 700 });
        assert_eq!(s.scan.readout(), "700 keys");
    }

    #[test]
    fn esc_cancels_an_in_flight_scan() {
        let s = started(10_000);
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(cmds, vec![Command::CancelScan]);
    }

    #[test]
    fn esc_closes_help_first_and_leaves_the_scan_alone() {
        // Help is the nearer thing to back out of. Cancelling the scan as well
        // would make one keypress do two unrelated things.
        let mut s = started(10_000);
        s.help_open = true;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!s.help_open);
        assert!(cmds.is_empty());
        assert!(s.scan.is_running());
    }

    #[test]
    fn esc_with_nothing_in_flight_does_nothing() {
        let (_, cmds) = update(State::default(), Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(cmds.is_empty());
    }

    #[test]
    fn a_failed_scan_keeps_what_it_had_and_names_the_error() {
        let (s, _) = update(started(100), Msg::ScanBatch { keys: batch(30, 0) });
        let (s, _) = update(
            s,
            Msg::ScanFailed {
                error: "LOADING".into(),
            },
        );
        assert_eq!(s.keys.len(), 30, "partial results are still worth showing");
        assert!(s.scan.readout().contains("LOADING"));
    }
}

#[cfg(test)]
mod metadata_tests {
    //! Metadata is fetched lazily and only for what is on screen (R2.4).

    use super::*;
    use crate::msg::{KeyCode, MetadataEntry};
    use crate::state::KeyKind;
    use crate::state::loaded::TTL_NONE;

    fn browsing(n: usize) -> State {
        let mut state = State {
            rows: 30,
            ..State::default()
        };
        (state, _) = update(
            state,
            Msg::ScanStarted {
                estimated_total: n as u64,
            },
        );
        let keys = (0..n).map(|i| format!("k:{i}").into_bytes()).collect();
        let (state, _) = update(state, Msg::ScanBatch { keys });
        state
    }

    #[test]
    fn only_the_visible_window_is_ever_requested() {
        let state = browsing(10_000);
        let indices = state.rows_needing_metadata();
        assert_eq!(indices.len(), state.visible_rows());
        assert!(
            indices.iter().all(|i| *i < state.visible_rows()),
            "requested a row nobody can see"
        );
    }

    #[test]
    fn arriving_metadata_lands_on_the_right_rows() {
        let state = browsing(100);
        let (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries: vec![MetadataEntry {
                    index: 2,
                    kind: KeyKind::Hash,
                    ttl_seconds: 2_537,
                    size_bytes: 2_150,
                }],
                gone: Vec::new(),
            },
        );
        assert_eq!(state.keys.kind(2), Some(KeyKind::Hash));
        assert_eq!(state.keys.ttl(2), Some(2_537));
        assert_eq!(state.keys.size(2), Some(2_150));
        assert_eq!(state.keys.kind(1), None, "its neighbours are untouched");
    }

    #[test]
    fn rows_that_already_have_metadata_are_not_requested_again() {
        let state = browsing(100);
        let entries = (0..state.visible_rows())
            .map(|i| MetadataEntry {
                index: i,
                kind: KeyKind::String,
                ttl_seconds: TTL_NONE,
                size_bytes: 10,
            })
            .collect();
        let (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries,
                gone: Vec::new(),
            },
        );
        assert!(
            state.rows_needing_metadata().is_empty(),
            "the visible window is fully known, so nothing more is owed"
        );
    }

    #[test]
    fn scrolling_asks_for_what_scrolling_revealed() {
        let state = browsing(10_000);
        let entries = (0..state.visible_rows())
            .map(|i| MetadataEntry {
                index: i,
                kind: KeyKind::String,
                ttl_seconds: TTL_NONE,
                size_bytes: 10,
            })
            .collect();
        let (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries,
                gone: Vec::new(),
            },
        );

        // Page down past the known rows.
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::PageDown)));
        let Some(Command::FetchMetadata { indices }) = cmds.first() else {
            panic!("scrolling into unknown rows must ask for them, got {cmds:?}");
        };
        assert!(!indices.is_empty());
        assert!(
            indices.iter().all(|i| state.keys.kind(*i).is_none()),
            "already-known rows must not be re-fetched"
        );
    }

    #[test]
    fn navigation_is_clamped_to_the_loaded_set() {
        let state = browsing(5);
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::End)));
        assert_eq!(state.view.selected, 4);
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(state.view.selected, 4, "cannot walk off the end");
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Home)));
        assert_eq!(state.view.selected, 0);
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Up)));
        assert_eq!(state.view.selected, 0, "nor off the start");
    }

    #[test]
    fn navigating_an_empty_keyspace_does_nothing_rather_than_panicking() {
        let (state, cmds) = update(State::default(), Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(state.view.selected, 0);
        assert!(cmds.is_empty());
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
        let (state, _) = update(
            State::default(),
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
        assert!(cmds.contains(&Command::RefetchOpenKey));
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
mod viewer_scroll_tests {
    //! Severity-4 UI task: paging and jump-to-start/end for the open value —
    //! useful on a 500-entry zset or a large hex dump, where single-line
    //! Ctrl+Up/Down alone is too slow to be worth using.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::open::OpenKey;
    use crate::state::value::{MemberValue, Value};

    fn open_with(n: usize) -> State {
        let value = Value::Set(MemberValue {
            members: (0..n).map(|i| format!("m{i}")).collect(),
            total: n,
        });
        State {
            open: Some(OpenKey::new(0, "k".into(), value, -1, 10, 0)),
            ..State::default()
        }
    }

    fn press(state: State, code: KeyCode, ctrl: bool) -> (State, Vec<Command>) {
        let key = if ctrl {
            KeyPress::ctrl(code)
        } else {
            KeyPress::plain(code)
        };
        update(state, Msg::Key(key))
    }

    #[test]
    fn page_down_moves_by_twenty_rows_and_clamps_at_the_end() {
        let (state, _) = press(open_with(100), KeyCode::PageDown, true);
        assert_eq!(state.open.unwrap().offset, 20);
    }

    #[test]
    fn page_down_past_the_end_clamps_rather_than_overshooting() {
        let (state, _) = press(open_with(10), KeyCode::PageDown, true);
        assert_eq!(state.open.unwrap().offset, 9, "clamped to the last row");
    }

    #[test]
    fn page_up_moves_back_and_clamps_at_zero() {
        let mut state = open_with(100);
        state.open.as_mut().unwrap().offset = 25;
        let (state, _) = press(state, KeyCode::PageUp, true);
        assert_eq!(state.open.as_ref().unwrap().offset, 5);

        let (state, _) = press(state, KeyCode::PageUp, true);
        assert_eq!(state.open.unwrap().offset, 0, "clamped, not negative");
    }

    #[test]
    fn ctrl_home_jumps_to_the_top_in_one_keystroke() {
        let mut state = open_with(500);
        state.open.as_mut().unwrap().offset = 300;
        let (state, _) = press(state, KeyCode::Home, true);
        assert_eq!(state.open.unwrap().offset, 0);
    }

    #[test]
    fn ctrl_end_jumps_to_the_last_row_in_one_keystroke() {
        let (state, _) = press(open_with(500), KeyCode::End, true);
        assert_eq!(state.open.unwrap().offset, 499);
    }

    #[test]
    fn jumping_away_from_the_top_means_updates_are_announced_not_applied() {
        // The existing apply-if-idle rule (ADR-0006) must hold for paging and
        // jumping exactly as it already does for single-step scrolling.
        let (state, _) = press(open_with(500), KeyCode::End, true);
        assert!(!state.open.unwrap().may_apply());
    }

    #[test]
    fn jumping_back_to_the_top_does_not_by_itself_restore_at_rest() {
        // Consistent with the existing single-step behaviour: the reader
        // chooses when a held update lands, rather than it being inferred from
        // scroll position alone.
        let mut state = open_with(500);
        state.open.as_mut().unwrap().offset = 300;
        state.open.as_mut().unwrap().at_rest = false;
        let (state, _) = press(state, KeyCode::Home, true);
        let open = state.open.unwrap();
        assert_eq!(open.offset, 0);
        assert!(!open.at_rest);
    }

    #[test]
    fn scrolling_with_nothing_open_does_nothing() {
        let (state, cmds) = press(State::default(), KeyCode::PageDown, true);
        assert!(state.open.is_none());
        assert!(cmds.is_empty());
    }

    #[test]
    fn the_hints_for_paging_and_jumping_are_reachable_in_the_help_overlay() {
        let keymap = crate::keymap::Keymap::default();
        for action in [
            crate::keymap::Action::ViewerPageDown,
            crate::keymap::Action::ViewerPageUp,
            crate::keymap::Action::ViewerTop,
            crate::keymap::Action::ViewerBottom,
        ] {
            assert!(keymap.hint(action).is_some(), "{action:?} has no binding");
        }
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
        assert_eq!(state.single_pane_view, SinglePaneView::Keys);
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        assert_eq!(state.single_pane_view, SinglePaneView::Value);
        assert!(matches!(cmds.first(), Some(Command::OpenKey { .. })));
    }

    #[test]
    fn esc_pops_back_to_keys_from_value_view() {
        let state = browsing();
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        assert_eq!(state.single_pane_view, SinglePaneView::Value);

        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(state.single_pane_view, SinglePaneView::Keys);
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
            state.single_pane_view,
            SinglePaneView::Value,
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
        assert_eq!(state.single_pane_view, SinglePaneView::Keys);
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
        assert_eq!(state.single_pane_view, SinglePaneView::Value);
        // No assertion on rendered output here — render::layout's own test
        // (`single_pane_view_is_ignored_at_any_wider_density`) is the proof;
        // this just confirms the state transition still happens uniformly.
    }
}
