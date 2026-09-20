//! The keys pane: selection, the filter capture mode, tree folding, sort,
//! and opening a key from the list into the Viewer.

use super::*;

/// The row of the parent of `row` — the nearest *preceding* row one depth
/// shallower — or `None` at depth 0, which has no parent.
///
/// True by construction: `Tree::rebuild` pushes rows in a single
/// left-to-right pass and only ever reuses an already-pushed ancestor rather
/// than duplicating it, so the nearest preceding row at each shallower depth
/// is always the immediate parent. Works the same for a Key row or a Group
/// row — `Row::depth` is defined for both — which is what lets `Left`
/// (`Action::CollapseGroup`) treat "collapse, then go to parent" as one walk
/// regardless of what row it started on.
pub(super) fn parent_row(state: &State, row: usize) -> Option<usize> {
    use crate::state::tree::Row;
    let depth = state.tree.row(row)?.depth();
    if depth == 0 {
        return None;
    }
    (0..row)
        .rev()
        .find(|&r| matches!(state.tree.row(r), Some(Row::Group { depth: d, .. }) if d == depth - 1))
}

/// The prefix a group row stands for, e.g. `user:profile:` for a depth-1
/// group under `user:`.
///
/// Built from the rows themselves — this row and each ancestor Group row
/// above it via [`parent_row`], every one of which already carries its own
/// segment's arena location — so it needs nothing *beneath* `row` to exist.
/// An earlier version reconstructed the prefix by searching forward for the
/// first visible Key row under the group instead, which broke the moment the
/// group was collapsed: `Tree::row` hides a collapsed group's children
/// entirely, so the search skipped straight past them onto a *different*
/// group's key (or found nothing), reconstructing the wrong prefix or none
/// at all — folding a group worked, unfolding it from the same row didn't
/// (#16).
pub(super) fn group_prefix_at(state: &State, row: usize) -> Option<String> {
    use crate::state::tree::Row;
    let Some(Row::Group { .. }) = state.tree.row(row) else {
        return None;
    };

    let mut rows = vec![row];
    let mut r = row;
    while let Some(parent) = parent_row(state, r) {
        rows.push(parent);
        r = parent;
    }
    rows.reverse();

    let sep = state.tree.separator;
    let mut out = String::new();
    for r in rows {
        let Some(Row::Group { offset, len, .. }) = state.tree.row(r) else {
            return None;
        };
        out.push_str(&String::from_utf8_lossy(
            state.keys.arena_slice(offset, len)?,
        ));
        out.push(sep);
    }
    Some(out)
}

/// Move the selection, clamped to the Loaded set.
pub(super) fn move_selection(mut state: State, by: isize) -> (State, Vec<Command>) {
    if state.row_count() == 0 {
        return (state, Vec::new());
    }
    let last = state.row_count() as isize - 1;
    let next = (state.view.selected as isize + by).clamp(0, last);
    state.view.selected = next as usize;
    after_move(state)
}

/// Scrolling reveals rows whose metadata has not been fetched, so every move
/// asks for what is newly visible — and only what is visible (R2.4).
pub(super) fn after_move(mut state: State) -> (State, Vec<Command>) {
    // Moving in the key list is not scrolling the value, so the Viewer returns
    // to rest: whatever arrives next may land without asking.
    if let Some(open) = &mut state.open
        && open.offset == 0
    {
        open.at_rest = true;
    }
    let height = state.visible_rows();
    state.view = state.view.scrolled_to_selection(height);
    let indices = state.rows_needing_metadata();
    if indices.is_empty() {
        (state, Vec::new())
    } else {
        (state, vec![Command::FetchMetadata { indices }])
    }
}

/// Keys typed while the filter is capturing.
pub(super) fn filter_key(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    match key.code {
        KeyCode::Esc => {
            // Esc abandons the filter entirely rather than keeping a partial
            // pattern nobody typed on purpose.
            state.filtering = false;
            state.list.filter.clear();
            state.rebuild_list();
            after_move(state)
        }
        KeyCode::Enter => {
            state.filtering = false;
            (state, Vec::new())
        }
        KeyCode::Backspace => {
            state.list.filter.pop();
            state.rebuild_list();
            after_move(state)
        }
        KeyCode::Char(c) if !key.ctrl && !key.alt => {
            state.list.filter.push(c);
            state.rebuild_list();
            after_move(state)
        }
        _ => (state, Vec::new()),
    }
}

