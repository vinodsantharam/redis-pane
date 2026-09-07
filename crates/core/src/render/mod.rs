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
use crate::state::{Attachment, Link, Liveness, PendingRead, State};
use crate::theme::{Theme, Token, env_token};

/// Render the whole frame into a fresh buffer of the given size.
pub fn frame(state: &State, theme: &Theme, clock: &dyn Clock, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    let plan = layout::layout(area, state.focus, state.split_adjust);
    title_bar(state, theme, clock, area, &mut buf);

    let open_row = keys::render(state, theme, clock, plan.keys, plan.density, &mut buf);
    if let Some(value) = plan.value {
        // Standalone below 70 columns: the value fills the whole pane with no
        // adjacent keys pane to separate from, and the list it came from is
        // off screen, so its own header carries a breadcrumb back to it.
        let standalone = plan.density == layout::Density::Single;
        value_pane(state, theme, clock, value, standalone, open_row, &mut buf);
    }
    status_bar(state, theme, clock, area, &mut buf);

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
#[allow(clippy::too_many_arguments)]
fn value_pane(
    state: &State,
    theme: &Theme,
    clock: &dyn Clock,
    area: Rect,
    standalone: bool,
    open_row: keys::OpenRowMark,
    buf: &mut Buffer,
) {
    if area.height == 0 || area.width < 6 {
        return;
    }

    // A read is in flight for a key that isn't (yet) what's on screen — a
    // first Open, or a switch to a different key while another's value was
    // showing. Neither the previous value nor a blank "no key selected" pane
    // says anything happened, so this shows a placeholder instead — the
    // keyspace browser's "pending cell, no layout shift" idiom (DESIGN §6.2),
    // applied to the body. A Refetch of the key already on screen (same name)
    // is handled below instead, without disturbing the value in view.
    if let Some(pending) = &state.open_pending
        && state.open.as_ref().map(|o| o.name.as_str()) != Some(pending.name.as_str())
    {
        opening_placeholder(state, theme, area, standalone, pending, buf);
        return;
    }

    let Some(open) = &state.open else {
        solid_divider(buf, area, standalone, theme);
        let label = if state.tree_mode && state.row_count() > 0 {
            "a group is selected"
        } else if state.row_count() > 0 {
            "→ to open"
        } else {
            "no key selected"
        };
        put(buf, area.x + 1, area.y, label, theme.style(Token::Muted));
        return;
    };

    // A key confirmed gone before it was ever loaded — arrowed onto directly,
    // or requested while a different key was open. No value was ever read,
    // so there is nothing behind a type line, a size, a TTL, or even the
    // attachment disclosure below (which answers "is this pane about the row
    // you're on", a question that presupposes there is a pane's worth of
    // content to be about). One line: the name, and that it does not exist.
    //
    // Deliberately distinct from a key that *was* loaded and is deleted while
    // open — that key keeps its last value and the full header below, because
    // ADR-0006's "what was in it" question still has an answer for it. This
    // one never had one.
    let Some(value) = &open.value else {
        solid_divider(buf, area, standalone, theme);
        if standalone {
            let hint = state
                .keymap
                .hint(crate::keymap::Action::Cancel)
                .unwrap_or_default();
            let x1 = put(
                buf,
                area.x + 1,
                area.y,
                &format!("{hint} back"),
                theme.style(Token::Muted),
            );
            let x1 = put(buf, x1, area.y, "  ·  ", theme.style(Token::Border));
            put(buf, x1, area.y, &open.name, theme.style(Token::Text));
            put(
                buf,
                area.x + 1,
                area.y + 1,
                "✕ gone",
                theme.style(Token::Danger),
            );
        } else {
            put(
                buf,
                area.x + 1,
                area.y,
                &open.name,
                theme.style(Token::Text),
            );
            put_right(
                buf,
                area.x,
                area.y,
                area.width.saturating_sub(1),
                "✕ gone",
                theme.style(Token::Danger),
            );
        }
        return;
    };

    // Is this pane showing the key the cursor is on?
    let attachment = state.attachment();
    let detached = !matches!(attachment, None | Some(Attachment::Attached));

    // The wash. It cannot be painted underneath and then written over: `put`
    // resets the style of every cell it touches, so the background has to ride
    // on each style drawn in this pane. `sty` is how it does that, and it is
    // the reason nothing here calls `theme.style` directly.
    let wash = detached.then(|| theme.style(Token::SurfaceDetached));
    let sty = |token: Token| match wash {
        Some(w) => theme.style(token).patch(w),
        None => theme.style(token),
    };
    if let Some(w) = wash {
        // Fill first so the pane's empty space is washed too — below the body,
        // and to the right of every short line.
        let blank = " ".repeat(area.width as usize);
        for y in 0..area.height {
            put(buf, area.x, area.y + y, &blank, w);
        }
    }

    // The left rule separates keys from value in the two-pane layouts; there
    // is nothing to separate from when this pane is the entire screen. Dashed
    // while detached: a state of the whole right-hand side, readable without
    // being read, and a glyph rather than a hue so it survives monochrome.
    if !standalone {
        let x = area.x.saturating_sub(1);
        let rule = if detached { "┊" } else { "│" };
        for y in 0..area.height {
            put(buf, x, area.y + y, rule, theme.style(Token::Border));
        }
        // …and tied to the Open key's row where that row is on screen, so the
        // two panes are visibly one thing rather than two.
        match open_row {
            keys::OpenRowMark::At(y) => put(buf, x, y, "├", theme.style(Token::Warn)),
            // Scrolled out of the window: point the reader the right way rather
            // than leaving `not in view` to be searched for by hand.
            keys::OpenRowMark::Above if detached => {
                put(buf, x, area.y, "▲", theme.style(Token::Warn))
            }
            keys::OpenRowMark::Below if detached => put(
                buf,
                x,
                area.y + area.height.saturating_sub(1),
                "▼",
                theme.style(Token::Warn),
            ),
            _ => 0,
        };
    }

    // ── header: identical for every type (R3.1) ────────────────────────────
    let now = clock.now_ms();
    let x0 = area.x + 1;
    // Standalone (below 70 columns), the list this key came from is off
    // screen entirely — DESIGN §2's "breadcrumb replaces columns". The
    // effective binding, not a hard-coded key, per R7.5.
    let name_end = if standalone {
        let hint = state
            .keymap
            .hint(crate::keymap::Action::Cancel)
            .unwrap_or_default();
        let x1 = put(buf, x0, area.y, &format!("{hint} back"), sty(Token::Muted));
        put(buf, x1, area.y, "  ·  ", sty(Token::Border));
        put(buf, x1 + 5, area.y, &open.name, sty(Token::Text))
    } else {
        // The key name is this pane's header, and like the keys pane's column
        // header it carries the focus (DESIGN §4): `r` refetches here and
        // rescans there, so which pane has focus must be readable at a glance.
        // Standalone below 70 columns there is only one pane on screen, so it
        // always has focus and there is nothing to distinguish.
        let name_style = sty(if state.keys_pane_focused() {
            Token::Muted
        } else {
            Token::Text
        });
        put(buf, x0, area.y, &open.name, name_style)
    };

    // The chip. The wash says *that* the Viewer is off the cursor; this says it
    // in words, which is what makes the state survive monochrome and what makes
    // it mean something the first time it is seen.
    //
    // Drawn in both layouts. It used to sit inside the two-pane branch, which
    // left the one place it matters most with no signal at all: below 70
    // columns the divider, the tie glyph and the row underline are all gone by
    // construction — there is no second pane to carry them — so the wash was
    // the only thing left, and the wash is nothing in monochrome. Every fixture
    // written for this feature was 130 or 80 columns wide, so nothing caught it.
    //
    // Dropped before the key name is, following the title bar's rule: the name
    // is the pane's identity and is never sacrificed to a qualifier.
    if let Some(chips) = detached_chip(attachment) {
        let right = area.width.saturating_sub(1);
        for chip in chips {
            let width = chip.chars().count() as u16;
            // One space of daylight, so a long name and the chip can never read
            // as one string.
            if area.x + right.saturating_sub(width) > name_end {
                put_right(buf, area.x, area.y, right, chip, sty(Token::Warn));
                break;
            }
        }
    }

    let viewer = value.viewer();
    let kind = value.kind();
    // Consistent everywhere a type appears (DESIGN §5): the same hue the keys
    // pane's dot uses, here on the one word that names the type.
    let x1 = put(
        buf,
        x0,
        area.y + 1,
        kind.label(),
        sty(crate::theme::type_token(Some(kind))),
    );
    let x2 = put(
        buf,
        x1,
        area.y + 1,
        &format!(" · {}", viewer.measure()),
        sty(Token::Muted),
    );
    // `measure` is the value's real length; the body can only show what the
    // read brought back. When those differ, saying so is not decoration — it is
    // the difference between "this is all of it" and "this is the newest 500",
    // and it carries Warn rather than Muted for the same reason the scan-cap
    // banner does: a limit nobody notices is one they will mistake for the
    // whole. Both figures sit together so neither can be read without the other.
    let x3 = match viewer.window() {
        Some(shown) => put(
            buf,
            x2,
            area.y + 1,
            &format!(" · {shown} shown"),
            sty(Token::Warn),
        ),
        None => x2,
    };
    put(
        buf,
        x3,
        area.y + 1,
        &format!(" · {}", keys::format_size(open.size_bytes)),
        sty(Token::Muted),
    );

    // TTL is counted down locally: the most time-sensitive figure on screen
    // costs no round trip (R3.9).
    let ttl = keys::format_ttl(open.ttl_now(now));
    put(
        buf,
        x0,
        area.y + 2,
        &format!("ttl {ttl}"),
        sty(Token::Muted),
    );

    // A Refetch of this same key is in flight (invalidation re-arm, reconnect
    // re-arm, manual Refetch, …): the value already on screen stays exactly
    // as it is — clobbering it on every liveness read would be worse than
    // showing nothing — and only the status text says a read is outstanding.
    let refetching = state
        .open_pending
        .as_ref()
        .is_some_and(|p| p.name == open.name);
    let currency = if refetching {
        "⟳ fetching…".to_string()
    } else {
        open.currency(state.liveness() == Liveness::Live, now)
    };
    let token = if open.deleted_at_ms.is_some() {
        Token::Danger
    } else if open.pending.is_some() || open.editing {
        // Editing shares Warn with a held update: something to pay attention
        // to, nothing broken. Checked before the plain `●` case below, or an
        // idle edit with no pending change would render as plain green live.
        Token::Warn
    } else if currency.starts_with('●') {
        Token::Ok
    } else {
        Token::Muted
    };
    put_right(
        buf,
        area.x,
        area.y + 2,
        area.width.saturating_sub(1),
        &currency,
        sty(token),
    );
    // A held update needs a way to ask for it, and the hint must name the
    // effective binding (R7.5).
    if open.pending.is_some()
        && let Some(hint) = state.keymap.hint(crate::keymap::Action::Refetch)
    {
        put_right(
            buf,
            area.x,
            area.y + 3,
            area.width.saturating_sub(1),
            &format!("{hint} to load"),
            sty(Token::Muted),
        );
    }

    // ── body: the only part that differs by type ───────────────────────────
    let body_top = area.y + 4;
    let body_height = (area.y + area.height).saturating_sub(body_top);
    if body_height == 0 {
        return;
    }
    let cols = viewer.columns();
    let mut y = body_top;
    let inner = area.width.saturating_sub(2);
    let col_w = if cols.len() > 1 {
        inner / cols.len() as u16
    } else {
        inner
    };

    if !cols.is_empty() {
        for (i, heading) in cols.iter().enumerate() {
            put(buf, x0 + i as u16 * col_w, y, heading, sty(Token::Muted));
        }
        y += 1;
    }

    // Only visible rows are formatted, whatever the value's size.
    let rows = (area.y + area.height).saturating_sub(y) as usize;
    for r in 0..rows {
        let i = open.offset + r;
        if i >= viewer.row_count() {
            break;
        }
        for (c, cell) in viewer.row(i, now).into_iter().enumerate() {
            let width = if cols.len() > 1 { col_w } else { inner };
            put(
                buf,
                x0 + c as u16 * col_w,
                y + r as u16,
                &clip(&cell, width.saturating_sub(1) as usize),
                sty(if c == 0 && cols.len() > 1 {
                    Token::Muted
                } else {
                    Token::Text
                }),
            );
        }
    }
}

/// The value pane while a first Open's read has not answered yet.
///
/// No value exists to show, so the body fills with a dashed placeholder
/// rather than staying blank or holding over whatever key was open before —
/// the same "pending cell, no layout shift" idiom DESIGN §6.2 already uses
/// for async key-list metadata. The type isn't known yet either, so there
/// are no column headings to draw — just a name, a status, and rows that say
/// "something is coming" without claiming to know its shape.
fn opening_placeholder(
    state: &State,
    theme: &Theme,
    area: Rect,
    standalone: bool,
    pending: &PendingRead,
    buf: &mut Buffer,
) {
    solid_divider(buf, area, standalone, theme);

    let body_top = if standalone {
        let hint = state
            .keymap
            .hint(crate::keymap::Action::Cancel)
            .unwrap_or_default();
        let x1 = put(
            buf,
            area.x + 1,
            area.y,
            &format!("{hint} back"),
            theme.style(Token::Muted),
        );
        let x1 = put(buf, x1, area.y, "  ·  ", theme.style(Token::Border));
        put(buf, x1, area.y, &pending.name, theme.style(Token::Text));
        put(
            buf,
            area.x + 1,
            area.y + 1,
            "⟳ fetching…",
            theme.style(Token::Muted),
        );
        area.y + 2
    } else {
        put(
            buf,
            area.x + 1,
            area.y,
            &pending.name,
            theme.style(Token::Text),
        );
        put_right(
            buf,
            area.x,
            area.y,
            area.width.saturating_sub(1),
            "⟳ fetching…",
            theme.style(Token::Muted),
        );
        area.y + 1
    };

    let inner = area.width.saturating_sub(2) as usize;
    if inner == 0 {
        return;
    }
    let dash: String = "┈".repeat(inner);
    for y in body_top..area.y + area.height {
        put(buf, area.x + 1, y, &dash, theme.style(Token::Muted));
    }
}

/// The plain rule between the panes, with none of the dashing or the tie
/// glyph the attachment disclosure draws — for the two value-pane states that
/// precede it and have nothing to disclose: no key open, and a key confirmed
/// gone before it was ever loaded. Nothing to separate from when this pane is
/// the whole screen.
fn solid_divider(buf: &mut Buffer, area: Rect, standalone: bool, theme: &Theme) {
    if standalone {
        return;
    }
    let x = area.x.saturating_sub(1);
    for y in 0..area.height {
        put(buf, x, area.y + y, "│", theme.style(Token::Border));
    }
}

/// What the Viewer says when it is holding a key the cursor is not on, widest
/// phrasing first.
///
/// Deliberately about *identity*, never about freshness: the value on screen is
/// a live tracked read either way (ADR-0006), and CONTEXT.md bans "stale" as a
/// word for exactly the confusion it would cause here. The Selected key is the
/// glossary's term, so it is the term on screen.
///
/// Position is not stated here — the divider carries that, with `├` on the
/// Open key's row or `▲`/`▼` when it has scrolled out of the window. This says
/// what, the divider says where.
fn detached_chip(attachment: Option<Attachment>) -> Option<&'static [&'static str]> {
    match attachment? {
        Attachment::Attached => None,
        Attachment::Detached { .. } => Some(&["⊘ not the selected key", "⊘ not selected", "⊘"]),
        // No row to point at, so this one has to say why on its own.
        Attachment::DetachedOffList => Some(&["⊘ not in the list", "⊘ not listed", "⊘"]),
    }
}

fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return String::new();
    }
    let mut out: String = s.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// The status bar: scan progress and its cancel affordance (DESIGN §6.2).
fn status_bar(state: &State, theme: &Theme, clock: &dyn Clock, area: Rect, buf: &mut Buffer) {
    let readout = state.scan.readout();
    let quiet = readout.is_empty()
        && state.list.sort_readout().is_none()
        && state.notice_now(clock.now_ms()).is_none()
        && state.error_text().is_none();
    if quiet || area.height < 2 {
        return;
    }
    let y = area.height - if area.height >= 24 { 2 } else { 1 };
    let token = if state.error_text().is_some() {
        Token::Danger
    } else if state.notice_now(clock.now_ms()).is_some() {
        Token::Ok
    } else {
        match state.scan {
            crate::state::ScanState::Capped { .. } | crate::state::ScanState::Failed { .. } => {
                Token::Warn
            }
            _ => Token::Muted,
        }
    };
    let mut line = readout;
    if let Some(sort) = state.list.sort_readout() {
        line = format!("{line}   {sort}");
    }
    // A copy confirmation displaces the scan readout for a moment rather than
    // claiming another row (G7). A failure outranks both and stays until it is
    // dismissed, because an error nobody read is an error nobody handled.
    if let Some(notice) = state.notice_now(clock.now_ms()) {
        line = notice.to_string();
    }
    if let Some(error) = state.error_text() {
        let dismiss = state
            .keymap
            .hint(crate::keymap::Action::Cancel)
            .unwrap_or_default();
        line = format!("✕ {error}   {dismiss} dismiss");
    }
    let x = put(buf, 1, y, &line, theme.style(token));
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

    // Anything short of live owes the reader a fact. While a retry is actually
    // scheduled the useful one is when it lands: ADR-0009 requires the backoff
    // be visible, because a silent wait is a freeze wearing a different name.
    // With nothing scheduled the useful one is the Read age — the same fact
    // every other not-live state shows, and the one ADR-0009 names for a
    // dropped link. A countdown here would describe a schedule that does not
    // exist, which is the opposite of what that requirement is for.
    match &state.link {
        Link::Reconnecting {
            retry_in_ms: Some(ms),
            ..
        } => {
            out.push((format!(" · retry {}s", ms.div_ceil(1000)), Token::Muted));
        }
        _ if liveness != Liveness::Live => {
            out.push((format!(" · {}", read_age(state, clock)), Token::Muted));
        }
        _ => {}
    }
    // Offer `r` only where it can do something. Disconnected, a Refetch reads a
    // dead client and returns an error — the same reason a replica reads
    // `locked` rather than advertising `⌃R`: a key that cannot work is worse
    // than no key, because the reader spends the incident pressing it.
    if liveness == Liveness::Manual
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
    .map(|(a, k)| {
        format!(
            "{} {}",
            key_label(&k),
            a.label_in(state.keys_pane_focused())
        )
    })
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
    let color = |c: Option<ratatui::style::Color>, label: &str| match c {
        None => format!("{label}=none"),
        Some(ratatui::style::Color::Rgb(r, g, b)) => format!("{label}=#{r:02x}{g:02x}{b:02x}"),
        Some(ratatui::style::Color::Indexed(i)) => format!("{label}=ansi{i}"),
        Some(other) => format!("{label}={other:?}"),
    };
    let fg = color(s.fg, "fg");
    // A background is rare enough (only Token::Selected sets one) that it is
    // worth calling out explicitly rather than silently dropping it, which is
    // what this function did before the selection-highlight feature existed.
    let bg = match s.bg {
        None => String::new(),
        Some(_) => format!(" {}", color(s.bg, "bg")),
    };
    let mods = if s.add_modifier.is_empty() {
        String::new()
    } else {
        format!(" {:?}", s.add_modifier)
    };
    format!("{fg}{bg}{mods}")
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
