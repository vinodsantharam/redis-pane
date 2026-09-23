//! The TTL edit grammar: one field, three writes (PLAN M2 task 10, D3, D4,
//! D10, D12, ADR-0019).
//!
//! [`parse_ttl_edit`] turns what the reader typed into a [`TtlEdit`] — set,
//! persist or shift — refusing anything the grammar does not admit.
//! [`resolve_ttl_edit`] then checks that edit against the key's current TTL,
//! which is where the "no expiry to change" and "would expire it now"
//! refusals live: those depend on the key, not the text, so
//! [`parse_ttl_edit`] alone cannot decide them.
//!
//! Both are pure and clock-free (ADR-0011) — `update`'s `⌃S` block runs them
//! against the raw read TTL, and `render`'s resolution line runs them
//! against the counted-down TTL, and this module is the one place both read
//! the grammar from, so the two cannot drift onto different rules (D12).
//!
//! [`format_duration`] is the third export: the finer, two-most-significant-
//! units formatter the resolution line and the confirm dialog's `old → new`
//! line need — distinct from [`crate::render::keys::format_ttl`]'s coarser,
//! single-unit column form (D10). See that function's doc comment, which
//! points back here; `render::keys` re-exports this one so the two sit
//! beside each other.

use crate::state::loaded::TTL_NONE;

/// This app's own ceiling, in seconds: `OpenKey::ttl_seconds`,
/// `LoadedSet::ttls` and `format_ttl` are all `i32`, so a set that resolves
/// above this is refused before it can overflow any of them (D4). Six
/// orders of magnitude tighter than Redis's own bound — measured for
/// ADR-0019 at roughly `9223370246680933` seconds — so this is the one that
/// actually binds.
const CEILING_SECONDS: u64 = i32::MAX as u64;

/// What the duration grammar parsed to, before it is checked against the
/// key's current TTL (D3, D4, D12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlEdit {
    /// A bare duration or `<int><unit>` chain: a guarded `EXPIRE key
    /// <seconds>`.
    Set(i32),
    /// Empty input, or `never`: a guarded `PERSIST`.
    Persist,
    /// A signed `+`/`-` duration: a guarded extend (positive) or shorten
    /// (negative), by this many seconds.
    Shift(i32),
}

/// What a [`TtlEdit`] resolves to against the key's current TTL — the data
/// the resolution line and the confirm dialog's `old → new` line are built
/// from (D3, D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlOutcome {
    /// The TTL after a set: exactly the seconds typed.
    Set(i32),
    /// The expiry is cleared.
    Persist,
    /// The TTL after a shift, computed locally as a courtesy figure for
    /// display — **not** the figure the write actually uses. The script
    /// (ADR-0019 D5) applies the delta to the TTL as the server sees it at
    /// write time, which can differ from this if time has passed since
    /// `current` was read (D7); this is close enough for the reader to
    /// judge intent, and the real number arrives on the post-write refetch.
    Shift(i32),
}

/// Why a duration expression was refused — either it never parsed, or it
/// parsed but does not make sense against the key's current TTL (D4).
///
/// Each variant owns its own wording in [`TtlEditRefusal::reason`], the way
/// [`crate::mutation::NotWritten::reason`] does, so a new refusal forces a
/// wording decision once rather than silently inheriting a neighbour's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlEditRefusal {
    /// Did not parse at all: garbage, a fractional unit, an internal space,
    /// or a unit chain that is not strictly descending (`30m2h`, `1h1h`).
    Unreadable,
    /// `0`, or a set that resolves to `0` — `EXPIRE key 0` deletes the key
    /// immediately (verified for ADR-0019), so this is refused at the
    /// keyboard rather than sent.
    ZeroDeletes,
    /// A `+`/`-` shift on a key whose current TTL is [`TTL_NONE`] — nothing
    /// to extend or shorten.
    NoExpiry,
    /// A shorten that would land at or below zero — the same
    /// `EXPIRE key 0`-deletes hazard as [`TtlEditRefusal::ZeroDeletes`], one
    /// step removed.
    WouldExpireNow,
    /// A set above this app's own ceiling (`i32::MAX` seconds, roughly 68
    /// years).
    TooLong,
    /// `never`/empty on a key that is already [`TTL_NONE`].
    AlreadyNever,
}