/// `d` with the keys pane focused: stage a delete of the Selected key
/// (D4, PLAN M2 task 6).
///
/// The Selected key, not the Open key — the same target every other keys-pane
/// action takes. A gone row has nothing left to delete.
pub(super) fn delete_selected_key(mut state: State) -> (State, Vec<Command>) {
    let Some(index) = state.selected_key() else {
        return (state, Vec::new());
    };
    if state.keys.is_gone(index) {
        return (state, Vec::new());
    }
    let Some(name) = state.keys.name(index).map(KeyName::from) else {
        return (state, Vec::new());
    };
    state.confirm = Some(PendingMutation::DeleteKey { index, name });
    (state, Vec::new())
}

/// `→`/`Enter` on a row in the keys pane: expand a collapsed group, step into
/// an already-expanded one, or open a key.
pub(super) fn open_selected(mut state: State) -> (State, Vec<Command>) {
    // While the value cursor is active, Left/Right are not bound to
    // anything in the value pane (only Up/Down/PgUp/PgDn/Home/End
    // move the cursor there) — but the key list underneath is still
    // "selected" in the state-machine sense, so without this guard
    // Right/Left would silently walk the tree selection out from
    // under the open value. Esc, not an arrow key, is what leaves
    // cursor mode (`Action::Cancel`'s handler).
    if cursor_active(&state) {
        return (state, Vec::new());
    }
    // Right on a group row: expand it if collapsed, or step into its
    // first child if it is already expanded. Right never collapses —
    // `CollapseGroup` is the only key that does — matching the
    // standard treeview Right-arrow behavior (VS Code, macOS/Windows
    // outline views, the WAI-ARIA treeview pattern).
    if state.tree_mode
        && let Some(crate::state::tree::Row::Group { expanded, .. }) =
            state.tree.row(state.view.selected)
    {
        if expanded {
            return move_selection(state, 1);
        }
        if let Some(prefix) = group_prefix_at(&state, state.view.selected) {
            state.tree.toggle(&prefix);
            state.rebuild_list();
        }
        return after_move(state);
    }
    let Some(index) = state.selected_key() else {
        return (state, Vec::new());
    };
    let Some(name) = state.keys.name(index).map(KeyName::from) else {
        return (state, Vec::new());
    };
    // Opening a key moves focus onto it, at every width. Below 70
    // columns that is the "push" half of stack navigation (DESIGN §2);
    // above it both panes already show and this decides only which one
    // a pane-scoped key acts on. `Tab` and `Esc` both move it back
    // without closing the key — `Esc` additionally exits value-cursor
    // mode first, if it was active (`Action::Cancel`'s handler).
    state.focus = Pane::Value;
    let token = issue_read(&mut state);
    state.open_pending = Some(PendingRead {
        name: name.clone(),
        token,
        index: Some(index),
        issued_at_ms: None,
        activate_cursor: false,
        own_write: false,
    });
    let command = read_key(&state, name, Some(index), token);
    (state, vec![command])
}

/// `s`: cycle the active sort. A no-op in tree mode — folding needs name
/// order to group consecutive keys in one pass (`Tree::rebuild`'s doc
/// comment); cycling to another sort while folded fragments every group into
/// repeated headers with undercounted descendants. `rebuild_list` would snap
/// the sort back to Name immediately anyway, so this is a no-op either way —
/// guarding here just avoids advertising a key that visibly does nothing.
pub(super) fn cycle_sort(mut state: State) -> (State, Vec<Command>) {
    if state.tree_mode {
        return (state, Vec::new());
    }
    state.list.sort = state.list.sort.next();
    state.rebuild_list();
    after_move(state)
}

/// `t`: toggle tree mode.
pub(super) fn toggle_tree(mut state: State) -> (State, Vec<Command>) {
    state.tree_mode = !state.tree_mode;
    state.view.selected = 0;
    state.view.offset = 0;
    state.rebuild_list();
    after_move(state)
}

/// `←`: collapse an expanded group in place, or move the cursor to the
/// parent group (PLAN M2 task 6).
///
/// See the matching guard in [`open_selected`]. Left never expands (`Open`
/// is the only key that does): collapses an expanded group in place; a group
/// that is already collapsed, or a key row (which has no children of its own
/// to collapse), moves the cursor to the parent group instead. A top-level
/// group with nothing left to collapse has no parent to go to either, so
/// this is a no-op — the standard treeview Left-arrow behavior.
pub(super) fn collapse_group(mut state: State) -> (State, Vec<Command>) {
    if cursor_active(&state) {
        return (state, Vec::new());
    }
    if state.tree_mode {
        if let Some(crate::state::tree::Row::Group { expanded: true, .. }) =
            state.tree.row(state.view.selected)
        {
            if let Some(prefix) = group_prefix_at(&state, state.view.selected) {
                state.tree.toggle(&prefix);
                state.rebuild_list();
            }
        } else if let Some(parent) = parent_row(&state, state.view.selected) {
            state.view.selected = parent;
        }
    }
    after_move(state)
}
