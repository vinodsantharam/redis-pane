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
use ratatui::widgets::Widget;

use crate::clock::Clock;
use crate::keymap::{Action, key_label};
use crate::state::value::{Value, format_score};
use crate::state::{
    Attachment, EditBuffer, EditTarget, FieldPart, Link, Liveness, PendingMutation, PendingRead,
    State, is_valid_zset_score,
};
use crate::theme::{Theme, Token, env_token};
use crate::update::{Mode, mode};

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
    // Not `mode()`: drawing the dialog is not a precedence question. The
    // overlay is drawn last, so it is on top whenever one is staged, and
    // asking the mode here would only buy an `expect` on the way to the
    // `PendingMutation` this needs anyway.
    if let Some(pending) = &state.confirm {
        confirm_overlay(state, pending, theme, area, &mut buf);
    }
    buf
}

/// The value pane. Viewers land in M1.8; until then it states what is selected
/// so the two-pane layout is real rather than a promise.
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
    let now = clock.now_epoch_ms();

    // A read is in flight for a key that isn't (yet) what's on screen — a
    // first Open, or a switch to a different key while another's value was
    // showing. Neither the previous value nor a blank "no key selected" pane
    // says anything happened, so this shows a placeholder instead — the
    // keyspace browser's "pending cell, no layout shift" idiom (DESIGN §6.2),
    // applied to the body. A Refetch of the key already on screen (same name)
    // is handled below instead, without disturbing the value in view.
    //
    // Gated on `issued_at_ms`: a read that lands inside `APPEAR_DELAY_MS`
    // never shows this at all, which is what a fast local Redis needs — the
    // indicator used to flash on and off within a frame or two on almost
    // every keypress, reading as a glitch rather than feedback.
    if let Some(pending) = &state.open_pending
        && state.open.as_ref().map(|o| &o.name) != Some(&pending.name)
        && pending
            .issued_at_ms
            .is_some_and(|t| now.saturating_sub(t) >= PendingRead::APPEAR_DELAY_MS)
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
            put(
                buf,
                x1,
                area.y,
                &open.name.display(),
                theme.style(Token::Text),
            );
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
                &open.name.display(),
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
        put(buf, x1 + 5, area.y, &open.name.display(), sty(Token::Text))
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
        put(buf, x0, area.y, &open.name.display(), name_style)
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
    let ttl_end = put(
        buf,
        x0,
        area.y + 2,
        &format!("ttl {ttl}"),
        sty(Token::Muted),
    );
    // Whether the inline editor's current text still parses as JSON, when
    // the value being edited was JSON-classified to begin with (ADR-0014) —
    // the live counterpart to the confirm dialog's `⚠ no longer valid JSON`.
    if let Some(valid) = open.editor().and_then(EditBuffer::json_valid) {
        let (label, token) = if valid {
            (" · json ✓", Token::Muted)
        } else {
            (" · json ✗", Token::Warn)
        };
        put(buf, ttl_end, area.y + 2, label, sty(token));
    }

    // A Refetch of this same key is in flight (invalidation re-arm, reconnect
    // re-arm, manual Refetch, …): the value already on screen stays exactly
    // as it is — clobbering it on every liveness read would be worse than
    // showing nothing — and only the status text says a read is outstanding.
    // Gated on `issued_at_ms` the same way the First-Open placeholder is: a
    // Refetch that lands inside `APPEAR_DELAY_MS` never touches the header at
    // all, which matters here even more than for First Open — an invalidation
    // re-arm fires on every write to the key, so an ungated indicator would
    // have flickered on essentially every keystroke against a live server.
    let refetching = state.open_pending.as_ref().is_some_and(|p| {
        p.name == open.name
            && p.issued_at_ms
                .is_some_and(|t| now.saturating_sub(t) >= PendingRead::APPEAR_DELAY_MS)
    });
    let currency = if refetching {
        "⟳ fetching…".to_string()
    } else {
        open.currency(state.liveness() == Liveness::Live, now)
    };
    let token = if open.deleted_at_ms.is_some() {
        Token::Danger
    } else if open.pending.is_some() || open.is_editing() {
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

    // The inline editor takes over the whole body in place of the viewer's
    // own rows (ADR-0014) — the same frame, header included, with an
    // unsaved buffer where the read-only rows would otherwise be.
    //
    // The stored `TextArea` is drawn itself, never a clone: drawing is where
    // it learns the pane's width and keeps its scroll position, and without
    // that width `Up`/`Down` move by whole lines, which in a token is none.
    // The text colour is painted underneath instead of set on the widget,
    // since setting a style needs `&mut` and render only borrows `State`.
    if let Some(editor) = open.editor() {
        // The TTL capture (D3, D11, D13, ADR-0019): one hand-painted line,
        // no FIELD/VALUE split — `field_name()` answers `None` for this
        // target on purpose (it feeds the Hash/ZSet shown-duplicate checks,
        // which a duration expression has no analogue of), so this has to
        // be checked ahead of that branch rather than folded into it.
        if let EditTarget::Ttl { text } = editor.target() {
            put(buf, x0, body_top, "TTL", sty(Token::Muted));
            let content_x = x0 + 1 + "TTL".len() as u16 + 1;
            let text_end = put(buf, content_x, body_top, text, sty(Token::Text));
            if text_end < area.x + area.width {
                let cell = &mut buf[(text_end, body_top)];
                cell.set_symbol(" ");
                cell.set_style(sty(Token::Selected));
            }
            if body_height < 2 {
                return;
            }
            // The resolution line (D3, D13): the live indicator for a TTL
            // capture lives here, not in the hint bar — a TTL edit has a
            // resolved *value* to show, not just a valid/invalid bit
            // (ADR-0018 D4 made the hint bar carry that for a score; this
            // diverges on purpose, per ADR-0019 D13). Runs the same
            // clock-free grammar the `⌃S` block in `update` uses (D12), but
            // against the counted-down TTL, which is what a reader looking
            // at the screen while typing actually wants to see.
            let line_row = body_top + 1;
            let current = open.ttl_now(now);
            // An untouched, empty capture on a key that already has no
            // expiry is the field's resting state for such a key (D11), not
            // a mistake — `resolve_ttl_edit` would correctly refuse it as
            // `AlreadyNever` (empty parses as persist, D4's table), but
            // showing that refusal on a field nobody has typed into yet
            // reads as an accusation. So this one case shows the grammar
            // itself instead, the same "teach by placeholder" idiom the
            // Hash/ZSet add form's empty VALUE part already uses
            // (`·· Enter to write the value` above). `⌃S` is unaffected —
            // it still runs the real `resolve_ttl_edit` and still refuses.
            let line = if text.is_empty() && current == crate::state::loaded::TTL_NONE {
                "·· 5m · +30m · never".to_string()
            } else {
                match crate::state::ttl::parse_ttl_edit(text) {
                    Ok(edit) => {
                        // The verb is read off what was *typed* (the sign
                        // on a shift), not off the resulting figure — a
                        // resulting TTL equal to `current` would otherwise
                        // be ambiguous between the two.
                        let verb = match edit {
                            crate::state::ttl::TtlEdit::Set(_) => "set",
                            crate::state::ttl::TtlEdit::Persist => "persist",
                            crate::state::ttl::TtlEdit::Shift(delta) if delta >= 0 => "extend",
                            crate::state::ttl::TtlEdit::Shift(_) => "shorten",
                        };
                        match crate::state::ttl::resolve_ttl_edit(edit, current) {
                            Ok(outcome) => {
                                let new_text = match outcome {
                                    crate::state::ttl::TtlOutcome::Set(s) => {
                                        keys::format_duration(s)
                                    }
                                    crate::state::ttl::TtlOutcome::Persist => "never".to_string(),
                                    crate::state::ttl::TtlOutcome::Shift(s) => {
                                        keys::format_duration(s)
                                    }
                                };
                                format!(
                                    "·· {verb} · {} → {}",
                                    keys::format_duration(current),
                                    new_text
                                )
                            }
                            Err(refusal) => format!("·· {}", refusal.reason()),
                        }
                    }
                    Err(refusal) => format!("·· {}", refusal.reason()),
                }
            };
            put(buf, content_x, line_row, &line, sty(Token::Muted));
            return;
        }
        if let Some(name) = editor.field_name() {
            // The two-part FIELD/VALUE form (PLAN M2 task 6 follow-up, F):
            // adding a field shows both, name first; editing an existing one
            // shows both too, but FIELD is fixed (renaming is a follow-up)
            // and VALUE is always the active half. The active half carries a
            // `▌` marker before its label — a glyph, not a colour alone, so
            // which half is "live" survives monochrome (DESIGN §5).
            // Layout follows which part is active; the `▌` marker itself is
            // suppressed once the buffer is staged — under the confirm
            // dialog the form is frozen (D3), so nothing on it should still
            // read as "the one taking keys right now".
            let name_part = editor.active_part() == Some(FieldPart::Name);
            let value_part = !name_part;
            let show_marker = mode(state) == Mode::Editing;
            let name_active = show_marker && name_part;
            let value_active = show_marker && value_part;
            // The ZSet add form's two parts are a MEMBER and a SCORE, not a
            // FIELD and a VALUE (D6, ADR-0018) — the labels have to say so,
            // or the on-screen form contradicts the hint bar directly above
            // it (which already says "member"/"score") and every doc
            // comment describing this form. `MEMBER` (6 chars) is the widest
            // label either form uses, so the column gap is sized off
            // whichever pair is actually on screen rather than a fixed
            // constant that was only ever true for FIELD/VALUE.
            let is_zset_add = matches!(editor.target(), EditTarget::NewZSetMember { .. });
            let (name_label, value_label) = if is_zset_add {
                ("MEMBER", "SCORE")
            } else {
                ("FIELD", "VALUE")
            };
            let label_w = name_label.len().max(value_label.len()) as u16;
            let content_x = x0 + 1 + label_w + 2;
            let mark = |active: bool| if active { "▌" } else { " " };
            let mark_token = |active: bool| {
                if active {
                    Token::Selected
                } else {
                    Token::Muted
                }
            };

            put(
                buf,
                x0,
                body_top,
                mark(name_active),
                sty(mark_token(name_active)),
            );
            put(buf, x0 + 1, body_top, name_label, sty(Token::Muted));
            let name_end = put(buf, content_x, body_top, name, sty(Token::Text));
            if name_active {
                // The same cursor cell the capture line used to draw.
                if name_end < area.x + area.width {
                    let cell = &mut buf[(name_end, body_top)];
                    cell.set_symbol(" ");
                    cell.set_style(sty(Token::Selected));
                }
                let duplicate = if is_zset_add {
                    open.zset_member_shown_duplicate()
                } else {
                    open.hash_field_shown_duplicate()
                };
                if duplicate {
                    put(buf, name_end + 2, body_top, "⚠ exists", sty(Token::Warn));
                }
            }

            if body_height < 2 {
                return;
            }
            let value_row = body_top + 1;
            put(
                buf,
                x0,
                value_row,
                mark(value_active),
                sty(mark_token(value_active)),
            );
            put(buf, x0 + 1, value_row, value_label, sty(Token::Muted));

            if value_part {
                let editor_area = Rect::new(
                    content_x,
                    value_row,
                    (area.x + area.width).saturating_sub(content_x + 1).max(1),
                    body_height - 1,
                );
                buf.set_style(editor_area, sty(Token::Text));
                editor.widget().render(editor_area, buf);
                repaint_reversed_cursor(buf, editor_area, sty(Token::Selected));
            } else {
                // The name part is active: no multi-row editor yet, just a
                // one-line preview of VALUE so far — empty on a fresh add, so
                // this is also where the placeholder tells the reader what
                // comes next.
                let text = editor.text();
                let first_line =
                    String::from_utf8_lossy(text.split(|&b| b == b'\n').next().unwrap_or(&[]))
                        .into_owned();
                if first_line.is_empty() {
                    put(
                        buf,
                        content_x,
                        value_row,
                        "·· Enter to write the value",
                        sty(Token::Muted),
                    );
                } else {
                    put(buf, content_x, value_row, &first_line, sty(Token::Text));
                }
            }
            return;
        }

        // A plain String edit (`EditTarget::Value`): the editor takes over
        // the whole body, unchanged since M2 task 4 (ADR-0014).
        let editor_area = Rect::new(x0, body_top, area.width.saturating_sub(2), body_height);
        buf.set_style(editor_area, sty(Token::Text));
        editor.widget().render(editor_area, buf);
        repaint_reversed_cursor(buf, editor_area, sty(Token::Selected));
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
        // The value cursor (`Enter`/`Action::EnterValueCursor`), highlighted
        // the same way the keys pane highlights its own selected row — one
        // visual language for "the row you're on", not two.
        let cursor_row = open.cursor_active && i == open.cursor;
        if cursor_row {
            let blank = " ".repeat(inner as usize);
            put(buf, x0, y + r as u16, &blank, sty(Token::Selected));
        }
        for (c, cell) in viewer.row(i, now).into_iter().enumerate() {
            let width = if cols.len() > 1 { col_w } else { inner };
            put(
                buf,
                x0 + c as u16 * col_w,
                y + r as u16,
                &clip(&cell, width.saturating_sub(1) as usize),
                sty(if cursor_row {
                    Token::Selected
                } else if c == 0 && cols.len() > 1 {
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
        put(
            buf,
            x1,
            area.y,
            &pending.name.display(),
            theme.style(Token::Text),
        );
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
            &pending.name.display(),
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
        && state.notice_now(clock.now_epoch_ms()).is_none()
        && state.error_text().is_none();
    if quiet || area.height < 2 {
        return;
    }
    let y = area.height - if area.height >= 24 { 2 } else { 1 };
    let token = if state.error_text().is_some() {
        Token::Danger
    } else if state.notice_now(clock.now_epoch_ms()).is_some() {
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
    if let Some(notice) = state.notice_now(clock.now_epoch_ms()) {
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

/// How many lines of a String value's old/new side the diff block in
/// [`confirm_overlay`] shows before saying how many more there are. A stacked
/// old/new block, not a real line-level diff (v1 — DESIGN §6.5 asks only for
/// "see the real change before it runs", not a specific diff algorithm).
const MAX_DIFF_LINES: usize = 8;

/// Truncate from the *right*, keeping the start — the complement of
/// [`truncate_left`]. Diff content is read left-to-right like code, so what
/// distinguishes one line from the next is usually at the front.
fn truncate_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len <= width {
        return s.to_string();
    }
    if width <= 1 {
        return String::new();
    }
    let keep = width - 1;
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Renders one side of a String-edit diff into `lines`: up to
/// [`MAX_DIFF_LINES`] rows of `bytes`, each prefixed with `mark`, plus a
/// "N more lines" footer when it was longer than that.
fn push_diff_side(lines: &mut Vec<(String, Token)>, mark: &str, bytes: &[u8], token: Token) {
    let text = String::from_utf8_lossy(bytes);
    let rows: Vec<&str> = text.split('\n').collect();
    for row in rows.iter().take(MAX_DIFF_LINES) {
        lines.push((format!("{mark} {row}"), token));
    }
    if rows.len() > MAX_DIFF_LINES {
        lines.push((
            format!("  … {} more lines", rows.len() - MAX_DIFF_LINES),
            Token::Muted,
        ));
    }
}

/// The `old` side of a TTL dialog's `old → new` line (PLAN M2 task 10, D9).
///
/// Distinct from [`keys::format_duration`] in exactly one case, which is the
/// case that matters: `TTL_NONE` is `-1`, and `format_duration` clamps a
/// negative to `0s`. A key with no expiry therefore rendered as
/// `ttl 0s → 5m` — telling the reader it was about to expire, directly above
/// the `⚠ this key had no expiry` warning telling them it never would. The
/// `new` side never needs this: a write's resulting TTL is either a real
/// duration or the literal word `never`, spelled out by its own arm.
fn ttl_before(seconds: i32) -> String {
    if seconds == crate::state::loaded::TTL_NONE {
        "never".to_string()
    } else {
        keys::format_duration(seconds)
    }
}

/// The mutation-preview dialog (R4.4, DESIGN §6.5).
///
/// Composes the real command first, and only then says whether Read-only
/// Mode will let it run — the reader learns what they were about to do
/// before they learn they are not allowed to, never the other way round.
fn confirm_overlay(
    state: &State,
    pending: &PendingMutation,
    theme: &Theme,
    area: Rect,
    buf: &mut Buffer,
) {
    let refused = state.read_only;
    let hint = match refused {
        Some(reason) => format!("read-only ({}) · Esc dismiss", reason.label()),
        None => "y confirm · Esc cancel".to_string(),
    };

    let mut lines: Vec<(String, Token)> = Vec::new();
    match pending {
        PendingMutation::DeleteKey { .. } => {
            lines.push((pending.command_text(), Token::Text));
        }
        PendingMutation::SetString { name, old, new, .. } => {
            // The value itself is the `+` side of the diff below.
            lines.push((format!("SET {} KEEPTTL XX", name.display()), Token::Text));
            push_diff_side(&mut lines, "-", old, Token::Danger);
            push_diff_side(&mut lines, "+", new, Token::Ok);
            if pending.json_warning() == Some(true) {
                lines.push(("⚠ no longer valid JSON".to_string(), Token::Warn));
            }
        }
        // Guarded Hash writes (D1, D2, ADR-0015): the command line is the
        // effective command, never the `EVAL "<script>" …` it is actually
        // sent as — unreadable in the dialog — followed by the one muted
        // guard line naming what the script checks before it writes.
        PendingMutation::SetHashField { old, new, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard.to_string(), Token::Muted));
            }
            push_diff_side(&mut lines, "-", old, Token::Danger);
            push_diff_side(&mut lines, "+", new, Token::Ok);
            if pending.json_warning() == Some(true) {
                lines.push(("⚠ no longer valid JSON".to_string(), Token::Warn));
            }
        }
        PendingMutation::AddHashField { value, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard.to_string(), Token::Muted));
            }
            // `+` side only: there is no prior value to diff against.
            push_diff_side(&mut lines, "+", value, Token::Ok);
        }
        PendingMutation::DeleteHashField { last_field, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if *last_field {
                lines.push((
                    "last field — the key will be deleted".to_string(),
                    Token::Warn,
                ));
            }
        }
        // Guarded Set write (D2, D3, ADR-0016): same shape as the Hash add
        // above, one part narrower — a member has no name half to show.
        PendingMutation::AddSetMember { member, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard.to_string(), Token::Muted));
            }
            push_diff_side(&mut lines, "+", member, Token::Ok);
        }
        PendingMutation::DeleteSetMember { last_member, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if *last_member {
                lines.push((
                    "last member — the key will be deleted".to_string(),
                    Token::Warn,
                ));
            }
        }
        // Guarded List writes (D2, D3, D5, D6, ADR-0017): the same shape as
        // the Hash/Set arms above. Not reachable until PLAN M2 task 8 phase
        // 3 wires `e`/`a`/`d` for a List — no golden frame exercises these
        // yet, since nothing stages any of the three variants — the arms
        // exist now because `PendingMutation` is matched exhaustively.
        PendingMutation::SetListElement { old, new, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            push_diff_side(&mut lines, "-", old, Token::Danger);
            push_diff_side(&mut lines, "+", new, Token::Ok);
            if pending.json_warning() == Some(true) {
                lines.push(("⚠ no longer valid JSON".to_string(), Token::Warn));
            }
        }
        PendingMutation::AddListElement { value, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            push_diff_side(&mut lines, "+", value, Token::Ok);
        }
        PendingMutation::DeleteListElement { last_element, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if *last_element {
                lines.push((
                    "last element — the key will be deleted".to_string(),
                    Token::Warn,
                ));
            }
        }
        // ZSet score edit (D1, D2, D7, ADR-0018): not reachable until PLAN
        // M2 task 9 phase 3 wires `e` for a ZSet — no golden frame exercises
        // this yet, since nothing stages `SetZSetScore` — the arm exists now
        // because `PendingMutation` is matched exhaustively (PLAN M2 task 8,
        // D8). A score diff, deliberately distinct from a membership diff
        // (ADR-0018's preview requirement, PLAN row 9's "Proves"): the
        // member is shown once, plainly, since it never changes (D1) — not
        // as a `-`/`+` side, which would misread as the member itself being
        // replaced — and only the score gets an `old → new` line.
        PendingMutation::SetZSetScore {
            member,
            old_score,
            new_score,
            ..
        } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            lines.push((
                format!("member {}", String::from_utf8_lossy(member)),
                Token::Text,
            ));
            lines.push((
                format!(
                    "score {} → {}",
                    format_score(*old_score),
                    format_score(*new_score)
                ),
                Token::Text,
            ));
        }
        // ZSet add (D2, D3, D6, ADR-0018): same shape as the Hash/Set add
        // arms above — the `+` side carries the whole member+score pair,
        // since both are new (ADR-0018's preview requirement).
        PendingMutation::AddZSetMember { member, score, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            push_diff_side(
                &mut lines,
                "+",
                format!(
                    "{} {}",
                    String::from_utf8_lossy(member),
                    format_score(*score)
                )
                .as_bytes(),
                Token::Ok,
            );
        }
        PendingMutation::DeleteZSetMember {
            member,
            last_member,
            ..
        } => {
            lines.push((pending.command_text(), Token::Text));
            push_diff_side(&mut lines, "-", member, Token::Danger);
            if *last_member {
                lines.push((
                    "last member — the key will be deleted".to_string(),
                    Token::Warn,
                ));
            }
        }
        // TTL edit (D1, D6, D8, D9, ADR-0019): not reachable until PLAN M2
        // task 10 phase 3 wires `t` in the value pane — no golden frame
        // exercises this yet, since nothing stages a `SetTtl`/`PersistTtl`/
        // `ShiftTtl` — the arms exist now because `PendingMutation` is
        // matched exhaustively (PLAN M2 task 8, D8). A scalar `old → new`
        // line, the shape closest to this (ADR-0018's `SetZSetScore` arm
        // above): a TTL is metadata, not a byte diff.
        //
        // The `old` side goes through `ttl_before`, not `format_duration`.
        // `TTL_NONE` is `-1`, and `format_duration` clamps a negative to
        // `0s` — so a key that never expires rendered as `ttl 0s → 5m`,
        // claiming it was about to expire, on the line directly above the
        // `⚠ this key had no expiry` warning saying the opposite. Two
        // adjacent lines contradicting each other, in the dialog where the
        // reader decides. Caught by this task's first golden frame of this
        // arm; see `ttl_before`.
        PendingMutation::SetTtl {
            old_ttl, new_ttl, ..
        } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            lines.push((
                format!(
                    "ttl {} → {}",
                    ttl_before(*old_ttl),
                    keys::format_duration(*new_ttl)
                ),
                Token::Text,
            ));
            // D9: the risky direction — a permanent key becoming disposable,
            // the TTL analogue of a last-member warning. Deliberately not a
            // warning for a short resulting TTL, which is a normal thing to
            // ask for (D9's explicit rejection).
            if *old_ttl == crate::state::loaded::TTL_NONE {
                lines.push(("⚠ this key had no expiry".to_string(), Token::Warn));
            }
        }
        PendingMutation::PersistTtl { old_ttl, .. } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            lines.push((format!("ttl {} → never", ttl_before(*old_ttl)), Token::Text));
        }
        PendingMutation::ShiftTtl {
            old_ttl,
            delta_seconds,
            ..
        } => {
            lines.push((pending.command_text(), Token::Text));
            if let Some(guard) = pending.guard_text() {
                lines.push((guard, Token::Muted));
            }
            // A local courtesy figure for display only (D4, D7) — the
            // server applies the delta to the TTL as it sees it at write
            // time, which the post-write refetch is what actually shows.
            let resulting = (i64::from(*old_ttl) + i64::from(*delta_seconds)).max(0);
            let resulting = resulting.min(i64::from(i32::MAX)) as i32;
            lines.push((
                format!(
                    "ttl {} → {}",
                    ttl_before(*old_ttl),
                    keys::format_duration(resulting)
                ),
                Token::Text,
            ));
        }
    }
    let hint_token = if refused.is_some() {
        Token::Danger
    } else {
        Token::Text
    };
    // Kept separate from `lines` rather than pushed onto the end: the hint is
    // how the dialog is dismissed or confirmed, so it must always be the last
    // thing drawn, never a line a tall diff pushes past the bottom of a short
    // terminal.
    let hint_line = (hint, hint_token);

    // Capped well short of the frame, so one long JSON line never turns the
    // dialog into the whole screen — width and line count are both bounded,
    // so render cost here is a function of the cap, not of the value.
    let max_w = (area.width as usize).saturating_sub(6).clamp(10, 100);
    let inner_w = lines
        .iter()
        .chain(std::iter::once(&hint_line))
        .map(|(l, _)| l.chars().count().min(max_w))
        .max()
        .unwrap_or(10);
    let w = (inner_w + 4).min(area.width as usize);
    // +1 content lines, +1 hint, +4 for the border/title rows.
    let h = (lines.len() + 5).min(area.height as usize);
    let x0 = (area.width as usize - w) / 2;
    let y0 = (area.height as usize - h) / 2;

    let border = theme.style(if refused.is_some() {
        Token::Danger
    } else {
        Token::Warn
    });
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
        " confirm ",
        theme.style(Token::Text),
    );
    // The box may be shorter than there are lines to show (a value taller
    // than the terminal) — draw what fits, but the hint always gets the last
    // visible row: it is how the dialog is dismissed or confirmed, never the
    // thing a tall diff is allowed to push off screen.
    let visible_rows = h.saturating_sub(3);
    if visible_rows == 0 {
        return;
    }
    let content_rows = visible_rows.saturating_sub(1);
    for (i, (line, token)) in lines.iter().take(content_rows).enumerate() {
        let text = truncate_right(line, w.saturating_sub(4));
        put(
            buf,
            x0 as u16 + 2,
            (y0 + 2 + i) as u16,
            &text,
            theme.style(*token),
        );
    }
    let hint_text = truncate_right(&hint_line.0, w.saturating_sub(4));
    put(
        buf,
        x0 as u16 + 2,
        (y0 + 2 + visible_rows - 1) as u16,
        &hint_text,
        theme.style(hint_line.1),
    );
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
    // Offer `r` wherever it can do something. It used to be withheld while
    // disconnected on the reasoning that a Refetch reads a dead client and
    // returns an error — true when disconnected meant nothing could reconnect
    // it, but ADR-0009 promises `r` retries immediately rather than waiting
    // out the timer, and now it does (`Action::Refetch`, `update.rs`): the
    // key does something disconnected too, so it belongs here too.
    if liveness != Liveness::Live
        && let Some(hint) = state.keymap.hint(Action::Refetch)
    {
        out.push((format!("  {hint}"), Token::Muted));
    }
    out
}