impl TtlEditRefusal {
    /// The line shown under the field, `··`-prefixed by the caller (D4's
    /// table, verbatim — settled at ADR-0019's checkpoint 1).
    pub fn reason(&self) -> &'static str {
        match self {
            TtlEditRefusal::Unreadable => "can't read that — try 5m, +30m, or never",
            TtlEditRefusal::ZeroDeletes => "0 deletes the key — use d",
            TtlEditRefusal::NoExpiry => "no expiry to change — type 30m to set one",
            TtlEditRefusal::WouldExpireNow => "that would expire it now — use d to delete",
            // Fixed text, not interpolated: settled at ADR-0019's checkpoint
            // 1. `format_duration`'s two-most-significant-units rule would
            // render `i32::MAX` seconds as `24855d 3h`, which is
            // arithmetically right and useless — a reader who has just
            // typed something absurd needs the shape of the limit, not its
            // day count. "about" is doing real work: the bound is an `i32`
            // artifact, not a product decision, and stating it to the
            // second would dress it up as one.
            TtlEditRefusal::TooLong => "too long — the most is about 68 years",
            TtlEditRefusal::AlreadyNever => "already never expires",
        }
    }
}

/// Parse a duration expression (D3, D4).
///
/// Case-insensitive, surrounding whitespace trimmed. Accepts:
/// - an optional leading `+`/`-`;
/// - then either a bare non-negative integer (seconds), or one or more
///   `<integer><unit>` segments with units `s`/`m`/`h`/`d` in strictly
///   descending unit order (`2h30m` yes, `30m2h` no, `1h1h` no);
/// - or `never`, or the empty string, meaning persist.
///
/// Accumulation **saturates rather than overflows** — no wrapping, no
/// release-mode surprise — and a saturated total is refused by the ceiling
/// check below, exactly like any other over-ceiling input.
pub fn parse_ttl_edit(text: &str) -> Result<TtlEdit, TtlEditRefusal> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("never") {
        return Ok(TtlEdit::Persist);
    }
    let (sign, rest): (i8, &str) = match text.as_bytes().first() {
        Some(b'+') => (1, &text[1..]),
        Some(b'-') => (-1, &text[1..]),
        _ => (0, text),
    };
    if rest.is_empty() {
        return Err(TtlEditRefusal::Unreadable);
    }
    let magnitude = parse_magnitude(rest)?;
    if magnitude > CEILING_SECONDS {
        return Err(TtlEditRefusal::TooLong);
    }
    // Safe: bounded by `CEILING_SECONDS == i32::MAX` just above.
    let seconds = magnitude as i32;
    match sign {
        0 if seconds == 0 => Err(TtlEditRefusal::ZeroDeletes),
        0 => Ok(TtlEdit::Set(seconds)),
        1 => Ok(TtlEdit::Shift(seconds)),
        _ => Ok(TtlEdit::Shift(-seconds)),
    }
}

/// The magnitude half of the grammar — everything after an optional leading
/// sign — as a saturating total of seconds. Never panics: every arithmetic
/// step below is `saturating_*`, so a digit string or a unit chain far
/// longer than anything a real duration needs still returns a plain `u64`
/// rather than wrapping or aborting.
fn parse_magnitude(rest: &str) -> Result<u64, TtlEditRefusal> {
    if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(saturating_parse_u64(rest));
    }
    parse_unit_chain(rest)
}

/// A digit string as a saturating `u64` — never panics on a string far
/// longer than `u64` can hold.
fn saturating_parse_u64(digits: &str) -> u64 {
    let mut total: u64 = 0;
    for b in digits.bytes() {
        let digit = u64::from(b - b'0');
        total = total.saturating_mul(10).saturating_add(digit);
    }
    total
}

/// One or more `<integer><unit>` segments, `s`/`m`/`h`/`d`, in strictly
/// descending order — no repeats, no ascending pair, no internal space, no
/// fractional unit, no unit `EXPIRE` does not have a segment for.
fn parse_unit_chain(rest: &str) -> Result<u64, TtlEditRefusal> {
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return Err(TtlEditRefusal::Unreadable);
    }
    let mut i = 0;
    let mut total: u64 = 0;
    // Higher than any real rank, so the first segment's check always passes.
    let mut last_rank = u8::MAX;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            // A non-digit where a segment's leading number was expected:
            // a stray character, or a unit with nothing in front of it.
            return Err(TtlEditRefusal::Unreadable);
        }
        let digits = &rest[start..i];
        let Some(&unit_byte) = bytes.get(i) else {
            // Trailing digits with no unit after them — `90m5` or similar.
            return Err(TtlEditRefusal::Unreadable);
        };
        let (unit_seconds, rank): (u64, u8) = match unit_byte.to_ascii_lowercase() {
            b's' => (1, 1),
            b'm' => (60, 2),
            b'h' => (3_600, 3),
            b'd' => (86_400, 4),
            _ => return Err(TtlEditRefusal::Unreadable),
        };
        if rank >= last_rank {
            // Not strictly descending: same unit twice (`1h1h`) or an
            // ascending pair (`30m2h`) — both refused, deliberately, rather
            // than silently summed (D4).
            return Err(TtlEditRefusal::Unreadable);
        }
        last_rank = rank;
        i += 1; // consume the unit character
        let value = saturating_parse_u64(digits);
        let contribution = value.saturating_mul(unit_seconds);
        total = total.saturating_add(contribution);
    }
    Ok(total)
}

