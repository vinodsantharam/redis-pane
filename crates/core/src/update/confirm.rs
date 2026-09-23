//! The mutation preview: confirming or dismissing a staged
//! [`PendingMutation`], and giving meaning to what the server said once it
//! settles (review H1).

use super::*;

/// Keys read while a mutation preview is on screen. Only `y` confirms and
/// only Esc dismisses; every other key is ignored rather than discarding, so
/// a stray or leaked keystroke can never silently throw away a staged
/// mutation (ADR-0014) — spelled out here rather than resolved through the
/// keymap because a confirm dialog, like filter capture, is a mode of its
/// own rather than an ordinary action (R4.4, R4.6).
pub(super) fn confirm_key(
    mut state: State,
    pending: PendingMutation,
    key: KeyPress,
) -> (State, Vec<Command>) {
    match key.code {
        KeyCode::Char('y') if !key.ctrl && !key.alt => {
            // Read-only Mode refuses here, at confirm, not at the keypress
            // that staged the preview — the reader has already seen the real
            // command and its blast radius by the time this fires
            // (DESIGN §6.5).
            if let Some(reason) = state.read_only {
                clear_editing(&mut state);
                let notice = format!("read-only ({}): not executed", reason.label());
                return (state, vec![Command::Notify { text: notice }]);
            }
            // A confirmed `SetString` stays `editing` on purpose: R3.8's
            // guard needs to hold until the write settles (`write_landed`
            // clears it), not just until the dialog closes — otherwise
            // this very `SET`'s own Refetch would find `editing` still true
            // and hold its own reply instead of applying it. Delete never
            // sets `editing` in the first place, so this is a no-op for it.
            (state, vec![pending.into_command()])
        }
        KeyCode::Esc => {
            // A deliberate full discard, never a return to the editor —
            // consistent with every other Esc in the app, at the cost of
            // losing a draft to a misplaced keypress.
            clear_editing(&mut state);
            (state, Vec::new())
        }
        _ => {
            // Not the dialog's vocabulary. `key_press` already `take()`s
            // `state.confirm` before calling here, so it must go back rather
            // than vanish — this is the fix for the vim OSC 10/11
            // colour-query leak (PLAN M2 task 4 rework): a `y` swallowed by
            // this branch used to discard the preview silently.
            state.confirm = Some(pending);
            (state, Vec::new())
        }
    }
}

/// `Msg::MutationSettled`: the one place a write's outcome is given meaning
/// (review H1). The shell only reports what the server said.
pub(super) fn mutation_settled(
    state: State,
    mutation: Mutation,
    index: Option<usize>,
    result: Result<MutationOutcome, String>,
    at_ms: u64,
) -> (State, Vec<Command>) {
    match result {
        Err(detail) => failed(state, mutation.command_label(), detail, at_ms),
        Ok(MutationOutcome::Done) => match mutation {
            Mutation::DeleteKey { key } => key_deleted(state, index, key, at_ms),
            written => write_landed(state, written.key()),
        },
        Ok(MutationOutcome::NotWritten(why)) => not_written(state, &mutation, why, at_ms),
        Ok(MutationOutcome::NothingToRemove) => nothing_to_remove(state, &mutation, at_ms),
    }
}

/// A delete completed: the key is gone, whether it still existed at the
/// moment `DEL` ran or was already gone by then (R4.3).
///
/// Exactly the "gone" state ADR-0006 already has words and a badge for — the
/// row `set_gone`, and if this was the Open key, its last read value stays on
/// screen, tombstoned, never silently cleared. Guarded by name for the same
/// reason `ValueGone` is: a rescan between staging and confirming renumbers
/// the Loaded set, and an unguarded index would then tombstone an unrelated
/// key.
pub(super) fn key_deleted(
    mut state: State,
    index: Option<usize>,
    name: KeyName,
    at_ms: u64,
) -> (State, Vec<Command>) {
    if let Some(index) = index
        && state.keys.name(index) == Some(name.as_bytes())
    {
        state.keys.set_gone(index);
    }
    match &mut state.open {
        Some(open) if open.name == name => {
            open.deleted_at_ms = Some(at_ms);
            open.pending = None;
            staged_edit_found_key_gone(&mut state, &name, at_ms);
        }
        _ => {}
    }
    state.notice = Some((format!("deleted {name}"), at_ms));
    (state, Vec::new())
}