/// The hint bar: the effective binding for each action, never a hard-coded
/// label (R7.5).
///
/// Filter capture is the one input mode that already bypasses the keymap
/// (`update::filter_key` matches `KeyCode` directly, not through `Action`),
/// so there is no binding to look up here either — the hint has to be
/// hard-coded too, or the fact that Esc clears and exits stays invisible.
pub fn hint_bar(state: &State) -> String {
    if mode(state) == Mode::Filtering {
        return "Esc clear & exit   Enter apply".to_string();
    }
    // The inline editor is a mode of its own (ADR-0014), the same way filter
    // capture is above: hard-coded wording, effective bindings looked up
    // from the keymap so a rebinding still shows correctly (R7.5). The add
    // form's two parts each get their own wording (PLAN M2 task 6
    // follow-up, F/N) — the name part shares the editor's rank but not its
    // vocabulary, since `⌃S`/`↑` mean nothing there yet.
    if let Some(editor) = state.open.as_ref().and_then(crate::state::OpenKey::typing) {
        let cancel = state.keymap.hint(Action::Cancel).unwrap_or_default();
        // `Enter` stages every target except a plain String/JSON value
        // (2026-09-22 amendment to ADR-0014) — the hint names whichever key
        // actually does it, per target, rather than always naming the
        // keymap's `EditorStage` binding. `Enter` itself is not a rebindable
        // action (ADR-0014: `⌃S` was chosen over it precisely because no
        // terminal reliably tells "commit" from "insert a newline"), so it is
        // spelled out here the same way the name part's own "Enter value"
        // wording already is, a few lines below.
        let is_value = matches!(editor.target(), EditTarget::Value);
        // D6, ADR-0018: the ZSet add form's name part is a member, not a
        // Hash field — its own noun and its own duplicate check
        // ([`crate::state::OpenKey::zset_member_shown_duplicate`]), but the
        // same two-part shape, so it shares this match arm rather than
        // duplicating it.
        let is_zset_add = matches!(editor.target(), EditTarget::NewZSetMember { .. });
        match editor.active_part() {
            Some(FieldPart::Name) => {
                let duplicate = state.open.as_ref().is_some_and(|o| {
                    if is_zset_add {
                        o.zset_member_shown_duplicate()
                    } else {
                        o.hash_field_shown_duplicate()
                    }
                });
                if is_zset_add {
                    return if duplicate {
                        let edit = state.keymap.hint(Action::Edit).unwrap_or_default();
                        format!("member exists — {cancel}, then {edit} its score")
                    } else {
                        format!("Enter score · {cancel} cancel")
                    };
                }
                return if duplicate {
                    let edit = state.keymap.hint(Action::Edit).unwrap_or_default();
                    format!("field exists — {cancel}, then {edit} to edit")
                } else {
                    format!("Enter value · {cancel} cancel")
                };
            }
            Some(FieldPart::Value) => {
                // Reachable for the Hash add form's value part and the ZSet
                // add form's score part — the name part returns above — so
                // `is_value` is always false here and `Enter` always stages,
                // once D4's numeric guard (ZSet only) allows it.
                let undo = state.keymap.hint(Action::EditorUndo).unwrap_or_default();
                let back = if is_zset_add { "member" } else { "field" };
                if is_zset_add && !is_valid_zset_score(&String::from_utf8_lossy(&editor.text())) {
                    // D4's live indicator: `⌃S`/`Enter` are blocked
                    // (`value_part_stage_blocked`) while this holds, so the
                    // hint says so rather than silently doing nothing.
                    return format!("invalid score · ↑ {back} · {undo} undo · {cancel} cancel");
                }
                return format!("Enter stage · ↑ {back} · {undo} undo · {cancel} cancel");
            }
            // The List add form (D6, ADR-0017): `Tab` flips Head/Tail rather
            // than inserting a tab character (`editor_key`), so the hint
            // names it explicitly — nothing else on this bar mentions `Tab`
            // at all, and a reader would otherwise have no way to discover
            // it short of trying it.
            None if matches!(editor.target(), EditTarget::NewListElement { .. }) => {
                let undo = state.keymap.hint(Action::EditorUndo).unwrap_or_default();
                return format!("Enter stage   Tab head/tail   {undo} undo   {cancel} cancel");
            }
            // A ZSet score edit — existing member (D1, D4, ADR-0018): no
            // `MEMBER`/`SCORE` split (`active_part` is `None` the same way a
            // Hash field edit's is), but the score's own numeric guard
            // still applies, with the same live indicator the add form's
            // score part shows above.
            None if matches!(editor.target(), EditTarget::ZSetScore { .. }) => {
                let stage = state.keymap.hint(Action::EditorStage).unwrap_or_default();
                let undo = state.keymap.hint(Action::EditorUndo).unwrap_or_default();
                if !is_valid_zset_score(&String::from_utf8_lossy(&editor.text())) {
                    return format!("invalid score · {undo} undo · {cancel} cancel");
                }
                return format!("{stage} stage   {undo} undo   {cancel} cancel");
            }
            // The TTL capture (D13, ADR-0019): the bar stays constant here,
            // deliberately diverging from the ZSet score edit's live
            // invalid/valid indicator just above — a TTL edit always
            // resolves to a shown value (or a named refusal), and that
            // belongs on the resolution line under the field, not in the
            // bar (D13). `never persists` is spelled out because typing
            // nothing is itself a valid write here, unlike every other
            // capture on this bar.
            None if matches!(editor.target(), EditTarget::Ttl { .. }) => {
                let stage = state.keymap.hint(Action::EditorStage).unwrap_or_default();
                return format!("{stage} apply · never persists · {cancel} cancel");
            }
            None => {
                let stage = state.keymap.hint(Action::EditorStage).unwrap_or_default();
                let undo = state.keymap.hint(Action::EditorUndo).unwrap_or_default();
                return if is_value {
                    format!("{stage} stage   {undo} undo   {cancel} cancel")
                } else {
                    format!("Enter stage   {undo} undo   {cancel} cancel")
                };
            }
        }
    }
    // A Hash with the value cursor on a row, *and the value pane focused*:
    // `e`/`a`/`d` all mean something there (D4), and the hint names the
    // effective binding for each (R7.5). `Tab` (`Action::CyclePane`) can move
    // focus back to the keys pane without clearing `cursor_active`, and `d`
    // there is `DeleteKey`, not `HDEL` — the hint must not claim `remove`
    // for a `d` that is about to stage something else entirely.
    if !state.keys_pane_focused()
        && state
            .open
            .as_ref()
            .is_some_and(|o| o.cursor_active && matches!(o.value, Some(Value::Hash(_))))
    {
        let edit = state.keymap.hint(Action::Edit).unwrap_or_default();
        let add = state.keymap.hint(Action::Add).unwrap_or_default();
        let remove = state.keymap.hint(Action::Delete).unwrap_or_default();
        return format!("{edit} edit · {add} add · {remove} remove");
    }
    // A Set with the value cursor on a row, and the value pane focused: only
    // `a`/`d` mean anything (D1, ADR-0016) — `e` always refuses, since a
    // member has no name half to keep while "the value changes" and there is
    // no in-place edit to hint at. Naming `edit` here would promise a mode
    // `open_editor` never opens.
    if !state.keys_pane_focused()
        && state
            .open
            .as_ref()
            .is_some_and(|o| o.cursor_active && matches!(o.value, Some(Value::Set(_))))
    {
        let add = state.keymap.hint(Action::Add).unwrap_or_default();
        let remove = state.keymap.hint(Action::Delete).unwrap_or_default();
        return format!("{add} add · {remove} remove");
    }
    // A List with the value cursor on a row, and the value pane focused: all
    // three mean something here, like a Hash (D1, ADR-0017) — an element has
    // an identity, its index, that survives its bytes changing, so `e` is a
    // real in-place edit rather than the refusal it is on a Set.
    if !state.keys_pane_focused()
        && state
            .open
            .as_ref()
            .is_some_and(|o| o.cursor_active && matches!(o.value, Some(Value::List(_))))
    {
        let edit = state.keymap.hint(Action::Edit).unwrap_or_default();
        let add = state.keymap.hint(Action::Add).unwrap_or_default();
        let remove = state.keymap.hint(Action::Delete).unwrap_or_default();
        return format!("{edit} edit · {add} add · {remove} remove");
    }
    // A ZSet with the value cursor on a row, and the value pane focused: all
    // three mean something, like a Hash/List (D1, ADR-0018) — but `e` edits
    // the *score*, never the member (D1), so the hint says `score`, not
    // `edit`, matching `OpenKey::edit_verb`'s "editing score" wording — the
    // one word D1 calls out so this is not a surprise.
    if !state.keys_pane_focused()
        && state
            .open
            .as_ref()
            .is_some_and(|o| o.cursor_active && matches!(o.value, Some(Value::ZSet(_))))
    {
        let edit = state.keymap.hint(Action::Edit).unwrap_or_default();
        let add = state.keymap.hint(Action::Add).unwrap_or_default();
        let remove = state.keymap.hint(Action::Delete).unwrap_or_default();
        return format!("{edit} score · {add} add · {remove} remove");
    }
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
            a.label_in(
                state.keys_pane_focused(),
                state.liveness() == Liveness::Disconnected
            )
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
            let secs = clock.now_epoch_ms().saturating_sub(then) / 1000;
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

/// Repaint the inline editor's own cursor cell with the Viewer's cursor
/// token, over the whole of `area`.
///
/// `ratatui-textarea` draws its cursor in reverse video, and nothing else in
/// an editor area uses that modifier, so the one cell wearing it is always
/// exactly the cursor — found by the modifier rather than by asking the
/// widget for its position, since the crate does not expose one.
fn repaint_reversed_cursor(buf: &mut Buffer, area: Rect, style: Style) {
    let reversed = ratatui::style::Modifier::REVERSED;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if cell.modifier.contains(reversed) {
                cell.modifier.remove(reversed);
                cell.set_style(style);
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

#[cfg(test)]
mod hint_bar_tests {
    //! The hint bar's List-shaped arms (PLAN M2 task 8, D8; ADR-0017): the
    //! value cursor's `e edit · a add · d remove`, and the add form's `Tab`
    //! wording, which is the only place on the whole bar `Tab` means
    //! anything at all.

    use super::*;
    use crate::render::layout::Pane;
    use crate::state::open::OpenKey;
    use crate::state::value::IndexedValue;
    use crate::state::{EditBuffer, State};

    fn open_with_list(items: &[&str], total: usize) -> State {
        let value = Value::List(IndexedValue {
            items: items.iter().map(|i| i.as_bytes().to_vec()).collect(),
            total,
        });
        let mut state = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    #[test]
    fn a_list_with_the_cursor_active_and_the_value_pane_focused_hints_all_three() {
        let mut s = open_with_list(&["alpha"], 1);
        s.open.as_mut().unwrap().cursor_active = true;
        let hint = hint_bar(&s);
        assert!(hint.contains("edit"), "{hint}");
        assert!(hint.contains("add"), "{hint}");
        assert!(hint.contains("remove"), "{hint}");
    }

    #[test]
    fn a_list_with_the_cursor_active_but_the_keys_pane_focused_does_not_use_the_list_hint() {
        // The same discipline the Hash/Set branches already have: focus, not
        // merely `cursor_active`, decides which hint shows — `d` there is
        // `DeleteKey`, not `LREM`, and the hint must not claim otherwise.
        let mut s = open_with_list(&["alpha"], 1);
        s.open.as_mut().unwrap().cursor_active = true;
        s.focus = Pane::Keys;
        let hint = hint_bar(&s);
        assert!(!hint.contains("edit · "), "{hint}");
    }

    #[test]
    fn the_list_add_forms_hint_names_tab_and_nothing_else_does() {
        let mut s = open_with_list(&["alpha"], 1);
        s.open
            .as_mut()
            .unwrap()
            .begin_edit(EditBuffer::new_list_element());
        let hint = hint_bar(&s);
        assert!(hint.contains("Tab"), "{hint}");
        assert!(hint.contains("head/tail"), "{hint}");

        // Nothing else on the bar mentions `Tab` — the Hash add form's name
        // part, its value part, and a plain field edit all take this same
        // function's other branches.
        let mut hash = open_with_list(&["alpha"], 1);
        hash.open.as_mut().unwrap().value = Some(Value::Hash(crate::state::value::PairValue {
            pairs: vec![("f".into(), "v".into())],
            total: 1,
        }));
        hash.open
            .as_mut()
            .unwrap()
            .begin_edit(EditBuffer::new_hash_field());
        assert!(!hint_bar(&hash).contains("Tab"), "{}", hint_bar(&hash));
    }

    #[test]
    fn the_list_edit_forms_hint_never_mentions_tab() {
        // `EditTarget::ListElement` (an in-place edit, not the add form) has
        // no end to toggle — only `NewListElement` does (D6).
        let mut s = open_with_list(&["alpha"], 1);
        s.open
            .as_mut()
            .unwrap()
            .begin_edit(EditBuffer::list_element(0, b"alpha").unwrap());
        assert!(!hint_bar(&s).contains("Tab"), "{}", hint_bar(&s));
    }

    #[test]
    fn a_list_add_form_toggled_to_head_still_names_tab_in_the_hint() {
        let mut s = open_with_list(&["alpha"], 1);
        let mut buffer = EditBuffer::new_list_element();
        buffer.toggle_list_end();
        s.open.as_mut().unwrap().begin_edit(buffer);
        let hint = hint_bar(&s);
        assert!(hint.contains("Tab"), "{hint}");
    }
}

#[cfg(test)]
mod zset_hint_bar_tests {
    //! The hint bar's ZSet-shaped arms (PLAN M2 task 9, D1, D4, D6,
    //! ADR-0018): the value cursor's `e score · a add · d remove` — `score`,
    //! not `edit`, per D1 — and D4's live invalid-score indicator on both the
    //! existing-member editor and the add form's score part.

    use super::*;
    use crate::render::layout::Pane;
    use crate::state::open::OpenKey;
    use crate::state::value::ScoredValue;
    use crate::state::{EditBuffer, State};

    fn open_with_zset(entries: &[(&[u8], f64)], total: usize) -> State {
        let value = Value::ZSet(ScoredValue {
            entries: entries.iter().map(|(m, s)| (m.to_vec(), *s)).collect(),
            total,
        });
        let mut state = State {
            cols: 130,
            rows: 40,
            focus: Pane::Value,
            open: Some(OpenKey::new(Some(0), "k".into(), value, -1, 10, 0)),
            ..State::default()
        };
        state.keys.push(b"k");
        state.rebuild_list();
        state
    }

    #[test]
    fn a_zset_with_the_cursor_active_hints_score_not_edit() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.open.as_mut().unwrap().cursor_active = true;
        let hint = hint_bar(&s);
        assert!(hint.contains("score"), "{hint}");
        assert!(
            !hint.contains("edit"),
            "D1: e edits the score, not e edit — {hint}"
        );
        assert!(hint.contains("add"), "{hint}");
        assert!(hint.contains("remove"), "{hint}");
    }

    #[test]
    fn a_zset_with_the_cursor_active_but_the_keys_pane_focused_does_not_use_the_zset_hint() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.open.as_mut().unwrap().cursor_active = true;
        s.focus = Pane::Keys;
        let hint = hint_bar(&s);
        assert!(!hint.contains("score · "), "{hint}");
    }

    #[test]
    fn an_existing_score_edit_with_a_valid_score_hints_stage_not_invalid() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.open
            .as_mut()
            .unwrap()
            .begin_edit(EditBuffer::zset_score(b"alpha", 1.0));
        let hint = hint_bar(&s);
        assert!(!hint.contains("invalid"), "{hint}");
    }

    #[test]
    fn an_existing_score_edit_with_an_invalid_score_shows_the_live_indicator() {
        // D4: the buffer's text can be typed into invalidity even though it
        // was seeded valid — the hint must say so live, since `⌃S` is
        // silently blocked (`value_part_stage_blocked`) while this holds.
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let mut buffer = EditBuffer::zset_score(b"alpha", 1.0);
        buffer.insert_char('x');
        s.open.as_mut().unwrap().begin_edit(buffer);
        let hint = hint_bar(&s);
        assert!(hint.contains("invalid"), "{hint}");
    }

    #[test]
    fn the_add_forms_member_part_hints_enter_score() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        s.open
            .as_mut()
            .unwrap()
            .begin_edit(EditBuffer::new_zset_member());
        let hint = hint_bar(&s);
        assert!(hint.contains("score"), "{hint}");
    }

    #[test]
    fn the_add_forms_member_part_names_the_shown_duplicate() {
        let mut s = open_with_zset(&[(b"dup", 1.0)], 1);
        let mut buffer = EditBuffer::new_zset_member();
        buffer.name_push('d');
        buffer.name_push('u');
        buffer.name_push('p');
        s.open.as_mut().unwrap().begin_edit(buffer);
        let hint = hint_bar(&s);
        assert!(hint.contains("member exists"), "{hint}");
    }

    #[test]
    fn the_add_forms_score_part_with_a_valid_score_hints_stage() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let mut buffer = EditBuffer::new_zset_member();
        buffer.name_push('b');
        buffer.advance_to_value();
        buffer.insert_char('5');
        s.open.as_mut().unwrap().begin_edit(buffer);
        let hint = hint_bar(&s);
        assert!(hint.contains("stage"), "{hint}");
        assert!(!hint.contains("invalid"), "{hint}");
    }

    #[test]
    fn the_add_forms_score_part_with_an_invalid_score_shows_the_live_indicator() {
        let mut s = open_with_zset(&[(b"alpha", 1.0)], 1);
        let mut buffer = EditBuffer::new_zset_member();
        buffer.name_push('b');
        buffer.advance_to_value();
        buffer.insert_str("nope");
        s.open.as_mut().unwrap().begin_edit(buffer);
        let hint = hint_bar(&s);
        assert!(hint.contains("invalid"), "{hint}");
    }
}
