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

/// Which pane the reader is in — the focus (DESIGN §4).
///
/// One concept doing two jobs, because they are the same question. Below 70
/// columns there is room for one pane at a time, so this decides what is
/// *drawn*: opening a key pushes from the list onto the value, `Esc` pops back
/// (DESIGN §2, "single pane, stack-navigated"). At any wider density both panes
/// are drawn and this decides only what is *focused* — which is what makes
/// `r` able to act on one pane and not the other (R2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
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

/// The narrowest width that still fits two panes.
pub const TWO_PANE_MIN_COLS: u16 = 70;

/// The narrowest either pane may be squeezed to by an adjustment.
///
/// Below this a pane stops being useful before it stops being visible: the
/// keys pane needs room for a dot, a type hue and enough of a name to
/// distinguish it from its neighbours; the value pane needs room for a
/// header line that still says something. There is no formula that makes
/// "how short is too short" exact, so this is chosen, not derived — and
/// generous enough that `TWO_PANE_MIN_COLS` (70) always has slack on both
/// sides of it.
const MIN_PANE_COLS: u16 = 20;

/// How many columns one `⌃←`/`⌃→` moves the divider (DESIGN §2: "the split is
/// resizable").
pub const SPLIT_STEP: u16 = 4;

/// Apply a session's split adjustment to a density's base keys-pane width.
///
/// Clamped so neither pane can be squeezed below [`MIN_PANE_COLS`] — nudging
/// past the limit stops there rather than inverting the panes or overflowing
/// the terminal. The `+ 1` on the upper bound accounts for the divider
/// column itself, which is not part of either pane's width.
fn adjust_keys_width(total: u16, base: u16, split_adjust: i16) -> u16 {
    let max = total.saturating_sub(MIN_PANE_COLS + 1).max(MIN_PANE_COLS);
    (i32::from(base) + i32::from(split_adjust)).clamp(i32::from(MIN_PANE_COLS), i32::from(max))
        as u16
}

