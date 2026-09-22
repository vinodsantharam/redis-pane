//! The Viewer: what a read does to the open value, the value cursor, and
//! the clipboard payload a copy builds from whichever pane is focused.

use super::*;

/// One `Msg::ValueLoaded` reply, carried whole.
///
/// The seven fields arrive together and mean nothing apart, so they travel as
/// one — the same reason the key rows take a `RowCtx` rather than ten loose
/// arguments (review L4).
pub(super) struct ValueRead {
    pub token: ReadToken,
    pub index: Option<usize>,
    pub name: KeyName,
    pub value: Value,
    pub ttl_seconds: i32,
    pub size_bytes: u32,
    pub at_ms: u64,
}

pub(super) fn value_loaded(mut state: State, read: ValueRead) -> (State, Vec<Command>) {
    let ValueRead {
        token,
        index,
        name,
        mut value,
        ttl_seconds,
        size_bytes,
        at_ms,
    } = read;
    // Only the answer to the question actually being asked may change
    // what is open. Without this, opening a slow key and then a fast one
    // put the slow one's reply in the Viewer when it finally landed —
    // the value pane showing a key the reader had left. The value in
    // that reply was correct; it simply answered an older question.
    if token != state.read_token {
        return (state, Vec::new());
    }
    // Wrapped to the pane it is about to be drawn in; the shell does not
    // know that width, and should not (review M4).
    value.rewrap(state.value_wrap_width());
    // The question this reply answers is the one currently in
    // flight, so it is no longer in flight. Whether `Enter` asked
    // for the cursor the moment this landed travels with it — read
    // once here, since the pending read is dropped either way.
    let (activate_cursor, own_write) = state
        .open_pending
        .as_ref()
        .map_or((false, false), |p| (p.activate_cursor, p.own_write));
    state.open_pending = None;
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
        state.keys.set_ttl(index, ttl_seconds, epoch_secs(at_ms));
        state.keys.set_size(index, size_bytes);
    }
    match &mut state.open {
        // A read of the key already open is an update, and where it
        // lands depends on where the reader is (ADR-0006).
        Some(open) if open.name == name => {
            // This session's own write is exactly what the reader
            // asked to see, so it is not an update to hold (ADR-0006).
            if own_write {
                open.apply(value, ttl_seconds, size_bytes, at_ms);
            } else {
                open.absorb(value, ttl_seconds, size_bytes, at_ms);
            }
            if activate_cursor {
                open.cursor_active = true;
            }
        }
        _ => {
            let mut opened = OpenKey::new(index, name, value, ttl_seconds, size_bytes, at_ms);
            opened.cursor_active = activate_cursor;
            state.open = Some(opened);
            // A newly opened key has to find its row before either pane
            // can point at it.
            state.relocate_open_key();
        }
    }
    (state, Vec::new())
}

pub(super) fn value_gone(
    mut state: State,
    token: ReadToken,
    index: Option<usize>,
    name: KeyName,
    at_ms: u64,
) -> (State, Vec<Command>) {
    if token != state.read_token {
        return (state, Vec::new());
    }
    state.open_pending = None;
    // The row this reply is actually about — never `open.index`. A
    // token-only version of this message shipped once and used
    // `open.index` here, which meant a gone reply for a *different*
    // key than the one already open tombstoned the wrong row: a
    // perfectly healthy key marked `✕ … gone`, with nothing
    // afterwards to correct it (metadata is refetched only for rows
    // of unknown type, and this one's was known and wrong).
    //
    // Guarded by name, the same way `ValueLoaded`'s metadata
    // writeback is: a rescan between the read and its reply
    // renumbers the Loaded set, and this index would then describe
    // an unrelated key in confident detail.
    if let Some(index) = index
        && state.keys.name(index) == Some(name.as_bytes())
    {
        state.keys.set_gone(index);
    }
    match &mut state.open {
        // The key already open is the one that came back gone —
        // whether it had a value or was already this same
        // placeholder. This is ADR-0006's actual case: a value was
        // read, and the reader is watching it get deleted. The last
        // value stays on screen, badged. During an incident the
        // question is almost always what was in it, and this is the
        // moment that answer becomes unrecoverable.
        Some(open) if open.name == name => {
            open.deleted_at_ms = Some(at_ms);
            open.pending = None;
            staged_edit_found_key_gone(&mut state, &name, at_ms);
        }
        // A different key than whatever is open — including nothing
        // open at all. There is no "what was in it" to preserve here;
        // nothing was ever read. A fresh, minimal placeholder replaces
        // whatever was open, the same way `ValueLoaded`'s `_` arm
        // replaces the Viewer wholesale when a different key
        // succeeds — the two messages now agree on how a key changes
        // identity, not just on how it changes value.
        _ => {
            state.open = Some(OpenKey::gone(index, name, at_ms));
            state.relocate_open_key();
        }
    }
    (state, Vec::new())
}