/// Check a parsed [`TtlEdit`] against the key's current TTL (raw seconds,
/// [`TTL_NONE`] for no expiry), resolving it to what the write will do or
/// refusing it with the reason that depends on the key rather than the text
/// (D4).
///
/// This is a **courtesy**, not the authority. The server's guard scripts
/// (ADR-0019 D5) run atomically against the real TTL at write time and
/// refuse with `NotWritten::WouldExpireNow` if this local approximation was
/// wrong — the only way it can be wrong is time passing between `current`
/// being read and the write landing (D7). `current` is whatever the caller
/// has: the raw read TTL from `update`'s `⌃S` block, or the counted-down
/// figure from `render`'s resolution line (D12) — this function does not
/// care which, and both checks below are exact against whatever is passed.
pub fn resolve_ttl_edit(edit: TtlEdit, current: i32) -> Result<TtlOutcome, TtlEditRefusal> {
    match edit {
        TtlEdit::Set(seconds) => Ok(TtlOutcome::Set(seconds)),
        TtlEdit::Persist => {
            if current == TTL_NONE {
                Err(TtlEditRefusal::AlreadyNever)
            } else {
                Ok(TtlOutcome::Persist)
            }
        }
        TtlEdit::Shift(delta) => {
            if current == TTL_NONE {
                return Err(TtlEditRefusal::NoExpiry);
            }
            let resulting = i64::from(current) + i64::from(delta);
            if resulting <= 0 {
                return Err(TtlEditRefusal::WouldExpireNow);
            }
            // `resulting` is bounded well inside `i32` here: `current` was
            // already a valid `i32` TTL and `delta` came from `parse_ttl_edit`,
            // which itself is bounded by `CEILING_SECONDS`.
            Ok(TtlOutcome::Shift(resulting.min(i64::from(i32::MAX)) as i32))
        }
    }
}

