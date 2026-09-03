//! Keybindings as data (R7.5, PLAN M0.12).
//!
//! The keymap, the command palette and the on-screen hint bar all read from
//! this one source, so a hint always shows the **effective** binding after user
//! overrides. A hint bar that hard-codes its own labels is a hint bar that lies
//! to anyone who has remapped a key.

use crate::msg::{KeyCode, KeyPress};

/// Something the user can ask for. Actions are named for what they do, not for
/// the key that happens to invoke them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Action {
    Quit,
    /// Re-read the open key. A scoped Refetch, never a global refresh — there
    /// is no refresh button, because there is nothing to refresh (ADR-0006).
    Refetch,
    /// Lift or impose Read-only Mode. Refused when the reason is `replica`.
    ToggleReadOnly,
    Help,
    /// Back out of whatever is open; cancel an in-flight operation.
    Cancel,
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    /// Start capturing a filter pattern.
    Filter,
    /// Cycle the sort column.
    Sort,
    /// Fold the list on the separator, or unfold it.
    ToggleTree,
    /// Expand or collapse the group under the cursor.
    ToggleGroup,
    /// Open the selected key in the Viewer.
    Open,
    /// Move down inside the open value.
    ViewerDown,
    /// Move up inside the open value.
    ViewerUp,
    /// Page down inside the open value.
    ViewerPageDown,
    /// Page up inside the open value.
    ViewerPageUp,
    /// Jump to the top of the open value.
    ViewerTop,
    /// Jump to the bottom of the open value.
    ViewerBottom,
    /// Begin a copy. The next key chooses what (R3.5).
    Copy,
}

impl Action {
    /// The label used in the hint bar and the help overlay.
    pub fn label(&self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Refetch => "refetch",
            Action::ToggleReadOnly => "read-only",
            Action::Help => "help",
            Action::Cancel => "back",
            Action::MoveUp | Action::MoveDown => "move",
            Action::PageUp | Action::PageDown => "page",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::Filter => "filter",
            Action::Sort => "sort",
            Action::ToggleTree => "tree",
            Action::ToggleGroup => "fold",
            Action::Open => "open",
            Action::ViewerDown | Action::ViewerUp => "scroll",
            Action::ViewerPageDown | Action::ViewerPageUp => "page value",
            Action::ViewerTop => "value top",
            Action::ViewerBottom => "value bottom",
            Action::Copy => "copy",
        }
    }
}

/// One key bound to one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub key: KeyPress,
    pub action: Action,
}

/// How a key is written on screen. Kept next to the keymap so the hint bar and
/// the help overlay cannot disagree about how to spell a chord.
pub fn key_label(key: &KeyPress) -> String {
    let base = match key.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "⏎".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "⌫".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
    };
    match (key.ctrl, key.alt) {
        (true, _) => format!("⌃{}", base.to_uppercase()),
        (false, true) => format!("⌥{base}"),
        (false, false) => base,
    }
}

/// The bindings in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: vec![
                Binding {
                    key: KeyPress::plain(KeyCode::Char('q')),
                    action: Action::Quit,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Char('c')),
                    action: Action::Quit,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('r')),
                    action: Action::Refetch,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Char('r')),
                    action: Action::ToggleReadOnly,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('?')),
                    action: Action::Help,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Esc),
                    action: Action::Cancel,
                },
                // Vim keys and arrow keys both work, always (DESIGN principle 7).
                Binding {
                    key: KeyPress::plain(KeyCode::Down),
                    action: Action::MoveDown,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('j')),
                    action: Action::MoveDown,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Up),
                    action: Action::MoveUp,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('k')),
                    action: Action::MoveUp,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::PageDown),
                    action: Action::PageDown,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::PageUp),
                    action: Action::PageUp,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Home),
                    action: Action::Top,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::End),
                    action: Action::Bottom,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('/')),
                    action: Action::Filter,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('s')),
                    action: Action::Sort,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('t')),
                    action: Action::ToggleTree,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Enter),
                    action: Action::ToggleGroup,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Right),
                    action: Action::Open,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('l')),
                    action: Action::Open,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Down),
                    action: Action::ViewerDown,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Up),
                    action: Action::ViewerUp,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::PageDown),
                    action: Action::ViewerPageDown,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::PageUp),
                    action: Action::ViewerPageUp,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Home),
                    action: Action::ViewerTop,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::End),
                    action: Action::ViewerBottom,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('y')),
                    action: Action::Copy,
                },
            ],
        }
    }
}

impl Keymap {
    /// What this key does, if anything.
    pub fn action_for(&self, key: &KeyPress) -> Option<Action> {
        self.bindings
            .iter()
            .find(|b| b.key == *key)
            .map(|b| b.action)
    }