/// Builds the clipboard payload for `y` (key or value, by focus) and `Action::CopyCommand`.
pub(super) fn build_copy(state: State, what: CopyWhat) -> (State, Vec<Command>) {
    // The key name is copyable from the list alone; the other two need an open
    // value, because there is nothing to copy until the server has said what it
    // holds (ADR-0006).
    let nothing_open = || {
        vec![Command::Notify {
            text: "nothing open to copy".into(),
        }]
    };
    // A key confirmed gone before it was ever loaded has a name but no value —
    // there was never anything for the server to say. Distinct wording from
    // `nothing_open()`: something *is* open, it simply has nothing behind it.
    let gone = || {
        vec![Command::Notify {
            text: "gone — nothing to copy".into(),
        }]
    };
    let mut label = what.label().to_string();
    let text = match what {
        CopyWhat::Key => match state.open.as_ref().map(|o| o.name.display().into_owned()) {
            Some(name) => name,
            None => match state.selected_key().and_then(|i| state.keys.name_str(i)) {
                Some(name) => name.into_owned(),
                None => return (state, nothing_open()),
            },
        },
        CopyWhat::Value => match state.open.as_ref().map(|open| (open, open.value.as_ref())) {
            Some((open, Some(value))) => {
                // A windowed read brought back a slice, and the clipboard shows
                // no seams: 500 rows of a 12,000-item list look exactly like a
                // complete copy once pasted. The Viewer header already states
                // this fact about the same value (`12,000 items · 500 shown`);
                // the confirmation states it about the copy, in the same words,
                // rather than saying `copied value` and leaving the paste
                // buffer to be discovered as a prefix later.
                let viewer = value.viewer();
                if let Some(shown) = viewer.window() {
                    label = format!("{label} ({shown} of {})", viewer.measure());
                }
                value_text(value, open.read_at_ms)
            }
            Some((_, None)) => return (state, gone()),
            None => return (state, nothing_open()),
        },
        CopyWhat::Command => match state.open.as_ref().map(|open| (open, open.value.as_ref())) {
            Some((open, Some(value))) => {
                redis_cli_command(&state.connection.target, &open.name, value)
            }
            Some((_, None)) => return (state, gone()),
            None => return (state, nothing_open()),
        },
    };

    (state, vec![Command::CopyToClipboard { text, label }])
}

/// Whether plain movement currently drives the value cursor rather than the
/// key list (`Action::EnterValueCursor` sets this; `Action::Cancel` clears
/// it). A tiny helper so the six movement handlers read as a guard, not a
/// chain of `state.open.as_ref()...`.
pub(super) fn cursor_active(state: &State) -> bool {
    state.open.as_ref().is_some_and(|o| o.cursor_active)
}

/// A fixed stand-in for the value pane's visible row count, in the same
/// spirit the old `Ctrl+PgUp`/`PgDn` page size was ("the viewer does not know
/// its own rendered height here, and a fixed page beats no paging at all for
/// a 500-entry zset or a large hex dump") — `update()` has no access to the
/// layout the render pass computes (ADR-0011), so this is an approximation,
/// used both for paging and for keeping the cursor inside the visible window.
pub(super) const VALUE_PAGE_ROWS: usize = 20;

/// Move the value cursor by `by` rows, clamped to the value's length. Shared
/// by every viewer, because the frame around a value is one abstraction
/// (R3.1) — a hex dump and a hash pane both move this way.
pub(super) fn move_cursor(mut state: State, by: isize) -> (State, Vec<Command>) {
    let Some(open) = &mut state.open else {
        return (state, Vec::new());
    };
    let Some(value) = &open.value else {
        return (state, Vec::new());
    };
    let last = value.viewer().row_count().saturating_sub(1);
    open.cursor = (open.cursor as isize + by).clamp(0, last as isize) as usize;
    after_cursor_move(state)
}

