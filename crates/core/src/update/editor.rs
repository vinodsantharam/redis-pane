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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::KeyCode;

    fn open_with_string(text: &str) -> State {
        let value = crate::state::Value::Str(crate::state::value::StringValue::new(text, 80));
        let mut state = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    fn open_with_json(text: &str) -> State {
        let value = crate::state::Value::Json(crate::state::value::JsonValue::parse(text));
        let mut state = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    fn type_text(mut s: State, text: &str) -> State {
        for c in text.chars() {
            (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char(c))));
        }
        s
    }

    #[test]
    fn e_opens_a_buffer_holding_the_raw_text_not_the_wrapped_display_lines() {
        // A long, unwrapped value: `StringValue::new` would wrap this at 80
        // columns for display, and rejoining those wrapped lines would insert
        // newlines the value never had. The buffer must hold the original
        // text.
        let text = "x".repeat(200);
        let s = open_with_string(&text);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "R3.8's guard must be up immediately"
        );
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(editor.text(), text.into_bytes());
        assert!(cmds.is_empty(), "opening the editor emits no command");
    }

    #[test]
    fn e_opens_json_with_pretty_printed_lines_and_was_json_set() {
        let s = open_with_json("{\"a\":1}");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert!(editor.was_json());
        // Pretty-printed, not the compact original — editing opens the
        // already-pretty form on purpose.
        assert_eq!(editor.text(), b"{\n  \"a\": 1\n}");
    }

    #[test]
    fn typing_undo_and_redo_change_text() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        assert_eq!(s.open.as_ref().unwrap().editor().unwrap().text(), b"!old");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('z'))));
        assert_eq!(s.open.as_ref().unwrap().editor().unwrap().text(), b"old");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('y'))));
        assert_eq!(s.open.as_ref().unwrap().editor().unwrap().text(), b"!old");
    }

    #[test]
    fn e_on_a_collection_or_binary_value_refuses_with_a_notice_not_a_panic() {
        let hash = crate::state::Value::Hash(crate::state::value::PairValue {
            pairs: vec![("f".into(), "v".into())],
            total: 1,
        });
        let mut s = State {
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), hash, -1, 10, 0)),
            ..State::default()
        };
        s.keys.push(b"k");
        s.rebuild_list();
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
        assert!(matches!(cmds.as_slice(), [Command::Notify { .. }]));
    }

    #[test]
    fn ctrl_s_with_no_change_closes_the_buffer_silently() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
        assert!(s.confirm.is_none());
        assert!(cmds.is_empty());
    }

    #[test]
    fn ctrl_s_with_a_change_stages_set_string_and_keeps_editing() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "still mid-edit at preview"
        );
        assert!(
            matches!(
                s.open.as_ref().unwrap().edit,
                crate::state::EditPhase::Staged(_)
            ),
            "staged, still on screen under the dialog"
        );
        assert!(cmds.is_empty(), "staging emits no command of its own");
        match &s.confirm {
            Some(PendingMutation::SetString {
                name,
                old,
                new,
                was_json,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(old, b"old");
                assert_eq!(new, b"!old");
                assert!(!was_json);
            }
            other => panic!("expected a staged SetString, got {other:?}"),
        }
        assert_eq!(
            s.confirm.as_ref().unwrap().command_text(),
            "SET k !old KEEPTTL XX",
            "keeps the TTL, and never recreates a key that is gone"
        );

        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec()
                },
                index: None,
            }]
        );
    }

    fn staged_text(s: &State) -> Option<(Vec<u8>, bool)> {
        let open = s.open.as_ref()?;
        let editor = open.editor()?;
        Some((editor.text(), open.typing().is_none()))
    }

    #[test]
    fn a_staged_buffer_stays_on_screen_until_read_back_but_takes_no_input() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert_eq!(
            staged_text(&s),
            Some((b"!old".to_vec(), true)),
            "under the dialog"
        );
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, _) = update(s, Msg::Paste("zz".to_string()));
        assert_eq!(
            staged_text(&s),
            Some((b"!old".to_vec(), true)),
            "while the SET is in flight: still shown, never typed into"
        );
    }

    #[test]
    fn esc_at_the_dialog_drops_the_staged_buffer() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
    }

    #[test]
    fn e_while_the_last_edit_is_still_saving_is_refused_with_a_notice() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(matches!(cmds.as_slice(), [Command::Notify { .. }]));
        assert_eq!(staged_text(&s), Some((b"!old".to_vec(), true)));
    }

    #[test]
    fn a_key_gone_under_the_dialog_closes_it_and_hands_the_edit_back() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let token = s.read_token;
        let (s, cmds) = update(
            s,
            Msg::ValueGone {
                token,
                index: None,
                name: "k".into(),
                at_ms: 9_000,
            },
        );
        assert!(cmds.is_empty(), "nothing to write to, so nothing is sent");
        assert!(s.confirm.is_none(), "the dialog closes at once");
        assert!(s.notice.is_some());
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(9_000));
        assert!(open.is_editing(), "the buffer is open again");
        assert_eq!(staged_text(&s), Some((b"!old".to_vec(), false)));
        let s = type_text(s, "?");
        assert_eq!(
            staged_text(&s),
            Some((b"!?old".to_vec(), false)),
            "typed into again"
        );
    }

    #[test]
    fn a_key_gone_with_the_set_in_flight_leaves_its_reply_to_decide() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueGone {
                token,
                index: None,
                name: "k".into(),
                at_ms: 9_000,
            },
        );
        assert_eq!(
            staged_text(&s),
            Some((b"!old".to_vec(), true)),
            "the SET is already sent"
        );
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::KeyGone)),
                at_ms: 9_100,
            },
        );
        assert!(cmds.is_empty(), "never retried");
        assert_eq!(staged_text(&s), Some((b"!old".to_vec(), false)));
        assert!(s.open.as_ref().unwrap().is_editing());
    }

    #[test]
    fn a_set_that_finds_the_key_gone_tombstones_it_and_hands_the_edit_back() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::KeyGone)),
                at_ms: 9_100,
            },
        );
        assert!(cmds.is_empty(), "never retried, never recreated");
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(9_100));
        assert!(open.is_editing());
        assert_eq!(staged_text(&s), Some((b"!old".to_vec(), false)));
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("nothing written, edit kept"), "{text}");

        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        let open = s.open.unwrap();
        assert!(open.editor().is_none(), "Esc still discards it");
        assert!(!open.is_editing());
    }

    #[test]
    fn a_key_gone_after_the_write_landed_drops_the_staged_buffer() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, _) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::Done),
                at_ms: 9_000,
            },
        );
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueGone {
                token,
                index: None,
                name: "k".into(),
                at_ms: 9_100,
            },
        );
        let open = s.open.unwrap();
        assert!(open.editor().is_none());
        assert!(!open.is_editing());
    }

    #[test]
    fn value_set_key_gone_for_a_key_no_longer_open_touches_nothing() {
        let s = open_with_string("old");
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "some other key".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::KeyGone)),
                at_ms: 9_000,
            },
        );
        assert!(cmds.is_empty());
        assert!(s.error.is_none());
        assert!(s.open.unwrap().deleted_at_ms.is_none());
    }

    #[test]
    fn was_json_is_carried_from_the_pre_edit_value_not_guessed_from_the_new_text() {
        let s = open_with_json("{\"a\":1}");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        // Break the JSON, then stage — `was_json` still reflects that this
        // *was* a JSON value, independent of the edit's own validity.
        let s = type_text(s, "x");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let Some(PendingMutation::SetString { was_json, .. }) = &s.confirm else {
            panic!("expected a staged SetString");
        };
        assert!(*was_json);
        assert_eq!(s.confirm.as_ref().unwrap().json_warning(), Some(true));
    }

    #[test]
    fn esc_discards_the_buffer_with_nothing_staged() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none());
    }

    #[test]
    fn while_the_editor_is_open_a_value_loaded_for_that_key_is_held_not_applied() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token,
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::Str(crate::state::value::StringValue::new(
                    "from the server",
                    80,
                )),
                ttl_seconds: -1,
                size_bytes: 10,
                at_ms: 1_000,
            },
        );
        let open = s.open.unwrap();
        assert_eq!(
            open.value,
            Some(crate::state::Value::Str(
                crate::state::value::StringValue::new("old", 80)
            )),
            "the screen must not change under an open editor"
        );
        assert!(open.pending.is_some(), "held for later instead");
        assert!(
            open.editor().is_some(),
            "and the buffer itself is untouched"
        );
    }

    #[test]
    fn a_stray_key_at_the_confirm_dialog_keeps_it_up() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_some());
        // A leaked keystroke from something like the vim OSC 10/11
        // colour-query race (PLAN M2 task 4 rework) must not throw the
        // preview away.
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('c'))));
        assert!(s.confirm.is_some(), "the dialog must still be up");
        assert!(cmds.is_empty());
    }

    #[test]
    fn esc_at_the_confirm_dialog_clears_editing_with_nothing_sent() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!s.open.unwrap().is_editing());
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none());
    }

    #[test]
    fn a_failed_write_ends_the_edit() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Err("READONLY You can't write against a read only replica.".into()),
                at_ms: 0,
            },
        );
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.starts_with("SET k: READONLY"), "{text}");
        let open = s.open.unwrap();
        assert!(
            !open.is_editing(),
            "R3.8's guard does not outlive the write"
        );
        assert!(open.editor().is_none());
    }

    /// Review H2: an unrelated failure — a metadata fetch, a clipboard error —
    /// used to switch R3.8's guard off under a buffer still being typed into,
    /// so the next live update could land beneath unsaved text.
    #[test]
    fn an_unrelated_failure_never_ends_an_edit_being_typed() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(
            s,
            Msg::Failed {
                command: "fetching metadata".into(),
                detail: "timed out".into(),
                at_ms: 0,
            },
        );
        assert!(s.error.is_some(), "the failure is still shown");
        let open = s.open.as_ref().unwrap();
        assert!(open.is_editing(), "the guard still holds");
        assert_eq!(open.typing().unwrap().text(), b"!old");
    }

    #[test]
    fn a_paste_while_editing_inserts_once() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let (s, _) = update(s, Msg::Paste("!!!".to_string()));
        assert_eq!(s.open.as_ref().unwrap().editor().unwrap().text(), b"!!!old");
    }

    #[test]
    fn value_set_clears_editing_before_minting_the_refetch() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "still true until ValueSet"
        );

        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::Done),
                at_ms: 5_000,
            },
        );
        assert!(!s.open.as_ref().unwrap().is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::ReadKey { .. }]),
            "the reply, not this message, is what the Viewer will show (ADR-0006)"
        );
    }

    #[test]
    fn value_set_for_a_key_no_longer_open_touches_nothing() {
        let s = open_with_string("old");
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "some other key".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::Done),
                at_ms: 5_000,
            },
        );
        assert!(!s.open.unwrap().is_editing(), "was never true here");
        assert!(cmds.is_empty());
    }

    #[test]
    fn e_opens_the_editor_on_the_line_under_the_viewer_cursor() {
        let mut s = open_with_json(r#"{"a":1,"b":2}"#);
        s.open.as_mut().unwrap().cursor = 2;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(editor.widget().cursor(), (2, 0));
    }

    #[test]
    fn the_refetch_after_a_confirmed_write_lands_even_with_the_cursor_off_the_top() {
        let mut s = open_with_json(r#"{"a":1,"b":2}"#);
        let open = s.open.as_mut().unwrap();
        open.cursor = 2;
        open.at_rest = false;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "x");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetString {
                    key: "k".into(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::Done),
                at_ms: 5_000,
            },
        );
        let [Command::ReadKey { token, .. }] = cmds.as_slice() else {
            panic!("expected a Refetch, got {cmds:?}");
        };
        use crate::state::value::{StringValue, Value};
        let written = Value::Str(StringValue::new("new", 40));
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: *token,
                index: None,
                name: "k".into(),
                value: written.clone(),
                ttl_seconds: -1,
                size_bytes: 3,
                at_ms: 5_100,
            },
        );
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.value.as_ref(), Some(&written), "not held (ADR-0006)");
        assert!(open.pending.is_none());
        assert_eq!(open.cursor, 0, "clamped to the shorter value");
        assert!(
            open.editor().is_none(),
            "the read replaced the staged buffer"
        );
    }

    #[test]
    fn read_only_mode_does_not_refuse_at_e_only_at_y() {
        let s = State {
            read_only: Some(ReadOnlyReason::Environment),
            ..open_with_string("old")
        };
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            s.open.as_ref().unwrap().editor().is_some(),
            "opening is never refused"
        );
        assert!(cmds.is_empty());
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_some(), "the preview is composed anyway");
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                Command::Execute {
                    mutation: Mutation::SetString { .. },
                    ..
                }
            )),
            "but nothing was actually sent to the server"
        );
    }

    // ── PLAN M2 task 6 follow-up: focus-gated editing (G) ───────────────────

    #[test]
    fn e_in_the_keys_pane_gives_the_notice_and_does_not_open() {
        let mut s = open_with_string("old");
        s.focus = Pane::Keys;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let open = s.open.as_ref().unwrap();
        assert!(open.editor().is_none());
        assert!(!open.is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    fn open_with_hash_for_gating() -> State {
        let value = crate::state::Value::Hash(crate::state::value::PairValue {
            pairs: vec![("f".into(), "v".into())],
            total: 1,
        });
        let mut state = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    #[test]
    fn a_in_the_keys_pane_gives_the_notice_and_does_not_open() {
        let mut s = open_with_hash_for_gating();
        s.focus = Pane::Keys;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(s.open.as_ref().unwrap().editor().is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn e_with_nothing_open_in_the_keys_pane_says_open_a_key_first() {
        let s = State {
            focus: Pane::Keys,
            ..State::default()
        };
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "open a key first")
        );
    }

    #[test]
    fn e_with_nothing_open_in_the_value_pane_says_nothing_open_to_edit() {
        let s = State {
            focus: Pane::Value,
            ..State::default()
        };
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "nothing open to edit")
        );
    }

    #[test]
    fn e_and_a_refuse_a_key_confirmed_gone_before_it_ever_loaded() {
        let mut s = State {
            focus: Pane::Value,
            ..State::default()
        };
        s.open = Some(OpenKey::gone(None, "k".into(), 0));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "gone — nothing to edit")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "gone — nothing to edit")
        );
    }
}