    /// The key that currently invokes an action — the *effective* binding, which
    /// is what any hint must show.
    ///
    /// Where several keys invoke the same action, the first wins, so a user
    /// override placed at the front becomes the one displayed.
    pub fn key_for(&self, action: Action) -> Option<KeyPress> {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.key)
    }

    /// How to spell the effective binding for an action.
    pub fn hint(&self, action: Action) -> Option<String> {
        self.key_for(action).map(|k| key_label(&k))
    }

    /// Rebind an action. The override takes precedence over the default,
    /// including for hints — which is the whole point of bindings being data.
    pub fn bind(&mut self, action: Action, key: KeyPress) {
        self.bindings.retain(|b| b.key != key);
        self.bindings.insert(0, Binding { key, action });
    }

    /// Every binding, for the help overlay.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_resolve() {
        let k = Keymap::default();
        assert_eq!(
            k.action_for(&KeyPress::plain(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
        assert_eq!(
            k.action_for(&KeyPress::ctrl(KeyCode::Char('c'))),
            Some(Action::Quit)
        );
        assert_eq!(
            k.action_for(&KeyPress::plain(KeyCode::Char('r'))),
            Some(Action::Refetch)
        );
        assert_eq!(
            k.action_for(&KeyPress::ctrl(KeyCode::Char('r'))),
            Some(Action::ToggleReadOnly)
        );
        assert_eq!(k.action_for(&KeyPress::plain(KeyCode::Char('z'))), None);
    }

    #[test]
    fn ctrl_r_and_r_are_different_actions() {
        // Refetch is scoped; Read-only Mode is a safety control. Conflating
        // them would be a bad afternoon for somebody.
        let k = Keymap::default();
        assert_ne!(
            k.action_for(&KeyPress::plain(KeyCode::Char('r'))),
            k.action_for(&KeyPress::ctrl(KeyCode::Char('r')))
        );
    }

    #[test]
    fn chords_are_spelled_consistently() {
        assert_eq!(key_label(&KeyPress::plain(KeyCode::Char('q'))), "q");
        assert_eq!(key_label(&KeyPress::ctrl(KeyCode::Char('c'))), "⌃C");
        assert_eq!(key_label(&KeyPress::plain(KeyCode::Esc)), "Esc");
    }

    /// R7.5's proof: a hint shows the *effective* binding, not the default.
    #[test]
    fn an_override_changes_the_hint() {
        let mut k = Keymap::default();
        assert_eq!(k.hint(Action::Quit).as_deref(), Some("q"));

        k.bind(Action::Quit, KeyPress::ctrl(KeyCode::Char('x')));

        assert_eq!(
            k.hint(Action::Quit).as_deref(),
            Some("⌃X"),
            "the hint must follow the binding"
        );
        assert_eq!(
            k.action_for(&KeyPress::ctrl(KeyCode::Char('x'))),
            Some(Action::Quit)
        );
    }

    #[test]
    fn rebinding_a_key_that_was_taken_removes_the_old_meaning() {
        let mut k = Keymap::default();
        k.bind(Action::Help, KeyPress::plain(KeyCode::Char('q')));
        assert_eq!(
            k.action_for(&KeyPress::plain(KeyCode::Char('q'))),
            Some(Action::Help)
        );
        // `q` no longer quits, and the quit hint must say so rather than lie.
        assert_ne!(k.hint(Action::Quit).as_deref(), Some("q"));
    }

    #[test]
    fn every_action_has_a_default_binding() {
        let k = Keymap::default();
        for action in [
            Action::Quit,
            Action::Refetch,
            Action::ToggleReadOnly,
            Action::Help,
            Action::Cancel,
            Action::MoveUp,
            Action::MoveDown,
            Action::PageUp,
            Action::PageDown,
            Action::Top,
            Action::Bottom,
            Action::Filter,
            Action::Sort,
            Action::ToggleTree,
            Action::ToggleGroup,
            Action::Open,
            Action::ViewerDown,
            Action::ViewerUp,
            Action::ViewerPageDown,
            Action::ViewerPageUp,
            Action::ViewerTop,
            Action::ViewerBottom,
            Action::Copy,
        ] {
            assert!(k.key_for(action).is_some(), "{action:?} has no binding");
        }
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;

    #[test]
    fn vim_keys_and_arrow_keys_both_work_always() {
        // DESIGN principle 7: familiar to two tribes, no modal purity tests.
        let k = Keymap::default();
        for (key, action) in [
            (KeyCode::Char('j'), Action::MoveDown),
            (KeyCode::Down, Action::MoveDown),
            (KeyCode::Char('k'), Action::MoveUp),
            (KeyCode::Up, Action::MoveUp),
        ] {
            assert_eq!(k.action_for(&KeyPress::plain(key)), Some(action));
        }
    }

    #[test]
    fn the_hint_prefers_the_arrow_key_which_needs_no_explaining() {
        assert_eq!(
            Keymap::default().hint(Action::MoveDown).as_deref(),
            Some("↓")
        );
    }
}