/// Move the value cursor straight to `to`, clamped; `usize::MAX` means "the
/// end".
pub(super) fn cursor_to(mut state: State, to: usize) -> (State, Vec<Command>) {
    let Some(open) = &mut state.open else {
        return (state, Vec::new());
    };
    let Some(value) = &open.value else {
        return (state, Vec::new());
    };
    let last = value.viewer().row_count().saturating_sub(1);
    open.cursor = to.min(last);
    after_cursor_move(state)
}

/// Keep the cursor's row inside the visible window — reusing the key list's
/// own scroll-follow utility rather than re-deriving it — and hold live
/// updates once the cursor has moved off the top (ADR-0006). Moving back to
/// the top does not by itself undo that: the reader chooses when a held
/// update lands, the same discipline the old raw-offset scroll used.
pub(super) fn after_cursor_move(mut state: State) -> (State, Vec<Command>) {
    if let Some(open) = &mut state.open {
        let viewport = crate::render::keys::Viewport {
            offset: open.offset,
            selected: open.cursor,
        }
        .scrolled_to_selection(VALUE_PAGE_ROWS);
        open.offset = viewport.offset;
        if open.cursor > 0 {
            open.at_rest = false;
        }
    }
    (state, Vec::new())
}

/// `Enter` on a key row: put the value cursor on the Selected key's value,
/// opening it first if it is not already the Open key (PLAN M2 task 6
/// follow-up).
pub(super) fn enter_value_cursor(mut state: State) -> (State, Vec<Command>) {
    // A no-op on a group row: Enter only ever means "put a cursor in
    // the Selected key's value," and a group has no value of its own
    // to move through — `Right` (`Action::Open`) is what expands or
    // steps into one. Every row is implicitly a leaf outside tree
    // mode, so this only ever excludes a Group row.
    if state.tree_mode
        && !matches!(
            state.tree.row(state.view.selected),
            Some(crate::state::tree::Row::Key { .. })
        )
    {
        return (state, Vec::new());
    }
    let Some(index) = state.selected_key() else {
        return (state, Vec::new());
    };

    if matches!(state.attachment(), Some(Attachment::Attached)) {
        let open = state.open.as_mut().expect("Attached implies Some");
        // Nothing to move a cursor through: a key confirmed gone
        // before it ever loaded has no body, and one confirmed gone
        // since has nothing worth re-reading on every Enter — `r` is
        // the deliberate way to check again (ADR-0006: there is no
        // silent refresh button).
        if open.value.is_none() {
            return (state, Vec::new());
        }
        // Fast path: the Open key is already the Selected key and
        // already has a value on screen, so there is nothing to
        // fetch — just drop the cursor into what is already there.
        open.cursor_active = true;
        state.focus = Pane::Value;
        return (state, Vec::new());
    }

    // Otherwise the Selected key is not the Open key (or nothing is
    // open at all): open it exactly like `Action::Open` does, except
    // the reply also activates the cursor the moment it lands — the
    // "open and dive in" this action exists to shortcut over `→`
    // then `Enter`. `state.open` itself is left untouched until then,
    // same as `Action::Open`: the previous value stays on screen,
    // correctly badged detached, rather than being blanked out for
    // the read's duration.
    let Some(name) = state.keys.name(index).map(KeyName::from) else {
        return (state, Vec::new());
    };
    state.focus = Pane::Value;
    let token = issue_read(&mut state);
    state.open_pending = Some(PendingRead {
        name: name.clone(),
        token,
        index: Some(index),
        issued_at_ms: None,
        activate_cursor: true,
        own_write: false,
    });
    let command = read_key(&state, name, Some(index), token);
    (state, vec![command])
}

