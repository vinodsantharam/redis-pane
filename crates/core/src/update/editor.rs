//! The inline editor (ADR-0014): opening a buffer on a String/JSON value or a
//! Hash field, the two-part add-field form, staging an edit into a mutation
//! preview, and keys typed while a buffer is open.

use super::*;

/// Stages `e`: opens the inline editor on the Open value's whole body
/// (R3.2, R4.1, ADR-0014).
///
/// Only `Value::Str` and `Value::Json` are editable this way — a text editor
/// is not guaranteed to round-trip arbitrary bytes, so `Value::Binary` and
/// every collection type are refused with a notice rather than risking silent
/// corruption of a value nobody asked to have reformatted. Values over
/// [`crate::state::editor::MAX_EDIT_BYTES`] are refused the same way.
///
/// Emits no command: the buffer lives entirely in the core until it is
/// staged. Read-only Mode does not block opening — refusal stays at the
/// confirm dialog's `y`, so the reader always sees the real command before
/// learning whether they are allowed to run it (DESIGN §6.5).
pub(super) fn open_editor(mut state: State) -> (State, Vec<Command>) {
    let notify = |text: &str| vec![Command::Notify { text: text.into() }];
    // `e`/`a`/`d` are all focus-dependent (G, PLAN M2 task 6 follow-up): with
    // the keys pane focused, moving the cursor there fetches nothing, so
    // acting on whatever key happens to be open in the Viewer would be acting
    // on a key the reader may not even be looking at. `pane_is_on_screen`
    // still lets this action through in that case (there may be no value pane
    // to focus at all, below 70 columns) — the notice, not silence, is what
    // tells the reader to `Tab` over.
    if state.keys_pane_focused() {
        let text = if state.open.is_some() {
            "Tab to the value pane to edit"
        } else {
            "open a key first"
        };
        return (state, notify(text));
    }
    let Some(open) = state.open.as_ref() else {
        return (state, notify("nothing open to edit"));
    };
    if open.deleted_at_ms.is_some() {
        return (state, notify("gone — nothing to edit"));
    }
    // The previous edit's `SET` has not been read back yet; a new buffer
    // opened now would be replaced by that read the moment it lands.
    if open.is_editing() {
        return (state, notify("still saving the last edit"));
    }
    let Some(value) = open.value.as_ref() else {
        return (state, notify("nothing open to edit"));
    };
    // A Hash has no "whole value" to edit in place — `e` picks the field the
    // value cursor is on instead (D4, PLAN M2 task 6). `Enter`
    // (`Action::EnterValueCursor`) is what puts a cursor on a row at all, so
    // without one there is nothing to have picked.
    if let Value::Hash(pairs) = value {
        if !open.cursor_active {
            return (state, notify("Enter to pick a field"));
        }
        let Some((field, field_value)) = pairs.pairs.get(open.cursor).cloned() else {
            return (state, notify("Enter to pick a field"));
        };
        return match EditBuffer::for_hash_field(&field, &field_value) {
            Ok(buffer) => {
                if let Some(open) = state.open.as_mut() {
                    open.begin_edit(buffer);
                }
                (state, Vec::new())
            }
            Err(text) => (state, notify(text)),
        };
    }
    match EditBuffer::from_value(value, open.cursor) {
        Ok(buffer) => {
            if let Some(open) = state.open.as_mut() {
                open.begin_edit(buffer);
            }
            (state, Vec::new())
        }
        Err(text) => (state, notify(text)),
    }
}

/// Stages `a`: opens the two-part `FIELD`/`VALUE` add form on the Open Hash,
/// on the name part (PLAN M2 task 6 follow-up, F). No cursor prerequisite —
/// a new field has no row to have picked yet, unlike `e`.
///
/// Emits no command: the form lives entirely in the core, the same as every
/// other inline edit. `editing` is set from the moment it opens — there is no
/// longer a name-only capture stage before an [`EditBuffer`] exists, so this
/// is also the moment one is created.
pub(super) fn begin_add_field(mut state: State) -> (State, Vec<Command>) {
    let notify = |text: &str| vec![Command::Notify { text: text.into() }];
    if state.keys_pane_focused() {
        let text = if state.open.is_some() {
            "Tab to the value pane to edit"
        } else {
            "open a key first"
        };
        return (state, notify(text));
    }
    let Some(open) = state.open.as_ref() else {
        return (state, notify("nothing open to edit"));
    };
    if open.deleted_at_ms.is_some() {
        return (state, notify("gone — nothing to edit"));
    }
    if open.is_editing() {
        return (state, notify("still saving the last edit"));
    }
    if !matches!(open.value, Some(Value::Hash(_))) {
        return (state, notify("fields can only be added to a hash"));
    }
    if let Some(open) = state.open.as_mut() {
        open.begin_edit(EditBuffer::new_hash_field());
    }
    (state, Vec::new())
}

