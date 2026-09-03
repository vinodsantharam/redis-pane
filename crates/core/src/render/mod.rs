//! Frame rendering: panes, Viewers, title bar, hint bar.
//!
//! Rendering is a pure function of [`State`], a [`Theme`] and a [`Clock`]
//! reading. It draws into a [`Buffer`] and knows nothing about backends or
//! terminals — that is the shell's problem. This is what lets golden-frame
//! tests exist at all (ADR-0011).

pub mod keys;
pub mod layout;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::clock::Clock;
use crate::keymap::{Action, key_label};
use crate::state::{Link, Liveness, State};
use crate::theme::{Theme, Token, env_token};

/// Render the whole frame into a fresh buffer of the given size.
pub fn frame(state: &State, theme: &Theme, clock: &dyn Clock, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    let plan = layout::layout(area);
    title_bar(state, theme, clock, area, &mut buf);

    keys::render(state, theme, plan.keys, plan.density, &mut buf);
    if let Some(value) = plan.value {
        value_pane(state, theme, value, &mut buf);
    }
    status_bar(state, theme, area, &mut buf);

    if plan.hint_bar && area.height >= 3 {
        let hints = hint_bar(state);
        put(
            &mut buf,
            1,
            area.height - 1,
            &hints,
            theme.style(Token::Muted),
        );
    }
    if state.help_open {
        help_overlay(state, theme, area, &mut buf);
    }
    buf
}

/// The value pane. Viewers land in M1.8; until then it states what is selected
/// so the two-pane layout is real rather than a promise.
fn value_pane(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width < 4 {
        return;
    }
    for y in 0..area.height {
        put(
            buf,
            area.x.saturating_sub(1),
            area.y + y,
            "│",
            theme.style(Token::Border),
        );
    }
    match state.keys.name_str(state.view.selected) {
        Some(name) => {
            put(buf, area.x + 1, area.y, &name, theme.style(Token::Text));
            put(
                buf,
                area.x + 1,
                area.y + 2,
                "value viewers land in M1.8",
                theme.style(Token::Muted),
            );
        }
        None => {
            put(
                buf,
                area.x + 1,
                area.y,
                "no key selected",
                theme.style(Token::Muted),
            );
        }
    }
}

/// The status bar: scan progress and its cancel affordance (DESIGN §6.2).
fn status_bar(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let readout = state.scan.readout();
    if readout.is_empty() || area.height < 2 {
        return;
    }
    let y = area.height - if area.height >= 24 { 2 } else { 1 };
    let token = match state.scan {
        crate::state::ScanState::Capped { .. } | crate::state::ScanState::Failed { .. } => {
            Token::Warn
        }
        _ => Token::Muted,
    };
    let x = put(buf, 1, y, &readout, theme.style(token));
    if state.scan.is_running()
        && let Some(hint) = state.keymap.hint(crate::keymap::Action::Cancel)
    {
        put(
            buf,
            x + 2,
            y,
            &format!("{hint} cancel"),
            theme.style(Token::Muted),
        );
    }
}

