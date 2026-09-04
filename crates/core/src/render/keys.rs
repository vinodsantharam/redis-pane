//! The keyspace browser pane (R2.4, R2.6, PLAN M1.3 and M1.4).
//!
//! **Render cost is a function of viewport size, not keyspace size.** Only the
//! visible rows are formatted; a million-key Loaded set draws exactly as fast
//! as a ten-key one.
//!
//! Metadata arrives asynchronously (R2.4), so every cell has a pending state.
//! A pending cell renders a placeholder **in the same column position** as the
//! value it is waiting for — the list must never shift under someone's eyes
//! while they are reading it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::state::loaded::TTL_NONE;
use crate::state::{LoadedSet, State};
use crate::theme::{Theme, Token};

use super::layout::Density;

/// What a cell shows while its value has not arrived yet.
///
/// A middle dot rather than a blank, so the reader can tell "waiting" from
/// "there is nothing here" — the same distinction the sentinels make in the
/// Loaded set.
pub const PENDING: &str = "·";

/// Column geometry for a given pane width and density.
///
/// Every column has a fixed position, computed before any row is drawn, which
/// is what guarantees a late-arriving value lands exactly where its placeholder
/// was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    pub name: u16,
    pub name_width: u16,
    pub kind: Option<u16>,
    pub size: Option<(u16, u16)>,
    pub ttl: Option<(u16, u16)>,
}

const KIND_W: u16 = 7;
const SIZE_W: u16 = 9;
const TTL_W: u16 = 6;

impl Columns {
    pub fn for_pane(width: u16, density: Density) -> Columns {
        let inner = width.saturating_sub(2);
        match density {
            Density::Full => {
                let fixed = KIND_W + SIZE_W + TTL_W;
                let name_width = inner.saturating_sub(fixed);
                Columns {
                    name: 1,
                    name_width,
                    kind: Some(1 + name_width),
                    size: Some((1 + name_width + KIND_W, SIZE_W)),
                    ttl: Some((1 + name_width + KIND_W + SIZE_W, TTL_W)),
                }
            }
            // SIZE is the first column to go; TTL is the last, because it is
            // what people are hunting when the terminal is small.
            Density::NoSize | Density::Tight => {
                let fixed = KIND_W + TTL_W;
                let name_width = inner.saturating_sub(fixed);
                Columns {
                    name: 1,
                    name_width,
                    kind: Some(1 + name_width),
                    size: None,
                    ttl: Some((1 + name_width + KIND_W, TTL_W)),
                }
            }
            // Single-pane keeps TYPE as well as TTL. Dropping it would leave a
            // hash indistinguishable from a string, and DESIGN §5 requires the
            // type *name* to survive wherever colour cannot be relied on.
            Density::Single => {
                let fixed = KIND_W + TTL_W;
                let name_width = inner.saturating_sub(fixed);
                Columns {
                    name: 1,
                    name_width,
                    kind: Some(1 + name_width),
                    size: None,
                    ttl: Some((1 + name_width + KIND_W, TTL_W)),
                }
            }
        }
    }
}

/// Which rows are on screen. Scrolling changes this, never the Loaded set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Viewport {
    pub offset: usize,
    pub selected: usize,
}

