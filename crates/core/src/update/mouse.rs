//! Mouse handling. Spans both panes — it focuses, scrolls and drags the
//! divider — so it fits neither `keys.rs` nor `viewer.rs` (a documented
//! deviation from the review's sketch; see the M2 split plan).

use super::*;

/// How many rows one wheel notch moves — a handful, not one: on a 500-entry
/// zset, one row per notch would take a hundred notches to cross the
/// viewport, and a mouse wheel step is coarser than an arrow key on purpose.
pub(super) const MOUSE_SCROLL_ROWS: isize = 3;

/// Turn a mouse action into the same state changes a click or a key would
/// have produced — R7.3, and the last of the three things it names.
///
/// This is the one place `update` computes a [`layout::Layout`]. Doing it
/// here rather than threading pane rectangles through `Msg` keeps the
/// boundary in the same place as everywhere else: the shell reports what
/// happened in terminal cells, the core is the only thing that knows what a
/// cell means. `layout` is a pure function of `(area, focus, split_adjust)`
/// — no clock, no I/O — so calling it here does not reach outside the
/// functional core (ADR-0011); it is exactly the geometry the render side
/// computed for the frame the reader is looking at.
pub(super) fn mouse_action(mut state: State, action: MouseAction) -> (State, Vec<Command>) {
    let area = ratatui::layout::Rect::new(0, 0, state.cols, state.rows);
    let plan = layout::layout(area, state.focus, state.split_adjust);

    match action {
        MouseAction::Down { col, row } => {
            // Grabbing the divider begins a resize-drag; anywhere else in a
            // pane focuses it. The two share one gesture the way dragging a
            // window's edge and clicking inside it share "mouse down" in any
            // GUI. Below 70 columns there is no divider to grab (one pane
            // fills the screen), so only the focus half can apply there.
            if state.split_is_adjustable()
                && let Some(value) = plan.value
                && col == value.x.saturating_sub(1)
                && row >= plan.keys.y
                && row < plan.keys.y + plan.keys.height
            {
                state.resizing_split = true;
                return (state, Vec::new());
            }
            if let Some(pane) = pane_at(&plan, col, row) {
                state.focus = pane;
            }
            (state, Vec::new())
        }
        MouseAction::Drag { col, .. } => {
            if state.resizing_split {
                // The offset is relative to the density's own base width, not
                // to the absolute column — that is what `split_adjust` means
                // everywhere else it is read, including a `⌃←`/`⌃→` nudge
                // landing on top of a drag that came before it.
                let base = layout::layout(area, state.focus, 0).keys.width;
                state.split_adjust = i32::from(col) as i16 - base as i16;
                state.rewrap_open();
            }
            (state, Vec::new())
        }
        MouseAction::Up => {
            state.resizing_split = false;
            (state, Vec::new())
        }
        // Scrolling acts on whatever pane is under the cursor, focus or not —
        // the ordinary convention (a window manager does not require a click
        // first either) — and moves focus to match, so a keyboard action
        // right after does not silently land somewhere else (the same
        // reasoning `pane_visible` gates keyboard actions on).
        MouseAction::ScrollUp { col, row } => scroll_at(state, &plan, col, row, -1),
        MouseAction::ScrollDown { col, row } => scroll_at(state, &plan, col, row, 1),
    }
}

/// Which pane, if any, a cell belongs to.
pub(super) fn pane_at(plan: &layout::Layout, col: u16, row: u16) -> Option<Pane> {
    let contains = |r: ratatui::layout::Rect| {
        col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
    };
    if contains(plan.keys) {
        Some(Pane::Keys)
    } else if plan.value.is_some_and(contains) {
        Some(Pane::Value)
    } else {
        None
    }
}