/// A write landed (R4.1).
///
/// What follows is the same Refetch every other change to the open key goes
/// through: the reply is what reaches the Viewer, never the bytes this session
/// already knew it sent (ADR-0006: no value cache, not even a
/// one-message-long one). Guarded by the key: the reader may have moved on to
/// a different key by the time this lands.
pub(super) fn write_landed(mut state: State, key: &KeyName) -> (State, Vec<Command>) {
    if !state.open.as_ref().is_some_and(|o| o.name == *key) {
        return (state, Vec::new());
    }
    // Clear *before* minting the Refetch, not after its reply lands —
    // `refetch`'s own `Command::ReadKey` is answered by `Msg::ValueLoaded`,
    // which applies immediately only if `may_apply()` is already true by the
    // time it arrives.
    if let Some(open) = state.open.as_mut() {
        open.write_landed();
    }
    let commands = refetch(&mut state);
    if let Some(pending) = &mut state.open_pending {
        pending.own_write = true;
    }
    (state, commands)
}

/// `HDEL`/`SREM` found the field or member already gone (PLAN M2 task 6, D1,
/// D4; PLAN M2 task 7, D5, ADR-0016).
///
/// Not an error, and not a refusal: the command did exactly what was asked
/// and found nothing to remove, and there is no buffer to hand anything back
/// to — `Delete` never opens one. Reported as a notice, then a Refetch, the
/// same way every other change to the open key is (ADR-0006). Which noun the
/// notice uses is decided here, once, from the `Mutation` itself — the same
/// discipline `NotWritten::reason` uses to keep a member from being reported
/// in a field's words (CLAUDE.md's glossary distinction).
pub(super) fn nothing_to_remove(
    mut state: State,
    mutation: &Mutation,
    at_ms: u64,
) -> (State, Vec<Command>) {
    if !state
        .open
        .as_ref()
        .is_some_and(|o| o.name == *mutation.key())
    {
        return (state, Vec::new());
    }
    // Exhaustive over `Mutation`, not a wildcard fallback (PLAN M2 task 8,
    // D8): only `HDEL`/`SREM` ever settle with `NothingToRemove` today — the
    // shell's `execute` (`crates/app/src/redis/mutate.rs`) maps every List
    // write's "not written" case onto `NotWritten::ElementMoved`/`KeyGone`
    // instead, since ADR-0017 D2's compare-and-set makes "already gone"
    // indistinguishable from "moved" for an index-addressed element. Naming
    // every variant here, rather than `_ => "field"`, means a future write
    // that starts reporting `NothingToRemove` (a ZSet member removed twice,
    // task 9) has to pick its own noun instead of silently inheriting
    // "field".
    let what = match mutation {
        Mutation::DeleteHashField { .. } => "field",
        Mutation::DeleteSetMember { .. } => "member",
        // Unreachable today — ADR-0017 D2's compare-and-set reports a missing
        // element as `ElementMoved`, never `NothingToRemove` — but it gets the
        // noun a List actually uses rather than sharing the catch-all below.
        // Costing nothing now is the point: if a later write does start
        // settling this way, it already says "element".
        Mutation::DeleteListElement { .. } => "element",
        // Reachable, unlike the List arm above: ADR-0018 D2's remove is a
        // plain `ZREM`, with `0` removed reported as `NothingToRemove`
        // exactly as `HDEL`'s and `SREM`'s are — a member, not a field
        // (CLAUDE.md's glossary), the same reason `NotWritten::MemberGone`
        // gets its own wording instead of borrowing `FieldGone`'s.
        Mutation::DeleteZSetMember { .. } => "member",
        Mutation::DeleteKey { .. }
        | Mutation::SetString { .. }
        | Mutation::SetHashField { .. }
        | Mutation::AddHashField { .. }
        | Mutation::AddSetMember { .. }
        | Mutation::SetListElement { .. }
        | Mutation::AddListElement { .. }
        // Never settles this way — a score edit refuses via
        // `NotWritten::MemberGone`/`KeyGone`, and an add via
        // `NotWritten::MemberExists`/`KeyGone` (ADR-0018 D2) — but the
        // match stays exhaustive over `Mutation`, not a wildcard (PLAN M2
        // task 8, D8).
        | Mutation::SetZSetScore { .. }
        | Mutation::AddZSetMember { .. } => "entry",
    };
    state.notice = Some((
        format!("{}: {what} already gone", mutation.command_label()),
        at_ms,
    ));
    let commands = refetch(&mut state);
    (state, commands)
}