impl Viewport {
    /// Keep the selection inside the visible window.
    pub fn scrolled_to_selection(mut self, height: usize) -> Viewport {
        if height == 0 {
            return self;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        self
    }
}

/// Render the keys pane. Draws at most `area.height` rows regardless of how
/// many keys are loaded.
pub fn render(state: &State, theme: &Theme, area: Rect, density: Density, buf: &mut Buffer) {
    if area.width < 8 || area.height < 2 {
        return;
    }
    let cols = Columns::for_pane(area.width, density);
    let mut y = area.y;

    // A capped scan means the list on screen is not the whole keyspace — a
    // fact worth more than a status-bar line that a copy confirmation can
    // displace for a few seconds. Filtering, sorting and tree/flat all leave
    // this row alone: none of them re-scan, so "capped" stays true and stays
    // visible until a real ScanStarted resets it. Reserved only while it is
    // true (G7), same rule as the filter line just below it.
    if state.keys.is_capped() {
        cap_banner(state, theme, area, y, buf);
        y += 1;
    }

    // The filter line only exists when there is a filter, so an unfiltered list
    // spends no rows on it (G7: screen space is a budget).
    if state.filtering || !state.list.filter.is_empty() {
        filter_line(state, theme, area, y, buf);
        y += 1;
    }
    header(theme, Rect { y, ..area }, cols, buf);
    y += 1;

    let body_height = (area.y + area.height).saturating_sub(y) as usize;
    let view = state.view.scrolled_to_selection(body_height);

    for row in 0..body_height {
        let display_row = view.offset + row;
        if display_row >= state.row_count() {
            break;
        }
        let at = y + row as u16;
        let selected = display_row == view.selected;
        if state.tree_mode {
            tree_row(state, display_row, selected, theme, area, cols, at, buf);
        } else if let Some(i) = state.list.index_at(display_row) {
            key_row(&state.keys, i, selected, theme, area, cols, at, 0, buf);
        }
    }
}

/// `/ user:*:session          3,410 of 41,203`
/// The cap banner: a persistent warning that scanning stopped early, so what
/// is on screen is a prefix of the keyspace, not the whole of it.
///
/// Reuses `ScanState::readout()`'s own wording rather than inventing new copy
/// — this is the same fact the status bar already states, moved somewhere it
/// cannot be silently displaced by a copy confirmation or a sort readout.
fn cap_banner(state: &State, theme: &Theme, area: Rect, y: u16, buf: &mut Buffer) {
    super::put(
        buf,
        area.x + 1,
        y,
        &format!("⚠ {}", state.scan.readout()),
        theme.style(Token::Warn),
    );
}

fn filter_line(state: &State, theme: &Theme, area: Rect, y: u16, buf: &mut Buffer) {
    let x = super::put(buf, area.x + 1, y, "/ ", theme.style(Token::Warn));
    let cursor = if state.filtering { "▏" } else { "" };
    super::put(
        buf,
        x,
        y,
        &format!("{}{cursor}", state.list.filter),
        theme.style(Token::Text),
    );
    let readout = state.list.match_readout(state.keys.len());
    if !readout.is_empty() {
        super::put_right(
            buf,
            area.x,
            y,
            area.width.saturating_sub(1),
            &readout,
            theme.style(Token::Muted),
        );
    }
}

/// A folded group, or a key nested under one.
#[allow(clippy::too_many_arguments)]
fn tree_row(
    state: &State,
    display_row: usize,
    selected: bool,
    theme: &Theme,
    area: Rect,
    cols: Columns,
    y: u16,
    buf: &mut Buffer,
) {
    use crate::state::tree::Row;
    match state.tree.row(display_row) {
        Some(Row::Group {
            offset,
            len,
            depth,
            descendants,
            expanded,
        }) => {
            if selected {
                fill_row(buf, area, y, theme.style(Token::Selected));
            }
            let name = state
                .keys
                .arena_slice(offset, len)
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let marker = if expanded { "▾" } else { "▸" };
            let indent = area.x + cols.name + depth * 2;
            let style = theme.style(if selected {
                Token::Selected
            } else {
                Token::Text
            });
            super::put(
                buf,
                indent,
                y,
                &format!("{marker} {name}{}", state.tree.separator),
                style,
            );
            // A collapsed node states what it is hiding, so folding never loses
            // information about how much is down there.
            if let Some((x, w)) = cols.ttl.or(cols.size) {
                let meta = if selected {
                    Token::Selected
                } else {
                    Token::Muted
                };
                super::put_right(
                    buf,
                    area.x + x,
                    y,
                    w,
                    &descendants.to_string(),
                    theme.style(meta),
                );
            }
        }
        Some(Row::Key { index, depth }) => key_row(
            &state.keys,
            index as usize,
            selected,
            theme,
            area,
            cols,
            y,
            depth * 2,
            buf,
        ),
        None => {}
    }
}

fn header(theme: &Theme, area: Rect, cols: Columns, buf: &mut Buffer) {
    let style = theme.style(Token::Muted);
    super::put(buf, area.x + cols.name, area.y, "KEY", style);
    if let Some(x) = cols.kind {
        super::put(buf, area.x + x, area.y, "TYPE", style);
    }
    if let Some((x, w)) = cols.size {
        super::put_right(buf, area.x + x, area.y, w, "SIZE", style);
    }
    if let Some((x, w)) = cols.ttl {
        super::put_right(buf, area.x + x, area.y, w, "TTL", style);
    }
}

#[allow(clippy::too_many_arguments)]
fn key_row(
    keys: &LoadedSet,
    i: usize,
    selected: bool,
    theme: &Theme,
    area: Rect,
    cols: Columns,
    y: u16,
    indent: u16,
    buf: &mut Buffer,
) {
    if selected {
        fill_row(buf, area, y, theme.style(Token::Selected));
    }

    let kind = keys.kind(i);
    // A key that vanished between the scan and its metadata fetch. It keeps its
    // row so nothing below the cursor renumbers, and takes over the dot column
    // — the same `✕` the Viewer header uses for a deleted open key, so one mark
    // means one thing at both levels.
    let gone = keys.is_gone(i);
    // The dot carries the type's hue everywhere a key is listed — DESIGN §5's
    // "consistent everywhere a type appears" — while the TYPE column spells
    // the same fact out in words, which is what keeps it legible with no hue
    // at all. Selection overrides both to the row's single highlight colour,
    // same as every other cell on that row.
    let dot_style = theme.style(match (selected, gone) {
        (true, _) => Token::Selected,
        (false, true) => Token::Danger,
        (false, false) => crate::theme::type_token(kind),
    });
    let dot = match (gone, kind.is_some()) {
        (true, _) => "✕",
        (false, true) => "●",
        (false, false) => PENDING,
    };
    super::put(buf, area.x + cols.name + indent, y, dot, dot_style);

    // A gone key's name is history, not something to act on, so it drops to the
    // same weight as its metadata rather than reading as a live row.
    let name_style = theme.style(match (selected, gone) {
        (true, _) => Token::Selected,
        (false, true) => Token::Muted,
        (false, false) => Token::Text,
    });
    let meta_style = theme.style(if selected {
        Token::Selected
    } else {
        Token::Muted
    });

    // Two extra columns reserved for "<dot> ", so the name never overlaps it
    // and the TYPE column keeps its position regardless of the dot's glyph.
    const DOT_W: u16 = 2;

    // In tree mode only the leaf segment is shown: the ancestors are already on
    // screen as group rows above it, and repeating them wastes the column.
    let full = keys.name_str(i).unwrap_or_default();
    let name = if indent > 0 {
        full.rsplit(':').next().unwrap_or(&full).to_string()
    } else {
        full.to_string()
    };
    super::put(
        buf,
        area.x + cols.name + indent + DOT_W,
        y,
        &truncate(
            &name,
            cols.name_width.saturating_sub(indent + DOT_W) as usize,
        ),
        name_style,
    );

    // Each of these renders a placeholder at the same position when the value
    // has not arrived, so nothing moves when it does.
    if let Some(x) = cols.kind {
        // Without this the TYPE column would fall back to the pending
        // placeholder, which says "not fetched yet" — the one reading that is
        // wrong here, because the fetch is exactly what found the key missing.
        let text = match (gone, kind) {
            (true, _) => "gone".to_string(),
            (false, Some(k)) => k.label().to_string(),
            (false, None) => PENDING.to_string(),
        };
        let style = theme.style(match (selected, gone) {
            (true, _) => Token::Selected,
            (false, true) => Token::Danger,
            (false, false) => crate::theme::type_token(kind),
        });
        super::put(buf, area.x + x, y, &text, style);
    }
    if let Some((x, w)) = cols.size {
        let text = keys.size(i).map_or(PENDING.to_string(), format_size);
        super::put_right(buf, area.x + x, y, w, &text, meta_style);
    }
    if let Some((x, w)) = cols.ttl {
        // Size is retrospective — "it held 1.1 MB" stays true after a deletion,
        // and during an incident it is usually the only answer left. A TTL is
        // future-tense: "expires in 12m" is a claim about a key that is not
        // there to expire, so it becomes the not-applicable dash instead. Same
        // glyph the Stream viewer uses for an age it cannot compute.
        let text = match (gone, keys.ttl(i)) {
            (true, _) => "—".to_string(),
            (false, Some(t)) => format_ttl(t),
            (false, None) => PENDING.to_string(),
        };
        super::put_right(buf, area.x + x, y, w, &text, meta_style);
    }
}

/// Paint an entire row with one style before drawing text over it.
///
/// `put()` resets a cell's style before applying its own, so a background
/// painted here would otherwise be erased the moment any text is drawn on top
/// of it — every subsequent `put`/`put_right` call on a selected row must
/// carry the same [`Token::Selected`] style for the fill to read as one
/// continuous bar rather than a background with holes in it.
fn fill_row(buf: &mut Buffer, area: Rect, y: u16, style: ratatui::style::Style) {
    super::put(buf, area.x, y, &" ".repeat(area.width as usize), style);
}

/// Truncate from the right with an ellipsis. Key names share long prefixes, so
/// the distinguishing part is at the end — but a name that has been cut must
/// say so, or the reader will believe they are looking at the whole key.
fn truncate(s: &str, width: usize) -> String {
    let width = width.saturating_sub(1);
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// Bytes, rendered the way a human reads them.
pub fn format_size(bytes: u32) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

/// A TTL in seconds, at the coarsest useful precision.
///
/// `∞` for a key with no expiry — a fact, and visually distinct from the
/// placeholder that means "we have not asked yet".
pub fn format_ttl(seconds: i32) -> String {
    match seconds {
        TTL_NONE => "∞".into(),
        s if s < 0 => "—".into(),
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_people_read_them() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(880), "880 B");
        assert_eq!(format_size(2_150), "2.1 KB");
        assert_eq!(format_size(1_153_434), "1.1 MB");
    }

    #[test]
    fn ttls_are_coarse_but_never_ambiguous() {
        assert_eq!(format_ttl(TTL_NONE), "∞");
        assert_eq!(format_ttl(42), "42s");
        assert_eq!(format_ttl(2_537), "42m");
        assert_eq!(format_ttl(7_200), "2h");
        assert_eq!(format_ttl(172_800), "2d");
    }

    #[test]
    fn no_expiry_and_not_yet_fetched_look_different() {
        // The Loaded set keeps them apart; the renderer must too.
        assert_ne!(format_ttl(TTL_NONE), PENDING);
    }

    #[test]
    fn a_truncated_name_says_so() {
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("user:8812:session", 10), "user:881…");
        assert!(truncate("user:8812:session", 10).ends_with('…'));
    }