/// A TTL in seconds, at two most-significant units — never more (D10):
/// `45s`, `1m 30s`, `1h 12m`, `2d 3h`. If the second-highest unit is zero,
/// it is dropped rather than shown as `1h 0m` — this never cascades to a
/// third unit to compensate, which is the whole point of "never more".
///
/// Distinct from [`crate::render::keys::format_ttl`]'s coarser, single-unit
/// column form: this is for the one moment the exact figure matters, while
/// the reader is deciding what to type or confirm. See that function's own
/// doc comment, which points back here.
pub fn format_duration(seconds: i32) -> String {
    let s = u64::from(seconds.max(0) as u32);
    let days = s / 86_400;
    let rem_days = s % 86_400;
    let hours = rem_days / 3_600;
    let rem_hours = rem_days % 3_600;
    let minutes = rem_hours / 60;
    let secs = rem_hours % 60;
    if days > 0 {
        if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        }
    } else if minutes > 0 {
        if secs > 0 {
            format!("{minutes}m {secs}s")
        } else {
            format!("{minutes}m")
        }
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_accepted_shape_parses_to_the_right_edit() {
        let cases = [
            ("5m", TtlEdit::Set(300)),
            ("90", TtlEdit::Set(90)),
            ("2h30m", TtlEdit::Set(9_000)),
            ("1d2h30m10s", TtlEdit::Set(95_410)),
            ("+30m", TtlEdit::Shift(1_800)),
            ("-10m", TtlEdit::Shift(-600)),
            ("never", TtlEdit::Persist),
            ("NEVER", TtlEdit::Persist),
            ("", TtlEdit::Persist),
            ("  5m  ", TtlEdit::Set(300)),
            ("  never  ", TtlEdit::Persist),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_ttl_edit(text), Ok(expected), "{text:?}");
        }
    }

    #[test]
    fn unreadable_shapes_are_named_by_the_right_refusal() {
        for text in [
            "abc", "1.5h", "5 m", "30m2h", "1h1h", "+", "-", "5x", "90m5",
        ] {
            assert_eq!(
                parse_ttl_edit(text),
                Err(TtlEditRefusal::Unreadable),
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_descending_unit_rule_rejects_ascending_and_repeated_pairs() {
        assert_eq!(parse_ttl_edit("30m2h"), Err(TtlEditRefusal::Unreadable));
        assert_eq!(parse_ttl_edit("1h1h"), Err(TtlEditRefusal::Unreadable));
        assert_eq!(parse_ttl_edit("1s1m"), Err(TtlEditRefusal::Unreadable));
        // Strictly descending is fine, any number of segments.
        assert!(parse_ttl_edit("1d2h30m10s").is_ok());
    }

    #[test]
    fn zero_is_refused_by_name_bare_and_with_a_unit() {
        assert_eq!(parse_ttl_edit("0"), Err(TtlEditRefusal::ZeroDeletes));
        assert_eq!(parse_ttl_edit("0s"), Err(TtlEditRefusal::ZeroDeletes));
    }

    #[test]
    fn an_overflowing_accumulation_saturates_and_is_refused_as_too_long() {
        // Ten nines of days: far past both `u64` risk and the app's own
        // ceiling. Must not panic in any build profile.
        assert_eq!(parse_ttl_edit("9999999999d"), Err(TtlEditRefusal::TooLong));
        // A bare digit string long enough to threaten a naive parse.
        assert_eq!(
            parse_ttl_edit(&"9".repeat(30)),
            Err(TtlEditRefusal::TooLong)
        );
        // Right at the boundary: the app's ceiling is accepted...
        assert_eq!(
            parse_ttl_edit(&i32::MAX.to_string()),
            Ok(TtlEdit::Set(i32::MAX))
        );
        // ...one second past it is not.
        let over = i64::from(i32::MAX) + 1;
        assert_eq!(
            parse_ttl_edit(&over.to_string()),
            Err(TtlEditRefusal::TooLong)
        );
    }

    #[test]
    fn whitespace_padded_input_is_trimmed() {
        assert_eq!(parse_ttl_edit("   90   "), Ok(TtlEdit::Set(90)));
        assert_eq!(parse_ttl_edit("\t+5m\n"), Ok(TtlEdit::Shift(300)));
    }

    #[test]
    fn resolve_set_never_depends_on_the_current_ttl() {
        for current in [TTL_NONE, 0, 42, 1_000_000] {
            assert_eq!(
                resolve_ttl_edit(TtlEdit::Set(300), current),
                Ok(TtlOutcome::Set(300))
            );
        }
    }

    #[test]
    fn resolve_persist_refuses_a_key_already_at_infinity() {
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Persist, TTL_NONE),
            Err(TtlEditRefusal::AlreadyNever)
        );
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Persist, 42),
            Ok(TtlOutcome::Persist)
        );
    }

    #[test]
    fn resolve_shift_refuses_no_expiry_to_change() {
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(1_800), TTL_NONE),
            Err(TtlEditRefusal::NoExpiry)
        );
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(-1_800), TTL_NONE),
            Err(TtlEditRefusal::NoExpiry)
        );
    }

    #[test]
    fn resolve_shift_refuses_landing_at_or_below_zero() {
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(-100), 100),
            Err(TtlEditRefusal::WouldExpireNow),
            "exactly zero still deletes"
        );
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(-200), 100),
            Err(TtlEditRefusal::WouldExpireNow)
        );
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(-99), 100),
            Ok(TtlOutcome::Shift(1))
        );
    }

    #[test]
    fn resolve_shift_extends_and_shortens_exactly() {
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(1_800), 2_520),
            Ok(TtlOutcome::Shift(4_320))
        );
        assert_eq!(
            resolve_ttl_edit(TtlEdit::Shift(-1_920), 2_520),
            Ok(TtlOutcome::Shift(600))
        );
    }

    #[test]
    fn format_duration_shows_at_most_two_units_never_padding_a_zero_one() {
        let cases = [
            (0, "0s"),
            (45, "45s"),
            (59, "59s"),
            (60, "1m"),
            (61, "1m 1s"),
            (90, "1m 30s"),
            (119, "1m 59s"),
            (120, "2m"),
            (3_599, "59m 59s"),
            (3_600, "1h"),
            (3_601, "1h"),
            (4_320, "1h 12m"),
            (86_399, "23h 59m"),
            (86_400, "1d"),
            (90_000, "1d 1h"),
            (183_600, "2d 3h"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(format_duration(seconds), expected, "{seconds}s");
        }
    }

    #[test]
    fn format_duration_clamps_a_negative_input_rather_than_panicking() {
        assert_eq!(format_duration(-1), "0s");
    }
}