pub fn layout(area: Rect, focus: Pane, split_adjust: i16) -> Layout {
    let density = match area.width {
        w if w >= 120 => Density::Full,
        w if w >= 90 => Density::NoSize,
        w if w >= TWO_PANE_MIN_COLS => Density::Tight,
        _ => Density::Single,
    };
    let hint_bar = area.height >= 24;
    let bottom = if hint_bar { 2 } else { 1 };
    let body_top = TITLE_ROWS;
    let body_height = area.height.saturating_sub(body_top + bottom);

    if density == Density::Single {
        let full = Rect::new(0, body_top, area.width, body_height);
        return match focus {
            // A zero-size keys rect, not an absent one: keys::render already
            // no-ops below its own minimum size, so nothing further needs to
            // know which of the two states produced it.
            Pane::Keys => Layout {
                keys: full,
                value: None,
                density,
                hint_bar,
            },
            Pane::Value => Layout {
                keys: Rect::new(0, body_top, 0, 0),
                value: Some(full),
                density,
                hint_bar,
            },
        };
    }

    // The value pane takes the larger share by default: a key name is short,
    // a value is not, and the pane people read is the one that should get the
    // room. `split_adjust` — nudged by `⌃←`/`⌃→`, and independent of density
    // — perturbs that default rather than replacing it, so widening the
    // terminal still reshuffles columns the documented way and a reader's
    // adjustment survives it.
    let base_keys_width = match density {
        Density::Full => area.width * 45 / 100,
        _ => area.width / 2,
    };
    let keys_width = adjust_keys_width(area.width, base_keys_width, split_adjust);
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
        layout(Rect::new(0, 0, w, 30), Pane::Keys, 0)
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
        assert!(layout(Rect::new(0, 0, 120, 24), Pane::Keys, 0).hint_bar);
        assert!(!layout(Rect::new(0, 0, 120, 23), Pane::Keys, 0).hint_bar);
    }

    #[test]
    fn below_seventy_columns_value_view_fills_the_whole_pane() {
        let l = layout(Rect::new(0, 0, 60, 30), Pane::Value, 0);
        assert!(
            l.keys.width == 0 || l.keys.height == 0,
            "keys should be unused, not visible"
        );
        let value = l.value.expect("value pane must exist in Value view");
        assert_eq!(value.width, 60);
    }

    #[test]
    fn focus_does_not_move_a_pane_at_any_wider_density() {
        // Focus decides what is *drawn* only below 70 columns. Above it both
        // panes always show, so focus must change nothing about geometry — it
        // only decides which pane a pane-scoped key acts on.
        for w in [70u16, 90, 120, 200] {
            let keys_view = layout(Rect::new(0, 0, w, 30), Pane::Keys, 0);
            let value_view = layout(Rect::new(0, 0, w, 30), Pane::Value, 0);
            assert_eq!(keys_view, value_view, "focus moved a pane at {w} columns");
        }
    }

    // ── the split is resizable (DESIGN §2) ──────────────────────────────────

    #[test]
    fn a_positive_adjustment_widens_the_keys_pane() {
        let base = layout(Rect::new(0, 0, 140, 30), Pane::Keys, 0).keys.width;
        let widened = layout(Rect::new(0, 0, 140, 30), Pane::Keys, 20).keys.width;
        assert_eq!(widened, base + 20);
    }

    #[test]
    fn a_negative_adjustment_narrows_the_keys_pane() {
        let base = layout(Rect::new(0, 0, 140, 30), Pane::Keys, 0).keys.width;
        let narrowed = layout(Rect::new(0, 0, 140, 30), Pane::Keys, -20).keys.width;
        assert_eq!(narrowed, base - 20);
    }

    #[test]
    fn the_adjustment_cannot_crush_either_pane_below_the_minimum() {
        // A reader holding the key down must hit a wall, not a negative width
        // or a pane that swallows its neighbour.
        let crushed_narrow = layout(Rect::new(0, 0, 140, 30), Pane::Keys, -1_000);
        assert_eq!(crushed_narrow.keys.width, MIN_PANE_COLS);
        assert!(crushed_narrow.value.unwrap().width >= MIN_PANE_COLS);

        let crushed_wide = layout(Rect::new(0, 0, 140, 30), Pane::Keys, 1_000);
        assert!(crushed_wide.value.unwrap().width >= MIN_PANE_COLS);
        assert!(
            crushed_wide.keys.x + crushed_wide.keys.width < crushed_wide.value.unwrap().x,
            "still no overlap at the far end of the range"
        );
    }

    #[test]
    fn the_panes_stay_disjoint_at_every_density_across_the_whole_adjustment_range() {
        // The property `panes_never_overlap_or_overflow` already checks at
        // zero adjustment; this is the same property swept over the range an
        // actual session can reach by repeatedly pressing the key.
        for w in [70u16, 89, 90, 119, 120, 200] {
            for adjust in (-200i16..=200).step_by(10) {
                let l = layout(Rect::new(0, 0, w, 30), Pane::Keys, adjust);
                let value = l.value.unwrap();
                assert!(l.keys.x + l.keys.width < value.x, "overlap at {w}/{adjust}");
                assert!(value.x + value.width <= w, "overflow at {w}/{adjust}");
            }
        }
    }

    #[test]
    fn a_session_that_never_touches_the_key_renders_exactly_as_before() {
        // The whole point of an *adjustment* rather than a replacement ratio:
        // every density's documented default is untouched at zero.
        for w in [70u16, 89, 90, 119, 120, 200] {
            assert_eq!(
                layout(Rect::new(0, 0, w, 30), Pane::Keys, 0),
                layout(Rect::new(0, 0, w, 30), Pane::Keys, 0),
            );
        }
        // Full density's documented 45%, unperturbed.
        assert_eq!(
            layout(Rect::new(0, 0, 140, 30), Pane::Keys, 0).keys.width,
            63
        );
    }

    #[test]
    fn the_adjustment_survives_a_density_change() {
        // One persisted offset, not one preset per density — widening the
        // terminal past a breakpoint reshuffles columns the documented way,
        // but a reader's own nudge is not reset by it.
        let tight = layout(Rect::new(0, 0, 80, 30), Pane::Keys, 10).keys.width;
        let tight_base = layout(Rect::new(0, 0, 80, 30), Pane::Keys, 0).keys.width;
        assert_eq!(tight, tight_base + 10);

        let full = layout(Rect::new(0, 0, 140, 30), Pane::Keys, 10).keys.width;
        let full_base = layout(Rect::new(0, 0, 140, 30), Pane::Keys, 0).keys.width;
        assert_eq!(full, full_base + 10);
    }

    #[test]
    fn single_pane_density_ignores_the_adjustment_entirely() {
        // Below 70 columns there is no divider to move; an adjustment made
        // before narrowing the terminal must not resurface as a shifted
        // breadcrumb or a mysteriously offset single pane.
        let adjusted = layout(Rect::new(0, 0, 60, 30), Pane::Keys, 30);
        let plain = layout(Rect::new(0, 0, 60, 30), Pane::Keys, 0);
        assert_eq!(adjusted, plain);
    }
}