/// Whether `Enter`, `↓` and `⌃S` are blocked on the add form's name part
/// (PLAN M2 task 6 follow-up, D): an empty name, or one already in the
/// fetched window. `true` with nothing to check at all, so a caller need not
/// re-verify `open`/`editor` exist first.
pub(super) fn hash_add_blocked(state: &State) -> bool {
    let Some(open) = &state.open else {
        return true;
    };
    let Some(name) = open.editor().and_then(EditBuffer::field_name) else {
        return true;
    };
    name.is_empty() || open.hash_field_shown_duplicate()
}

/// Keys read while the add form's name part is active (PLAN M2 task 6
/// follow-up, F/N) — shaped like the old field-name capture it replaces:
/// plain characters append, Backspace removes one, Paste appends with
/// newlines stripped (handled in `update`'s `Msg::Paste` arm, not here).
/// `Enter`/`↓` advance to the value part and `⌃S` stages directly from here,
/// all three gated by [`hash_add_blocked`]; `Esc` discards the whole add.
pub(super) fn name_part_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    if let Some(action) = state.keymap.action_for(&key) {
        match action {
            Action::EditorStage => {
                if hash_add_blocked(&state) {
                    return (state, Vec::new());
                }
                return stage_editor(state);
            }
            // Esc always discards the buffer entirely, the same as every
            // other Esc in the app — never a return to a prior draft.
            Action::Cancel => {
                if let Some(open) = &mut state.open {
                    open.end_edit();
                }
                return (state, Vec::new());
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Enter | KeyCode::Down => {
            if !hash_add_blocked(&state)
                && let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut)
            {
                editor.advance_to_value();
            }
            (state, Vec::new())
        }
        KeyCode::Backspace => {
            if let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut) {
                editor.name_pop();
            }
            (state, Vec::new())
        }
        KeyCode::Char(c) if !key.ctrl && !key.alt => {
            if let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut) {
                editor.name_push(c);
            }
            (state, Vec::new())
        }
        _ => (state, Vec::new()),
    }
}

/// `⌃S`: stage the inline editor's buffer for confirmation, or close it
/// silently if nothing changed (ADR-0014).
///
/// `editing` stays true when something is staged — R3.8's guard needs to hold
/// until the write settles, not just until the buffer closes, or this
/// very `SET`'s own Refetch would find `editing` false and apply its own
/// reply immediately instead of going through the confirm dialog first.
pub(super) fn stage_editor(mut state: State) -> (State, Vec<Command>) {
    let Some(open) = state.open.as_mut() else {
        return (state, Vec::new());
    };
    let Some(editor) = open.typing() else {
        return (state, Vec::new());
    };
    // A brand-new field has no prior value to be unchanged from — an empty
    // value is a real value Redis allows, not "nothing to save" (D1).
    let is_new_field = matches!(editor.target(), EditTarget::NewHashField { .. });
    if !is_new_field && !editor.is_dirty() {
        open.end_edit();
        return (state, Vec::new());
    }
    let name = open.name.clone();
    // Staged, not closed: the pane keeps showing what is about to be
    // written under the dialog, instead of the value it replaces.
    let Some(editor) = open.stage_edit() else {
        return (state, Vec::new());
    };
    let original = editor.original().to_vec();
    let new = editor.text();
    let was_json = editor.was_json();
    let mutation = match editor.target().clone() {
        EditTarget::Value => PendingMutation::SetString {
            name,
            old: original,
            new,
            was_json,
        },
        EditTarget::HashField { field } => PendingMutation::SetHashField {
            name,
            field: field.into_bytes(),
            old: original,
            new,
            was_json,
        },
        EditTarget::NewHashField { field, .. } => PendingMutation::AddHashField {
            name,
            field: field.into_bytes(),
            value: new,
        },
    };
    state.confirm = Some(mutation);
    (state, Vec::new())
}