/// A guarded write's precondition was no longer true by the time it reached
/// the server (PLAN M2 task 6, D1, ADR-0014, ADR-0015).
///
/// `KeyGone` is exactly the String path's old behaviour: tombstone the key,
/// as surely as a read saying so would, and hand the edited text back to the
/// buffer since no read can recover it. `FieldGone`/`FieldExists` are not a
/// tombstone — the key is still there — so instead of ending the edit this
/// hands the buffer back and re-reads: R3.8 holds the reply, because the
/// buffer is open again by the time it lands.
pub(super) fn not_written(
    mut state: State,
    mutation: &Mutation,
    why: NotWritten,
    at_ms: u64,
) -> (State, Vec<Command>) {
    let name = mutation.key();
    if !state.open.as_ref().is_some_and(|o| o.name == *name) {
        return (state, Vec::new());
    }
    // The mutation names its own command, so the error cannot disagree with
    // what was actually sent (review H1).
    let command = mutation.command_label();
    match why {
        NotWritten::KeyGone => {
            let open = state.open.as_mut().expect("checked above");
            open.deleted_at_ms = Some(at_ms);
            open.pending = None;
            let kept = open.editor().is_some();
            if kept {
                open.unstage_buffer();
            } else {
                open.end_edit();
            }
            if let Some(index) = open.index
                && state.keys.name(index) == Some(name.as_bytes())
            {
                state.keys.set_gone(index);
            }
            let tail = if kept { ", edit kept" } else { "" };
            state.error = Some((
                format!("{command}: {} — nothing written{tail}", why.reason()),
                at_ms,
            ));
            (state, Vec::new())
        }
        NotWritten::FieldGone
        | NotWritten::FieldExists
        | NotWritten::MemberExists
        | NotWritten::ElementMoved
        | NotWritten::MemberGone => {
            if let Some(open) = state.open.as_mut() {
                open.unstage_buffer();
            }
            // The guards that refuse without the key going anywhere: the
            // write is dropped, the buffer comes back, and the Viewer
            // re-reads. `MemberExists` cannot arrive here until `a` on a Set
            // is wired (PLAN M2 task 7 phase 3); `ElementMoved` cannot arrive
            // until `e`/`a`/`d` on a List are wired (PLAN M2 task 8 phase 3,
            // ADR-0017); `MemberGone` cannot arrive until `e` on a ZSet is
            // wired (PLAN M2 task 9 phase 3, ADR-0018) — every arm is here
            // because `NotWritten` is matched exhaustively.
            state.error = Some((
                format!("{command}: {} — nothing written, edit kept", why.reason()),
                at_ms,
            ));
            let commands = refetch(&mut state);
            (state, commands)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::KeyCode;

    fn state_with_one_key() -> State {
        let (s, _) = update(
            State::default(),
            Msg::ScanBatch {
                keys: vec![b"k:0".to_vec()],
            },
        );
        s
    }

    #[test]
    fn delete_stages_a_preview_rather_than_executing_immediately() {
        let s = state_with_one_key();
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(cmds.is_empty(), "nothing runs before it is confirmed");
        match s.confirm {
            Some(PendingMutation::DeleteKey { index, ref name }) => {
                assert_eq!(index, 0);
                assert_eq!(name, b"k:0");
            }
            other => panic!("expected a staged delete, got {other:?}"),
        }
    }

    #[test]
    fn confirming_a_staged_delete_issues_del() {
        let s = state_with_one_key();
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(s.confirm.is_none(), "the dialog closes on confirm");
        assert_eq!(
            cmds,
            vec![Command::Execute {
                mutation: Mutation::DeleteKey { key: "k:0".into() },
                index: Some(0),
            }]
        );
    }

    #[test]
    fn esc_dismisses_a_staged_delete_without_running_it() {
        let s = state_with_one_key();
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(s.confirm.is_none());
        assert!(cmds.is_empty());
    }

    /// R4.4/DESIGN §6.5: the preview is composed first regardless of
    /// Read-only Mode — refusal happens only at the confirm keypress, so the
    /// reader always sees the real command before learning they cannot run
    /// it.
    #[test]
    fn read_only_mode_refuses_at_confirm_not_at_the_keypress() {
        let s = State {
            read_only: Some(ReadOnlyReason::Environment),
            ..state_with_one_key()
        };
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(s.confirm.is_some(), "the preview is composed anyway");
        assert!(cmds.is_empty());

        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('y'))));
        assert!(s.confirm.is_none(), "the dialog still closes");
        assert!(
            !cmds.iter().any(|c| matches!(
                c,
                Command::Execute {
                    mutation: Mutation::DeleteKey { .. },
                    ..
                }
            )),
            "but nothing was actually sent to the server"
        );
    }

    #[test]
    fn a_completed_delete_badges_the_row_gone() {
        let s = state_with_one_key();
        assert!(!s.keys.is_gone(0));
        let (s, _) = update(
            s,
            Msg::MutationSettled {
                mutation: Mutation::DeleteKey { key: "k:0".into() },
                index: Some(0),
                result: Ok(MutationOutcome::Done),
                at_ms: 1_000,
            },
        );
        assert!(s.keys.is_gone(0));
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
    fn d_in_the_keys_pane_stays_delete_key_not_hdel_with_a_hash_open() {
        let mut s = open_with_hash_for_gating();
        s.focus = Pane::Keys;
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Char('d'))));
        assert!(matches!(s.confirm, Some(PendingMutation::DeleteKey { .. })));
    }
}
