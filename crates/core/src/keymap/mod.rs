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
    /// Fold the list on the separator, or unfold it (keys pane focused), or
    /// edit the Open key's TTL (value pane focused) — the same
    /// one-`Action`-two-verbs shape `Refetch` already has for
    /// `refetch`/`rescan` (PLAN M2 task 10, D2, ADR-0019's Consequences).
    /// The name is kept rather than split or renamed — see `label_in` for
    /// which half is in force, and ADR-0019's Consequences for why a second
    /// `Action` or a rename were both rejected.
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
    /// Open the inline value editor on the Open value's whole body (R3.2,
    /// R4.1, ADR-0014), or on a Hash field's value (PLAN M2 task 6, D4). A
    /// String has exactly one thing to edit, so this acts on focus alone,
    /// like `Copy`; a Hash needs the value cursor active on a row first —
    /// there is no "the whole hash" to edit in place of one field.
    Edit,
    /// Add a field to the Open Hash (PLAN M2 task 6, D1, D4), or a member to
    /// the Open Set (PLAN M2 task 7, D3, ADR-0016): captures a field name
    /// then opens the inline editor on an empty value targeting it, or for a
    /// Set, opens straight onto a single-part capture — a member has no name
    /// half to type first. Value-pane scoped, like `Edit` — it needs no
    /// cursor, since a new field or member has no row yet to pick. Named
    /// `Add`, not `AddField` (PLAN M2 task 7, D6): it now serves both
    /// collection types, and the label was already the neutral "add".
    Add,
    /// Stage the inline editor's buffer for confirmation, or close it
    /// silently if nothing changed (ADR-0014).
    EditorStage,
    /// Undo the last edit inside the inline editor.
    EditorUndo,
    /// Redo the last undone edit inside the inline editor.
    EditorRedo,
    /// Open the command palette — the fuzzy list over every `Action` in this
    /// enum, reading this same keymap so it and the hint bar can never
    /// disagree on the effective binding (CONTEXT.md, DESIGN §3, PLAN M3
    /// task 1). Global, like `Help`: not scoped to either pane.
    OpenPalette,
}

