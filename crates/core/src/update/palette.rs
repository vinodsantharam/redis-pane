//! The command palette (`Ctrl-K`, PLAN M3 task 1): open, type-to-filter,
//! move the selection, dispatch, dismiss.
//!
//! `Enter` here calls [`super::dispatch_action`] — the same function
//! `key_press` calls for a direct keypress — rather than reimplementing what
//! any `Action` does. That is the whole point of the task: a Palette
//! selection and the bound key for the same `Action` can never disagree,
//! because they are, at the last step, the same call.

use super::*;
use crate::state::PaletteState;

/// `Ctrl-K` from Normal mode: open the Palette with every action listed and
/// nothing typed yet.
///
/// Reachable only from `Mode::Normal`, exactly like every other keymap
/// action (`Ctrl-R`, `?`, …) — `mode()` gives Confirm, Editing and Filtering
/// each full ownership of the keystroke stream while they are active, and
/// the Palette follows that same precedent rather than special-casing itself
/// an exception to reach a mid-edit or mid-filter keypress.
pub(super) fn open_palette(mut state: State) -> (State, Vec<Command>) {
    state.palette = Some(PaletteState::new());
    (state, Vec::new())
}

/// Every keypress while `Mode::Palette` is in force (`mode()` in
/// `update/mod.rs`): typing narrows the list, `↑↓` moves the selection,
/// `Enter` dispatches the highlighted `Action` and closes the Palette, `Esc`
/// closes it without dispatching anything.
pub(super) fn palette_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    match key.code {
        KeyCode::Esc => {
            state.palette = None;
            (state, Vec::new())
        }
        KeyCode::Enter => {
            // Taken, not borrowed: the Palette closes the moment `Enter` is
            // pressed, whether or not anything was highlighted to dispatch —
            // the same "closed before the effect runs" shape `confirm_key`
            // uses for `y`.
            let Some(palette) = state.palette.take() else {
                return (state, Vec::new());
            };
            let Some(action) = palette.highlighted() else {
                // Nothing matched the query — `Enter` on an empty list has
                // nothing to run.
                return (state, Vec::new());
            };
            // The same gate `key_press` applies to a direct keypress (PLAN
            // M3 row 1): a Palette entry for an action whose pane is not on
            // screen right now must do nothing, not act on something the
            // reader cannot see.
            if !action.pane_is_on_screen(&state) {
                return (state, Vec::new());
            }
            dispatch_action(state, action)
        }
        KeyCode::Up => {
            if let Some(palette) = &mut state.palette {
                palette.move_selection(-1);
            }
            (state, Vec::new())
        }
        KeyCode::Down => {
            if let Some(palette) = &mut state.palette {
                palette.move_selection(1);
            }
            (state, Vec::new())
        }
        KeyCode::Backspace => {
            if let Some(palette) = &mut state.palette {
                palette.query.pop();
                palette.refresh();
            }
            (state, Vec::new())
        }
        KeyCode::Char(c) if !key.ctrl && !key.alt => {
            if let Some(palette) = &mut state.palette {
                palette.query.push(c);
                palette.refresh();
            }
            (state, Vec::new())
        }
        _ => (state, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Action;
    use crate::msg::KeyCode;

    fn press(state: State, code: KeyCode) -> (State, Vec<Command>) {
        update(state, Msg::Key(KeyPress::plain(code)))
    }

    #[test]
    fn ctrl_k_opens_the_palette_with_every_action_listed() {
        let (state, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let palette = state.palette.expect("Ctrl-K opens the Palette");
        assert_eq!(palette.matches, crate::keymap::ALL_ACTIONS.to_vec());
        assert_eq!(palette.query, "");
        assert!(cmds.is_empty(), "opening is a plain state change, no I/O");
    }

    #[test]
    fn esc_closes_the_palette_without_dispatching_anything() {
        let (state, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let (state, cmds) = press(state, KeyCode::Esc);
        assert!(state.palette.is_none());
        assert!(cmds.is_empty());
        // Esc must not have quit the app or toggled anything else — the
        // Palette owned that keystroke exclusively.
        assert!(!state.quitting);
    }

    #[test]
    fn typing_narrows_the_match_list() {
        let (state, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let (state, _) = press(state, KeyCode::Char('q'));
        let (state, _) = press(state, KeyCode::Char('u'));
        let (state, _) = press(state, KeyCode::Char('i'));
        let (state, _) = press(state, KeyCode::Char('t'));
        let palette = state.palette.unwrap();
        assert_eq!(palette.query, "quit");
        assert_eq!(palette.matches, vec![Action::Quit]);
    }

    #[test]
    fn backspace_widens_the_match_list_back_out() {
        let (state, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let (state, _) = press(state, KeyCode::Char('q'));
        let (state, _) = press(state, KeyCode::Backspace);
        let palette = state.palette.unwrap();
        assert_eq!(palette.query, "");
        assert_eq!(palette.matches, crate::keymap::ALL_ACTIONS.to_vec());
    }

    #[test]
    fn up_and_down_move_the_selection() {
        let (state, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let (state, _) = press(state, KeyCode::Down);
        assert_eq!(state.palette.as_ref().unwrap().selected, 1);
        let (state, _) = press(state, KeyCode::Up);
        assert_eq!(state.palette.as_ref().unwrap().selected, 0);
    }

    /// The test this task exists to pass (PLAN M3 row 1's "Proves"): the
    /// Palette is not a second implementation of what an `Action` does —
    /// fuzzy-matching down to `ToggleReadOnly` and pressing `Enter` must
    /// produce *exactly* what pressing its bound key, `Ctrl-R`, produces,
    /// both starting from the same state and both resolved through the same
    /// keymap.
    #[test]
    fn palette_enter_matches_the_direct_keypress_exactly() {
        let start = State::default();

        let direct = update(start.clone(), Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))));

        let (opened, _) = update(start, Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))));
        let (typed, _) = press(opened, KeyCode::Char('r'));
        let (typed, _) = press(typed, KeyCode::Char('e'));
        let (typed, _) = press(typed, KeyCode::Char('a'));
        let (typed, _) = press(typed, KeyCode::Char('d'));
        let (typed, _) = press(typed, KeyCode::Char('o'));
        let (typed, _) = press(typed, KeyCode::Char('n'));
        let (typed, _) = press(typed, KeyCode::Char('l'));
        let (typed, _) = press(typed, KeyCode::Char('y'));
        assert_eq!(
            typed.palette.as_ref().unwrap().highlighted(),
            Some(Action::ToggleReadOnly),
            "\"readonly\" must fuzzy-match down to ToggleReadOnly"
        );
        let via_palette = press(typed, KeyCode::Enter);

        assert_eq!(
            via_palette, direct,
            "the Palette's Enter must produce exactly what Ctrl-R produces"
        );
    }

    #[test]
    fn enter_on_an_empty_match_list_does_nothing() {
        let (state, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let (state, _) = press(state, KeyCode::Char('z'));
        let (state, _) = press(state, KeyCode::Char('z'));
        let (state, _) = press(state, KeyCode::Char('z'));
        let (state, _) = press(state, KeyCode::Char('z'));
        assert!(state.palette.as_ref().unwrap().matches.is_empty());
        let (state, cmds) = press(state, KeyCode::Enter);
        assert!(
            state.palette.is_none(),
            "Enter still closes the Palette even with nothing to run"
        );
        assert!(cmds.is_empty());
    }

    #[test]
    fn quit_from_the_palette_actually_quits() {
        let (state, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('k'))),
        );
        let (state, _) = press(state, KeyCode::Char('q'));
        let (state, _) = press(state, KeyCode::Char('u'));
        let (state, _) = press(state, KeyCode::Char('i'));
        let (state, _) = press(state, KeyCode::Char('t'));
        let (state, cmds) = press(state, KeyCode::Enter);
        assert!(state.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
        assert!(state.palette.is_none());
    }
}
