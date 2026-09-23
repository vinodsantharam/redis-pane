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
    // A Set member is never edited in place (D1, ADR-0016): its bytes are its
    // whole identity, so "editing" one is a rename — `SREM old` + `SADD new`
    // — which belongs with Hash field rename at PLAN M2 task 14, not here.
    // `e` always refuses; which notice depends on whether there is a row to
    // refuse *about* yet (D4/ADR-0015's gating, one collection type over) and
    // on whether that row's bytes are even nameable in a notice — a
    // non-UTF-8 member gets the same wording a binary Hash field's failed
    // `for_hash_field` gives, since neither can be shown as text either way.
    if let Value::Set(members) = value {
        if !open.cursor_active {
            return (state, notify("Enter to pick a member"));
        }
        let Some(member) = members.members.get(open.cursor) else {
            return (state, notify("Enter to pick a member"));
        };
        let text = if std::str::from_utf8(member).is_ok() {
            "a member can't be edited in place — remove it, then add the new one"
        } else {
            "binary members aren't editable here yet"
        };
        return (state, notify(text));
    }
    // A List element *is* edited in place (D1, ADR-0017): unlike a Set
    // member, an element has an identity — its index — that survives its
    // bytes changing, so `LSET` is a real edit, not a rename. `index` is the
    // row's position within the fetched window, which is also its absolute
    // Redis index (D7 — the window always starts at 0), so no arithmetic is
    // needed to key ADR-0017 D2's compare-and-set guard.
    if let Value::List(items) = value {
        if !open.cursor_active {
            return (state, notify("Enter to pick an element"));
        }
        let Some(element) = items.items.get(open.cursor) else {
            return (state, notify("Enter to pick an element"));
        };
        return match EditBuffer::list_element(open.cursor, element) {
            Ok(buffer) => {
                if let Some(open) = state.open.as_mut() {
                    open.begin_edit(buffer);
                }
                (state, Vec::new())
            }
            Err(text) => (state, notify(text)),
        };
    }
    // `e` on a ZSet edits the score, never the member (D1, ADR-0018) — a
    // member is identity, exactly as a Set member is (D1, one type over),
    // and a member rename joins Hash field rename and Set member rename at
    // PLAN M2 task 14. Unlike every other row-level edit above, a non-UTF-8
    // member does **not** refuse here (D5): the score edit never touches the
    // member's bytes — they travel to the server exactly as read — and the
    // score itself is always ASCII, so [`EditBuffer::zset_score`] is
    // infallible, unlike [`EditBuffer::for_hash_field`]/
    // [`EditBuffer::list_element`] above it.
    if let Value::ZSet(scored) = value {
        if !open.cursor_active {
            return (state, notify("Enter to pick a member"));
        }
        let Some((member, score)) = scored.entries.get(open.cursor) else {
            return (state, notify("Enter to pick a member"));
        };
        let buffer = EditBuffer::zset_score(member, *score);
        if let Some(open) = state.open.as_mut() {
            open.begin_edit(buffer);
        }
        return (state, Vec::new());
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

/// Stages `a`: opens the add form on the Open Hash, Set or List — the
/// two-part `FIELD`/`VALUE` form on the name part for a Hash (PLAN M2 task 6
/// follow-up, F), or the single-part capture for a Set (PLAN M2 task 7, D3,
/// ADR-0016) or a List (PLAN M2 task 8, D6, ADR-0017), since neither a member
/// nor an element has a name half to type first. No cursor prerequisite
/// either way — a new field, member or element has no row to have picked yet,
/// unlike `e`.
///
/// Named for what it actually does, not for the type it was first written
/// against (PLAN M2 task 8, D8) — `begin_add_field` was accurate for one
/// collection type and misleading for three.
///
/// Emits no command: the form lives entirely in the core, the same as every
/// other inline edit. `editing` is set from the moment it opens — there is no
/// longer a name-only capture stage before an [`EditBuffer`] exists, so this
/// is also the moment one is created.
pub(super) fn begin_add_entry(mut state: State) -> (State, Vec<Command>) {
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
    // Exhaustive over `Value`, not a wildcard fallback (PLAN M2 task 8, D8):
    // a fourth addable type (ZSet, task 9) has to make this same decision
    // here, once, rather than silently falling through to this refusal the
    // way a `_` arm would let it. ZSet's add form is two-part — `MEMBER`
    // then `SCORE` (D6, ADR-0018) — mirroring the Hash add form's
    // `FIELD`/`VALUE` shape, unlike Set's/List's single-capture forms.
    let buffer = match &open.value {
        None => return (state, notify("nothing open to edit")),
        Some(Value::Hash(_)) => EditBuffer::new_hash_field(),
        Some(Value::Set(_)) => EditBuffer::new_set_member(),
        Some(Value::List(_)) => EditBuffer::new_list_element(),
        Some(Value::ZSet(_)) => EditBuffer::new_zset_member(),
        Some(Value::Str(_) | Value::Stream(_) | Value::Json(_) | Value::Binary(_)) => {
            return (
                state,
                notify(
                    "fields can only be added to a hash, members to a set or zset, elements to a list",
                ),
            );
        }
    };
    if let Some(open) = state.open.as_mut() {
        open.begin_edit(buffer);
    }
    (state, Vec::new())
}

/// Whether `Enter`, `↓` and `⌃S` are blocked on the add form's name part —
/// the Hash `FIELD` (PLAN M2 task 6 follow-up, D) or the ZSet `MEMBER` (PLAN
/// M2 task 9, D6, ADR-0018). `true` with nothing to check at all, so a
/// caller need not re-verify `open`/`editor` exist first.
///
/// The two targets' rules differ by one clause: Hash blocks an empty field
/// name outright, where a ZSet member does not — Redis allows an empty ZSet
/// member the same way it allows an empty Set member
/// ([`OpenKey::set_member_shown_duplicate`]'s doc comment), and D6 names
/// only the shown-duplicate guard as carrying over from Hash, not
/// emptiness. `true` for any other target: this function is only ever asked
/// about a name part, and every other target has none.
pub(super) fn add_form_name_blocked(state: &State) -> bool {
    let Some(open) = &state.open else {
        return true;
    };
    let Some(editor) = open.editor() else {
        return true;
    };
    match editor.target() {
        EditTarget::NewHashField { .. } => {
            let Some(name) = editor.field_name() else {
                return true;
            };
            name.is_empty() || open.hash_field_shown_duplicate()
        }
        EditTarget::NewZSetMember { .. } => open.zset_member_shown_duplicate(),
        _ => true,
    }
}

/// Whether `⌃S` is blocked on the Set add form (PLAN M2 task 7, D3,
/// ADR-0016): a shown duplicate, exact byte equality against a member
/// already in the fetched window. Unlike [`hash_add_blocked`], an empty
/// member does *not* block staging — Redis allows an empty Set member, the
/// same courtesy [`EditBuffer::new_set_member`] extends, so there is no
/// name-emptiness half to check the way a Hash field's name has. `false`
/// with nothing to check, so a caller need not re-verify `open`/`editor`
/// exist or that the buffer is even a Set add before calling this.
pub(super) fn set_member_blocked(state: &State) -> bool {
    state
        .open
        .as_ref()
        .is_some_and(OpenKey::set_member_shown_duplicate)
}

/// Whether the buffer's current text is a score `⌃S`/`Enter` may stage (D4,
/// ADR-0018) — checked against the raw buffer text regardless of which part
/// of the ZSet add form is active, since that text is exactly what
/// `stage_editor` parses whichever part the reader happens to be looking at.
/// `true` (nothing to block) for every target but [`EditTarget::ZSetScore`]
/// and [`EditTarget::NewZSetMember`], so folding this into a guard changes
/// nothing for the String/Hash/Set/List paths.
fn zset_score_valid(state: &State) -> bool {
    let Some(editor) = state.open.as_ref().and_then(OpenKey::typing) else {
        return true;
    };
    match editor.target() {
        EditTarget::ZSetScore { .. } | EditTarget::NewZSetMember { .. } => {
            is_valid_zset_score(&String::from_utf8_lossy(&editor.text()))
        }
        _ => true,
    }
}

/// Whether `⌃S`/`Enter` are blocked from the editor's main capture path —
/// not the add form's *name* part, gated separately by
/// [`add_form_name_blocked`] — on a shown Set duplicate (D3, ADR-0016) or an
/// invalid ZSet score (D4, ADR-0018): either an existing member's score
/// being edited, or the score half of the ZSet add form. `false` for every
/// other target, so this adds nothing to the String/Hash-field/Hash-add/List
/// paths.
pub(super) fn value_part_stage_blocked(state: &State) -> bool {
    set_member_blocked(state) || !zset_score_valid(state)
}

/// Keys read while the add form's name part is active (PLAN M2 task 6
/// follow-up, F/N) — shaped like the old field-name capture it replaces:
/// plain characters append, Backspace removes one, Paste appends with
/// newlines stripped (handled in `update`'s `Msg::Paste` arm, not here).
/// `Enter`/`↓` advance to the value part and `⌃S` stages directly from here,
/// all three gated by [`add_form_name_blocked`]; `Esc` discards the whole add.
pub(super) fn name_part_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    if let Some(action) = state.keymap.action_for(&key) {
        match action {
            Action::EditorStage => {
                // Staging directly from the name part (never having advanced
                // to the value/score part) uses whatever that part's buffer
                // already holds — empty for a fresh Hash add, and D4's
                // numeric guard must hold here too, or `⌃S` on the ZSet add
                // form's bare member part would stage an empty, unparseable
                // score.
                if add_form_name_blocked(&state) || !zset_score_valid(&state) {
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
            if !add_form_name_blocked(&state)
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
    // A brand-new field or member has no prior value to be unchanged from —
    // an empty value is a real value Redis allows, not "nothing to save"
    // (D1, and ADR-0016 D3 for the Set member one field narrower).
    let is_new_field = matches!(
        editor.target(),
        EditTarget::NewHashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::NewListElement { .. }
            | EditTarget::NewZSetMember { .. }
    );
    if !is_new_field && !editor.is_dirty() {
        open.end_edit();
        return (state, Vec::new());
    }
    // A ZSet score is the one edit whose text can be *invalid* rather than
    // merely unwanted (ADR-0018 D4), and the one `PendingMutation` payload
    // that is a number rather than bytes. Parse it here, before anything is
    // staged, and refuse to stage at all if it will not parse.
    //
    // `⌃S` is already guarded on D4, so this should be unreachable — but
    // "unreachable" is not a good enough reason to pick a fallback value,
    // because there is no honest one to pick. A fabricated `0.0` is a score
    // the reader never typed, and a `NaN` cannot live in these types at all:
    // `Mutation` and `PendingMutation` implement `Eq` by hand, `Eq` promises
    // `a == a`, and `NaN` is the one `f64` that breaks it. Refusing to stage
    // keeps both out and leaves `Eq` true by construction rather than by
    // assumption.
    let score = |bytes: &[u8]| -> Option<f64> {
        let text = String::from_utf8_lossy(bytes).into_owned();
        is_valid_zset_score(&text).then(|| text.parse().ok())?
    };
    let (old_score, new_score) = match editor.target() {
        EditTarget::ZSetScore { .. } => match (score(editor.original()), score(&editor.text())) {
            (Some(old), Some(new)) => (old, new),
            _ => return (state, Vec::new()),
        },
        EditTarget::NewZSetMember { .. } => match score(&editor.text()) {
            Some(new) => (0.0, new),
            None => return (state, Vec::new()),
        },
        // Every other target's payload is bytes, which cannot fail to parse.
        _ => (0.0, 0.0),
    };
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
        // `a` on a Set (PLAN M2 task 7 phase 3, ADR-0016 D3): the staging
        // shape mirrors `NewHashField`'s, one field narrower — no field name
        // to carry, since a member is only a value.
        EditTarget::NewSetMember => PendingMutation::AddSetMember { name, member: new },
        // `e`/`a` on a List (PLAN M2 task 8 phase 3, ADR-0017 D2/D6):
        // `open_editor`/`begin_add_entry` construct these targets. `old`/
        // `original` is the guard's `expected` half (ADR-0017 D2's
        // compare-and-set); `end` is D6's Head/Tail toggle, current at the
        // moment `⌃S`/`Enter` stages — `Tab` (`editor_key`, below) is what
        // changes it while the form is open.
        EditTarget::ListElement { index } => PendingMutation::SetListElement {
            name,
            index,
            old: original,
            new,
            was_json,
        },
        EditTarget::NewListElement { end } => PendingMutation::AddListElement {
            name,
            end,
            value: new,
        },
        // `e` on a ZSet (PLAN M2 task 9 phase 3, ADR-0018 D1, D2): not
        // reachable until `open_editor` constructs this target — the arm
        // exists now because `EditTarget` is matched exhaustively (PLAN M2
        // task 8, D8). The buffer holds the score's text, seeded and
        // validated as an f64 elsewhere (D4); `original` is the score's
        // text as read, parsed back for the guard's `old_score`. `member`
        // is the identity the write is keyed on — D7: rank is not identity,
        // so a reorder under the dialog cannot misdirect this.
        // Both scores were parsed above, before anything was staged — an
        // unparseable one returned early rather than reaching here, so these
        // are the reader's own numbers and never a stand-in.
        EditTarget::ZSetScore { member } => PendingMutation::SetZSetScore {
            name,
            member,
            old_score,
            new_score,
        },
        // `a` on a ZSet (PLAN M2 task 9 phase 3, ADR-0018 D2, D6): not
        // reachable until `begin_add_entry` constructs this target, for the
        // same reason the arm above is not.
        EditTarget::NewZSetMember { member, .. } => PendingMutation::AddZSetMember {
            name,
            member: member.into_bytes(),
            score: new_score,
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
            // The Set add form's shown-duplicate guard (D3, ADR-0016) and the
            // ZSet score's numeric guard (D4, ADR-0018) are checked here, not
            // in `stage_editor` itself — `stage_editor` is also how a
            // confirmed `y` at the dialog is *not* reached (that goes through
            // `confirm_key`), so the one call site that can actually
            // short-circuit staging on either is this key handler, the same
            // way `add_form_name_blocked` gates `⌃S` from the name part in
            // `name_part_key` above. Every other `EditTarget` leaves
            // `value_part_stage_blocked` `false`, so this adds nothing to the
            // String/Hash-field/Hash-add/List paths.
            Action::EditorStage => {
                if value_part_stage_blocked(&state) {
                    return (state, Vec::new());
                }
                return stage_editor(state);
            }
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
    // `Enter` stages for every target except a plain String/JSON value
    // (2026-09-22 amendment to ADR-0014, below the Decision heading): a
    // Hash field's value, the Set add form, and the value part of the Hash
    // add form (the name part never reaches here — it is routed to
    // `name_part_key`, above, which keeps `Enter`'s existing "advance to the
    // value part" meaning) all stage on `Enter` exactly as `⌃S` does. This is
    // checked ahead of the mutable borrow below so it can hand `state`
    // straight to `stage_editor` — the same call, and the same
    // `value_part_stage_blocked` guard, `Action::EditorStage` uses just
    // above. One call site for "stage from here," so `Enter` cannot refuse
    // something `⌃S` would let through, or the other way around. A String
    // stays on `Enter` inserting a newline: it is the one value shape a
    // reader genuinely needs to type more than one line into.
    if key.code == KeyCode::Enter
        && state
            .open
            .as_ref()
            .and_then(OpenKey::typing)
            .is_some_and(|editor| !matches!(editor.target(), EditTarget::Value))
    {
        if value_part_stage_blocked(&state) {
            return (state, Vec::new());
        }
        return stage_editor(state);
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
        // `Tab` flips D6's Head/Tail toggle on the List add form, rather
        // than inserting a tab character — the only target where `Tab`
        // means something other than "insert a tab" (ADR-0017 D6).
        KeyCode::Tab if matches!(editor.target(), EditTarget::NewListElement { .. }) => {
            editor.toggle_list_end();
        }
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
    // `SetHashField`/`AddHashField`/`AddSetMember`/`SetListElement`/
    // `AddListElement` dialog closes and hands its buffer back exactly the
    // same way; a `DeleteHashField`/`DeleteSetMember`/`DeleteListElement`
    // dialog simply closes with the notice, since none of the three ever had
    // a buffer to hand back (`unstage_buffer` is a no-op with none).
    //
    // A real `match`, not `matches!` with an or-pattern inside it (task 8
    // phase 2's "found while building" note, and PLAN M2 task 8, D8 above
    // it): `matches!` always expands to a `match` with a trailing `_ =>
    // false` arm, so an or-pattern inside it is never exhaustiveness-checked
    // against `PendingMutation` — it compiled fine without the three List
    // variants below and would have silently taken the wrong branch for a
    // staged List mutation the moment `e`/`a`/`d` could reach one. Matching
    // every variant by name here, with no wildcard arm, makes a future
    // `PendingMutation` variant (ZSet, task 9) a compile error in this
    // function until it is given the same decision, rather than a bug that
    // only shows up under a race.
    let dialog_up = match &state.confirm {
        Some(
            PendingMutation::SetString { name: staged, .. }
            | PendingMutation::SetHashField { name: staged, .. }
            | PendingMutation::AddHashField { name: staged, .. }
            | PendingMutation::DeleteHashField { name: staged, .. }
            | PendingMutation::AddSetMember { name: staged, .. }
            | PendingMutation::DeleteSetMember { name: staged, .. }
            | PendingMutation::SetListElement { name: staged, .. }
            | PendingMutation::AddListElement { name: staged, .. }
            | PendingMutation::DeleteListElement { name: staged, .. }
            | PendingMutation::SetZSetScore { name: staged, .. }
            | PendingMutation::AddZSetMember { name: staged, .. }
            | PendingMutation::DeleteZSetMember { name: staged, .. },
        ) => staged == name,
        // `DeleteKey` never opens a buffer and is handled entirely by
        // `key_deleted` (`update/confirm.rs`), not this function — the key
        // going gone under its own staged `DEL` is that path's job, not a
        // buffer to hand back. `None`: no dialog is up at all.
        Some(PendingMutation::DeleteKey { .. }) | None => false,
    };
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

    /// The user's explicit exception (2026-09-22 amendment to ADR-0014):
    /// every other `EditTarget` stages on `Enter` now, but a String is
    /// genuinely multi-line, so `Enter` still inserts a newline here and
    /// leaves nothing staged.
    #[test]
    fn enter_still_inserts_a_newline_for_a_plain_string_value() {
        let s = open_with_string("old");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        let text = s.open.as_ref().unwrap().editor().unwrap().text();
        assert_eq!(text.len(), 4, "one newline byte inserted into \"old\"");
        assert_eq!(text.iter().filter(|&&b| b == b'\n').count(), 1);
        assert!(s.confirm.is_none(), "Enter never stages a String edit");
    }

    /// Same exception for a JSON-classified String — `EditTarget::Value`
    /// covers both (2026-09-22 amendment to ADR-0014).
    #[test]
    fn enter_still_inserts_a_newline_for_a_json_value() {
        let s = open_with_json("{\"a\":1}");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let before = s.open.as_ref().unwrap().editor().unwrap().text();
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none());
        let after = s.open.as_ref().unwrap().editor().unwrap().text();
        assert_eq!(after.len(), before.len() + 1, "one newline byte inserted");
        assert_eq!(
            after.iter().filter(|&&b| b == b'\n').count(),
            before.iter().filter(|&&b| b == b'\n').count() + 1
        );
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
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
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

    // ── 2026-09-22 amendment to ADR-0014: `Enter` stages a Hash field ──────

    #[test]
    fn enter_stages_an_existing_hash_field_exactly_like_ctrl_s() {
        let s = with_cursor(open_with_hash(&[("f", "old")], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty(), "Enter only opens the confirm dialog");
        match &s.confirm {
            Some(PendingMutation::SetHashField {
                name, field, new, ..
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(field, b"f");
                assert_eq!(new, b"!old");
            }
            other => panic!("expected a staged SetHashField, got {other:?}"),
        }
        // Staging is not executing: nothing runs until `y` confirms it.
        assert!(
            !cmds.iter().any(|c| matches!(c, Command::Execute { .. })),
            "Enter must never itself execute a write"
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
            }],
            "the write Enter staged is identical to the one Ctrl-S would have"
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
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
            Some("only if the key still exists · never overwrites a field")
        );
    }

    #[test]
    fn enter_on_the_value_part_stages_add_hash_field_instead_of_a_newline() {
        // The name part's own `Enter` meaning ("advance to the value part")
        // is unchanged — this is `Enter` pressed a second time, once already
        // on the value part, which now stages rather than inserting a
        // newline (2026-09-22 amendment to ADR-0014).
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "value");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty(), "Enter only opens the confirm dialog");
        match &s.confirm {
            Some(PendingMutation::AddHashField { name, field, value }) => {
                assert_eq!(name, b"k");
                assert_eq!(field, b"new");
                assert_eq!(value, b"value");
            }
            other => panic!("expected a staged AddHashField, got {other:?}"),
        }
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
        // `Enter` now stages the Hash add form's value part (2026-09-22
        // amendment to ADR-0014), so it can no longer be used to build a
        // multi-line value the way it once could — a paste is the one
        // remaining way to get a newline into this form's value part
        // (`update::paste`, `insert_str` keeps `\n` outside the name part).
        let s = open_with_hash(&[("f", "v")], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "new");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let (s, _) = update(s, Msg::Paste("line1\nline2".to_string()));
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

#[cfg(test)]
mod set_member_edit_tests {
    //! `e`/`a`/`d` on a Set member (PLAN M2 task 7, D1, D3, D4, D5,
    //! ADR-0016): adding a member, removing one, and refusing to edit one in
    //! place — one field narrower than the Hash tests above, and sharing the
    //! same chokepoint and R3.8 guard.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::value::MemberValue;

    fn open_with_set(members: &[&str], total: usize) -> State {
        let value = crate::state::Value::Set(MemberValue {
            members: members.iter().map(|m| m.as_bytes().to_vec()).collect(),
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

    // ── D1: `e` never opens a buffer on a Set row ───────────────────────────

    #[test]
    fn e_without_a_cursor_on_a_set_gives_the_pick_a_member_notice() {
        let s = open_with_set(&["alpha"], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(!s.open.unwrap().is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick a member"),
            "{cmds:?}"
        );
    }

    #[test]
    fn e_on_a_set_row_refuses_with_d1s_notice_not_a_buffer() {
        let s = with_cursor(open_with_set(&["alpha"], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
        assert!(
            matches!(
                cmds.as_slice(),
                [Command::Notify { text }]
                    if text == "a member can't be edited in place — remove it, then add the new one"
            ),
            "{cmds:?}"
        );
    }

    #[test]
    fn e_on_a_binary_set_row_gives_d4s_notice_instead() {
        let value = crate::state::Value::Set(MemberValue {
            members: vec![vec![0xff, 0x80]],
            total: 1,
        });
        let mut s = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        s.keys.push(b"k");
        s.rebuild_list();
        let s = with_cursor(s, 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(!s.open.unwrap().is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "binary members aren't editable here yet"),
            "{cmds:?}"
        );
    }

    // ── Focus gating (ADR-0015 D4, mirrored for Sets) ───────────────────────

    #[test]
    fn e_in_the_keys_pane_with_a_set_open_gives_the_tab_notice() {
        let mut s = open_with_set(&["alpha"], 1);
        s.focus = Pane::Keys;
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn a_in_the_keys_pane_with_a_set_open_gives_the_tab_notice() {
        let mut s = open_with_set(&["alpha"], 1);
        s.focus = Pane::Keys;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(s.open.as_ref().unwrap().editor().is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn d_without_a_cursor_on_a_set_gives_the_pick_a_member_notice() {
        let s = open_with_set(&["alpha"], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(s.confirm.is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick a member")
        );
    }

    // ── D3: the add form is a single capture, and `a` needs no cursor ──────

    #[test]
    fn a_opens_a_single_part_buffer_with_no_cursor_needed() {
        let s = open_with_set(&["alpha"], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(cmds.is_empty());
        let open = s.open.as_ref().unwrap();
        assert!(open.is_editing(), "R3.8's guard is up immediately");
        let editor = open.editor().unwrap();
        assert_eq!(editor.target(), &EditTarget::NewSetMember);
        assert_eq!(editor.active_part(), None, "no FIELD/VALUE split");
        assert_eq!(editor.field_name(), None);
        assert_eq!(editor.text(), b"");
    }

    #[test]
    fn typing_and_ctrl_s_stage_add_set_member() {
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::AddSetMember { name, member }) => {
                assert_eq!(name, b"k");
                assert_eq!(member, b"beta");
            }
            other => panic!("expected a staged AddSetMember, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "SADD k");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
            Some("only if the key still exists · never duplicates a member")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::AddSetMember {
                    key: "k".into(),
                    member: b"beta".to_vec(),
                },
                index: None,
            }]
        );
    }

    // ── 2026-09-22 amendment to ADR-0014: `Enter` stages a Set member ──────

    #[test]
    fn enter_stages_add_set_member_exactly_like_ctrl_s() {
        // ADR-0016 D3 originally kept `Enter` as an ordinary newline on this
        // form, since a Set member has no name part to have advanced through
        // first. The 2026-09-22 ADR-0014 amendment supersedes that: every
        // target except a plain String now stages on `Enter`, member
        // included.
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty(), "Enter only opens the confirm dialog");
        match &s.confirm {
            Some(PendingMutation::AddSetMember { name, member }) => {
                assert_eq!(name, b"k");
                assert_eq!(member, b"beta");
            }
            other => panic!("expected a staged AddSetMember, got {other:?}"),
        }
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::AddSetMember {
                    key: "k".into(),
                    member: b"beta".to_vec(),
                },
                index: None,
            }],
            "the write Enter staged is identical to the one Ctrl-S would have"
        );
    }

    #[test]
    fn a_shown_duplicate_member_blocks_enter_exactly_as_it_blocks_ctrl_s() {
        // Both keys route through `set_member_blocked` at the same call
        // site, so a refusal cannot drift between them.
        let s = open_with_set(&["dup"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none(), "Enter blocked");
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "the buffer stays open, not discarded"
        );
    }

    #[test]
    fn ctrl_s_on_an_untouched_empty_buffer_still_stages_an_empty_member() {
        // Redis allows an empty Set member, the same courtesy the Hash add
        // form's value part gets — an unmodified empty buffer is a real
        // member to add, not "nothing to save" (D3's doc comment on
        // `EditBuffer::new_set_member`).
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        match &s.confirm {
            Some(PendingMutation::AddSetMember { member, .. }) => assert!(member.is_empty()),
            other => panic!("expected a staged AddSetMember, got {other:?}"),
        }
    }

    // ── The shown-duplicate guard (D3) ───────────────────────────────────────

    #[test]
    fn a_shown_duplicate_member_blocks_ctrl_s() {
        let s = open_with_set(&["dup"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none(), "⌃S blocked");
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "the buffer stays open, not discarded"
        );
    }

    #[test]
    fn removing_a_character_unblocks_a_duplicate_member() {
        let s = open_with_set(&["dup"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Backspace)));
        // "du" is not a shown member.
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_some(), "no longer blocked");
    }

    #[test]
    fn a_non_duplicate_member_stages_fine() {
        let s = open_with_set(&["alpha", "beta"], 2);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "gamma");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(matches!(
            s.confirm,
            Some(PendingMutation::AddSetMember { .. })
        ));
    }

    // ── D5: `d` in the value pane stages `DeleteSetMember` ─────────────────

    #[test]
    fn d_in_the_value_pane_stages_delete_set_member_and_marks_the_last_member() {
        let s = with_cursor(open_with_set(&["only"], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::DeleteSetMember {
                name,
                member,
                last_member,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(member, b"only");
                assert!(*last_member);
            }
            other => panic!("expected a staged DeleteSetMember, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "SREM k");
    }

    #[test]
    fn d_with_more_than_one_member_left_is_not_marked_as_the_last() {
        let s = with_cursor(open_with_set(&["a", "b"], 2), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteSetMember { last_member, .. }) => assert!(!last_member),
            other => panic!("expected a staged DeleteSetMember, got {other:?}"),
        }
    }

    #[test]
    fn d_in_the_keys_pane_still_stages_delete_key_with_a_set_open() {
        let mut s = open_with_set(&["a"], 1);
        s.focus = Pane::Keys;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(matches!(s.confirm, Some(PendingMutation::DeleteKey { .. })));
    }

    #[test]
    fn confirming_a_delete_set_member_issues_srem() {
        let s = with_cursor(open_with_set(&["only"], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::DeleteSetMember {
                    key: "k".into(),
                    member: b"only".to_vec(),
                },
                index: None,
            }]
        );
    }

    // ── Read-only Mode refuses at confirm, never at the keypress ───────────

    #[test]
    fn read_only_refuses_both_set_mutations_at_confirm_not_at_the_keypress() {
        let mutations = [
            PendingMutation::AddSetMember {
                name: b"k".to_vec().into(),
                member: b"m".to_vec(),
            },
            PendingMutation::DeleteSetMember {
                name: b"k".to_vec().into(),
                member: b"m".to_vec(),
                last_member: false,
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
    fn read_only_mode_does_not_refuse_a_on_a_set_only_at_y() {
        let s = State {
            read_only: Some(ReadOnlyReason::Environment),
            ..open_with_set(&["alpha"], 1)
        };
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(
            s.open.as_ref().unwrap().editor().is_some(),
            "opening the add form is never refused"
        );
        assert!(cmds.is_empty());
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_some(), "the preview is composed anyway");
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                Command::Execute {
                    mutation: Mutation::AddSetMember { .. },
                    ..
                }
            )),
            "but nothing was actually sent to the server"
        );
    }

    // ── `NotWritten::MemberExists` hands the buffer back and re-reads ──────

    #[test]
    fn not_written_member_exists_names_sadd_and_hands_the_buffer_back() {
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::AddSetMember {
                    key: "k".into(),
                    member: b"beta".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::MemberExists)),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("SADD k"), "{text}");
        assert!(text.contains("member already exists"), "{text}");
        let open = s.open.as_ref().unwrap();
        assert!(
            open.is_editing(),
            "held under R3.8 — the buffer is open again"
        );
        assert_eq!(open.editor().unwrap().text(), b"beta");
    }

    #[test]
    fn not_written_key_gone_tombstones_and_hands_the_set_add_back() {
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::AddSetMember {
                    key: "k".into(),
                    member: b"beta".to_vec(),
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
        assert_eq!(open.editor().unwrap().text(), b"beta");
    }

    #[test]
    fn key_gone_under_a_staged_add_set_member_dialog_closes_and_hands_the_buffer_back() {
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
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
        assert_eq!(open.editor().unwrap().text(), b"beta");
    }

    #[test]
    fn key_gone_under_a_staged_delete_set_member_dialog_simply_closes() {
        let s = with_cursor(open_with_set(&["a"], 1), 0);
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
    fn a_delete_set_member_srem_returning_zero_says_member_not_field() {
        // `nothing_to_remove` is shared with `DeleteHashField`'s "field
        // already gone" notice — this is the regression test for keeping a
        // member from being reported in a field's words.
        let s = open_with_set(&["a"], 1);
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::DeleteSetMember {
                    key: "k".into(),
                    member: b"a".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NothingToRemove),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        assert!(s.error.is_none(), "not an error");
        let (text, _) = s.notice.as_ref().unwrap();
        assert!(text.contains("SREM k"), "{text}");
        assert!(text.contains("member already gone"), "{text}");
        assert!(!text.contains("field"), "{text}");
    }

    // ── R3.8: a live update arriving while the add form is open is held ────

    #[test]
    fn an_update_arriving_while_the_set_add_form_is_open_is_held() {
        let s = open_with_set(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: crate::command::ReadToken::default(),
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::Set(MemberValue {
                    members: vec![b"alpha".to_vec(), b"changed-under-the-editor".to_vec()],
                    total: 2,
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
            b"beta",
            "the buffer is never touched (R3.8)"
        );
    }
}

#[cfg(test)]
mod list_element_edit_tests {
    //! `e`/`a`/`d` on a List element (PLAN M2 task 8, D1–D8, ADR-0017):
    //! editing an element in place, adding one at either end, removing one,
    //! and the D6 Head/Tail toggle — the third case of the same chokepoint
    //! and R3.8 guard the Hash and Set tests above already proved.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::value::{IndexedValue, ListEnd};

    fn open_with_list(items: &[&str], total: usize) -> State {
        let value = crate::state::Value::List(IndexedValue {
            items: items.iter().map(|i| i.as_bytes().to_vec()).collect(),
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

    // ── D1/D2: `e` edits a row in place ─────────────────────────────────────

    #[test]
    fn e_without_a_cursor_on_a_list_gives_the_pick_an_element_notice() {
        let s = open_with_list(&["alpha"], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(!s.open.unwrap().is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick an element"),
            "{cmds:?}"
        );
    }

    #[test]
    fn e_on_a_list_row_opens_the_raw_element_keyed_on_its_index() {
        let s = with_cursor(open_with_list(&["alpha", "beta", "gamma"], 3), 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(cmds.is_empty());
        let open = s.open.as_ref().unwrap();
        assert!(open.is_editing(), "R3.8's guard is up immediately");
        let editor = open.editor().unwrap();
        assert_eq!(editor.target(), &EditTarget::ListElement { index: 1 });
        assert_eq!(editor.text(), b"beta");
        assert_eq!(editor.field_name(), None, "an element has no field name");
        assert_eq!(editor.active_part(), None, "no FIELD/VALUE split");
    }

    // ── D4: a non-UTF-8 element refuses with a notice, not a buffer ────────

    #[test]
    fn e_on_a_binary_list_row_gives_d4s_notice_instead_of_a_buffer() {
        let value = crate::state::Value::List(IndexedValue {
            items: vec![vec![0xff, 0x80]],
            total: 1,
        });
        let mut s = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        s.keys.push(b"k");
        s.rebuild_list();
        let s = with_cursor(s, 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let open = s.open.unwrap();
        assert!(!open.is_editing());
        assert!(open.editor().is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "binary elements aren't editable here yet"),
            "{cmds:?}"
        );
    }

    // ── Focus gating (ADR-0015 D4, mirrored for Lists) ──────────────────────

    #[test]
    fn e_in_the_keys_pane_with_a_list_open_gives_the_tab_notice() {
        let mut s = open_with_list(&["alpha"], 1);
        s.focus = Pane::Keys;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(s.open.as_ref().unwrap().editor().is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn a_in_the_keys_pane_with_a_list_open_gives_the_tab_notice() {
        let mut s = open_with_list(&["alpha"], 1);
        s.focus = Pane::Keys;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(s.open.as_ref().unwrap().editor().is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn d_without_a_cursor_on_a_list_gives_the_pick_an_element_notice() {
        let s = open_with_list(&["alpha"], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(s.confirm.is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick an element")
        );
    }

    // ── D6: the add form is a single capture defaulting to Tail, with a Tab toggle ──

    #[test]
    fn a_opens_a_single_part_buffer_defaulting_to_tail_with_no_cursor_needed() {
        let s = open_with_list(&["alpha"], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(cmds.is_empty());
        let open = s.open.as_ref().unwrap();
        assert!(open.is_editing(), "R3.8's guard is up immediately");
        let editor = open.editor().unwrap();
        assert_eq!(
            editor.target(),
            &EditTarget::NewListElement { end: ListEnd::Tail }
        );
        assert_eq!(editor.active_part(), None, "no FIELD/VALUE split");
        assert_eq!(editor.field_name(), None);
        assert_eq!(editor.text(), b"");
    }

    #[test]
    fn tab_toggles_head_and_tail_while_the_add_form_is_open() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().target(),
            &EditTarget::NewListElement { end: ListEnd::Tail }
        );
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        assert!(cmds.is_empty(), "toggling emits no command");
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().target(),
            &EditTarget::NewListElement { end: ListEnd::Head },
            "one Tab flips to Head"
        );
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().target(),
            &EditTarget::NewListElement { end: ListEnd::Tail },
            "a second Tab flips back to Tail"
        );
    }

    #[test]
    fn tab_does_not_toggle_an_existing_elements_edit() {
        // Only the add form has an end to toggle — `Tab` on an in-place edit
        // still inserts a tab character, exactly as it does for every other
        // target.
        let s = with_cursor(open_with_list(&["alpha"], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let before = s.open.as_ref().unwrap().editor().unwrap().text();
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        let after = s.open.as_ref().unwrap().editor().unwrap().text();
        assert!(
            after.len() > before.len(),
            "Tab still inserts (spaces, per `insert_tab`), it just does not toggle an end"
        );
        assert_eq!(
            s.open.as_ref().unwrap().editor().unwrap().target(),
            &EditTarget::ListElement { index: 0 },
            "target unchanged — there is no end on this variant to toggle"
        );
    }

    #[test]
    fn typing_and_ctrl_s_stage_add_list_element_at_the_tail_by_default() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::AddListElement { name, end, value }) => {
                assert_eq!(name, b"k");
                assert_eq!(*end, ListEnd::Tail);
                assert_eq!(value, b"beta");
            }
            other => panic!("expected a staged AddListElement, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "RPUSH k");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
            Some("only if the key still exists")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::AddListElement {
                    key: "k".into(),
                    end: ListEnd::Tail,
                    value: b"beta".to_vec(),
                },
                index: None,
            }]
        );
    }

    #[test]
    fn tab_then_ctrl_s_stages_add_list_element_at_the_head() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        match &s.confirm {
            Some(PendingMutation::AddListElement { end, .. }) => assert_eq!(*end, ListEnd::Head),
            other => panic!("expected a staged AddListElement, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "LPUSH k");
    }

    #[test]
    fn enter_stages_add_list_element_exactly_like_ctrl_s() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty(), "Enter only opens the confirm dialog");
        assert!(matches!(
            s.confirm,
            Some(PendingMutation::AddListElement { .. })
        ));
    }

    #[test]
    fn ctrl_s_on_an_untouched_empty_buffer_still_stages_an_empty_element() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        match &s.confirm {
            Some(PendingMutation::AddListElement { value, .. }) => assert!(value.is_empty()),
            other => panic!("expected a staged AddListElement, got {other:?}"),
        }
    }

    // ── The header names the active end (D6) ────────────────────────────────

    #[test]
    fn the_header_names_which_end_the_add_form_is_pointed_at() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let currency = s.open.as_ref().unwrap().currency(true, 0);
        assert!(currency.contains("tail"), "{currency}");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Tab)));
        let currency = s.open.as_ref().unwrap().currency(true, 0);
        assert!(currency.contains("head"), "{currency}");
    }

    // ── D5/D2: `e` on a set of `LSET key <i>` and stages a compare-and-set ─

    #[test]
    fn ctrl_s_stages_set_list_element_with_the_expected_bytes_and_index() {
        let s = with_cursor(open_with_list(&["alpha", "beta"], 2), 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::SetListElement {
                name,
                index,
                old,
                new,
                was_json,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(*index, 1);
                assert_eq!(old, b"beta");
                assert_eq!(new, b"!beta");
                assert!(!was_json);
            }
            other => panic!("expected a staged SetListElement, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "LSET k 1");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
            Some("only if that element is still there · index 1")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::SetListElement {
                    key: "k".into(),
                    index: 1,
                    expected: b"beta".to_vec(),
                    value: b"!beta".to_vec(),
                },
                index: None,
            }]
        );
    }

    /// Regression: `PendingMutation::json_warning` originally matched only
    /// `SetString`/`SetHashField`, so a List element edit that broke JSON
    /// carried `was_json: true` all the way to the confirm dialog and the
    /// dialog's `if pending.json_warning() == Some(true)` check (`render/
    /// mod.rs`) silently never fired — found while writing phase 5's golden
    /// frame for this exact dialog, since nothing before this phase ever
    /// rendered a staged `SetListElement` through `confirm_overlay`.
    #[test]
    fn a_json_element_edited_into_something_invalid_warns() {
        let s = with_cursor(open_with_list(&["{\"a\":1}"], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(s.open.as_ref().unwrap().editor().unwrap().was_json());
        let s = type_text(s, "x"); // breaks the JSON
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert_eq!(s.confirm.as_ref().unwrap().json_warning(), Some(true));
    }

    // ── D5: `d` in the value pane stages `DeleteListElement` ───────────────

    #[test]
    fn d_in_the_value_pane_stages_delete_list_element_and_marks_the_last_element() {
        let s = with_cursor(open_with_list(&["only"], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::DeleteListElement {
                name,
                index,
                element,
                last_element,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(*index, 0);
                assert_eq!(element, b"only");
                assert!(*last_element);
            }
            other => panic!("expected a staged DeleteListElement, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "LREM k 0");
    }

    #[test]
    fn d_with_more_than_one_element_left_is_not_marked_as_the_last() {
        let s = with_cursor(open_with_list(&["x", "y"], 2), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteListElement { last_element, .. }) => {
                assert!(!last_element);
            }
            other => panic!("expected a staged DeleteListElement, got {other:?}"),
        }
    }

    #[test]
    fn d_removes_the_row_the_cursor_is_actually_on_not_the_first_match() {
        // The whole reason ADR-0017 D2 exists: `LREM` alone would remove the
        // *first* `x`, not the one at the selected row. Staging carries the
        // index the cursor was on, and `crates/app/src/redis/mutate.rs`'s
        // sentinel technique is what makes acting on it duplicate-safe.
        let s = with_cursor(open_with_list(&["x", "y", "x", "z"], 4), 2);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteListElement { index, element, .. }) => {
                assert_eq!(*index, 2);
                assert_eq!(element, b"x");
            }
            other => panic!("expected a staged DeleteListElement, got {other:?}"),
        }
    }

    #[test]
    fn d_in_the_keys_pane_still_stages_delete_key_with_a_list_open() {
        let mut s = open_with_list(&["a"], 1);
        s.focus = Pane::Keys;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(matches!(s.confirm, Some(PendingMutation::DeleteKey { .. })));
    }

    #[test]
    fn confirming_a_delete_list_element_issues_lrem() {
        let s = with_cursor(open_with_list(&["only"], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::DeleteListElement {
                    key: "k".into(),
                    index: 0,
                    expected: b"only".to_vec(),
                },
                index: None,
            }]
        );
    }

    // ── D7: a row inside the window behaves identically whatever `total` is ─

    #[test]
    fn a_long_lists_total_past_the_window_does_not_change_what_e_does_to_a_row_inside_it() {
        let items: Vec<String> = (0..500).map(|i| format!("item-{i}")).collect();
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let s = with_cursor(open_with_list(&refs, 12_000), 499);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(editor.target(), &EditTarget::ListElement { index: 499 });
        assert_eq!(editor.text(), b"item-499");
    }

    #[test]
    fn a_long_lists_total_past_the_window_does_not_change_what_d_does_to_a_row_inside_it() {
        let items: Vec<String> = (0..500).map(|i| format!("item-{i}")).collect();
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let s = with_cursor(open_with_list(&refs, 12_000), 250);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteListElement {
                index,
                element,
                last_element,
                ..
            }) => {
                assert_eq!(*index, 250);
                assert_eq!(element, b"item-250");
                assert!(
                    !last_element,
                    "12,000 items total — nowhere near the list's last element"
                );
            }
            other => panic!("expected a staged DeleteListElement, got {other:?}"),
        }
    }

    // ── Read-only Mode refuses at confirm, never at the keypress ───────────

    #[test]
    fn read_only_refuses_all_three_list_mutations_at_confirm_not_at_the_keypress() {
        let mutations = [
            PendingMutation::SetListElement {
                name: b"k".to_vec().into(),
                index: 0,
                old: b"old".to_vec(),
                new: b"new".to_vec(),
                was_json: false,
            },
            PendingMutation::AddListElement {
                name: b"k".to_vec().into(),
                end: ListEnd::Tail,
                value: b"m".to_vec(),
            },
            PendingMutation::DeleteListElement {
                name: b"k".to_vec().into(),
                index: 0,
                element: b"m".to_vec(),
                last_element: false,
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
    fn read_only_mode_does_not_refuse_e_on_a_list_only_at_y() {
        let s = State {
            read_only: Some(ReadOnlyReason::Environment),
            ..with_cursor(open_with_list(&["alpha"], 1), 0)
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
                    mutation: Mutation::SetListElement { .. },
                    ..
                }
            )),
            "but nothing was actually sent to the server"
        );
    }

    // ── `NotWritten::ElementMoved` hands the buffer back and re-reads ──────

    #[test]
    fn not_written_element_moved_names_lset_and_hands_the_buffer_back() {
        let s = with_cursor(open_with_list(&["alpha", "beta"], 2), 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetListElement {
                    key: "k".into(),
                    index: 1,
                    expected: b"beta".to_vec(),
                    value: b"!beta".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::ElementMoved)),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("LSET k 1"), "{text}");
        assert!(
            text.contains("that element moved — the list changed underneath it, look again"),
            "{text}"
        );
        let open = s.open.as_ref().unwrap();
        assert!(
            open.is_editing(),
            "held under R3.8 — the buffer is open again"
        );
        assert_eq!(open.editor().unwrap().text(), b"!beta");
    }

    #[test]
    fn not_written_key_gone_tombstones_and_hands_the_list_edit_back() {
        let s = with_cursor(open_with_list(&["alpha"], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetListElement {
                    key: "k".into(),
                    index: 0,
                    expected: b"alpha".to_vec(),
                    value: b"!alpha".to_vec(),
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
        assert_eq!(open.editor().unwrap().text(), b"!alpha");
    }

    #[test]
    fn key_gone_under_a_staged_set_list_element_dialog_closes_and_hands_the_buffer_back() {
        let s = with_cursor(open_with_list(&["alpha"], 1), 0);
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
        assert!(
            s.confirm.is_none(),
            "the compiler-enforced `dialog_up` match in \
             `staged_edit_found_key_gone` must recognize SetListElement"
        );
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(7_000));
        assert_eq!(open.editor().unwrap().text(), b"!alpha");
    }

    #[test]
    fn key_gone_under_a_staged_add_list_element_dialog_closes_and_hands_the_buffer_back() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
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
        assert_eq!(open.editor().unwrap().text(), b"beta");
    }

    #[test]
    fn key_gone_under_a_staged_delete_list_element_dialog_simply_closes() {
        let s = with_cursor(open_with_list(&["a"], 1), 0);
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

    // ── R3.8: a live update arriving while the editor is open is held ──────

    #[test]
    fn an_update_arriving_while_the_list_edit_is_open_is_held() {
        let s = with_cursor(open_with_list(&["alpha", "beta"], 2), 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = type_text(s, "!");
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: crate::command::ReadToken::default(),
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::List(IndexedValue {
                    items: vec![b"alpha".to_vec(), b"changed-under-the-editor".to_vec()],
                    total: 2,
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
            b"!beta",
            "the buffer is never touched (R3.8)"
        );
    }

    #[test]
    fn an_update_arriving_while_the_list_add_form_is_open_is_held() {
        let s = open_with_list(&["alpha"], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: crate::command::ReadToken::default(),
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::List(IndexedValue {
                    items: vec![b"alpha".to_vec(), b"changed-under-the-editor".to_vec()],
                    total: 2,
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
            b"beta",
            "the buffer is never touched (R3.8)"
        );
    }
}

#[cfg(test)]
mod zset_score_staging_tests {
    //! Staging a ZSet score (PLAN M2 task 9, ADR-0018 D1/D4).
    //!
    //! `e` on a ZSet row is not wired until phase 3, so these build the
    //! buffer directly and call `stage_editor`, which is the seam the
    //! behaviour under test lives on.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::value::ScoredValue;

    fn editing_score(member: &[u8], score: f64, typed: &str) -> State {
        let value = crate::state::Value::ZSet(ScoredValue {
            entries: vec![(member.to_vec(), score)],
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
            .open
            .as_mut()
            .unwrap()
            .begin_edit(EditBuffer::zset_score(member, score));
        for c in typed.chars() {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char(c))));
        }
        state
    }

    /// Typing garbage into the score must leave nothing staged.
    ///
    /// D4's `⌃S` guard (phase 3) is the real gate, so this path should be
    /// unreachable in the finished feature. It is pinned anyway because the
    /// only alternative to refusing is inventing a score: a `0.0` nobody
    /// typed, or a `NaN`, which `Mutation`/`PendingMutation` cannot hold
    /// honestly — both implement `Eq` by hand, and `NaN != NaN`.
    #[test]
    fn an_unparseable_score_refuses_to_stage_rather_than_inventing_one() {
        let (state, cmds) = stage_editor(editing_score(b"beta", 2.0, "abc"));
        assert!(
            state.confirm.is_none(),
            "an unparseable score must not reach the confirm dialog"
        );
        assert!(cmds.is_empty(), "{cmds:?}");
    }

    /// `nan` parses as an `f64` but Redis refuses it, and it is the one
    /// value that would break `Eq` — so it must not stage either.
    #[test]
    fn nan_refuses_to_stage_even_though_it_parses() {
        let (state, _) = stage_editor(editing_score(b"beta", 2.0, "nan"));
        assert!(state.confirm.is_none(), "nan must not reach the dialog");
    }

    /// A valid score stages, and the staged mutation equals itself.
    ///
    /// The reflexivity assertion is the point: a hand-written `Eq` on a type
    /// carrying an `f64` is sound only while no `NaN` can get in.
    #[test]
    fn a_valid_score_stages_and_the_staged_mutation_equals_itself() {
        let (state, _) = stage_editor(editing_score(b"beta", 2.0, "9"));
        let staged = state.confirm.as_ref().expect("a valid score stages");
        assert_eq!(staged, staged, "Eq must be reflexive for a staged score");
        assert!(
            matches!(
                staged,
                PendingMutation::SetZSetScore { member, old_score, .. }
                    if member == b"beta" && *old_score == 2.0
            ),
            "{staged:?}"
        );
    }
}

#[cfg(test)]
mod zset_score_edit_tests {
    //! `e`/`a`/`d` on a ZSet row (PLAN M2 task 9 phase 3, ADR-0018): `e`
    //! opens the score editor (D1, D5), `a` opens the two-part `MEMBER`/
    //! `SCORE` add form (D6), and `d` stages `ZREM` (D8) — sharing the same
    //! chokepoint and R3.8 guard the Hash/Set/List wiring above does.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::value::ScoredValue;

    fn open_with_zset(entries: &[(&[u8], f64)], total: usize) -> State {
        let value = crate::state::Value::ZSet(ScoredValue {
            entries: entries.iter().map(|(m, s)| (m.to_vec(), *s)).collect(),
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

    /// Replace the score buffer's whole text — [`EditBuffer::zset_score`]
    /// opens with the cursor at the *start* of the seeded text, not the end
    /// (there is no viewer row to map back from the way a String's is), so a
    /// bare Backspace deletes nothing there. This moves to the end first and
    /// clears every character before typing the replacement.
    fn retype_score(mut s: State, new: &str) -> State {
        (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::End)));
        for _ in 0..32 {
            (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Backspace)));
        }
        type_text(s, new)
    }

    // ── D1: `e` opens the score, never the member ───────────────────────────

    #[test]
    fn e_without_a_cursor_on_a_zset_gives_the_pick_a_member_notice() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(!s.open.unwrap().is_editing());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick a member"),
            "{cmds:?}"
        );
    }

    #[test]
    fn e_on_a_zset_row_opens_the_score_seeded_from_format_score_with_the_member_fixed() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 3.5)], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "R3.8's guard must be up immediately"
        );
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(
            editor.target(),
            &EditTarget::ZSetScore {
                member: b"alpha".to_vec()
            }
        );
        assert_eq!(
            editor.text(),
            b"3.5",
            "seeded from format_score, not the member"
        );
        assert!(cmds.is_empty(), "opening the editor emits no command");
    }

    /// D5: unlike Hash/Set/List, a binary member does not refuse `e` — the
    /// score edit never touches the member's bytes.
    #[test]
    fn e_on_a_binary_zset_member_edits_the_score_anyway() {
        let s = with_cursor(open_with_zset(&[(&[0xff, 0x80], 1.0)], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let open = s.open.unwrap();
        assert!(open.is_editing(), "D5: a binary member does not refuse e");
        let editor = open.editor().unwrap();
        assert_eq!(
            editor.target(),
            &EditTarget::ZSetScore {
                member: vec![0xff, 0x80]
            }
        );
        assert_eq!(editor.text(), b"1");
        assert!(cmds.is_empty());
    }

    // ── Focus gating (ADR-0015 D4, mirrored for ZSets) ──────────────────────

    #[test]
    fn e_in_the_keys_pane_with_a_zset_open_gives_the_tab_notice() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.focus = Pane::Keys;
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn a_in_the_keys_pane_with_a_zset_open_gives_the_tab_notice() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.focus = Pane::Keys;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(s.open.as_ref().unwrap().editor().is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Tab to the value pane to edit")
        );
    }

    #[test]
    fn d_without_a_cursor_on_a_zset_gives_the_pick_a_member_notice() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(s.confirm.is_none());
        assert!(
            matches!(cmds.as_slice(), [Command::Notify { text }] if text == "Enter to pick a member")
        );
    }

    #[test]
    fn d_in_the_keys_pane_still_stages_delete_key_with_a_zset_open() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.focus = Pane::Keys;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(matches!(s.confirm, Some(PendingMutation::DeleteKey { .. })));
    }

    // ── D4: the numeric guard blocks ⌃S/Enter while the score is invalid ───

    #[test]
    fn an_invalid_score_blocks_ctrl_s_on_an_existing_score_edit() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        // Replace the seeded "1" with garbage.
        let s = retype_score(s, "abc");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none(), "⌃S blocked on an invalid score");
        assert!(
            s.open.as_ref().unwrap().is_editing(),
            "the buffer stays open, not discarded"
        );
    }

    #[test]
    fn nan_blocks_ctrl_s_even_though_it_parses_as_an_f64() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "nan");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_none(), "nan must not reach the dialog");
    }

    #[test]
    fn enter_is_blocked_the_same_way_ctrl_s_is_on_an_invalid_score() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "abc");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none(), "Enter blocked exactly as ⌃S is");
    }

    #[test]
    fn a_valid_score_unblocks_ctrl_s_and_stages_set_zset_score() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "9.5");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::SetZSetScore {
                name,
                member,
                old_score,
                new_score,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(member, b"alpha");
                assert_eq!(*old_score, 1.0);
                assert_eq!(*new_score, 9.5);
            }
            other => panic!("expected a staged SetZSetScore, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "ZADD k alpha");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
            Some("only if that member still exists")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::SetZSetScore {
                    key: "k".into(),
                    member: b"alpha".to_vec(),
                    score: 9.5,
                },
                index: None,
            }]
        );
    }

    #[test]
    fn inf_is_accepted_and_unblocks_ctrl_s() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "inf");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(matches!(
            s.confirm,
            Some(PendingMutation::SetZSetScore { new_score, .. }) if new_score.is_infinite()
        ));
    }

    // ── D6: the add form is two-part, MEMBER then SCORE ─────────────────────

    #[test]
    fn a_opens_a_two_part_buffer_on_the_member_part() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        assert!(cmds.is_empty());
        let open = s.open.as_ref().unwrap();
        assert!(open.is_editing(), "R3.8's guard is up immediately");
        let editor = open.editor().unwrap();
        assert_eq!(
            editor.target(),
            &EditTarget::NewZSetMember {
                member: String::new(),
                part: FieldPart::Name,
            }
        );
        assert_eq!(editor.active_part(), Some(FieldPart::Name));
        assert_eq!(editor.field_name(), Some(""));
    }

    #[test]
    fn enter_advances_from_the_member_part_to_the_score_part() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty(), "Enter only advances, it does not stage");
        assert!(s.confirm.is_none());
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(editor.active_part(), Some(FieldPart::Value));
        assert_eq!(editor.field_name(), Some("beta"), "the member is kept");
    }

    #[test]
    fn a_shown_duplicate_member_blocks_advancing_past_the_member_part() {
        let s = open_with_zset(&[(b"dup", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        assert!(cmds.is_empty());
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(
            editor.active_part(),
            Some(FieldPart::Name),
            "still on the member part — a shown duplicate blocks the advance"
        );
    }

    #[test]
    fn removing_a_character_unblocks_a_duplicate_member_advance() {
        let s = open_with_zset(&[(b"dup", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "dup");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Backspace)));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(
            editor.active_part(),
            Some(FieldPart::Value),
            "no longer blocked"
        );
    }

    #[test]
    fn typing_a_member_then_a_score_and_ctrl_s_stages_add_zset_member() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "3.5");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::AddZSetMember {
                name,
                member,
                score,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(member, b"beta");
                assert_eq!(*score, 3.5);
            }
            other => panic!("expected a staged AddZSetMember, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "ZADD k NX");
        assert_eq!(
            s.confirm.as_ref().unwrap().guard_text().as_deref(),
            Some("only if the key still exists · never overwrites a member's score")
        );
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::AddZSetMember {
                    key: "k".into(),
                    member: b"beta".to_vec(),
                    score: 3.5,
                },
                index: None,
            }]
        );
    }

    #[test]
    fn an_invalid_score_on_the_add_forms_score_part_blocks_ctrl_s() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "not-a-number");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none(), "⌃S blocked on an invalid score");
        assert!(s.open.as_ref().unwrap().is_editing());
    }

    #[test]
    fn ctrl_s_directly_from_the_member_part_is_blocked_by_the_empty_unparseable_score() {
        // The score part's buffer starts empty, and an empty string is not a
        // valid score — so ⌃S pressed before ever advancing must not stage
        // an AddZSetMember with an invented score.
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, cmds) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(cmds.is_empty());
        assert!(s.confirm.is_none());
        assert!(s.open.as_ref().unwrap().is_editing());
    }

    // ── D8: `d` in the value pane stages `DeleteZSetMember` ────────────────

    #[test]
    fn d_in_the_value_pane_stages_delete_zset_member_and_marks_the_last_member() {
        let s = with_cursor(open_with_zset(&[(b"only", 1.0)], 1), 0);
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(cmds.is_empty());
        match &s.confirm {
            Some(PendingMutation::DeleteZSetMember {
                name,
                member,
                last_member,
            }) => {
                assert_eq!(name, b"k");
                assert_eq!(member, b"only");
                assert!(*last_member);
            }
            other => panic!("expected a staged DeleteZSetMember, got {other:?}"),
        }
        assert_eq!(s.confirm.as_ref().unwrap().command_text(), "ZREM k");
        assert_eq!(s.confirm.as_ref().unwrap().guard_text(), None);
    }

    #[test]
    fn d_with_more_than_one_member_left_is_not_marked_as_the_last() {
        let s = with_cursor(open_with_zset(&[(b"a", 1.0), (b"b", 2.0)], 2), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteZSetMember { last_member, .. }) => assert!(!last_member),
            other => panic!("expected a staged DeleteZSetMember, got {other:?}"),
        }
    }

    #[test]
    fn confirming_a_delete_zset_member_issues_zrem() {
        let s = with_cursor(open_with_zset(&[(b"only", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::DeleteZSetMember {
                    key: "k".into(),
                    member: b"only".to_vec(),
                },
                index: None,
            }]
        );
    }

    // ── Read-only Mode refuses at confirm, never at the keypress ───────────

    #[test]
    fn read_only_refuses_all_three_zset_mutations_at_confirm_not_at_the_keypress() {
        let mutations = [
            PendingMutation::SetZSetScore {
                name: b"k".to_vec().into(),
                member: b"m".to_vec(),
                old_score: 1.0,
                new_score: 2.0,
            },
            PendingMutation::AddZSetMember {
                name: b"k".to_vec().into(),
                member: b"m".to_vec(),
                score: 1.0,
            },
            PendingMutation::DeleteZSetMember {
                name: b"k".to_vec().into(),
                member: b"m".to_vec(),
                last_member: false,
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
    fn read_only_mode_does_not_refuse_e_on_a_zset_only_at_y() {
        let s = State {
            read_only: Some(ReadOnlyReason::Environment),
            ..with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0)
        };
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        assert!(
            s.open.as_ref().unwrap().editor().is_some(),
            "opening is never refused"
        );
        assert!(cmds.is_empty());
        let s = retype_score(s, "9");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        assert!(s.confirm.is_some(), "the preview is composed anyway");
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                Command::Execute {
                    mutation: Mutation::SetZSetScore { .. },
                    ..
                }
            )),
            "but nothing was actually sent to the server"
        );
    }

    // ── `NotWritten::MemberGone`/`MemberExists` hand the buffer back ───────

    #[test]
    fn not_written_member_gone_names_zadd_and_hands_the_score_edit_back() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "9");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetZSetScore {
                    key: "k".into(),
                    member: b"alpha".to_vec(),
                    score: 9.0,
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::MemberGone)),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("ZADD k alpha"), "{text}");
        assert!(text.contains("member no longer exists"), "{text}");
        let open = s.open.as_ref().unwrap();
        assert!(
            open.is_editing(),
            "held under R3.8 — the buffer is open again"
        );
        assert_eq!(open.editor().unwrap().text(), b"9");
    }

    #[test]
    fn not_written_member_exists_names_zadd_nx_and_hands_the_add_form_back() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "5");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::AddZSetMember {
                    key: "k".into(),
                    member: b"beta".to_vec(),
                    score: 5.0,
                },
                index: None,
                result: Ok(MutationOutcome::NotWritten(NotWritten::MemberExists)),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        let (text, _) = s.error.as_ref().unwrap();
        assert!(text.contains("ZADD k NX"), "{text}");
        assert!(text.contains("member already exists"), "{text}");
        assert!(s.open.as_ref().unwrap().is_editing());
    }

    #[test]
    fn not_written_key_gone_tombstones_and_hands_the_score_edit_back() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "9");
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('s'))));
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::SetZSetScore {
                    key: "k".into(),
                    member: b"alpha".to_vec(),
                    score: 9.0,
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
        assert_eq!(open.editor().unwrap().text(), b"9");
    }

    #[test]
    fn key_gone_under_a_staged_set_zset_score_dialog_closes_and_hands_the_buffer_back() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0)], 1), 0);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "9");
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
        assert!(
            s.confirm.is_none(),
            "the compiler-enforced `dialog_up` match in \
             `staged_edit_found_key_gone` must recognize SetZSetScore"
        );
        let open = s.open.as_ref().unwrap();
        assert_eq!(open.deleted_at_ms, Some(7_000));
        assert_eq!(open.editor().unwrap().text(), b"9");
    }

    #[test]
    fn key_gone_under_a_staged_add_zset_member_dialog_closes_and_hands_the_buffer_back() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Enter)));
        let s = type_text(s, "5");
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
        assert_eq!(open.editor().unwrap().field_name(), Some("beta"));
    }

    #[test]
    fn key_gone_under_a_staged_delete_zset_member_dialog_simply_closes() {
        let s = with_cursor(open_with_zset(&[(b"a", 1.0)], 1), 0);
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
    fn a_delete_zset_member_zrem_returning_zero_says_member_not_field() {
        let s = open_with_zset(&[(b"a", 1.0)], 1);
        let (s, cmds) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::DeleteZSetMember {
                    key: "k".into(),
                    member: b"a".to_vec(),
                },
                index: None,
                result: Ok(MutationOutcome::NothingToRemove),
                at_ms: 5_000,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
        assert!(s.error.is_none(), "not an error");
        let (text, _) = s.notice.as_ref().unwrap();
        assert!(text.contains("ZREM k"), "{text}");
        assert!(text.contains("member already gone"), "{text}");
        assert!(!text.contains("field"), "{text}");
    }

    // ── R3.8: a live update arriving while the editor is open is held ──────

    #[test]
    fn an_update_arriving_while_the_zset_score_edit_is_open_is_held() {
        let s = with_cursor(open_with_zset(&[(b"alpha", 1.0), (b"beta", 2.0)], 2), 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let s = retype_score(s, "9");
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: crate::command::ReadToken::default(),
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::ZSet(ScoredValue {
                    entries: vec![(b"alpha".to_vec(), 1.0), (b"changed".to_vec(), 3.0)],
                    total: 2,
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
            b"9",
            "the buffer is never touched (R3.8)"
        );
    }

    #[test]
    fn an_update_arriving_while_the_zset_add_form_is_open_is_held() {
        let s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('a'))));
        let s = type_text(s, "beta");
        assert!(s.open.as_ref().unwrap().is_editing());
        let (s, _) = update(
            s,
            Msg::ValueLoaded {
                token: crate::command::ReadToken::default(),
                index: Some(0),
                name: "k".into(),
                value: crate::state::Value::ZSet(ScoredValue {
                    entries: vec![(b"alpha".to_vec(), 1.0), (b"changed".to_vec(), 3.0)],
                    total: 2,
                }),
                ttl_seconds: -1,
                size_bytes: 10,
                at_ms: 9_000,
            },
        );
        let open = s.open.as_ref().unwrap();
        assert!(open.pending.is_some(), "held, not applied");
        assert_eq!(
            open.editor().unwrap().field_name(),
            Some("beta"),
            "the buffer is never touched (R3.8)"
        );
    }

    // ── D7 (phase-1 note): a row inside the window behaves identically past
    //    the 500-member fetch window ──────────────────────────────────────

    #[test]
    fn a_long_zsets_total_past_the_window_does_not_change_what_e_does_to_a_row_inside_it() {
        let entries: Vec<(Vec<u8>, f64)> = (0..500)
            .map(|i| (format!("member-{i}").into_bytes(), i as f64))
            .collect();
        let refs: Vec<(&[u8], f64)> = entries.iter().map(|(m, s)| (m.as_slice(), *s)).collect();
        let s = with_cursor(open_with_zset(&refs, 12_000), 499);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('e'))));
        let editor = s.open.as_ref().unwrap().editor().unwrap();
        assert_eq!(
            editor.target(),
            &EditTarget::ZSetScore {
                member: b"member-499".to_vec()
            }
        );
        assert_eq!(editor.text(), b"499");
    }

    #[test]
    fn a_long_zsets_total_past_the_window_does_not_change_what_d_does_to_a_row_inside_it() {
        let entries: Vec<(Vec<u8>, f64)> = (0..500)
            .map(|i| (format!("member-{i}").into_bytes(), i as f64))
            .collect();
        let refs: Vec<(&[u8], f64)> = entries.iter().map(|(m, s)| (m.as_slice(), *s)).collect();
        let s = with_cursor(open_with_zset(&refs, 12_000), 250);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        match &s.confirm {
            Some(PendingMutation::DeleteZSetMember {
                member,
                last_member,
                ..
            }) => {
                assert_eq!(member, b"member-250");
                assert!(
                    !last_member,
                    "12,000 members total — nowhere near the set's last member"
                );
            }
            other => panic!("expected a staged DeleteZSetMember, got {other:?}"),
        }
    }
}