    #[test]
    fn columns_shed_size_first_and_ttl_last() {
        let full = Columns::for_pane(60, Density::Full);
        assert!(full.size.is_some() && full.ttl.is_some());

        let no_size = Columns::for_pane(50, Density::NoSize);
        assert!(no_size.size.is_none());
        assert!(no_size.ttl.is_some(), "TTL is the last to go");

        let single = Columns::for_pane(40, Density::Single);
        assert!(
            single.size.is_none(),
            "SIZE is gone by the time we are this narrow"
        );
        assert!(
            single.kind.is_some(),
            "the type name must survive: DESIGN §5"
        );
        assert!(single.ttl.is_some(), "TTL is the last to go");
    }

    #[test]
    fn columns_do_not_depend_on_what_has_been_fetched() {
        // The whole reason a placeholder cannot shift the layout: geometry is
        // computed from the pane, never from the data.
        let a = Columns::for_pane(60, Density::Full);
        let b = Columns::for_pane(60, Density::Full);
        assert_eq!(a, b);
    }

    #[test]
    fn the_viewport_follows_the_selection_in_both_directions() {
        let v = Viewport {
            offset: 0,
            selected: 30,
        }
        .scrolled_to_selection(10);
        assert_eq!(
            v.offset, 21,
            "scrolling down keeps the selection on the last row"
        );

        let v = Viewport {
            offset: 20,
            selected: 5,
        }
        .scrolled_to_selection(10);
        assert_eq!(v.offset, 5, "scrolling up puts it on the first row");

        let v = Viewport {
            offset: 0,
            selected: 3,
        }
        .scrolled_to_selection(10);
        assert_eq!(v.offset, 0, "already visible, so nothing moves");
    }
}
