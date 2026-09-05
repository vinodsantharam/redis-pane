//! The single entry point into the core (PLAN M0.4).

use crate::command::ReadToken;
use crate::keymap::Action;
use crate::msg::KeyCode;
use crate::msg::KeyPress;
use crate::render::layout::Pane;
use crate::state::copy::{CopyWhat, redis_cli_command, value_text};
use crate::state::{Link, OpenKey, ReadOnlyReason, ScanState, Tracking};
use crate::{Command, Msg, State};

/// Takes a message, returns new state plus commands for a shell to execute.
///
/// Pure: no I/O, no clock, no randomness. Time arrives inside the message
/// (see [`Msg::ReadCompleted`]) rather than being read here, which is what
/// keeps a frame a function of state alone (ADR-0011).
/// Mint the token for a read about to be issued, superseding any in flight.
///
/// Every read goes through here, for the same reason every read goes through
/// one `Command`: a read issued without bumping the token would be answered by
/// a reply the core could not tell apart from a stale one.
fn issue_read(state: &mut State) -> ReadToken {
    state.read_token = ReadToken(state.read_token.0.wrapping_add(1));
    state.read_token
}

pub fn update(mut state: State, msg: Msg) -> (State, Vec<Command>) {
    match msg {
        Msg::Key(key) => key_press(state, key),
        Msg::Resized { cols, rows } => {
            state.cols = cols;
            state.rows = rows;
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
                vec![Command::RefetchOpenKey {
                    token: issue_read(&mut state),
                }]
            } else {
                Vec::new()
            };
            (state, commands)
        }
        Msg::ConnectionLost => {
            // No countdown: nothing has scheduled a retry yet, and the shell
            // may never schedule one. The header shows the Read age instead,
            // which is the fact ADR-0009 actually asks for in this state and
            // the only one that is true.
            state.link = Link::Reconnecting {
                attempt: 1,
                retry_in_ms: None,
            };
            (state, vec![Command::Reconnect { after_ms: 0 }])
        }
        Msg::ReconnectScheduled {
            attempt,
            retry_in_ms,
        } => {
            state.link = Link::Reconnecting {
                attempt,
                retry_in_ms: Some(retry_in_ms),
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
            let token = issue_read(&mut state);
            (state, vec![Command::RefetchOpenKey { token }])
        }
        Msg::ScanStarted { estimated_total } => {
            state.keys.clear();
            // The Open key survives a rescan; its *index* must not. `SCAN` has
            // no stable order, so the same number will address a different key
            // once the set refills, and every use of it — the tombstone
            // writeback, the row marker in the keys pane — would then be
            // confidently wrong rather than merely absent. It is re-resolved by
            // name in `scan_batch` when the key comes back.
            if let Some(open) = &mut state.open {
                open.index = None;
            }
            state.rebuild_list();
            state.scan = ScanState::Running {
                scanned: 0,
                estimated_total,
            };
            (state, Vec::new())
        }
        Msg::ScanBatch { keys } => scan_batch(state, keys),
        Msg::MetadataBatch { entries, gone } => {
            let open_index = state.open.as_ref().and_then(|open| open.index);
            let mut row_says_alive = false;
            for e in entries {
                state.keys.set_kind(e.index, e.kind);
                state.keys.set_ttl(e.index, e.ttl_seconds);
                state.keys.set_size(e.index, e.size_bytes);
                row_says_alive |= Some(e.index) == open_index;
            }
            // A deleted key keeps its row and its last-known TTL and size; only
            // the type byte gives way to the tombstone. Removing the row would
            // renumber everything below the cursor between one frame and the
            // next, which is a worse lie than a row that says it is gone.
            let mut row_says_gone = false;
            for index in gone {
                state.keys.set_gone(index);
                row_says_gone |= Some(index) == open_index;
            }

            // 958b311 taught the row what the Viewer knew. This is the other
            // direction, which it did not cover: a tombstoned row has no type,
            // so it matches `rows_needing_metadata` forever and the next cursor
            // move refetches it — and a key that was deleted and then written
            // again came back to `● string 64 B ∞` in the list while the Viewer
            // still read `✕ deleted 40s ago`. With tracking off nothing ever
            // corrected it, and the list is the more believable of the two
            // because it is the one that looks untouched.
            //
            // Both directions of the disagreement are resolved the same way,
            // and not by copying one pane's opinion onto the other: ask the
            // server. The read path already dates its own answer and re-arms
            // tracking (ADR-0006), so whichever pane was wrong is corrected by
            // evidence rather than by inference.
            let viewer_says_gone = state
                .open
                .as_ref()
                .is_some_and(|open| open.deleted_at_ms.is_some());
            if (row_says_alive && viewer_says_gone) || (row_says_gone && !viewer_says_gone) {
                let token = issue_read(&mut state);
                return (state, vec![Command::RefetchOpenKey { token }]);
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
            token,
            index,
            name,
            value,
            ttl_seconds,
            size_bytes,
            at_ms,
        } => {
            // Only the answer to the question actually being asked may change
            // what is open. Without this, opening a slow key and then a fast one
            // put the slow one's reply in the Viewer when it finally landed —
            // the value pane showing a key the reader had left. The value in
            // that reply was correct; it simply answered an older question.
            if token != state.read_token {
                return (state, Vec::new());
            }
            // A value came back, so the key is there. Says so on the row too —
            // the same two-pane agreement `ValueGone` keeps in the other
            // direction, and what un-badges a key that was deleted and then
            // written again. Setting the kind rather than merely clearing the
            // tombstone means the row is right immediately instead of showing
            // a pending placeholder until the next scroll refetches it.
            //
            // Guarded by the name, not just by the token: a rescan between the
            // read and its reply renumbers the Loaded set, and this index would
            // then describe an unrelated key in confident detail.
            if let Some(index) = index
                && state.keys.name(index) == Some(name.as_bytes())
            {
                state.keys.set_kind(index, value.kind());
                state.keys.set_ttl(index, ttl_seconds);
                state.keys.set_size(index, size_bytes);
            }
            match &mut state.open {
                // A read of the key already open is an update, and where it
                // lands depends on where the reader is (ADR-0006).
                Some(open) if open.name == name => {
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
                    ));
                    // A newly opened key has to find its row before either pane
                    // can point at it.
                    state.relocate_open_key();
                }
            }
            (state, Vec::new())
        }
        Msg::ValueGone { token, at_ms } => {
            // Identity matters more here than anywhere else. This message
            // tombstones the Open key *and* its row, so an unidentified one
            // badges whichever key happens to be open when it lands — marking a
            // healthy key deleted in both panes, with nothing afterwards to
            // correct it.
            if token != state.read_token {
                return (state, Vec::new());
            }
            // The value stays on screen, badged. During an incident the
            // question is almost always what was in it, and this is the moment
            // that answer becomes unrecoverable.
            if let Some(open) = &mut state.open {
                open.deleted_at_ms = Some(at_ms);
                open.pending = None;
                // The row this key came from learns it too. Without this the
                // two panes state different things about one key at the same
                // moment — the Viewer badged `✕ deleted` while the list still
                // showed `● string 64 B` — and the list is the more believable
                // of the two, because it is the one that looks untouched.
                // Nothing else would correct it: metadata is only refetched for
                // rows whose type is unknown, and this row's is known and wrong.
                //
                // Skipped while the index is `None`: after a rescan there is no
                // row this key is known to be on, and tombstoning a guess would
                // put a `✕ gone` badge on a key that is perfectly alive.
                if let Some(index) = open.index {
                    state.keys.set_gone(index);
                }
            }
            (state, Vec::new())
        }
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
    // A rescan took the Open key's index away (`ScanStarted`). Watch for the
    // name coming back so it can be restored — here rather than by searching
    // the arena afterwards, because the index is simply the length before the
    // push, and this is the one place that knows it.
    let looking_for = match &state.open {
        Some(open) if open.index.is_none() => Some(open.name.clone()),
        _ => None,
    };
    for key in keys {
        let index = state.keys.len();
        if !state.keys.push(&key) {
            state.scan = ScanState::Capped {
                at: state.keys.len(),
            };
            return (state, vec![Command::CancelScan]);
        }
        // Only after the push succeeded: a key the cap refused has no index,
        // and claiming one would point the Open key at a row that is not there.
        if looking_for.as_deref().map(str::as_bytes) == Some(key.as_slice())
            && let Some(open) = &mut state.open
        {
            open.index = Some(index);
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
            if state.focus == Pane::Value {
                state.focus = Pane::Keys;
                return (state, Vec::new());
            }
            // Every in-flight operation is cancellable (PRD R7.3).
            if state.scan.is_running() {
                return (state, vec![Command::CancelScan]);
            }
            (state, Vec::new())
        }
        Action::Refetch => {
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
            let token = issue_read(&mut state);
            (state, vec![Command::RefetchOpenKey { token }])
        }
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
            // Opening a key moves focus onto it, at every width. Below 70
            // columns that is the "push" half of stack navigation (DESIGN §2);
            // above it both panes already show and this decides only which one
            // a pane-scoped key acts on. `Tab` moves it back without closing
            // the key, and `Esc` closes the key and moves it back with it.
            state.focus = Pane::Value;
            let token = issue_read(&mut state);
            (state, vec![Command::OpenKey { index, name, token }])
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
fn copy_key(state: State, key: KeyPress) -> (State, Vec<Command>) {
    let what = match key.code {
        KeyCode::Char('y') | KeyCode::Char('k') => CopyWhat::Key,
        KeyCode::Char('v') => CopyWhat::Value,
        KeyCode::Char('c') => CopyWhat::Command,
        _ => return (state, Vec::new()),
    };

    // The key name is copyable from the list alone; the other two need an open
    // value, because there is nothing to copy until the server has said what it
    // holds (ADR-0006).
    let nothing_open = || {
        vec![Command::Notify {
            text: "nothing open to copy".into(),
        }]
    };
    let mut label = what.label().to_string();
    let text = match what {
        CopyWhat::Key => match state.open.as_ref().map(|o| o.name.clone()) {
            Some(name) => name,
            None => match state.selected_key().and_then(|i| state.keys.name_str(i)) {
                Some(name) => name.into_owned(),
                None => return (state, nothing_open()),
            },
        },
        CopyWhat::Value => match &state.open {
            Some(open) => {
                // A windowed read brought back a slice, and the clipboard shows
                // no seams: 500 rows of a 12,000-item list look exactly like a
                // complete copy once pasted. The Viewer header already states
                // this fact about the same value (`12,000 items · 500 shown`);
                // the confirmation states it about the copy, in the same words,
                // rather than saying `copied value` and leaving the paste
                // buffer to be discovered as a prefix later.
                let viewer = open.value.viewer();
                if let Some(shown) = viewer.window() {
                    label = format!("{label} ({shown} of {})", viewer.measure());
                }
                value_text(&open.value, open.read_at_ms)
            }
            None => return (state, nothing_open()),
        },
        CopyWhat::Command => match &state.open {
            Some(open) => redis_cli_command(&state.connection.target, open),
            None => return (state, nothing_open()),
        },
    };

    (state, vec![Command::CopyToClipboard { text, label }])
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
            [Command::RefetchOpenKey { .. }]
        ));
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
                    [Command::RefetchOpenKey { .. }]
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
                token: ReadToken::default(),
                index: Some(0),
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
        assert!(matches!(cmds.as_slice(), [Command::RefetchOpenKey { .. }]));
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
        assert!(matches!(cmds.as_slice(), [Command::RefetchOpenKey { .. }]));

        let (rearmed, _) = update(back, Msg::TrackingArmed);
        assert_eq!(rearmed.liveness(), Liveness::Live);
    }

    /// ADR-0006: tracking is consumed by its own invalidation, so every
    /// invalidation must produce a Refetch — and Refetch is the only read path,
    /// so it always re-arms.
    #[test]
    fn an_invalidation_consumes_the_arming_and_forces_a_refetch() {
        let (after, cmds) = update(armed(), Msg::Invalidated);
        assert!(matches!(cmds.as_slice(), [Command::RefetchOpenKey { .. }]));

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
                retry_in_ms: Some(4_000)
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
                    retry_in_ms: None,
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

    /// The two panes must never state different things about one key.
    ///
    /// Found in use: with a key open and then deleted, the Viewer badged
    /// `✕ deleted just now` while that key's row in the list still read
    /// `● string 64 B ∞`. Nothing would have corrected it — metadata is only
    /// refetched for rows whose type is *unknown*, and this row's was known
    /// and stale — so the list quietly kept the more believable of two
    /// contradictory claims.
    #[test]
    fn a_deleted_open_key_is_badged_in_the_list_as_well_as_the_viewer() {
        use crate::state::value::{StringValue, Value};

        let state = browsing(10);
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: ReadToken::default(),
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 64,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.keys.kind(3), Some(KeyKind::String));
        assert!(!state.keys.is_gone(3));

        let (state, _) = update(
            state,
            Msg::ValueGone {
                token: ReadToken::default(),
                at_ms: 2_000,
            },
        );
        assert!(
            state.open.as_ref().unwrap().deleted_at_ms.is_some(),
            "the Viewer knows"
        );
        assert!(state.keys.is_gone(3), "and so must the row it came from");
        assert!(!state.keys.is_gone(2), "its neighbours are untouched");
    }

    #[test]
    fn a_key_that_comes_back_stops_saying_gone() {
        use crate::state::value::{PairValue, Value};

        let mut state = browsing(10);
        state.keys.set_gone(3);
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: ReadToken::default(),
                index: Some(3),
                name: "k:3".into(),
                value: Value::Hash(PairValue::default()),
                ttl_seconds: 90,
                size_bytes: 128,
                at_ms: 3_000,
            },
        );
        assert!(
            !state.keys.is_gone(3),
            "a value came back, so the key is there"
        );
        assert_eq!(
            state.keys.kind(3),
            Some(KeyKind::Hash),
            "and the row is right at once, not a placeholder until the next scroll"
        );
        assert_eq!(state.keys.ttl(3), Some(90));
        assert_eq!(state.keys.size(3), Some(128));
    }

    fn browsing(n: usize) -> State {
        let mut state = State {
            // Wide enough for two panes, so both are on screen and a
            // pane-scoped key is not silently gated out from under the test.
            cols: 130,
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

    use crate::state::Attachment;

    // ── Which key is the Viewer showing? (CONTEXT.md: Open key) ────────────
    //
    // The reported defect was the cursor on one key and the value pane showing
    // another. Three separate causes, two of them races, and a race that is
    // only *usually* won is a bug that gets reported once a quarter forever.

    /// Open a slow key, then a fast one. The slow reply lands last and must not
    /// win: it is a correct value answering a question the reader has left.
    #[test]
    fn a_reply_from_a_superseded_read_never_reaches_the_viewer() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        // Open row 3 …
        state.view.selected = 3;
        let (mut state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token: slow, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        // … then change your mind and open row 5 before the first came back.
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token: fast, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        assert_ne!(slow, fast, "two reads, two identities");

        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: fast,
                index: Some(5),
                name: "k:5".into(),
                value: Value::Str(StringValue::new("five", 40)),
                ttl_seconds: -1,
                size_bytes: 4,
                at_ms: 1_000,
            },
        );
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: slow,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("three", 40)),
                ttl_seconds: -1,
                size_bytes: 5,
                at_ms: 2_000,
            },
        );

        assert_eq!(
            state.open.as_ref().unwrap().name,
            "k:5",
            "the last key asked for is the key on screen"
        );
        assert!(
            state.keys.kind(3).is_none(),
            "and the superseded reply wrote nothing anywhere"
        );
    }

    /// The sharper half: `ValueGone` tombstones the Open key *and* its row, so
    /// a stale one marks a perfectly healthy key deleted in both panes — and
    /// nothing afterwards corrects it.
    #[test]
    fn a_superseded_gone_reply_does_not_bury_the_key_that_is_open_now() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (mut state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token: doomed, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token: current, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: current,
                index: Some(5),
                name: "k:5".into(),
                value: Value::Str(StringValue::new("five", 40)),
                ttl_seconds: -1,
                size_bytes: 4,
                at_ms: 1_000,
            },
        );

        // k:3 really was deleted — but that is news about k:3, not about k:5.
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token: doomed,
                at_ms: 2_000,
            },
        );
        assert!(
            state.open.as_ref().unwrap().deleted_at_ms.is_none(),
            "the open key is alive and must not be badged as deleted"
        );
        assert!(!state.keys.is_gone(5), "nor may its row be tombstoned");
    }

    /// `SCAN` has no stable order, so an index outlives its meaning. Keeping it
    /// would put the row marker and the tombstone on whatever key inherited the
    /// number — confidently wrong, which is worse than absent.
    #[test]
    fn a_rescan_takes_the_open_keys_index_and_a_later_batch_gives_it_back() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.open.as_ref().unwrap().index, Some(3));
        assert_eq!(state.open.as_ref().unwrap().row, Some(3));

        let (state, _) = update(
            state,
            Msg::ScanStarted {
                estimated_total: 10,
            },
        );
        assert_eq!(
            state.open.as_ref().unwrap().index,
            None,
            "the number means nothing until the key is seen again"
        );
        assert_eq!(state.open.as_ref().unwrap().row, None);
        assert_eq!(
            state.attachment(),
            Some(Attachment::DetachedOffList),
            "and the Viewer says so rather than pointing at a row"
        );

        // The keyspace comes back in a different order, as it may.
        let keys = ["k:7", "k:3", "k:1"]
            .iter()
            .map(|k| k.as_bytes().to_vec())
            .collect();
        let (state, _) = update(state, Msg::ScanBatch { keys });
        assert_eq!(
            state.open.as_ref().unwrap().index,
            Some(1),
            "re-resolved by name, at wherever it landed this time"
        );
    }

    /// The metadata writeback is guarded by the name, not only by the token: a
    /// rescan can land between a read and its reply without either being stale.
    #[test]
    fn a_value_reply_never_writes_metadata_onto_a_renumbered_row() {
        use crate::state::value::{PairValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        // The rescan refills row 3 with a different key.
        let (state, _) = update(state, Msg::ScanStarted { estimated_total: 4 });
        let keys = ["a", "b", "c", "somebody-else"]
            .iter()
            .map(|k| k.as_bytes().to_vec())
            .collect();
        let (state, _) = update(state, Msg::ScanBatch { keys });

        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Hash(PairValue::default()),
                ttl_seconds: 90,
                size_bytes: 128,
                at_ms: 1_000,
            },
        );
        assert!(
            state.keys.kind(3).is_none(),
            "row 3 is `somebody-else` now and knows nothing about k:3"
        );
        assert_eq!(
            state.open.as_ref().unwrap().name,
            "k:3",
            "the Viewer still took the value, which was never in doubt"
        );
    }

    /// The three answers the panes are drawn from.
    #[test]
    fn attachment_distinguishes_on_the_cursor_from_elsewhere_from_nowhere() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        assert_eq!(state.attachment(), None, "nothing open, no question to ask");

        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.attachment(), Some(Attachment::Attached));

        state.view.selected = 1;
        assert_eq!(
            state.attachment(),
            Some(Attachment::Detached { rows: 2 }),
            "two rows below the cursor"
        );

        state.list.filter = "k:9".into();
        state.rebuild_list();
        assert_eq!(
            state.attachment(),
            Some(Attachment::DetachedOffList),
            "filtered away, so there is no row to point at"
        );
    }

    /// The hint bar names the action for the focused pane; `r` has to perform
    /// that one. The held-update branch used to run first, so `r` in the keys
    /// pane applied a value in the *other* pane and did not rescan — while the
    /// hint said `r rescan`, the list did not move, and no scan readout
    /// appeared to explain it.
    #[test]
    fn r_in_the_keys_pane_rescans_even_with_an_update_waiting_in_the_viewer() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v1", 40)),
                ttl_seconds: -1,
                size_bytes: 2,
                at_ms: 1_000,
            },
        );
        // Scrolled, so the next value is held rather than applied.
        {
            let open = state.open.as_mut().unwrap();
            open.offset = 3;
            open.at_rest = false;
        }
        let token = state.read_token;
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v2", 40)),
                ttl_seconds: -1,
                size_bytes: 2,
                at_ms: 2_000,
            },
        );
        assert!(state.open.as_ref().unwrap().pending.is_some(), "held");

        state.focus = Pane::Keys;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('r'))));
        assert!(
            matches!(cmds.as_slice(), [Command::StartScan { .. }]),
            "the hint says `r rescan`, so `r` rescans: {cmds:?}"
        );
        assert!(
            state.open.as_ref().unwrap().pending.is_some(),
            "and the held update is still held, untouched by a keys-pane key"
        );
    }

    /// 958b311 taught the row what the Viewer knew. The other direction was
    /// open: a tombstoned row has no type, so it is refetched on the next
    /// cursor move, and a key deleted and then written again came back to
    /// `● string 64 B ∞` in the list while the Viewer still read `✕ deleted`.
    #[test]
    fn a_key_that_comes_back_makes_the_viewer_go_and_ask() {
        use crate::msg::MetadataEntry;
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        let token = state.read_token;
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                at_ms: 2_000,
            },
        );
        assert!(state.keys.is_gone(3), "both panes agree it is gone");

        // The row is refetched, because a tombstone has no type, and the server
        // says the key is there again.
        let (state, cmds) = update(
            state,
            Msg::MetadataBatch {
                entries: vec![MetadataEntry {
                    index: 3,
                    kind: crate::state::KeyKind::String,
                    ttl_seconds: -1,
                    size_bytes: 64,
                }],
                gone: Vec::new(),
            },
        );
        assert!(
            matches!(cmds.as_slice(), [Command::RefetchOpenKey { .. }]),
            "the panes disagree, so ask the server rather than guess: {cmds:?}"
        );
        assert!(
            state.open.as_ref().unwrap().deleted_at_ms.is_some(),
            "and the badge stays until the answer arrives — no guessing either way"
        );
    }

    /// The same rule in the other direction: metadata noticed the deletion
    /// first, and the Viewer is still showing a live-looking value.
    #[test]
    fn a_row_that_goes_makes_the_viewer_go_and_ask_too() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::OpenKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        let (_, cmds) = update(
            state,
            Msg::MetadataBatch {
                entries: Vec::new(),
                gone: vec![3],
            },
        );
        assert!(
            matches!(cmds.as_slice(), [Command::RefetchOpenKey { .. }]),
            "{cmds:?}"
        );
    }

    // ── An action must not act on a pane that is not on screen ─────────────
    //
    // Below 70 columns one pane is drawn at a time (DESIGN §2). Aiming a
    // pane-scoped key at the other one produced no visible change whatsoever,
    // which is the single worst thing a TUI can look like: indistinguishable
    // from having stopped responding.

    fn narrow_viewing() -> State {
        let mut state = browsing(10);
        state.cols = 60;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert_eq!(state.focus, Pane::Value, "→ pushes onto the value");
        assert!(
            !state.keys_pane_visible(),
            "and the list is off screen there"
        );
        state
    }

    /// The sharpest case. `/` began capturing into a filter line rendered
    /// inside a pane of zero width: every keypress after it disappeared, `q`
    /// typed a `q` instead of quitting, and only `Esc` escaped.
    #[test]
    fn slash_cannot_open_an_invisible_keystroke_sink() {
        let state = narrow_viewing();
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(
            !state.filtering,
            "no capture may start while the pane that would show it is not drawn"
        );

        // And `q` still means quit, which is the part that mattered.
        let (_, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('q'))));
        assert_eq!(cmds, vec![Command::Quit]);
    }

    /// The cursor moved an arbitrary distance with nothing on screen changing,
    /// and `Esc` then returned to a list that had silently wandered.
    #[test]
    fn the_key_list_does_not_move_while_it_is_off_screen() {
        let state = narrow_viewing();
        let before = state.view.selected;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::End)));
        assert_eq!(state.view.selected, before, "the cursor stayed put");
    }

    /// The same defect mirrored: on the list screen the value is not drawn, so
    /// its scroll keys were moving an offset nobody could see.
    #[test]
    fn the_value_does_not_scroll_while_it_is_off_screen() {
        let mut state = browsing(10);
        state.cols = 60;
        state.open = Some(OpenKey::new(
            Some(0),
            "k".into(),
            crate::state::value::Value::Str(crate::state::value::StringValue::new("v", 40)),
            -1,
            10,
            0,
        ));
        assert!(
            !state.value_pane_visible(),
            "the list is what is drawn here"
        );
        let (state, _) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Down)));
        assert_eq!(state.open.as_ref().unwrap().offset, 0);
    }

    /// Both panes are drawn above 70 columns, so the two-pane keymap is
    /// untouched: plain arrows drive the list from either pane and `⌃`-arrows
    /// drive the value, exactly as DESIGN §4 specifies.
    #[test]
    fn nothing_is_gated_while_both_panes_are_on_screen() {
        let state = browsing(10);
        let (mut state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert_eq!(state.focus, Pane::Value);
        assert!(state.keys_pane_visible() && state.value_pane_visible());

        let before = state.view.selected;
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_ne!(
            state.view.selected, before,
            "the list still moves from the Viewer at two-pane widths"
        );
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(state.filtering, "and `/` still filters");
    }

    /// Reached by dragging a window edge rather than by pressing a key: a
    /// filter being typed at two-pane width, then narrowed past the breakpoint
    /// with the Viewer focused, would leave the capture running off screen.
    #[test]
    fn narrowing_the_terminal_keeps_an_open_filter_where_it_can_be_seen() {
        let state = browsing(10);
        let (mut state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(state.filtering);

        let (state, _) = update(state, Msg::Resized { cols: 60, rows: 30 });
        assert!(state.filtering, "the filter is still being typed");
        assert!(
            state.keys_pane_visible(),
            "so the pane showing it must be the one on screen"
        );
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
        assert!(
            cmds.iter()
                .any(|c| matches!(c, Command::RefetchOpenKey { .. }))
        );
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
            // A real width: at `State::default()`'s zero columns the layout is
            // single-pane and the Viewer is not on screen, so its own scroll
            // keys would correctly do nothing.
            cols: 130,
            rows: 40,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
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
        assert_eq!(state.focus, Pane::Keys);
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('l'))));
        assert_eq!(state.focus, Pane::Value);
        assert!(matches!(cmds.first(), Some(Command::OpenKey { .. })));
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