#[cfg(test)]
mod hash_field_edit_tests {
    //! `e`/`a`/`d` on a Hash field (PLAN M2 task 6, D1–D4): editing a field,
    //! adding one, removing one, all through the same mutation chokepoint and
    //! R3.8 guard the String editor already proved.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::value::PairValue;

    fn open_with_hash(pairs: &[(&str, &str)], total: usize) -> State {
        let value = crate::state::Value::Hash(PairValue {
            pairs: pairs
                .iter()
                .map(|(f, v)| (f.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
            total,
        });
        let mut state = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    /// The value cursor's row (`Enter` first) is what `e`/`a`/`d` act on.
    fn with_cursor(mut s: State, row: usize) -> State {
        let open = s.open.as_mut().unwrap();
        open.cursor_active = true;
        open.cursor = row;
        s
    }

    fn type_text(mut s: State, text: &str) -> State {
        for c in text.chars() {
            (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char(c))));
        }
        s
    }

    #[test]
    fn e_without_a_cursor_gives_the_notice() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(!s.open.unwrap().is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick a field")
        );
    }

    #[test]
    fn e_on_a_row_opens_the_raw_field_value_not_reformatted_json() {
        let s = with_cursor(open_with_hash(&[("a", "1"), ("b", "{\"x\":1}")], 2), 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(cmds.is_empty());
        let open = s.open.as_ref().unwrap();
        assert!(open.is_editing());
        let editor = open.editor().unwrap();
        assert_eq!(editor.text(), b"{\"x\":1}", "raw, not pretty-printed");
        assert!(
            editor.was_json(),
            "still classified, for the dialog's warning"
        );
        assert!(matches!(
            editor.target(),
            EditTarget::HashField { field } if field == "b"
        ));
    }

    /// A bare scalar parses as valid JSON syntax but is not what "this field
    /// was JSON" means to a reader — the review that caught this used a
    /// numeric field ("id": "8812") to show the false-positive warning it
    /// used to produce. Shares `looks_like_json` with the shell's
    /// `string_value`, which never treated a bare number as JSON either.
    #[test]
    fn a_numeric_field_edited_into_text_stages_with_no_json_warning() {
        let s = with_cursor(open_with_hash(&[("id", "8812")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            !s.open.as_ref().unwrap().editor().unwrap().was_json(),
            "a bare number is not JSON-shaped"
        );
        let s = type_text(s, "x");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert_eq!(
            s.confirm.as_ref().unwrap().json_warning(),
            None,
            "the question must not even arise for a field that was never JSON-shaped"
        );
    }

    #[test]
    fn a_json_object_field_edited_into_something_invalid_warns() {
        let s = with_cursor(open_with_hash(&[("f", "{\"a\":1}")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(s.open.as_ref().unwrap().editor().unwrap().was_json());
        let s = type_text(s, "x"); // breaks the JSON
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert_eq!(s.confirm.as_ref().unwrap().json_warning(), Some(true));
    }

    #[test]
    fn ctrl_s_stages_set_hash_field_with_the_right_field_old_new_and_command_text() {
        let s = with_cursor(open_with_hash(&[("f", "old")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::SetHashField {
                name,
                field,
                old,
                new,
                was_json,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(field, b"f");
                assert_eq!(old, b"old");
                assert_eq!(new, b"!old");
                assert!(!was_json);
            }
            other => panic!("expected a staged SetHashField, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "HSET k f");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text(),
            Some("only if the field still exists · keeps its TTL")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::SetHashField {
                    key: "k".into(),
                    field: b"f".to_vec(),
                    value: b"!old".to_vec()
                },
                index: None,
            }]
        );
    }

    #[test]
    fn ctrl_s_with_no_change_to_a_hash_field_closes_silently() {
        let s = with_cursor(open_with_hash(&[("f", "v")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
        assert!(s.confirm.is_none());
        assert!(cmds.is_empty());
    }

    // ── PLAN M2 task 6 follow-up: the two-part FIELD/VALUE add form (F, N) ──

    #[test]
    fn a_opens_a_buffer_on_the_name_part() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(cmds.is_empty());
        let open = s.open.as_ref().unwrap();
        assert!(
            open.is_editing(),
            "R3.8's guard is up from the moment the form opens"
        );
        let editor = open.editor().unwrap();
        assert_eq!(editor.active_part(), Some(FieldPart::Name));
        assert_eq!(editor.field_name(), Some(""));
        assert_eq!(editor.text(), b"", "the value side starts empty too");
    }

    #[test]
    fn typing_and_backspace_edit_the_name() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().field_name(),
            Some("new")
        );
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Backspace)));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().field_name(),
            Some("ne")
        );
    }

    #[test]
    fn enter_on_an_empty_name_does_nothing() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(
            editor.active_part(),
            Some(FieldPart::Name),
            "still on the name part"
        );
    }

    #[test]
    fn enter_moves_to_the_value_part_then_ctrl_s_stages_add_hash_field() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().active_part(),
            Some(FieldPart::Value),
            "Enter moved to the value part"
        );

        let s = type_text(s, "value");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::AddHashField { name, field, value }) => {
                assert_eq!(name, b"k");
                assert_eq!(field, b"new");
                assert_eq!(value, b"value");
            }
            other => panic!("expected a staged AddHashField, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "HSETNX k new");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text(),
            Some("only if the key still exists · never overwrites a field")
        );
    }

    #[test]
    fn down_also_moves_to_the_value_part() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().active_part(),
            Some(FieldPart::Value)
        );
    }

    #[test]
    fn up_on_the_values_top_row_returns_to_the_name_keeping_the_value_text() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "fr");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Up)));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(editor.active_part(), Some(FieldPart::Name));
        assert_eq!(editor.text(), b"fr", "the value text is kept");
        assert_eq!(editor.field_name(), Some("new"));
    }

    #[test]
    fn up_inside_a_multiline_value_moves_up_a_line_and_stays_in_the_value_part() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "line1");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "line2");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Up)));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(
            editor.active_part(),
            Some(FieldPart::Value),
            "moved up a line, not out of the value part"
        );
        assert_eq!(editor.text(), b"line1\nline2");

        // A second `↑`, now genuinely on the top row, does leave.
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Up)));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(editor.active_part(), Some(FieldPart::Name));
        assert_eq!(editor.text(), b"line1\nline2", "still kept");
    }

    #[test]
    fn ctrl_s_from_the_name_part_stages_with_an_empty_value() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        // Staged straight from the name part — no typed value, no advance.
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::AddHashField { name, field, value }) => {
                assert_eq!(name, b"k");
                assert_eq!(field, b"new");
                assert!(value.is_empty(), "Redis allows an empty field value");
            }
            other => panic!("expected a staged AddHashField, got {other:?}"),
        }
    }

    #[test]
    fn esc_from_the_name_part_discards_and_clears_editing() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        let open = s.open.unwrap();
        assert!(open.editor().is_none());
        assert!(!open.is_editing());
    }

    #[test]
    fn esc_from_the_value_part_discards_and_clears_editing() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "value");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        let open = s.open.unwrap();
        assert!(open.editor().is_none());
        assert!(!open.is_editing());
    }

    #[test]
    fn a_paste_into_the_name_strips_newlines() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let (s, _) = update(s, Msg::Paste("new\r\nfield\n".into()));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().field_name(),
            Some("newfield")
        );
    }

    // ── PLAN M2 task 6 follow-up: the shown-duplicate guard (D) ─────────────

    #[test]
    fn a_shown_duplicate_name_blocks_enter_down_and_ctrl_s() {
        let s = open_with_hash(&[("dup", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");

        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().active_part(),
            Some(FieldPart::Name),
            "Enter blocked"
        );
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().active_part(),
            Some(FieldPart::Name),
            "Down blocked too"
        );
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none(), "⌃S blocked too");
    }

    #[test]
    fn removing_a_character_unblocks_a_duplicate_name() {
        let s = open_with_hash(&[("dup", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Backspace)));
        // "du" is not a shown field.
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().active_part(),
            Some(FieldPart::Value),
            "no longer a duplicate, so Enter advances"
        );
    }

    #[test]
    fn d_in_the_value_pane_stages_delete_hash_field_and_marks_the_last_field() {
        let s = with_cursor(open_with_hash(&[("f", "v")], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::DeleteHashField {
                name,
                field,
                last_field,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(field, b"f");
                assert!(*last_field);
            }
            other => panic!("expected a staged DeleteHashField, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "HDEL k f");
    }

    #[test]
    fn d_with_more_than_one_field_left_is_not_marked_as_the_last() {
        let s = with_cursor(open_with_hash(&[("a", "1"), ("b", "2")], 2), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteHashField { last_field, .. }) => assert!(!last_field),
            other => panic!("expected a staged DeleteHashField, got {other:?}"),
        }
    }

    #[test]
    fn d_without_a_cursor_in_the_value_pane_gives_the_notice() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(s.confirm.is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick a field")
        );
    }

    #[test]
    fn d_in_the_keys_pane_still_stages_delete_key() {
        let mut s = open_with_hash(&[("f", "v")], 1);
        s.focus = Pane::Keys;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(matches!(s.confirm, Some(PendingMutation::DeleteKey { .. })));
    }

    #[test]
    fn read_only_refuses_all_three_hash_mutations_at_confirm() {
        let mutations = [
            PendingMutation::SetHashField {
                name: b"k".to_vec().into(),
                field: b"f".to_vec(),
                old: b"o".to_vec(),
                new: b"n".to_vec(),
                was_json: false,
            },
            PendingMutation::AddHashField {
                name: b"k".to_vec().into(),
                field: b"f".to_vec(),
                value: b"v".to_vec(),
            },
            PendingMutation::DeleteHashField {
                name: b"k".to_vec().into(),
                field: b"f".to_vec(),
                last_field: false,
            },
        ];
        for mutation in mutations {
            let s = State {
                read_only: Some(ReadOnlyReason::User),
                confirm: Some(mutation.clone()),
                ..State::default()
            };
            let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
            assert!(s.confirm.is_none(), "{mutation:?}");
            assert!(
                matches!(cmds.as_slice(), [Command::Notify { text }] if text.contains("read-only")),
                "{mutation:?}"
            );
        }
    }

    #[test]
    fn not_written_field_gone_hands_the_buffer_back_and_refetches() {
        let s = with_cursor(open_with_hash(&[("f", "old")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetHashField {
                    key: "k".into(),
                    field: b"f".to_vec(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::FieldGone)),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        let open = s.open.as_ref().unwrap();
        assert!(
            open.is_editing(),
            "held under R3.8 — the buffer is open again"
        );
        let editor = open.editor().unwrap();
        assert_eq!(editor.text(), b"!old");
        assert!(open.typing().is_some(), "typing again, not staged");
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("HSET k f"), "{text}");
        assert!(text.contains("field no longer exists"), "{text}");
        assert!(text.contains("edit kept"), "{text}");
    }

    #[test]
    fn not_written_field_exists_names_hsetnx_and_hands_the_buffer_back() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "value");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::AddHashField {
                    key: "k".into(),
                    field: b"new".to_vec(),
                    value: b"value".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::FieldExists)),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("HSETNX k new"), "{text}");
        assert!(text.contains("field already exists"), "{text}");
    }

    #[test]
    fn not_written_key_gone_tombstones_and_hands_the_hash_edit_back() {
        let s = with_cursor(open_with_hash(&[("f", "old")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetHashField {
                    key: "k".into(),
                    field: b"f".to_vec(),
                    value: b"!old".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::KeyGone)),
                at_ms: 6_000,
            },
        );
        assert!(cmds.is_empty(), "never retried, never recreated");
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(6_000));
        assert!(open.is_editing(), "the buffer is open again");
        assert_eq!(open.editor().unwrap().text(), b"!old");
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("HSET k f"), "{text}");
        assert!(text.contains("key no longer exists"), "{text}");
    }

    #[test]
    fn key_gone_under_a_staged_set_hash_field_dialog_closes_and_hands_the_buffer_back() {
        let s = with_cursor(open_with_hash(&[("f", "old")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_some());
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueGone {
                token,
                index: None,
                name: "k".into(),
                at_ms: 7_000,
            },
        );
        assert!(s.confirm.is_none());
        assert!(s.notice.is_some());
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(7_000));
        assert!(open.is_editing());
        let editor = open.editor().unwrap();
        assert_eq!(editor.text(), b"!old");
        assert!(open.typing().is_some(), "typing again, not staged");
    }

    #[test]
    fn key_gone_under_a_staged_add_hash_field_dialog_closes_and_hands_the_buffer_back() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "value");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_some());
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueGone {
                token,
                index: None,
                name: "k".into(),
                at_ms: 7_000,
            },
        );
        assert!(s.confirm.is_none());
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(7_000));
        assert_eq!(open.editor().unwrap().text(), b"value");
    }

    #[test]
    fn key_gone_under_a_staged_delete_hash_field_dialog_simply_closes() {
        let s = with_cursor(open_with_hash(&[("f", "v")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(s.confirm.is_some());
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueGone {
                token,
                index: None,
                name: "k".into(),
                at_ms: 7_000,
            },
        );
        assert!(s.confirm.is_none());
        assert!(s.notice.is_some());
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(7_000));
        assert!(open.editor().is_none(), "delete never had a buffer");
    }

    #[test]
    fn a_delete_hash_field_hdel_returning_false_is_a_notice_not_an_error() {
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::DeleteHashField {
                    key: "k".into(),
                    field: b"f".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NothingToRemove),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        assert!(s.error.is_none(), "not an error");
        let (text, _) = s.notice.as_ref().unwrap();
        assert!(text.contains("HDEL k f"), "{text}");
        assert!(text.contains("already gone"), "{text}");
    }

    #[test]
    fn an_update_arriving_while_a_field_is_edited_is_held() {
        let s = with_cursor(open_with_hash(&[("f", "old")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: crate::command::ReadToken::default(),
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::Hash(PairValue {
                    pairs: vec![("f".into(), "changed-under-the-editor".into())],
                    total: 1,
                }),
                ttl_seconds: -1,
                size_bytes: 10,
                at_ms: 9_000,
            },
        );
        let open = s.open.as_ref().unwrap();
        assert!(open.pending.is_some(), "held, not applied");
        assert_eq!(
            open.editor().unwrap().text(),
            b"old",
            "the buffer is never touched (R3.8)"
        );
    }

    #[test]
    fn the_cursor_is_clamped_after_a_delete_reads_back_fewer_fields() {
        let s = with_cursor(open_with_hash(&[("a", "1"), ("b", "2")], 2), 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::DeleteHashField {
                    key: "k".into(),
                    field: b"b".to_vec()
                },
                index: None,
            }]
        );
        let (s, _) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::DeleteHashField {
                    key: "k".into(),
                    field: b"b".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::Done),
                at_ms: 9_000,
            },
        );
        let token = s.read_token;
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token,
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::Hash(PairValue {
                    pairs: vec![("a".into(), "1".into())],
                    total: 1,
                }),
                ttl_seconds: -1,
                size_bytes: 5,
                at_ms: 9_100,
            },
        );
        assert_eq!(
            s.open.as_ref().unwrap().cursor,
            0,
            "clamped once the row it was on disappeared"
        );
    }
}
