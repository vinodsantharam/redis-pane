//! Frame rendering: panes, Viewers, title bar, hint bar.
//!
//! Rendering is a pure function of [`State`], a [`Theme`] and a [`Clock`]
//! reading. It draws into a [`Buffer`] and knows nothing about backends or
//! terminals — that is the shell's problem. This is what lets golden-frame
//! tests exist at all (ADR-0011).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::clock::Clock;
use crate::state::State;
use crate::theme::{Theme, Token, env_token};

/// Render the whole frame into a fresh buffer of the given size.
pub fn frame(state: &State, theme: &Theme, clock: &dyn Clock, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    title_bar(state, theme, clock, area, &mut buf);
    buf
}

/// The title bar: what we are connected to, and where that came from.
///
/// The Source readout is not decoration. Connection resolution never prompts
/// (ADR-0001), and showing the target *and* its Source at all times is the
/// mitigation for resolving silently.
fn title_bar(state: &State, theme: &Theme, clock: &dyn Clock, area: Rect, buf: &mut Buffer) {
    if area.width < 8 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    let border = theme.style(Token::Border);

    buf.set_string(0, 0, "─".repeat(w), border);
    let mut x = 0u16;
    x = put(buf, x, 0, "─ redis-pane ─ ", theme.style(Token::Border));
    x = put(
        buf,
        x,
        0,
        "● ",
        theme.style(env_token(state.connection.environment)),
    );
    x = put(
        buf,
        x,
        0,
        state.connection.environment.label(),
        theme.style(env_token(state.connection.environment)),
    );
    x = put(buf, x, 0, " · ", theme.style(Token::Muted));
    x = put(
        buf,
        x,
        0,
        &state.connection.target,
        theme.style(Token::Text),
    );
    x = put(buf, x, 0, " · ", theme.style(Token::Muted));
    x = put(
        buf,
        x,
        0,
        &state.connection.source.label(),
        theme.style(Token::Muted),
    );
    put(buf, x, 0, " ", theme.style(Token::Border));

    // The read age is why this frame depends on the injected clock: it is a
    // function of *when* the frame was drawn, not only of what is in State.
    if area.height > 1 {
        let age = read_age(state, clock);
        let x = w.saturating_sub(age.chars().count() + 2) as u16;
        put(buf, x, 1, &age, theme.style(Token::Muted));
    }
}

/// How long ago the displayed value was read. Shown whenever Liveness is
/// unavailable, because a value with no stated age is one the user must guess
/// about (ADR-0006).
pub fn read_age(state: &State, clock: &dyn Clock) -> String {
    match state.last_read_ms {
        None => "never read".to_string(),
        Some(then) => {
            let secs = clock.now_ms().saturating_sub(then) / 1000;
            if secs < 1 {
                "read just now".to_string()
            } else if secs < 60 {
                format!("read {secs}s ago")
            } else {
                format!("read {}m ago", secs / 60)
            }
        }
    }
}

/// Write a run of text, *replacing* the style of the cells it covers.
///
/// `Buffer::set_string` patches style rather than replacing it, so text drawn
/// over an already-styled cell inherits whatever was underneath. That is
/// invisible under truecolor, where the foreground is overwritten anyway, and
/// very visible in monochrome, where a border's DIM bled into every character
/// painted on top of it. Resetting first is the fix.
fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) -> u16 {
    if x >= buf.area.width {
        return x;
    }
    buf.set_string(x, y, s, Style::reset().patch(style));
    x + s.chars().count() as u16
}

/// A buffer rendered as plain text, one `String` per row.
///
/// This is the golden-frame representation: the same character grid the design
/// mockups use, so the spec and the fixtures cannot drift apart.
pub fn to_lines(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// A buffer rendered as one newline-joined string, for snapshot comparison.
pub fn to_text(buf: &Buffer) -> String {
    to_lines(buf).join("\n")
}

/// A buffer rendered as a golden fixture: the character grid, a legend of the
/// distinct styles in it, and a map from cell to style.
///
/// The text alone would be identical across colour depths, which would make a
/// theme snapshot prove nothing. The map is what gives the fixture teeth.
pub fn to_golden(buf: &Buffer) -> String {
    let mut legend: Vec<Style> = Vec::new();
    let mut rows: Vec<String> = Vec::new();

    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            let style = buf.cell((x, y)).map_or_else(Style::default, |c| c.style());
            if is_plain(&style) {
                row.push('.');
                continue;
            }
            let idx = legend.iter().position(|s| *s == style).unwrap_or_else(|| {
                legend.push(style);
                legend.len() - 1
            });
            row.push((b'a' + idx as u8) as char);
        }
        rows.push(row.trim_end().to_string());
    }

    let mut out = to_text(buf);
    out.push_str("\n--- styles ---\n");
    for (i, style) in legend.iter().enumerate() {
        out.push_str(&format!(
            "{} {}\n",
            (b'a' + i as u8) as char,
            describe_style(style)
        ));
    }
    out.push_str("--- map ---\n");
    out.push_str(&rows.join("\n"));
    out
}

/// Whether a cell carries no styling at all. A freshly emptied ratatui cell has
/// `fg`/`bg` of `Color::Reset` rather than `None`, so comparing against
/// `Style::default()` would mark every blank cell as styled and bury the
/// fixture in noise.
fn is_plain(s: &Style) -> bool {
    let plain_fg = matches!(s.fg, None | Some(ratatui::style::Color::Reset));
    let plain_bg = matches!(s.bg, None | Some(ratatui::style::Color::Reset));
    plain_fg && plain_bg && s.add_modifier.is_empty()
}

fn describe_style(s: &Style) -> String {
    let fg = match s.fg {
        None => "fg=none".to_string(),
        Some(ratatui::style::Color::Rgb(r, g, b)) => format!("fg=#{r:02x}{g:02x}{b:02x}"),
        Some(ratatui::style::Color::Indexed(i)) => format!("fg=ansi{i}"),
        Some(other) => format!("fg={other:?}"),
    };
    let mods = if s.add_modifier.is_empty() {
        String::new()
    } else {
        format!(" {:?}", s.add_modifier)
    };
    format!("{fg}{mods}")
}