/// Keys read while the inline editor holds a buffer (ADR-0014), routed first
/// by which part of the add form is active — the name part is a capture mode
/// of its own ([`name_part_key`]), and everything else (a plain String, an
/// existing field's value, or the add form's own value part) shares this
/// function.
///
/// Keymap-resolved actions (`EditorStage`/`EditorUndo`/`EditorRedo`/`Cancel`)
/// are checked first, so a rebinding takes effect here too; everything else
/// is routed by raw `KeyCode`, the same discipline `filter_key` uses for its
/// own capture mode, since a text editor's movement and insertion keys are
/// not meaningfully "actions" a user would rebind one at a time.
pub(super) fn editor_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    if state
        .open
        .as_ref()
        .and_then(OpenKey::typing)
        .is_some_and(|e| e.active_part() == Some(FieldPart::Name))
    {
        return name_part_key(state, key);
    }
    if let Some(action) = state.keymap.action_for(&key) {
        match action {
            Action::EditorStage => return stage_editor(state),
            Action::EditorUndo => {
                if let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut) {
                    editor.undo();
                }
                return (state, Vec::new());
            }
            Action::EditorRedo => {
                if let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut) {
                    editor.redo();
                }
                return (state, Vec::new());
            }
            // Esc always discards the buffer entirely — never a return to a
            // prior draft, consistent with every other Esc in the app.
            Action::Cancel => {
                if let Some(open) = &mut state.open {
                    open.end_edit();
                }
                return (state, Vec::new());
            }
            _ => {}
        }
    }
    let Some(editor) = state.open.as_mut().and_then(OpenKey::typing_mut) else {
        return (state, Vec::new());
    };
    match key.code {
        // On the add form's value part, `↑` returns to the name part once the
        // cursor genuinely has nowhere left to go — the top screen row,
        // including inside a wrapped first line (PLAN M2 task 6 follow-up,
        // N). Everywhere else (an existing field, a plain String) `↑` is
        // ordinary movement with nothing to leave to.
        KeyCode::Up if editor.active_part() == Some(FieldPart::Value) => {
            let before = editor.cursor();
            editor.move_cursor(CursorMove::Up);
            if editor.cursor() == before {
                editor.return_to_name();
            }
        }
        KeyCode::Up => editor.move_cursor(CursorMove::Up),
        KeyCode::Down => editor.move_cursor(CursorMove::Down),
        KeyCode::Left => editor.move_cursor(CursorMove::Back),
        KeyCode::Right => editor.move_cursor(CursorMove::Forward),
        KeyCode::Home => editor.move_cursor(CursorMove::Head),
        KeyCode::End => editor.move_cursor(CursorMove::End),
        KeyCode::PageUp => {
            for _ in 0..VALUE_PAGE_ROWS {
                editor.move_cursor(CursorMove::Up);
            }
        }
        KeyCode::PageDown => {
            for _ in 0..VALUE_PAGE_ROWS {
                editor.move_cursor(CursorMove::Down);
            }
        }
        KeyCode::Enter => editor.insert_newline(),
        KeyCode::Tab => editor.insert_tab(),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete_forward(),
        KeyCode::Char(c) if !key.ctrl && !key.alt => editor.insert_char(c),
        // Ctrl/Alt chars not bound to an action above are ignored, rather
        // than falling through to insertion — a modifier chord the keymap
        // does not recognize is not the reader asking to type its letter.
        _ => {}
    }
    (state, Vec::new())
}

/// The Open key came back gone while an edit of it may be staged (ADR-0014).
///
/// Under the confirm dialog there is no key left for `SET … XX` to write to,
/// so the dialog closes and the buffer is handed back to be typed into: the
/// reader's text is the one thing on screen no read can recover. With the
/// `SET` already sent, its own `Msg::MutationSettled` decides. Once the
/// write has landed, a staged buffer was
/// only standing in for the read back, and goes.
pub(super) fn staged_edit_found_key_gone(state: &mut State, name: &KeyName, at_ms: u64) {
    // Every mutation that names this key, not just `SetString` — a
    // `SetHashField`/`AddHashField` dialog closes and hands its buffer back
    // exactly the same way; a `DeleteHashField` dialog simply closes with the
    // notice, since it never had a buffer to hand back (`unstage_buffer` is a
    // no-op with none).
    let dialog_up = matches!(
        &state.confirm,
        Some(
            PendingMutation::SetString { name: staged, .. }
            | PendingMutation::SetHashField { name: staged, .. }
            | PendingMutation::AddHashField { name: staged, .. }
            | PendingMutation::DeleteHashField { name: staged, .. }
        ) if staged == name
    );
    let Some(open) = state.open.as_mut().filter(|o| o.name == *name) else {
        return;
    };
    if dialog_up {
        open.unstage_buffer();
        state.confirm = None;
        state.notice = Some((
            format!("{name} is gone — nothing written, edit kept"),
            at_ms,
        ));
    } else if !open.is_editing() {
        open.drop_staged_buffer();
    }
}
