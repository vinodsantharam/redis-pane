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
    /// Collapse the group under the cursor, matching the standard treeview
    /// Left-arrow behavior (VS Code, macOS/Windows outline views, the
    /// WAI-ARIA treeview pattern): an already-collapsed group moves the
    /// cursor to its parent instead — the only thing left for Left to do —
    /// and a key row (which has no children of its own) does the same. Right
    /// (`Action::Open`) is the only key that ever expands; this one never
    /// does, so folding and moving up the tree are always unambiguous.
    CollapseGroup,
    /// Right-arrow behavior on the selected row: opens a key in the Viewer;
    /// on a group, expands it if collapsed, or — matching the same standard
    /// this key follows everywhere else — steps into its first child if it
    /// is already expanded, since Right never collapses (`CollapseGroup` is
    /// the only key that does).
    Open,
    /// Start moving a cursor inside the open value. No-op with nothing open.
    /// Deliberately a separate, explicit action from `Tab`/focus — merely
    /// looking at the value pane must never silently reprogram what plain
    /// movement does; `Enter` is the one deliberate key that does.
    EnterValueCursor,
    /// Copy the key name or the value to the clipboard, whichever pane is
    /// focused (R3.5, DESIGN §4) — no mnemonic, no chord.
    Copy,
    /// Copy a ready-to-paste `redis-cli` command for the open key (R3.5).
    /// Not focus-dependent: there is only one sensible target.
    CopyCommand,
    /// Move focus between the keys pane and the Viewer (DESIGN §4).
    ///
    /// With two panes this is the only way to say which one a pane-scoped key
    /// acts on; below 70 columns, where one pane is drawn at a time, it is the
    /// same movement as `Open`/`Esc` and so changes what is on screen.
    CyclePane,
    /// Nudge the divider toward the Viewer, widening the keys pane
    /// (DESIGN §2: "the split is resizable").
    WidenKeysPane,
    /// Nudge the divider toward the keys pane, widening the Viewer.
    NarrowKeysPane,
    /// Stage the selected key's delete for confirmation — never executes by
    /// itself (R4.3, R4.6). Keys-pane-scoped: it acts on the Selected key,
    /// not the Open key, the same target every other list-scoped action
    /// takes.
    Delete,
    /// Confirm whatever mutation is currently staged and run it, or say why
    /// not if Read-only Mode refuses it. No-op with nothing staged.
    ConfirmMutation,
    /// Stage an edit of the Open value's whole body in `$EDITOR` (R3.2, R4.1).
    /// Acts on focus alone, like `Copy` — no cursor-mode prerequisite, since a
    /// String has exactly one thing to edit, not rows to navigate to first.
    Edit,
}

impl Action {
    /// Whether the pane this action operates on is currently drawn.
    ///
    /// Actions divide cleanly: some move or reshape the key list, some
    /// belong to the app rather than to either pane, and the six movement
    /// actions move *whichever* of the two the reader is currently working
    /// in. Only below 70 columns, where one pane is on screen at a time, can
    /// an action be aimed at something the reader cannot see.
    pub fn pane_is_on_screen(&self, state: &crate::State) -> bool {
        use crate::render::layout::Pane;
        match self {
            // Movement acts on the value cursor while one is active — which
            // only happens with a key open, and opening one already moves
            // focus onto it — and the key list otherwise.
            Action::MoveUp
            | Action::MoveDown
            | Action::PageUp
            | Action::PageDown
            | Action::Top
            | Action::Bottom => {
                if state.open.as_ref().is_some_and(|o| o.cursor_active) {
                    state.pane_visible(Pane::Value)
                } else {
                    state.pane_visible(Pane::Keys)
                }
            }
            // The rest of the key list: reshaping it, or opening from it.
            Action::Filter
            | Action::Sort
            | Action::ToggleTree
            | Action::CollapseGroup
            | Action::Open
            | Action::Delete => state.pane_visible(Pane::Keys),
            // Edits the Open value's body — meaningless without the value
            // pane on screen to hold it.
            Action::Edit => state.pane_visible(Pane::Value),
            // Everything else is the app's, not a pane's: quitting, help, Esc,
            // `Tab` (which is what *changes* which pane is on screen), `r`
            // (already pane-scoped by R2.7 on its own terms), `Enter`
            // (no-ops itself with nothing open to enter), copying, and the
            // read-only toggle.
            _ => true,
        }
    }

