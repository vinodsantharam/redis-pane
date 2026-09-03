//! The single entry point into the core (PLAN M0.4).

use crate::msg::{KeyCode, KeyPress};
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
        Msg::Quit => quit(state),
    }
}

/// Provisional bindings. Keybindings are data (R7.5) and this becomes a lookup
/// against the keymap in M0.12; until then the two that must always work are
/// wired directly so the app is never unquittable.
fn key_press(state: State, key: KeyPress) -> (State, Vec<Command>) {
    if key.is_char('q') || (key.ctrl && key.code == KeyCode::Char('c')) {
        return quit(state);
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