/// A dismissible overlay rather than resident chrome (G7): screen space is a
/// budget, and the help is only needed while it is being read.
fn help_overlay(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let lines = help_lines(state);
    let inner_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(10);
    let w = (inner_w + 4).min(area.width as usize);
    let h = (lines.len() + 4).min(area.height as usize);
    let x0 = (area.width as usize - w) / 2;
    let y0 = (area.height as usize - h) / 2;

    let border = theme.style(Token::BorderFocus);
    for y in 0..h {
        let row = (y0 + y) as u16;
        let line = if y == 0 || y == h - 1 {
            format!(
                "{}{}{}",
                if y == 0 { "┌" } else { "└" },
                "─".repeat(w - 2),
                if y == 0 { "┐" } else { "┘" }
            )
        } else {
            format!("│{}│", " ".repeat(w - 2))
        };
        put(buf, x0 as u16, row, &line, border);
    }
    put(
        buf,
        x0 as u16 + 2,
        y0 as u16,
        " keys ",
        theme.style(Token::Text),
    );
    for (i, line) in lines.iter().enumerate() {
        if y0 + 2 + i < y0 + h - 1 {
            put(
                buf,
                x0 as u16 + 2,
                (y0 + 2 + i) as u16,
                line,
                theme.style(Token::Text),
            );
        }
    }
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

    let env = state.connection.environment;
    let source = state.connection.source.label();
    let prefix = "─ redis-pane ─ ";

    // DESIGN §2 fixes the priority: the Environment and the Source are never
    // sacrificed. That pair is the entire mitigation for resolving a Connection
    // silently (ADR-0001), so losing it to a narrow window would quietly remove
    // the safeguard exactly when the screen is cramped and the user is rushed.
    // Everything else yields to it — the target first, then the readout.
    let required =
        prefix.chars().count() + 2 + env.label().chars().count() + 3 + source.chars().count() + 1;

    // Trim the readout from the end until it fits. Liveness goes before the
    // safety badge, because the Viewer header carries liveness too (§6.4) while
    // READ-ONLY appears nowhere else.
    let mut readout = status_readout(state, clock);
    let readout_width =
        |r: &[(String, Token)]| -> usize { r.iter().map(|(t, _)| t.chars().count()).sum() };
    while !readout.is_empty() && required + readout_width(&readout) + 3 > w {
        readout.pop();
    }
    let readout_w = readout_width(&readout);
    let left_budget = w.saturating_sub(readout_w + 3);

    let mut x = 0u16;
    x = put(buf, x, 0, prefix, border);
    x = put(buf, x, 0, "● ", theme.style(env_token(env)));
    x = put(buf, x, 0, env.label(), theme.style(env_token(env)));

    // Whatever is left over, the target may have — truncated from the left, so
    // the part that distinguishes one host from another survives.
    let target_budget = left_budget.saturating_sub(required + 3);
    if target_budget >= 4 {
        let target = truncate_left(&state.connection.target, target_budget);
        x = put(buf, x, 0, " · ", theme.style(Token::Muted));
        x = put(buf, x, 0, &target, theme.style(Token::Text));
    }

    x = put(buf, x, 0, " · ", theme.style(Token::Muted));
    x = put(buf, x, 0, &source, theme.style(Token::Muted));
    put(buf, x, 0, " ", border);

    if readout_w > 0 && readout_w + 4 < w {
        let mut x = (w - readout_w - 3) as u16;
        x = put(buf, x, 0, " ", border);
        for (text, token) in &readout {
            x = put(buf, x, 0, text, theme.style(*token));
        }
        put(buf, x, 0, " ", border);
    }
}

/// Truncate a target from the left, keeping the end.
///
/// Hosts differ at the end (`cache-01` vs `cache-02`, and the port), so cutting
/// the front keeps what distinguishes one server from another.
fn truncate_left(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len <= width {
        return s.to_string();
    }
    if width <= 1 {
        return String::new();
    }
    let keep = width - 1;
    let mut out = String::from("…");
    out.extend(s.chars().skip(len - keep));
    out
}