    /// The label used in the help overlay, where every binding is listed at
    /// once and no pane is focused. See [`Action::label_in`] for the hint bar,
    /// which describes what the key will do right now.
    pub fn label(&self) -> &'static str {
        match self {
            Action::Quit => "quit",
            // Both halves, because the help overlay is the one place that has
            // to explain the whole of R2.7 rather than the half in force.
            Action::Refetch => "refetch / rescan",
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
            Action::CollapseGroup => "collapse / parent",
            Action::Open => "open / expand",
            Action::EnterValueCursor => "open / move in value",
            Action::Copy => "copy",
            Action::CopyCommand => "copy redis-cli command",
            Action::CyclePane => "focus",
            Action::WidenKeysPane => "widen keys",
            Action::NarrowKeysPane => "narrow keys",
            Action::Delete => "delete",
            Action::ConfirmMutation => "confirm",
            Action::Edit => "edit",
        }
    }

    /// The label for the hint bar, which describes what the key does *now*.
    ///
    /// `Refetch` differs in two ways: it acts on the focused pane (R2.7), so a
    /// bar that always read "refetch" would name the wrong half of it half the
    /// time; and disconnected, there is nothing to refetch or rescan — `r`
    /// retries the connection instead (ADR-0009), so it must say so rather
    /// than naming an action that would just error against a dead client.
    /// Takes the answer rather than a `&State` so the keymap stays free of the
    /// rest of the core, and so this is trivially testable all three ways.
    pub fn label_in(&self, keys_pane_focused: bool, disconnected: bool) -> &'static str {
        match self {
            Action::Refetch if disconnected => "reconnect",
            Action::Refetch if keys_pane_focused => "rescan",
            Action::Refetch => "refetch",
            other => other.label(),
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
                    key: KeyPress::plain(KeyCode::Tab),
                    action: Action::CyclePane,
                },
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
                    key: KeyPress::plain(KeyCode::Right),
                    action: Action::Open,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('l')),
                    action: Action::Open,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Left),
                    action: Action::CollapseGroup,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('h')),
                    action: Action::CollapseGroup,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Enter),
                    action: Action::EnterValueCursor,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('c')),
                    action: Action::Copy,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('C')),
                    action: Action::CopyCommand,
                },
                // Horizontal chords for a horizontal action. `⌃←`/`⌃→` are
                // otherwise idle, so this adds no ambiguity with plain
                // `Left`/`Right`, which are `Open`/`CollapseGroup` above.
                Binding {
                    key: KeyPress::ctrl(KeyCode::Right),
                    action: Action::WidenKeysPane,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Left),
                    action: Action::NarrowKeysPane,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('d')),
                    action: Action::Delete,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('y')),
                    action: Action::ConfirmMutation,
                },
                Binding {
                    key: KeyPress::plain(KeyCode::Char('e')),
                    action: Action::Edit,
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

    /// Disconnected outranks pane focus: there is nothing to refetch or
    /// rescan without a connection, only a reconnect to retry (ADR-0009).
    #[test]
    fn refetch_says_reconnect_while_disconnected_regardless_of_focus() {
        assert_eq!(Action::Refetch.label_in(true, true), "reconnect");
        assert_eq!(Action::Refetch.label_in(false, true), "reconnect");
    }

    #[test]
    fn refetch_still_splits_on_pane_focus_when_connected() {
        assert_eq!(Action::Refetch.label_in(true, false), "rescan");
        assert_eq!(Action::Refetch.label_in(false, false), "refetch");
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
            Action::CollapseGroup,
            Action::Open,
            Action::EnterValueCursor,
            Action::Copy,
            Action::CopyCommand,
            Action::Delete,
            Action::ConfirmMutation,
            Action::Edit,
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
