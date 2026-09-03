//! Responsive layout (DESIGN §2, R7.1, PLAN M1.3).
//!
//! Screen space is a budget, not a canvas. As the terminal narrows the app
//! sheds columns in a fixed order, and `TTL` is the last metadata column to go
//! because it is the field people are hunting when the terminal is small and
//! the situation is urgent.

use ratatui::layout::Rect;

/// How much of the key list's metadata fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    /// `KEY TYPE SIZE TTL`.
    Full,
    /// `KEY TYPE TTL` — `SIZE` is the first to go.
    NoSize,
    /// Two panes, but everything is tight.
    Tight,
    /// Below 70 columns: one pane, stack-navigated.
    Single,
}

/// Below 70 columns there is room for one pane at a time. Opening a key
/// pushes from the list onto the value; `Esc` pops back (DESIGN §2: "single
/// pane, stack-navigated").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SinglePaneView {
    #[default]
    Keys,
    Value,
}

/// Where the panes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub keys: Rect,
    /// `None` below 70 columns, where there is only one pane.
    pub value: Option<Rect>,
    pub density: Density,
    /// Below 24 rows the hint bar collapses into the status bar.
    pub hint_bar: bool,
}

/// Rows reserved at the top (title) and bottom (hints).
const TITLE_ROWS: u16 = 2;

pub fn layout(area: Rect, single_pane: SinglePaneView) -> Layout {
    let density = match area.width {
        w if w >= 120 => Density::Full,
        w if w >= 90 => Density::NoSize,
        w if w >= 70 => Density::Tight,
        _ => Density::Single,
    };
    let hint_bar = area.height >= 24;
    let bottom = if hint_bar { 2 } else { 1 };
    let body_top = TITLE_ROWS;
    let body_height = area.height.saturating_sub(body_top + bottom);

    if density == Density::Single {
        let full = Rect::new(0, body_top, area.width, body_height);
        return match single_pane {
            // A zero-size keys rect, not an absent one: keys::render already
            // no-ops below its own minimum size, so nothing further needs to
            // know which of the two states produced it.
            SinglePaneView::Keys => Layout {
                keys: full,
                value: None,
                density,
                hint_bar,
            },
            SinglePaneView::Value => Layout {
                keys: Rect::new(0, body_top, 0, 0),
                value: Some(full),
                density,
                hint_bar,
            },
        };
    }

    // The value pane takes the larger share: a key name is short, a value is
    // not, and the pane people read is the one that should get the room.
    let keys_width = match density {
        Density::Full => area.width * 45 / 100,
        _ => area.width / 2,
    };
    Layout {
        keys: Rect::new(0, body_top, keys_width, body_height),
        value: Some(Rect::new(
            keys_width + 1,
            body_top,
            area.width.saturating_sub(keys_width + 1),
            body_height,
        )),
        density,
        hint_bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(w: u16) -> Layout {
        layout(Rect::new(0, 0, w, 30), SinglePaneView::Keys)
    }

    #[test]
    fn columns_are_shed_in_the_documented_order() {
        assert_eq!(at(140).density, Density::Full);
        assert_eq!(at(120).density, Density::Full);
        assert_eq!(at(119).density, Density::NoSize);
        assert_eq!(at(90).density, Density::NoSize);
        assert_eq!(at(89).density, Density::Tight);
        assert_eq!(at(70).density, Density::Tight);
        assert_eq!(at(69).density, Density::Single);
    }

    #[test]
    fn below_seventy_columns_there_is_one_pane() {
        assert!(at(60).value.is_none());
        assert!(at(70).value.is_some());
    }

    #[test]
    fn the_value_pane_takes_the_larger_share_when_there_is_room() {
        let l = at(140);
        let value = l.value.unwrap();
        assert!(
            value.width > l.keys.width,
            "{} vs {}",
            value.width,
            l.keys.width
        );
    }

    #[test]
    fn panes_never_overlap_or_overflow() {
        for w in [70u16, 89, 90, 119, 120, 200] {
            let l = at(w);
            let value = l.value.unwrap();
            assert!(l.keys.x + l.keys.width < value.x, "overlap at {w}");
            assert!(value.x + value.width <= w, "overflow at {w}");
        }
    }

    #[test]
    fn the_hint_bar_collapses_on_a_short_terminal() {
        assert!(layout(Rect::new(0, 0, 120, 24), SinglePaneView::Keys).hint_bar);
        assert!(!layout(Rect::new(0, 0, 120, 23), SinglePaneView::Keys).hint_bar);
    }

    #[test]
    fn below_seventy_columns_value_view_fills_the_whole_pane() {
        let l = layout(Rect::new(0, 0, 60, 30), SinglePaneView::Value);
        assert!(
            l.keys.width == 0 || l.keys.height == 0,
            "keys should be unused, not visible"
        );
        let value = l.value.expect("value pane must exist in Value view");
        assert_eq!(value.width, 60);
    }

    #[test]
    fn single_pane_view_is_ignored_at_any_wider_density() {
        // Both panes always show above 70 columns; the field must not matter.
        for w in [70u16, 90, 120, 200] {
            let keys_view = layout(Rect::new(0, 0, w, 30), SinglePaneView::Keys);
            let value_view = layout(Rect::new(0, 0, w, 30), SinglePaneView::Value);
            assert_eq!(
                keys_view, value_view,
                "SinglePaneView leaked into a two-pane width"
            );
        }
    }
}
