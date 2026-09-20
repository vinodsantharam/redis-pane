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