/// `d` with the value pane focused: delete the field, member or element the
/// cursor is on, in an open Hash (D4, PLAN M2 task 6), Set (D5, PLAN M2 task
/// 7, ADR-0016) or List (D5, PLAN M2 task 8, ADR-0017) — one function stages
/// whichever mutation matches what is open, the same way `begin_add_entry`
/// serves all three under one name.
///
/// Renamed from `delete_hash_field` (PLAN M2 task 8, D8): accurate for one
/// collection type, misleading for three.
pub(super) fn delete_value_row(mut state: State) -> (State, Vec<Command>) {
    let notify = |text: &str| vec![Command::Notify { text: text.into() }];
    let Some(open) = state.open.as_ref() else {
        return (state, notify("nothing to remove here"));
    };
    // Exhaustive over `Value`, not a wildcard fallback (PLAN M2 task 8, D8):
    // a fourth removable type (ZSet, task 9) has to make this same decision
    // here, once, rather than silently falling through to this refusal.
    match &open.value {
        Some(Value::Hash(pairs)) => {
            if !open.cursor_active {
                return (state, notify("Enter to pick a field"));
            }
            let Some((field, _)) = pairs.pairs.get(open.cursor).cloned() else {
                return (state, notify("Enter to pick a field"));
            };
            let last_field = pairs.total == 1;
            let name = open.name.clone();
            state.confirm = Some(PendingMutation::DeleteHashField {
                name,
                field,
                last_field,
            });
            (state, Vec::new())
        }
        Some(Value::Set(members)) => {
            if !open.cursor_active {
                return (state, notify("Enter to pick a member"));
            }
            let Some(member) = members.members.get(open.cursor).cloned() else {
                return (state, notify("Enter to pick a member"));
            };
            // D5, ADR-0016: whether this is the set's only member *at the
            // moment it was staged* — the same `total == 1` snapshot
            // `last_field` above takes, not re-checked when the dialog's `y`
            // actually runs.
            let last_member = members.total == 1;
            let name = open.name.clone();
            state.confirm = Some(PendingMutation::DeleteSetMember {
                name,
                member,
                last_member,
            });
            (state, Vec::new())
        }
        Some(Value::List(items)) => {
            if !open.cursor_active {
                return (state, notify("Enter to pick an element"));
            }
            let Some(element) = items.items.get(open.cursor).cloned() else {
                return (state, notify("Enter to pick an element"));
            };
            // D5, ADR-0017: same `total == 1` snapshot-at-staging-time
            // discipline as `last_field`/`last_member` above. `index` is the
            // cursor's row, which is also the element's absolute Redis index
            // — D7, the fetched window always starts at 0 — so this needs no
            // arithmetic, and a row past the window is never selectable in
            // the first place, so this cannot stage an index the window
            // cannot support.
            let last_element = items.total == 1;
            let index = open.cursor;
            let name = open.name.clone();
            state.confirm = Some(PendingMutation::DeleteListElement {
                name,
                index,
                element,
                last_element,
            });
            (state, Vec::new())
        }
        Some(
            Value::Str(_) | Value::ZSet(_) | Value::Stream(_) | Value::Json(_) | Value::Binary(_),
        )
        | None => (state, notify("nothing to remove here")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::KeyCode;

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

    /// Review M4: a String was wrapped once, by the shell, at half the
    /// terminal's width, whatever the pane actually was, and stayed that way
    /// through a resize or a divider drag.
    #[test]
    fn a_string_is_wrapped_to_its_pane_and_rewrapped_when_the_pane_changes() {
        use crate::state::value::{StringValue, Value};
        let first_row = |s: &State| match &s.open.as_ref().unwrap().value {
            Some(Value::Str(v)) => v.lines[0].chars().count(),
            other => panic!("expected a string, got {other:?}"),
        };
        let state = State {
            cols: 130,
            rows: 40,
            ..State::default()
        };
        let token = state.read_token;
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: None,
                name: "k".into(),
                // As the shell builds it: not wrapped at all.
                value: Value::Str(StringValue::new(&"x".repeat(500), usize::MAX)),
                ttl_seconds: -1,
                size_bytes: 500,
                at_ms: 0,
            },
        );
        let wide = state.value_wrap_width();
        assert_eq!(first_row(&state), wide, "wrapped to the pane on arrival");

        let (state, _) = update(state, Msg::Resized { cols: 90, rows: 40 });
        let narrow = state.value_wrap_width();
        assert!(narrow < wide);
        assert_eq!(
            first_row(&state),
            narrow,
            "and again when the terminal narrows"
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
}

#[cfg(test)]
mod cursor_mode_tests {
    //! `Enter` activates a real cursor inside the open value; `Esc` exits it
    //! without closing the key. While active, the six movement keys act on
    //! the value cursor instead of the key list — `viewer_scroll_tests`
    //! covers the paging/jump half of that once it is active.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::open::OpenKey;
    use crate::state::value::{PairValue, Value};

    /// A key open and Attached: the Selected row is the Open key's own row,
    /// which is what lets plain `Enter` take the fast path straight into
    /// cursor mode without a read. `attached_and_gone_tests` covers the
    /// opposite case, where Enter has to open the Selected key first.
    fn open_with(pairs: usize) -> State {
        let value = Value::Hash(PairValue {
            pairs: (0..pairs)
                .map(|i| (format!("f{i}").into_bytes(), "v".into()))
                .collect(),
            total: pairs,
        });
        let mut state = State {
            cols: 130,
            rows: 40,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    fn press(state: State, code: KeyCode) -> (State, Vec<Command>) {
        update(state, Msg::Key(KeyPress::plain(code)))
    }

    #[test]
    fn enter_activates_cursor_mode_and_focuses_the_value_pane() {
        let (state, _) = press(open_with(5), KeyCode::Enter);
        let open = state.open.unwrap();
        assert!(open.cursor_active);
        assert_eq!(state.focus, Pane::Value);
    }

    #[test]
    fn enter_with_nothing_open_does_nothing() {
        let (state, cmds) = press(State::default(), KeyCode::Enter);
        assert!(state.open.is_none());
        assert!(cmds.is_empty());
    }

    #[test]
    fn enter_on_a_key_gone_before_load_does_nothing() {
        // No body to move a cursor through — a name and a badge, nothing
        // else. Attached (not merely open), so this exercises the branch
        // that would otherwise re-read a key already confirmed gone on
        // every Enter press.
        let mut state = State {
            open: Some(OpenKey::gone(Some(0), "k".into(), 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        assert_eq!(state.attachment(), Some(Attachment::Attached));

        let (state, cmds) = press(state, KeyCode::Enter);
        assert!(!state.open.unwrap().cursor_active);
        assert!(cmds.is_empty(), "a confirmed-gone key is not re-read");
    }

    #[test]
    fn plain_movement_acts_on_the_key_list_until_enter_is_pressed() {
        let state = open_with(5);
        let (mut state, _) = update(state, Msg::ScanStarted { estimated_total: 2 });
        state = update(
            state,
            Msg::ScanBatch {
                keys: vec![b"a".to_vec(), b"b".to_vec()],
            },
        )
        .0;
        let before = state.view.selected;

        let (state, _) = press(state, KeyCode::Down);
        assert_eq!(
            state.view.selected,
            before + 1,
            "not in cursor mode yet, so ↓ moves the key list"
        );
        assert_eq!(
            state.open.unwrap().cursor,
            0,
            "the value cursor never moved"
        );
    }

    #[test]
    fn plain_movement_acts_on_the_value_cursor_once_active() {
        let state = open_with(5);
        let selected_before = state.view.selected;

        let (state, _) = press(state, KeyCode::Enter);
        let (state, _) = press(state, KeyCode::Down);

        assert_eq!(state.open.unwrap().cursor, 1);
        assert_eq!(
            state.view.selected, selected_before,
            "the key list is untouched while the cursor is active"
        );
    }

    #[test]
    fn esc_exits_cursor_mode_without_closing_the_key() {
        let (state, _) = press(open_with(5), KeyCode::Enter);
        let (state, _) = press(state, KeyCode::Esc);

        let open = state.open.unwrap();
        assert!(!open.cursor_active);
        assert_eq!(state.focus, Pane::Keys);
        assert!(
            open.value.is_some(),
            "the open key is untouched, not closed"
        );
    }

    /// A moved cursor holds live updates (ADR-0006), and exiting cursor mode
    /// must not silently let one through — the reader chooses when a held
    /// update lands, same as every other apply-if-idle path.
    #[test]
    fn esc_does_not_restore_at_rest_on_its_own() {
        let (state, _) = press(open_with(5), KeyCode::Enter);
        let (state, _) = press(state, KeyCode::Down);
        assert!(!state.open.as_ref().unwrap().may_apply());

        let (state, _) = press(state, KeyCode::Esc);
        assert!(!state.open.unwrap().may_apply());
    }

    /// Below 70 columns only one pane is drawn at a time; movement must be
    /// aimed at whichever one that is, cursor mode or not (the same
    /// discipline `pane_is_on_screen` already applies to the key list).
    #[test]
    fn narrow_width_moves_the_value_cursor_when_it_is_the_pane_on_screen() {
        let mut state = open_with(5);
        state.cols = 60;
        let (state, _) = press(state, KeyCode::Enter);
        let (state, _) = press(state, KeyCode::Down);
        assert_eq!(state.open.unwrap().cursor, 1);
    }

    /// Left/Right are unbound while the value cursor is active — only
    /// ↑↓/PgUp/PgDn/Home/End move it. Without the `cursor_active` guard in
    /// `Action::CollapseGroup`/`Action::Open`, Left silently walked the tree
    /// selection to its parent group underneath the open value, which is
    /// what a live session actually hit: the open value stopped matching the
    /// selected key ("not the selected key") while the cursor still visibly
    /// sat on a row.
    #[test]
    fn left_and_right_do_not_move_the_key_list_while_the_cursor_is_active() {
        let mut state = open_with(5);
        state.tree_mode = true;
        for name in ["page:a", "page:b"] {
            state.keys.push(name.as_bytes());
        }
        state.rebuild_list();

        let (state, _) = press(state, KeyCode::Enter);
        let selected_before = state.view.selected;
        let tree_before = state.tree.clone();

        let (state, cmds) = press(state, KeyCode::Left);
        assert_eq!(
            state.view.selected, selected_before,
            "Left must not move the key list while the value cursor is active"
        );
        assert_eq!(
            state.tree, tree_before,
            "Left must not collapse/expand a group while the value cursor is active"
        );
        assert!(cmds.is_empty());
        assert!(state.open.as_ref().unwrap().cursor_active);

        let (state, cmds) = press(state, KeyCode::Right);
        assert_eq!(state.view.selected, selected_before);
        assert!(cmds.is_empty());
        assert!(state.open.unwrap().cursor_active);
    }

    /// Reported live: open a key, move the cursor, `Esc` back to the key
    /// list, move the selection to a *different* key, press `Enter` — the
    /// value pane kept showing the old key's value, un-highlighted, badged
    /// `not the selected key`. Blocking `Enter` there would just trade one
    /// confusing no-op for another (CLAUDE.md: an operation that silently
    /// vanishes is indistinguishable from a bug); refetching what `Enter`
    /// actually points at is the fix, and it is what `Action::Open` (`→`)
    /// already does in the same situation — this only skips the extra
    /// keystroke.
    #[test]
    fn enter_on_a_different_key_opens_it_and_activates_the_cursor_once_it_lands() {
        let mut state = open_with(5);
        state.keys.push(b"other");
        state.rebuild_list();
        // Name order: "k" (the Open key, row 0) then "other" (row 1) — select
        // the latter, so the Open key is Detached rather than Attached.
        state.view.selected = 1;
        assert_eq!(state.attachment(), Some(Attachment::Detached { rows: -1 }));

        let (state, cmds) = press(state, KeyCode::Enter);
        // The stale value stays on screen, correctly badged, until the fresh
        // read lands — not blanked out, and not silently ignored.
        assert_eq!(state.open.as_ref().unwrap().name, "k");
        assert!(!state.open.as_ref().unwrap().cursor_active);
        assert_eq!(state.focus, Pane::Value);
        let Some(Command::ReadKey {
            token,
            index,
            key: name,
            ..
        }) = cmds.into_iter().next()
        else {
            panic!("Enter on a detached key must open it, same as →");
        };
        assert_eq!(index, state.selected_key());

        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index,
                name,
                value: Value::Hash(PairValue {
                    pairs: vec![("f".into(), "v".into())],
                    total: 1,
                }),
                ttl_seconds: -1,
                size_bytes: 4,
                at_ms: 0,
            },
        );
        let open = state.open.unwrap();
        assert_eq!(open.name, "other");
        assert!(
            open.cursor_active,
            "the cursor Enter asked for activates the moment the read it \
             had to wait on actually lands"
        );
        assert_eq!(state.focus, Pane::Value);
    }

    #[test]
    fn enter_with_nothing_open_opens_the_selected_key_and_activates_the_cursor_once_it_lands() {
        let mut state = State {
            rows: 40,
            ..State::default()
        };
        state.keys.push(b"only");
        state.rebuild_list();

        let (state, cmds) = press(state, KeyCode::Enter);
        assert!(state.open.is_none(), "nothing to show yet");
        let Some(Command::ReadKey {
            token,
            index,
            key: name,
            ..
        }) = cmds.into_iter().next()
        else {
            panic!("Enter with a key selected but none open must open it");
        };

        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index,
                name,
                value: Value::Hash(PairValue {
                    pairs: vec![("f".into(), "v".into())],
                    total: 1,
                }),
                ttl_seconds: -1,
                size_bytes: 4,
                at_ms: 0,
            },
        );
        assert!(state.open.unwrap().cursor_active);
    }

    #[test]
    fn enter_on_a_collapsed_group_row_does_nothing() {
        let mut state = State {
            tree_mode: true,
            ..State::default()
        };
        state.keys.push(b"page:a");
        state.keys.push(b"page:b");
        state.rebuild_list();
        assert!(matches!(
            state.tree.row(0),
            Some(crate::state::tree::Row::Group { .. })
        ));

        let (state, cmds) = press(state, KeyCode::Enter);
        assert!(state.open.is_none(), "a group has no value to open");
        assert!(cmds.is_empty());
    }
}

#[cfg(test)]
mod loading_indicator_tests {
    //! `state.open_pending` — the loading indicator's state. Set the moment a
    //! read is issued (`Command::ReadKey`), cleared the
    //! moment its reply lands, whatever that reply turns out to be.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::value::{StringValue, Value};

    fn browsing(n: usize) -> State {
        let mut state = State {
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

    #[test]
    fn opening_a_key_names_it_as_the_pending_read() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        let pending = state.open_pending.expect("a read was just issued");
        assert_eq!(pending.name, "k:3");
        assert_eq!(pending.token, token);
    }

    #[test]
    fn a_landed_value_clears_the_pending_read() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 64,
                at_ms: 1_000,
            },
        );
        assert!(state.open_pending.is_none());
    }

    #[test]
    fn a_gone_reply_clears_the_pending_read_too() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index: Some(3),
                name: "k:3".into(),
                at_ms: 1_000,
            },
        );
        assert!(state.open_pending.is_none());
    }

    /// A stale reply answers a question the reader has already moved on from
    /// (`metadata_tests::a_reply_from_a_superseded_read_never_reaches_the_viewer`)
    /// — it must not clear the indicator for the read that superseded it.
    #[test]
    fn a_superseded_reply_does_not_clear_the_current_pending_read() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (mut state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: stale, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: current, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: stale,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("three", 40)),
                ttl_seconds: -1,
                size_bytes: 5,
                at_ms: 2_000,
            },
        );
        let pending = state.open_pending.expect("the current read is still out");
        assert_eq!(pending.token, current);
        assert_eq!(pending.name, "k:5");
    }

    /// Without this, a failed read leaves the header reading `⟳ fetching…`
    /// forever — the exact "operation vanishes, nothing on screen explains
    /// it" defect the error toast exists to prevent.
    #[test]
    fn a_failed_read_clears_the_pending_indicator() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert!(state.open_pending.is_some());
        let (state, _) = update(
            state,
            Msg::Failed {
                command: "reading k:3".into(),
                detail: "WRONGTYPE".into(),
                at_ms: 1_000,
            },
        );
        assert!(state.open_pending.is_none());
    }

    #[test]
    fn a_lost_connection_clears_the_pending_indicator() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert!(state.open_pending.is_some());
        let (state, _) = update(state, Msg::ConnectionLost);
        assert!(state.open_pending.is_none());
    }

    /// A Refetch of the key already open (manual `r`) is issued against the
    /// Open key's own name, not the row under the cursor.
    #[test]
    fn a_manual_refetch_names_the_open_key_as_the_pending_read() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 64,
                at_ms: 1_000,
            },
        );
        // Move the cursor elsewhere before refetching, so a bug that named the
        // Selected row instead of the Open key would be caught.
        state.view.selected = 7;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('r'))));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected a refetch, got {cmds:?}");
        };
        let pending = state.open_pending.expect("a refetch was just issued");
        assert_eq!(pending.name, "k:3", "the Open key, not row 7");
        assert_eq!(pending.token, token);
    }

    #[test]
    fn a_pending_read_starts_unstamped() {
        // `update()` has no clock of its own (ADR-0011) — only the shell
        // knows when a read was actually dispatched, so this starts `None`
        // until `Msg::ReadIssued` arrives.
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert_eq!(state.open_pending.unwrap().issued_at_ms, None);
    }

    #[test]
    fn read_issued_stamps_the_matching_pending_read() {
        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        let (state, _) = update(
            state,
            Msg::ReadIssued {
                token,
                at_ms: 5_000,
            },
        );
        assert_eq!(state.open_pending.unwrap().issued_at_ms, Some(5_000));
    }

    #[test]
    fn read_issued_for_a_superseded_token_is_ignored() {
        // The read it names is no longer the outstanding one — the same
        // discipline `ValueLoaded`/`ValueGone` already apply to a stale token.
        let mut state = browsing(10);
        state.view.selected = 3;
        let (mut state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: stale, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: current, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ReadIssued {
                token: stale,
                at_ms: 5_000,
            },
        );
        let pending = state.open_pending.expect("the current read is still out");
        assert_eq!(pending.token, current);
        assert_eq!(
            pending.issued_at_ms, None,
            "a stamp for a superseded read must not land on the current one"
        );
    }

    #[test]
    fn read_issued_with_nothing_pending_does_nothing() {
        let (state, _) = update(
            State::default(),
            Msg::ReadIssued {
                token: ReadToken::default(),
                at_ms: 5_000,
            },
        );
        assert!(state.open_pending.is_none());
    }
}