/// What the title bar says about safety and currency, right to left.
///
/// This is DESIGN §6.8's table expressed as code. The ordering is deliberate:
/// a condition that rejects writes outranks Read-only Mode, because it is the
/// more surprising fact and the one that explains a failure the user is about
/// to hit.
pub fn status_readout(state: &State, clock: &dyn Clock) -> Vec<(String, Token)> {
    let mut out: Vec<(String, Token)> = Vec::new();

    if let Some(condition) = state.condition {
        out.push((condition.readout(), Token::Danger));
        out.push(("  ".into(), Token::Muted));
    }

    if let Some(reason) = state.read_only {
        out.push((format!("READ-ONLY {}", reason.label()), Token::Warn));
        // Never offer a key where the server will refuse anyway (ADR-0009).
        let hint = if reason.liftable() {
            state
                .keymap
                .hint(Action::ToggleReadOnly)
                .unwrap_or_default()
        } else {
            "locked".into()
        };
        out.push((format!(" {hint}"), Token::Muted));
        out.push(("  ".into(), Token::Muted));
    }

    let liveness = state.liveness();
    out.push((
        liveness.readout().to_string(),
        match liveness {
            Liveness::Live => Token::Ok,
            Liveness::Manual => Token::Muted,
            Liveness::Disconnected => Token::Danger,
        },
    ));

    // Anything short of live owes the reader a fact and a way to act. While
    // reconnecting, the useful fact is when the next attempt happens: ADR-0009
    // requires the backoff be visible, because a silent wait is a freeze
    // wearing a different name.
    match &state.link {
        Link::Reconnecting { retry_in_ms, .. } => {
            out.push((
                format!(" · retry {}s", retry_in_ms.div_ceil(1000)),
                Token::Muted,
            ));
        }
        _ if liveness != Liveness::Live => {
            out.push((format!(" · {}", read_age(state, clock)), Token::Muted));
        }
        _ => {}
    }
    if liveness != Liveness::Live
        && let Some(hint) = state.keymap.hint(Action::Refetch)
    {
        out.push((format!("  {hint}"), Token::Muted));
    }
    out
}

/// The hint bar: the effective binding for each action, never a hard-coded
/// label (R7.5).
pub fn hint_bar(state: &State) -> String {
    [
        Action::Cancel,
        Action::Refetch,
        Action::ToggleReadOnly,
        Action::Help,
        Action::Quit,
    ]
    .into_iter()
    .filter_map(|a| state.keymap.key_for(a).map(|k| (a, k)))
    .map(|(a, k)| format!("{} {}", key_label(&k), a.label()))
    .collect::<Vec<_>>()
    .join("   ")
}

/// The help overlay: every binding in force, read from the same keymap.
pub fn help_lines(state: &State) -> Vec<String> {
    state
        .keymap
        .bindings()
        .iter()
        .map(|b| format!("{:<6}  {}", key_label(&b.key), b.action.label()))
        .collect()
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
/// Write text right-aligned inside a field of `width` starting at `x`.
///
/// Right alignment is what makes a column of sizes or TTLs scannable: the
/// magnitudes line up instead of the first digits.
pub(crate) fn put_right(buf: &mut Buffer, x: u16, y: u16, width: u16, s: &str, style: Style) {
    let len = s.chars().count() as u16;
    put(buf, x + width.saturating_sub(len), y, s, style);
}

pub(crate) fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) -> u16 {
    // Clip rather than panic. `Buffer::set_string` panics on an out-of-bounds
    // index, and a panic in a TUI leaves the user's terminal in raw mode — the
    // rudest possible failure, and one that would arrive precisely when the
    // window is small and awkward. Every write goes through here, so this one
    // check makes the whole renderer safe at any size.
    if x >= buf.area.width || y >= buf.area.height {
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

#[cfg(test)]
mod safety {
    //! A TUI that panics leaves the terminal in raw mode. Rendering must
    //! survive any size the user's window manager can produce.

    use super::*;
    use crate::clock::FixedClock;
    use crate::state::{LoadedSet, State};
    use crate::theme::ColorDepth;

    fn populated() -> State {
        let mut keys = LoadedSet::default();
        for i in 0..50 {
            keys.push(format!("key:{i}").as_bytes());
        }
        State {
            keys,
            ..State::default()
        }
    }

    #[test]
    fn no_terminal_size_can_make_rendering_panic() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let clock = FixedClock(1_000);
        for w in [0u16, 1, 2, 7, 8, 20, 69, 70, 89, 90, 119, 120, 300] {
            for h in [0u16, 1, 2, 3, 5, 23, 24, 60] {
                let _ = frame(&populated(), &theme, &clock, Rect::new(0, 0, w, h));
            }
        }
    }

    #[test]
    fn an_empty_keyspace_renders_at_every_size() {
        let theme = Theme::new(ColorDepth::Monochrome);
        let clock = FixedClock(1_000);
        for (w, h) in [(0u16, 0u16), (1, 1), (80, 24), (200, 60)] {
            let _ = frame(&State::default(), &theme, &clock, Rect::new(0, 0, w, h));
        }
    }
}
