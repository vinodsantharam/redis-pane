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

/// `d` with the value pane focused: delete the field the cursor is on, in an
/// open Hash (D4, PLAN M2 task 6).
pub(super) fn delete_hash_field(mut state: State) -> (State, Vec<Command>) {
    let notify = |text: &str| vec![Command::Notify { text: text.into() }];
    let Some(open) = state.open.as_ref() else {
        return (state, notify("nothing to remove here"));
    };
    let Some(Value::Hash(pairs)) = &open.value else {
        return (state, notify("nothing to remove here"));
    };
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