#[cfg(test)]
mod viewer_scroll_tests {
    //! Paging and jump-to-start/end for the value cursor — useful on a
    //! 500-entry zset or a large hex dump, where single-line `↑↓` alone is
    //! too slow to be worth using. All of this only applies once `Enter`
    //! (`Action::EnterValueCursor`) has activated the cursor; plain movement
    //! acts on the key list otherwise (`cursor_mode_tests` covers that half).

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::open::OpenKey;
    use crate::state::value::{MemberValue, Value};

    fn open_with(n: usize) -> State {
        let value = Value::Set(MemberValue {
            members: (0..n).map(|i| format!("m{i}").into_bytes()).collect(),
            total: n,
        });
        let mut state = State {
            // A real width: at `State::default()`'s zero columns the layout is
            // single-pane and the Viewer is not on screen, so its own scroll
            // keys would correctly do nothing.
            cols: 130,
            rows: 40,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.open.as_mut().unwrap().cursor_active = true;
        state
    }

    fn press(state: State, code: KeyCode) -> (State, Vec<Command>) {
        update(state, Msg::Key(KeyPress::plain(code)))
    }

    #[test]
    fn page_down_moves_by_twenty_rows_and_clamps_at_the_end() {
        let (state, _) = press(open_with(100), KeyCode::PageDown);
        assert_eq!(state.open.unwrap().cursor, 20);
    }

    #[test]
    fn page_down_past_the_end_clamps_rather_than_overshooting() {
        let (state, _) = press(open_with(10), KeyCode::PageDown);
        assert_eq!(state.open.unwrap().cursor, 9, "clamped to the last row");
    }

    #[test]
    fn page_up_moves_back_and_clamps_at_zero() {
        let mut state = open_with(100);
        state.open.as_mut().unwrap().cursor = 25;
        let (state, _) = press(state, KeyCode::PageUp);
        assert_eq!(state.open.as_ref().unwrap().cursor, 5);

        let (state, _) = press(state, KeyCode::PageUp);
        assert_eq!(state.open.unwrap().cursor, 0, "clamped, not negative");
    }

    #[test]
    fn home_jumps_to_the_top_in_one_keystroke() {
        let mut state = open_with(500);
        state.open.as_mut().unwrap().cursor = 300;
        let (state, _) = press(state, KeyCode::Home);
        assert_eq!(state.open.unwrap().cursor, 0);
    }

    #[test]
    fn end_jumps_to_the_last_row_in_one_keystroke() {
        let (state, _) = press(open_with(500), KeyCode::End);
        assert_eq!(state.open.unwrap().cursor, 499);
    }

    #[test]
    fn jumping_away_from_the_top_means_updates_are_announced_not_applied() {
        // The existing apply-if-idle rule (ADR-0006) must hold for paging and
        // jumping exactly as it already does for single-step movement.
        let (state, _) = press(open_with(500), KeyCode::End);
        assert!(!state.open.unwrap().may_apply());
    }

    #[test]
    fn jumping_back_to_the_top_does_not_by_itself_restore_at_rest() {
        // Consistent with the existing single-step behaviour: the reader
        // chooses when a held update lands, rather than it being inferred from
        // cursor position alone.
        let mut state = open_with(500);
        state.open.as_mut().unwrap().cursor = 300;
        state.open.as_mut().unwrap().at_rest = false;
        let (state, _) = press(state, KeyCode::Home);
        let open = state.open.unwrap();
        assert_eq!(open.cursor, 0);
        assert!(!open.at_rest);
    }

    #[test]
    fn movement_with_nothing_open_does_nothing() {
        // Not in cursor mode (nothing to activate it on), so this falls
        // through to the key-list branch — which is also empty, hence still
        // a no-op, just via the other path.
        let (state, cmds) = press(State::default(), KeyCode::PageDown);
        assert!(state.open.is_none());
        assert!(cmds.is_empty());
    }
}