/// Scroll whichever pane the wheel was over, `dir` notches of
/// [`MOUSE_SCROLL_ROWS`] each.
pub(super) fn scroll_at(
    mut state: State,
    plan: &layout::Layout,
    col: u16,
    row: u16,
    dir: isize,
) -> (State, Vec<Command>) {
    match pane_at(plan, col, row) {
        Some(Pane::Keys) => {
            state.focus = Pane::Keys;
            move_selection(state, dir * MOUSE_SCROLL_ROWS)
        }
        Some(Pane::Value) => {
            state.focus = Pane::Value;
            if let Some(open) = &mut state.open {
                open.cursor_active = true;
            }
            move_cursor(state, dir * MOUSE_SCROLL_ROWS)
        }
        None => (state, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key open in the Viewer *and focused*, at a two-pane width — the state
    /// that `Action::Open` produces, since opening a key moves focus onto it.
    fn viewing() -> State {
        State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(
                Some(0),
                "k".into(),
                crate::state::value::Value::Str(crate::state::value::StringValue::new("v", 40)),
                -1,
                10,
                0,
            )),
            ..State::default()
        }
    }

    // ── mouse: click-to-focus, scroll, drag-to-resize (R7.3) ────────────────

    fn plan_at(state: &State) -> layout::Layout {
        layout::layout(
            ratatui::layout::Rect::new(0, 0, state.cols, state.rows),
            state.focus,
            state.split_adjust,
        )
    }

    #[test]
    fn a_click_in_the_keys_pane_focuses_it() {
        let state = viewing(); // focus starts on Value
        let plan = plan_at(&state);
        let (state, cmds) = update(
            state,
            Msg::Mouse(MouseAction::Down {
                col: plan.keys.x,
                row: plan.keys.y,
            }),
        );
        assert_eq!(state.focus, Pane::Keys);
        assert!(cmds.is_empty(), "a focus change is a plain state mutation");
    }

    #[test]
    fn a_click_in_the_value_pane_focuses_it() {
        let mut state = viewing();
        state.focus = Pane::Keys;
        let plan = plan_at(&state);
        let value = plan.value.unwrap();
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Down {
                col: value.x,
                row: value.y,
            }),
        );
        assert_eq!(state.focus, Pane::Value);
    }

    #[test]
    fn clicking_outside_both_panes_changes_nothing() {
        // The title bar row, for instance — row 0 is above `plan.keys.y`.
        let state = viewing();
        let before = state.focus;
        let (state, _) = update(state, Msg::Mouse(MouseAction::Down { col: 5, row: 0 }));
        assert_eq!(state.focus, before);
    }

    #[test]
    fn grabbing_the_divider_begins_a_resize_drag_instead_of_a_focus_change() {
        let mut state = viewing();
        state.focus = Pane::Keys;
        let plan = plan_at(&state);
        let divider_col = plan.value.unwrap().x - 1;
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Down {
                col: divider_col,
                row: plan.keys.y,
            }),
        );
        assert!(state.resizing_split);
        assert_eq!(
            state.focus,
            Pane::Keys,
            "grabbing the divider is not a click"
        );
    }

    #[test]
    fn dragging_after_a_grab_follows_the_cursor() {
        let state = viewing();
        let plan = plan_at(&state);
        let divider_col = plan.value.unwrap().x - 1;
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Down {
                col: divider_col,
                row: plan.keys.y,
            }),
        );
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Drag {
                col: divider_col + 15,
                row: plan.keys.y,
            }),
        );
        assert_eq!(state.split_adjust, 15);

        // And it can move the divider back the other way just as freely.
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Drag {
                col: divider_col - 5,
                row: plan.keys.y,
            }),
        );
        assert_eq!(state.split_adjust, -5);
    }

    #[test]
    fn dragging_with_no_prior_grab_does_nothing() {
        // A drag that started somewhere else entirely — text selection in a
        // future feature, say — must not be mistaken for a resize.
        let state = viewing();
        assert!(!state.resizing_split);
        let (state, _) = update(state, Msg::Mouse(MouseAction::Drag { col: 90, row: 5 }));
        assert_eq!(state.split_adjust, 0);
    }

    #[test]
    fn mouse_up_ends_the_drag() {
        let state = viewing();
        let plan = plan_at(&state);
        let divider_col = plan.value.unwrap().x - 1;
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Down {
                col: divider_col,
                row: plan.keys.y,
            }),
        );
        let (state, _) = update(state, Msg::Mouse(MouseAction::Up));
        assert!(!state.resizing_split);

        // A drag after releasing must not still be honoured.
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::Drag {
                col: divider_col + 20,
                row: plan.keys.y,
            }),
        );
        assert_eq!(state.split_adjust, 0);
    }

    #[test]
    fn the_divider_cannot_be_grabbed_below_seventy_columns() {
        // No divider is drawn there (DESIGN §2, stack navigation) — nothing
        // should arm a resize that has no visible effect.
        let mut state = State {
            cols: 60,
            rows: 40,
            focus: Pane::Keys,
            ..State::default()
        };
        state.split_adjust = 0;
        let (state2, _) = update(
            state.clone(),
            Msg::Mouse(MouseAction::Down { col: 40, row: 10 }),
        );
        assert!(!state2.resizing_split);
    }

    /// A local stand-in for `scan_tests::browsing` — that helper lives in a
    /// separate test module and this one does not reach across module
    /// boundaries for its fixtures, matching the rest of this file.
    fn many_rows(n: usize) -> State {
        let mut state = State {
            cols: 130,
            rows: 30,
            ..State::default()
        };
        (state, _) = update(
            state,
            Msg::ScanStarted {
                estimated_total: n as u64,
            },
        );
        let keys = (0..n).map(|i| format!("k:{i}").into_bytes()).collect();
        update(state, Msg::ScanBatch { keys }).0
    }

    #[test]
    fn scrolling_the_keys_pane_moves_the_selection_and_takes_focus() {
        let mut state = many_rows(50);
        state.focus = Pane::Value; // prove scrolling does not need a prior click
        let plan = plan_at(&state);
        let before = state.view.selected;
        let (state, cmds) = update(
            state,
            Msg::Mouse(MouseAction::ScrollDown {
                col: plan.keys.x,
                row: plan.keys.y,
            }),
        );
        assert_eq!(
            state.focus,
            Pane::Keys,
            "the pane under the cursor, not held focus"
        );
        assert_eq!(state.view.selected, before + MOUSE_SCROLL_ROWS as usize);
        let _ = cmds; // metadata fetches are allowed; not the point of this test
    }

    #[test]
    fn scrolling_the_value_pane_scrolls_the_viewer_and_takes_focus() {
        use crate::state::value::{MemberValue, Value};

        let mut state = viewing();
        state.focus = Pane::Keys;
        state.open = Some(OpenKey::new(
            Some(0),
            "k".into(),
            Value::Set(MemberValue {
                members: (0..50).map(|i| format!("m{i}").into_bytes()).collect(),
                total: 50,
            }),
            -1,
            10,
            0,
        ));
        let plan = plan_at(&state);
        let value = plan.value.unwrap();
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::ScrollDown {
                col: value.x,
                row: value.y,
            }),
        );
        assert_eq!(state.focus, Pane::Value);
        let open = state.open.unwrap();
        assert!(
            open.cursor_active,
            "scrolling the value pane enters cursor mode"
        );
        assert_eq!(open.cursor, MOUSE_SCROLL_ROWS as usize);
    }

    #[test]
    fn scrolling_up_at_the_top_of_the_viewer_clamps_rather_than_going_negative() {
        let state = viewing();
        let plan = plan_at(&state);
        let value = plan.value.unwrap();
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::ScrollUp {
                col: value.x,
                row: value.y,
            }),
        );
        assert_eq!(state.open.unwrap().offset, 0);
    }

    #[test]
    fn scrolling_outside_both_panes_changes_nothing() {
        let state = many_rows(50);
        let before = state.view.selected;
        let (state, _) = update(
            state,
            Msg::Mouse(MouseAction::ScrollDown { col: 5, row: 0 }),
        );
        assert_eq!(state.view.selected, before);
        assert_eq!(state.focus, Pane::Keys);
    }
}