/// Every `Action` that exists, in a stable order.
///
/// `Action` is `#[non_exhaustive]` to keep another crate from constructing or
/// exhaustively matching it, but this crate still has to be able to list
/// every variant it defines — the Palette has nothing else to search over.
/// There is no `derive`-able way to enumerate an enum's variants without a
/// dependency this crate has no other reason to take on (ADR-0011's boundary
/// rule is about `crossterm`/`tokio`/`fred`, but a proc-macro crate would
/// still be more surface than the ~30 actions here justify), so this list is
/// maintained by hand, the same way `every_action_has_a_default_binding`
/// already was before this array existed — see
/// `tests::every_action_has_a_short_label_and_description` for the test that
/// keeps `label`/`description` honest against it.
pub const ALL_ACTIONS: &[Action] = &[
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
    Action::CyclePane,
    Action::WidenKeysPane,
    Action::NarrowKeysPane,
    Action::Delete,
    Action::ConfirmMutation,
    Action::Edit,
    Action::Add,
    Action::EditorStage,
    Action::EditorUndo,
    Action::EditorRedo,
    Action::OpenPalette,
];

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
            Action::Filter | Action::Sort | Action::CollapseGroup | Action::Open => {
                state.pane_visible(Pane::Keys)
            }
            // `d` is focus-dependent, like `c` (D4, PLAN M2 task 6): the keys
            // pane's Selected-key delete and the Viewer's Hash-field delete
            // are different commands on different targets, and whichever
            // pane is not drawn has no target for its half to act on.
            //
            // `t` joins this group one row down (PLAN M2 task 10, D2,
            // ADR-0019): the keys pane's tree toggle and the Viewer's TTL
            // editor are two different commands on two different targets,
            // exactly the same shape — not "is the value pane merely drawn",
            // which `open_ttl_editor`'s own ladder checks again for the same
            // reason `open_editor`'s does.
            Action::Delete | Action::Edit | Action::Add | Action::ToggleTree => {
                if state.keys_pane_focused() {
                    state.pane_visible(Pane::Keys)
                } else {
                    state.pane_visible(Pane::Value)
                }
            }
            // The editor-scoped actions only ever matter while the value pane
            // holds an open buffer, which itself requires that pane focused.
            Action::EditorStage | Action::EditorUndo | Action::EditorRedo => {
                state.pane_visible(Pane::Value)
            }
            // Everything else is the app's, not a pane's: quitting, help, Esc,
            // `Tab` (which is what *changes* which pane is on screen), `r`
            // (already pane-scoped by R2.7 on its own terms), `Enter`
            // (no-ops itself with nothing open to enter), copying, the
            // read-only toggle, and the Palette — it has nothing to say about
            // either pane until an entry is picked, at which point the picked
            // `Action`'s own gate applies (see `update::palette`).
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
            // Both halves, the same reason `Refetch`'s does (PLAN M2 task 10,
            // D2): the help overlay has to explain all of `t`, not just the
            // half in force in whichever pane happens to be focused.
            Action::ToggleTree => "tree / edit ttl",
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
            Action::Add => "add",
            Action::EditorStage => "stage",
            Action::EditorUndo => "undo",
            Action::EditorRedo => "redo",
            Action::OpenPalette => "palette",
        }
    }

    /// The Palette row's second column (PLAN M3 task 1): what the action
    /// does, in enough words to place it but few enough to fit one row at
    /// the narrowest supported width (DESIGN §2, 80 cols) — a description
    /// too long to fit there needs rewriting, not a wider Palette
    /// (CLAUDE.md, "screen space is a budget, not a canvas").
    pub fn description(&self) -> &'static str {
        match self {
            Action::Quit => "Exit redis-pane",
            Action::Refetch => "Re-read the open key, or rescan the keyspace",
            Action::ToggleReadOnly => "Lift or impose Read-only Mode",
            Action::Help => "Show every keybinding",
            Action::Cancel => "Back out of whatever is open",
            Action::MoveUp => "Move up",
            Action::MoveDown => "Move down",
            Action::PageUp => "Move up one page",
            Action::PageDown => "Move down one page",
            Action::Top => "Jump to the top",
            Action::Bottom => "Jump to the bottom",
            Action::Filter => "Filter the key list",
            Action::Sort => "Cycle the sort column",
            Action::ToggleTree => "Fold or unfold the tree, or edit the open key's TTL",
            Action::CollapseGroup => "Collapse the group under the cursor",
            Action::Open => "Open the selected key, or expand a group",
            Action::EnterValueCursor => "Start moving a cursor inside the open value",
            Action::Copy => "Copy the key name or the open value",
            Action::CopyCommand => "Copy a redis-cli command for the open key",
            Action::CyclePane => "Move focus between the panes",
            Action::WidenKeysPane => "Widen the keys pane",
            Action::NarrowKeysPane => "Narrow the keys pane",
            Action::Delete => "Stage a delete for confirmation",
            Action::ConfirmMutation => "Confirm the staged mutation",
            Action::Edit => "Edit the open value",
            Action::Add => "Add a field or member to the open value",
            Action::EditorStage => "Stage the inline editor's buffer",
            Action::EditorUndo => "Undo the last edit in the inline editor",
            Action::EditorRedo => "Redo the last undone edit",
            Action::OpenPalette => "Open the command palette",
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
            // PLAN M2 task 10, D2, ADR-0019: the same split `Refetch` makes
            // above, one row down — `disconnected` plays no part here, since
            // both halves of `t` work offline.
            Action::ToggleTree if keys_pane_focused => "tree",
            Action::ToggleTree => "ttl",
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
        KeyCode::Delete => "Del".into(),
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
                Binding {
                    key: KeyPress::plain(KeyCode::Char('a')),
                    action: Action::Add,
                },
                // Ctrl+S is reliable in raw mode on every platform this ships
                // for (IXON cleared on Unix, processed input off on Windows).
                // Ctrl+Enter/Shift+Enter are not, so they are not used here
                // (ADR-0014).
                Binding {
                    key: KeyPress::ctrl(KeyCode::Char('s')),
                    action: Action::EditorStage,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Char('z')),
                    action: Action::EditorUndo,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Char('y')),
                    action: Action::EditorRedo,
                },
                Binding {
                    key: KeyPress::ctrl(KeyCode::Char('k')),
                    action: Action::OpenPalette,
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

    /// PLAN M2 task 10, D2: `t` now follows `pane_is_on_screen`'s
    /// focus-dependent group exactly the way `d` already does — both check
    /// visibility of *whichever pane is currently focused*, so both are
    /// always on screen regardless of terminal width: the action is always
    /// aimed at the pane the reader is looking at, never the other one. This
    /// is the fix over the old grouping, where `t` (then only ever "toggle
    /// tree") checked the keys pane's visibility unconditionally, so below
    /// 70 columns with the Viewer focused — exactly the state that now means
    /// "edit ttl" — it was blocked outright.
    #[test]
    fn toggle_tree_follows_the_focused_pane_exactly_like_delete() {
        use crate::render::layout::Pane;
        let narrow = crate::State {
            cols: 60,
            rows: 24,
            ..crate::State::default()
        };
        let keys_focused = crate::State {
            focus: Pane::Keys,
            ..narrow.clone()
        };
        let value_focused = crate::State {
            focus: Pane::Value,
            ..narrow
        };
        for action in [Action::ToggleTree, Action::Delete] {
            assert!(
                action.pane_is_on_screen(&keys_focused),
                "{action:?} in the keys pane, narrow"
            );
            assert!(
                action.pane_is_on_screen(&value_focused),
                "{action:?} in the value pane, narrow — this is the case that used to be blocked for ToggleTree"
            );
        }
    }

    /// PLAN M2 task 10, D2: `t` splits the same way `r` does — `tree` in the
    /// keys pane, `ttl` in the Viewer — regardless of connection state.
    #[test]
    fn toggle_tree_splits_on_pane_focus() {
        assert_eq!(Action::ToggleTree.label_in(true, false), "tree");
        assert_eq!(Action::ToggleTree.label_in(false, false), "ttl");
        assert_eq!(Action::ToggleTree.label_in(true, true), "tree");
        assert_eq!(Action::ToggleTree.label_in(false, true), "ttl");
    }

    /// The help overlay explains both halves at once, the same shape
    /// `Refetch`'s `"refetch / rescan"` already has.
    #[test]
    fn toggle_tree_label_names_both_halves() {
        assert_eq!(Action::ToggleTree.label(), "tree / edit ttl");
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
        // `CyclePane`, `WidenKeysPane` and `NarrowKeysPane` were always
        // exercised via a literal `KeyPress` in their own tests rather than
        // by name here — walking `ALL_ACTIONS` instead of a second hand-typed
        // list closes that gap along with covering `OpenPalette`.
        let k = Keymap::default();
        for action in ALL_ACTIONS {
            assert!(k.key_for(*action).is_some(), "{action:?} has no binding");
        }
    }

    #[test]
    fn ctrl_k_opens_the_palette() {
        let k = Keymap::default();
        assert_eq!(
            k.action_for(&KeyPress::ctrl(KeyCode::Char('k'))),
            Some(Action::OpenPalette)
        );
    }

    /// Every action in [`ALL_ACTIONS`] has a label and a description short
    /// enough to fit one Palette row at the narrowest supported width
    /// (DESIGN §2, 80 cols) — checked here rather than left to a golden
    /// frame, since a too-long description is wrong regardless of how it
    /// happens to wrap.
    #[test]
    fn every_action_has_a_short_label_and_description() {
        for action in ALL_ACTIONS {
            assert!(!action.label().is_empty(), "{action:?} has no label");
            let description = action.description();
            assert!(!description.is_empty(), "{action:?} has no description");
            assert!(
                description.chars().count() <= 60,
                "{action:?}'s description is too long for one Palette row: {description:?}"
            );
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
