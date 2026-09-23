//! The inline value editor's unsaved text (R3.8, PLAN M2 task 4 rework,
//! ADR-0014).
//!
//! [`EditBuffer`] is the reader's *unsaved* text, not a cache of the server
//! value — the distinction CONTEXT.md draws between the two. It never reaches
//! the server on its own; `EditorStage` turns it into a
//! [`crate::state::PendingMutation::SetString`], which goes through the one
//! mutation chokepoint like every other write.

use ratatui_textarea::{CursorMove, TextArea, WrapMode};

use super::value::{StringValue, Value, format_score};

/// Values whose raw byte length exceeds this are refused inline (ADR-0014).
///
/// Set from Phase 0's spike measurements (`crates/core/examples/editor_spike.rs`,
/// throwaway): a single unwrapped line's p99 keystroke-plus-render time was
/// clean up to 200KB (≤8.5ms, comfortably inside the 16ms frame budget) on
/// every run, while 300KB was noisy across runs. The cost is dominated by
/// re-wrapping one long logical line, not by the edit operation itself.
pub const MAX_EDIT_BYTES: usize = 200 * 1024;

/// What an [`EditBuffer`] writes back, when staged (PLAN M2 task 6, D1, D3).
///
/// Distinct from the value being edited, which is always plain text in the
/// buffer either way — this is what `EditorStage` builds a
/// [`crate::state::PendingMutation`] out of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    /// The Open value's whole body — a String or a JSON-classified String
    /// (M2 task 4's original behaviour, unchanged).
    Value,
    /// One field of the Open Hash, being overwritten (`HSET`, guarded).
    /// Always on [`FieldPart::Value`] — an existing field's name is read-only
    /// (renaming is a follow-up).
    HashField { field: String },
    /// A brand-new field of the Open Hash, not yet on the server (`HSETNX`,
    /// guarded) — the two-part `FIELD`/`VALUE` add form (PLAN M2 task 6
    /// follow-up, F). `field` is itself being typed while `part` is
    /// [`FieldPart::Name`].
    NewHashField { field: String, part: FieldPart },
    /// A brand-new member of the Open Set, not yet on the server (`SADD`,
    /// guarded) — PLAN M2 task 7, D3, ADR-0016. Carries no [`FieldPart`]: a
    /// Set member has no name/value split the way a Hash field does — it is
    /// only bytes — so the add form is a single capture, not the Hash add
    /// form's two-part `FIELD`/`VALUE` shape.
    NewSetMember,
    /// One element of the Open List, being overwritten (`LSET`, guarded —
    /// PLAN M2 task 8, D2, ADR-0017). Always on its **raw** value, never
    /// reformatted, mirroring [`EditTarget::HashField`]. `index` is the
    /// element's position, not a byte offset — the compare-and-set guard
    /// (ADR-0017 D2) is keyed on it, and a rescan between opening and
    /// staging cannot renumber it, since nothing here re-derives it from a
    /// row.
    ListElement { index: usize },
    /// A brand-new element of the Open List, not yet on the server
    /// (`LPUSH`/`RPUSH`, guarded) — PLAN M2 task 8, D6, ADR-0017. Carries no
    /// [`FieldPart`]: a single capture, the same shape [`EditTarget::NewSetMember`]'s
    /// form is — a List element has no name to type first either. `end` is
    /// which end `a` will push to; `Tab` toggles it while the form is open
    /// (phase 3's wiring, not this type's job).
    NewListElement { end: super::value::ListEnd },
    /// The score of one member of the Open ZSet, being overwritten (`ZADD
    /// ... XX`, guarded — PLAN M2 task 9, D1, D2, D5, ADR-0018). Unlike
    /// every other row-level [`EditTarget`], the member's bytes are not
    /// refused for being non-UTF-8 (D5): a score edit never touches them —
    /// they travel to the server exactly as read — and the score itself is
    /// always ASCII, so `member` is `Vec<u8>`, not `String`, the same
    /// binary-safety [`super::value::ScoredValue::entries`] already carries.
    /// A member is never edited in place (D1, mirroring
    /// [`EditTarget::NewSetMember`]'s reasoning one type over) — a changed
    /// member is a rename, deferred to PLAN M2 task 14.
    ZSetScore { member: Vec<u8> },
    /// A brand-new member+score of the Open ZSet, not yet on the server
    /// (`ZADD ... NX`, guarded — PLAN M2 task 9, D2, D3, D6, ADR-0018) — the
    /// two-part `MEMBER`/`SCORE` add form, mirroring
    /// [`EditTarget::NewHashField`]'s `FIELD`/`VALUE` shape. `member` is
    /// itself being typed while `part` is [`FieldPart::Name`]; the score is
    /// captured while `part` is [`FieldPart::Value`]. Unlike
    /// [`EditTarget::ZSetScore`], the member here is `String`, not
    /// `Vec<u8>`: the add form's member capture is text-only and still
    /// refuses a non-UTF-8 typed member, the same as every other add form
    /// (D5 only exempts an existing score edit, not a new member's bytes).
    NewZSetMember { member: String, part: FieldPart },
    /// The Open key's TTL, being typed as a duration expression (`t` in the
    /// value pane — PLAN M2 task 10, D1, D3, D11, ADR-0019). A guarded
    /// `EXPIRE`/`PERSIST`, resolved from `text` by
    /// [`crate::state::ttl::parse_ttl_edit`]. Unlike every other row-level
    /// target, this addresses the key's metadata, not part of a `Value` —
    /// there is no type branch and no cursor prerequisite (D1): every Redis
    /// type has exactly one TTL, edited identically.
    Ttl { text: String },
}

/// Which half of the add form (`FIELD`/`VALUE`) is active, while adding a new
/// Hash field (PLAN M2 task 6 follow-up, F/N). Meaningless for
/// [`EditTarget::Value`] and [`EditTarget::HashField`] — a String has no
/// parts, and an existing field's name is never editable, so both are always
/// "on the value" without needing to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldPart {
    Name,
    Value,
}

/// The reader's unsaved text in the value pane.
///
/// `TextArea` is `Clone`/`Debug`/`Default` but not `PartialEq`/`Send`; a
/// manual `PartialEq` is implemented below comparing the fields that matter.
/// `!Send` is fine here — `State` lives only on the main loop, driven by
/// `runtime.block_on` in `crates/app/src/main.rs`, never sent across threads.
#[derive(Debug, Clone)]
pub struct EditBuffer {
    area: TextArea<'static>,
    /// The bytes exactly as read, before any keystroke. Compared against
    /// [`EditBuffer::text`] to decide whether `EditorStage` has anything to
    /// stage at all.
    original: Vec<u8>,
    /// Whether the value being edited was rendered through the JSON viewer —
    /// a fact about the read that produced `original`, carried through so a
    /// staged `SetString` can warn if the edit no longer parses (mirrors
    /// `PendingMutation::SetString::was_json`).
    was_json: bool,
    /// What this buffer writes back when staged (PLAN M2 task 6, D3).
    target: EditTarget,
}

impl PartialEq for EditBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.area.lines() == other.area.lines()
            && self.area.cursor() == other.area.cursor()
            && self.original == other.original
            && self.was_json == other.was_json
            && self.target == other.target
    }
}

impl Eq for EditBuffer {}

impl EditBuffer {
    /// Build a buffer from a freshly read [`Value`], refusing anything that
    /// is not editable inline.
    ///
    /// Only `Value::Str` and `Value::Json` are editable here — a text editor
    /// is not guaranteed to round-trip arbitrary bytes, so `Value::Binary`
    /// and every collection type are refused with a notice rather than
    /// risking silent corruption of a value nobody asked to have reformatted.
    ///
    /// The cursor opens where the Viewer's cursor was: `viewer_row` is a row
    /// of the value as the Viewer draws it, mapped back to a line of the text.
    pub fn from_value(value: &Value, viewer_row: usize) -> Result<EditBuffer, &'static str> {
        let (text, was_json, (line, col)) = match value {
            // The text exactly as read — never the wrapped display `lines`,
            // which cannot be losslessly turned back into the original bytes
            // (see `StringValue::raw`'s doc comment).
            Value::Str(s) => (s.raw.clone(), false, string_position(s, viewer_row)),
            // Already pretty-printed at read time (`JsonValue::parse`) —
            // handed to the editor in that form on purpose. A JSON-looking
            // string is still a string underneath: whatever comes back is
            // staged as a plain `SET`, reformatting included. Its rows are its
            // lines, one for one.
            Value::Json(j) => (j.lines.join("\n"), true, (viewer_row, 0)),
            Value::Binary(_) => return Err("binary values aren't editable here yet"),
            Value::Hash(_) | Value::List(_) | Value::Set(_) | Value::ZSet(_) | Value::Stream(_) => {
                return Err("only string values are editable so far");
            }
        };
        if text.len() > MAX_EDIT_BYTES {
            return Err(
                "too large to edit inline (over 200KB) — an external-editor escape hatch is planned",
            );
        }
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let mut area = TextArea::new(lines);
        // `Word` never splits a word wider than the pane, so a token or any
        // other space-free value would sit on one row with the cursor
        // off-screen; `WordOrGlyph` falls back to splitting it.
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        // The crate underlines the whole logical line under the cursor, which
        // for a token is every row of it. The cursor itself stays the crate's
        // reverse-video block, which needs no colour.
        area.set_cursor_line_style(ratatui::style::Style::default());
        // `Jump` clamps both to the buffer, so a row past the end lands on
        // the last line.
        let to_u16 = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        area.move_cursor(CursorMove::Jump(to_u16(line), to_u16(col)));
        Ok(EditBuffer {
            area,
            original: text.into_bytes(),
            was_json,
            target: EditTarget::Value,
        })
    }

    /// Build a buffer on one Hash field's raw value, to overwrite it (`e` on
    /// a Hash row, PLAN M2 task 6, D4).
    ///
    /// The **raw** field value, never reformatted — unlike [`Value::Json`]'s
    /// pretty-printing, a hash field is shown and edited exactly as read.
    /// `was_json` is still classified, so the dialog can warn if a
    /// JSON-shaped field stops parsing, the same courtesy `SetString`
    /// extends a String. The cursor opens at the start: a field has no
    /// viewer row of its own to map back from, the way a String's rows do.
    ///
    /// Classified with [`super::value::looks_like_json`], the same rule the
    /// shell's `string_value` uses to choose the JSON viewer over the plain
    /// String one — not bare `serde_json::from_str(..).is_ok()`, which also
    /// accepts a scalar like `8812` or `true`. A numeric Hash field is not
    /// "JSON" in any sense either caller means, and classifying it as such
    /// warned `⚠ no longer valid JSON` on every edit that turned it into
    /// plain text.
    pub fn for_hash_field(field: &[u8], value: &[u8]) -> Result<EditBuffer, &'static str> {
        // A text editor cannot round-trip arbitrary bytes, and a lossy field
        // name would write to a different field, so a field whose name or
        // value is not UTF-8 is refused the way `Value::Binary` is (review C2).
        let (Ok(field), Ok(value)) = (std::str::from_utf8(field), std::str::from_utf8(value))
        else {
            return Err("binary fields aren't editable here yet");
        };
        if value.len() > MAX_EDIT_BYTES {
            return Err(
                "too large to edit inline (over 200KB) — an external-editor escape hatch is planned",
            );
        }
        let was_json = super::value::looks_like_json(value);
        let lines: Vec<String> = value.split('\n').map(str::to_string).collect();
        let mut area = TextArea::new(lines);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        Ok(EditBuffer {
            area,
            original: value.as_bytes().to_vec(),
            was_json,
            target: EditTarget::HashField {
                field: field.to_string(),
            },
        })
    }

    /// An empty buffer for a field that does not exist on the server yet
    /// (`a`, PLAN M2 task 6 follow-up, F). Both the name and the value start
    /// empty and the form opens on the name part — there is no "original" to
    /// compare the value against but an empty one, and Redis allows an empty
    /// field value, so an unmodified empty buffer still stages an `HSETNX`
    /// with an empty value rather than being treated as "nothing to save".
    pub fn new_hash_field() -> EditBuffer {
        let mut area = TextArea::new(vec![String::new()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: Vec::new(),
            was_json: false,
            target: EditTarget::NewHashField {
                field: String::new(),
                part: FieldPart::Name,
            },
        }
    }

    /// An empty buffer for a member that does not exist on the server yet
    /// (`a` on a Set, PLAN M2 task 7, D3, ADR-0016). A single capture, unlike
    /// [`EditBuffer::new_hash_field`]'s two-part name/value form — a Set
    /// member has no name to type first, so the form opens straight onto the
    /// value. Redis allows an empty member, the same as it allows an empty
    /// Hash field value, so an unmodified empty buffer still stages an `SADD`
    /// with an empty member rather than being treated as "nothing to save".
    pub fn new_set_member() -> EditBuffer {
        let mut area = TextArea::new(vec![String::new()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: Vec::new(),
            was_json: false,
            target: EditTarget::NewSetMember,
        }
    }

    /// Build a buffer on one List element's raw value, to overwrite it (`e`
    /// on a List row, PLAN M2 task 8, D2, D4, ADR-0017).
    ///
    /// The **raw** element value, never reformatted, mirroring
    /// [`EditBuffer::for_hash_field`]. `index` is the element's position
    /// within the list, not a byte offset — D2's compare-and-set guard is
    /// keyed on it. A non-UTF-8 element is refused the same way a binary
    /// Hash field is (D4): a text editor cannot round-trip arbitrary bytes.
    pub fn list_element(index: usize, value: &[u8]) -> Result<EditBuffer, &'static str> {
        let Ok(value) = std::str::from_utf8(value) else {
            return Err("binary elements aren't editable here yet");
        };
        if value.len() > MAX_EDIT_BYTES {
            return Err(
                "too large to edit inline (over 200KB) — an external-editor escape hatch is planned",
            );
        }
        let was_json = super::value::looks_like_json(value);
        let lines: Vec<String> = value.split('\n').map(str::to_string).collect();
        let mut area = TextArea::new(lines);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        Ok(EditBuffer {
            area,
            original: value.as_bytes().to_vec(),
            was_json,
            target: EditTarget::ListElement { index },
        })
    }

    /// An empty buffer for a new List element that does not exist on the
    /// server yet (`a` on a List, PLAN M2 task 8, D6, ADR-0017). A single
    /// capture, the same shape [`EditBuffer::new_set_member`] is — a List
    /// element has no name to type first either. Opens on
    /// [`super::value::ListEnd`]'s default, [`super::value::ListEnd::Tail`]
    /// (D6): appending is the common case, and the one that does not
    /// renumber the rows the reader is looking at.
    pub fn new_list_element() -> EditBuffer {
        let mut area = TextArea::new(vec![String::new()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: Vec::new(),
            was_json: false,
            target: EditTarget::NewListElement {
                end: super::value::ListEnd::default(),
            },
        }
    }

    /// Build a buffer on one ZSet member's score, to overwrite it (`e` on a
    /// ZSet row, PLAN M2 task 9, D1, D4, D5, ADR-0018).
    ///
    /// Seeded from [`format_score`] — the same text the Viewer's SCORE
    /// column shows — so what the reader edits is exactly what they saw.
    /// Display → parse → f64 is lossless in both of `format_score`'s
    /// branches (`state::value`'s own `format_score_round_trips_losslessly`
    /// pins this), which is what makes seeding the buffer from it safe.
    /// Unlike [`EditBuffer::for_hash_field`]/[`EditBuffer::list_element`], a
    /// non-UTF-8 `member` does **not** refuse here (D5): the member's bytes
    /// never travel back changed — only the score, always ASCII — so there
    /// is nothing here a text editor could fail to round-trip.
    pub fn zset_score(member: &[u8], score: f64) -> EditBuffer {
        let text = format_score(score);
        let mut area = TextArea::new(vec![text.clone()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: text.into_bytes(),
            was_json: false,
            target: EditTarget::ZSetScore {
                member: member.to_vec(),
            },
        }
    }

    /// An empty buffer for a member+score that does not exist on the server
    /// yet (`a` on a ZSet, PLAN M2 task 9, D6, ADR-0018). Two-part, like
    /// [`EditBuffer::new_hash_field`] — a member is typed first, then
    /// `Enter` advances to the score — opening on the member part with both
    /// halves empty.
    pub fn new_zset_member() -> EditBuffer {
        let mut area = TextArea::new(vec![String::new()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: Vec::new(),
            was_json: false,
            target: EditTarget::NewZSetMember {
                member: String::new(),
                part: FieldPart::Name,
            },
        }
    }

    /// Build a buffer on the Open key's TTL, seeded from its current value
    /// (`t` in the value pane, PLAN M2 task 10, D1, D3, D11, ADR-0019).
    ///
    /// `ttl_seconds` is the **raw** read TTL, not the counted-down figure —
    /// `open_ttl_editor` runs in `update`, which has no clock (ADR-0011), so
    /// the seed is at most a few seconds stale. A key with no expiry
    /// ([`TTL_NONE`]) seeds empty rather than the word `never`, so an
    /// untouched field and a deliberately-typed `never` are the same input
    /// to the parser either way.
    ///
    /// The seed is [`super::ttl::format_duration`]'s two-unit form (`42m`,
    /// `1h 12m`),
    /// not raw seconds — D3's own example seeds `42m`. This is safe even
    /// though `format_duration`'s spaced multi-unit output (`1h 12m`) is not
    /// itself always parseable back: unlike every other row edit, this
    /// buffer is a hand-painted single-line capture (D11) whose
    /// [`EditBuffer::is_dirty`] check never applies to it — an unedited
    /// field is never staged at all (`update`'s `is_new_field`-style bypass,
    /// PLAN M2 task 10 phase 3), so what is parsed is always whatever the
    /// reader actually typed, never the seed itself.
    ///
    /// Unlike [`EditBuffer::zset_score`]/[`EditBuffer::for_hash_field`]/
    /// [`EditBuffer::list_element`], typing here goes through
    /// [`EditBuffer::name_push`]/[`EditBuffer::name_pop`]/
    /// [`EditBuffer::name_push_str`] into [`EditTarget::Ttl`]'s own `text`
    /// field, not into the internal `TextArea` — the same mechanism the
    /// Hash/ZSet add forms' name half uses (D11), and the reason this target
    /// does not inherit the seeded-cursor-at-position-0 defect those add
    /// forms' *value* halves have (task 9's phase 3 "found while building"):
    /// there is no cursor to be in the wrong place, so `Backspace` removes
    /// the last character exactly as a reader expects of a seeded scalar.
    pub fn ttl(ttl_seconds: i32) -> EditBuffer {
        let seed = if ttl_seconds == super::loaded::TTL_NONE {
            String::new()
        } else {
            super::ttl::format_duration(ttl_seconds.max(0))
        };
        // Unused by this target's capture (the hand-painted `text` field is
        // what typing mutates), but every `EditBuffer` needs one.
        let mut area = TextArea::new(vec![String::new()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: seed.clone().into_bytes(),
            was_json: false,
            target: EditTarget::Ttl { text: seed },
        }
    }

    /// What this buffer writes back when staged.
    pub fn target(&self) -> &EditTarget {
        &self.target
    }

    /// The Hash field name, being typed ([`EditTarget::NewHashField`]) or
    /// fixed ([`EditTarget::HashField`]). `None` for a plain String edit or a
    /// Set member, neither of which has a field of its own — a Set member is
    /// only a value (D3, ADR-0016).
    pub fn field_name(&self) -> Option<&str> {
        match &self.target {
            EditTarget::HashField { field } | EditTarget::NewHashField { field, .. } => Some(field),
            // The member being typed, the same shape a Hash field's name is
            // (PLAN M2 task 9, D6, ADR-0018) — the add form's member capture
            // is text, unlike `ZSetScore`'s.
            EditTarget::NewZSetMember { member, .. } => Some(member),
            EditTarget::Value
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            // Not a name being typed or fixed — a score edit's member is
            // fixed identity carried as bytes (D5), not necessarily UTF-8,
            // so there is no `&str` to hand back here even if there were a
            // reason to.
            | EditTarget::ZSetScore { .. }
            // PLAN M2 task 10, D11, ADR-0019: **must not** return the TTL
            // text — it feeds the shown-duplicate checks, which a duration
            // expression has no business being compared against. See
            // `ttl_text` for what the parser and preview actually read.
            | EditTarget::Ttl { .. } => None,
        }
    }

    /// The TTL duration expression being typed ([`EditTarget::Ttl`]). `None`
    /// for every other target. Deliberately separate from
    /// [`EditBuffer::field_name`] (D11) — that one feeds the shown-duplicate
    /// checks a TTL has nothing to do with; this is what the parser and the
    /// resolution line actually read.
    pub fn ttl_text(&self) -> Option<&str> {
        match &self.target {
            EditTarget::Ttl { text } => Some(text),
            _ => None,
        }
    }

    /// Whether the add form's name part, or [`EditTarget::Ttl`], is the
    /// thing actually being typed into right now — the property
    /// `editor_key`'s routing (PLAN M2 task 10 phase 3) needs in place of
    /// `active_part() == Some(FieldPart::Name)`, since a TTL capture has no
    /// [`FieldPart`] to be on at all (D11): it is a single field, always
    /// active, the same shape [`EditTarget::NewSetMember`]/
    /// [`EditTarget::ListElement`] already have, except that a TTL, like the
    /// add forms' name half, types into this struct's own `text`/`field`/
    /// `member` rather than the internal `TextArea`.
    pub fn is_single_line_capture(&self) -> bool {
        matches!(
            self.target,
            EditTarget::NewHashField {
                part: FieldPart::Name,
                ..
            } | EditTarget::NewZSetMember {
                part: FieldPart::Name,
                ..
            } | EditTarget::Ttl { .. }
        )
    }

    /// Which part of the add form is active. `None` outside
    /// [`EditTarget::NewHashField`] — editing an existing field, and a plain
    /// String, both have nothing but the value to be on.
    ///
    /// Exhaustive, not a wildcard fallback (PLAN M2 task 8, D8): a List's
    /// two targets have no [`FieldPart`] to be on either — `NewListElement`'s
    /// `Head`/`Tail` toggle is a different kind of "part" than
    /// `FIELD`/`VALUE`, wired by its own mechanism in phase 3, not this one
    /// — but that has to be a decision made here, once, the same reasoning
    /// [`crate::state::PendingMutation::guard_text`] is exhaustive for.
    pub fn active_part(&self) -> Option<FieldPart> {
        match &self.target {
            EditTarget::NewHashField { part, .. } => Some(*part),
            // PLAN M2 task 9, D6, ADR-0018: the case the M3 inventory
            // predicted would need a real `Some(..)` here — a ZSet add's
            // `MEMBER`/`SCORE` split is exactly `FieldPart`-shaped, the same
            // way the Hash add form's `FIELD`/`VALUE` split is.
            EditTarget::NewZSetMember { part, .. } => Some(*part),
            EditTarget::Value
            | EditTarget::HashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            // A score edit has no FIELD/VALUE-shaped split — one field, the
            // score, always active (D1).
            | EditTarget::ZSetScore { .. }
            // PLAN M2 task 10, D11, ADR-0019: honestly `None` — a TTL edit
            // has no FIELD/VALUE-shaped split any more than a score edit
            // does; [`EditBuffer::is_single_line_capture`] is the property
            // that actually routes it.
            | EditTarget::Ttl { .. } => None,
        }
    }

    /// Append to the name being typed. A no-op unless the name part is
    /// active, so a stray call from the wrong mode can never corrupt it.
    ///
    /// Exhaustive over [`EditTarget`], not an `if let` (PLAN M2 task 9,
    /// phase 3 "Found while building"): with two targets now carrying a
    /// [`FieldPart::Name`] half — [`EditTarget::NewHashField`]'s field and
    /// [`EditTarget::NewZSetMember`]'s member — an `if let` naming only one
    /// of them would silently do nothing for the other, exactly the shape
    /// PLAN M2 task 8's D8 spent a phase eliminating elsewhere. A future
    /// third add-form-with-a-name target now has to answer this here too.
    pub fn name_push(&mut self, c: char) {
        match &mut self.target {
            EditTarget::NewHashField {
                field,
                part: FieldPart::Name,
            } => field.push(c),
            EditTarget::NewZSetMember {
                member,
                part: FieldPart::Name,
            } => member.push(c),
            // PLAN M2 task 10, D11, ADR-0019: the TTL capture's own `text`,
            // the same "types into this struct, not the `TextArea`"
            // mechanism the add forms' name half uses above.
            EditTarget::Ttl { text } => text.push(c),
            EditTarget::NewHashField { .. }
            | EditTarget::NewZSetMember { .. }
            | EditTarget::Value
            | EditTarget::HashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            | EditTarget::ZSetScore { .. } => {}
        }
    }

    /// One paste into the name, already stripped of newlines by the caller
    /// (`Msg::Paste`'s job, the same as it is for the filter and the old
    /// field-name capture). Exhaustive for the same reason [`EditBuffer::name_push`] is.
    pub fn name_push_str(&mut self, s: &str) {
        match &mut self.target {
            EditTarget::NewHashField {
                field,
                part: FieldPart::Name,
            } => field.push_str(s),
            EditTarget::NewZSetMember {
                member,
                part: FieldPart::Name,
            } => member.push_str(s),
            EditTarget::Ttl { text } => text.push_str(s),
            EditTarget::NewHashField { .. }
            | EditTarget::NewZSetMember { .. }
            | EditTarget::Value
            | EditTarget::HashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            | EditTarget::ZSetScore { .. } => {}
        }
    }

    /// Exhaustive for the same reason [`EditBuffer::name_push`] is.
    pub fn name_pop(&mut self) {
        match &mut self.target {
            EditTarget::NewHashField {
                field,
                part: FieldPart::Name,
            } => {
                field.pop();
            }
            EditTarget::NewZSetMember {
                member,
                part: FieldPart::Name,
            } => {
                member.pop();
            }
            EditTarget::Ttl { text } => {
                text.pop();
            }
            EditTarget::NewHashField { .. }
            | EditTarget::NewZSetMember { .. }
            | EditTarget::Value
            | EditTarget::HashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            | EditTarget::ZSetScore { .. } => {}
        }
    }

    /// Move from the name part to the value part (`Enter`/`↓`). The caller
    /// checks the name is non-empty and not a shown duplicate first (PLAN M2
    /// task 6 follow-up, D) — this only ever moves a genuinely blank capture
    /// forward if asked to, so the guard lives once, at the call site.
    /// Exhaustive for the same reason [`EditBuffer::name_push`] is — D6's
    /// member→score advance is this same "move from name to value" motion,
    /// not a new mechanism.
    pub fn advance_to_value(&mut self) {
        match &mut self.target {
            EditTarget::NewHashField { part, .. } | EditTarget::NewZSetMember { part, .. } => {
                *part = FieldPart::Value;
            }
            EditTarget::Value
            | EditTarget::HashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            | EditTarget::ZSetScore { .. }
            // No value part to advance to — a TTL is one field, always
            // active (D11).
            | EditTarget::Ttl { .. } => {}
        }
    }

    /// Move from the value part back to the name part (`↑` at the top row).
    /// A no-op outside [`EditTarget::NewHashField`]/[`EditTarget::NewZSetMember`] —
    /// every other target has no name part to return to. Exhaustive for the
    /// same reason [`EditBuffer::name_push`] is.
    pub fn return_to_name(&mut self) {
        match &mut self.target {
            EditTarget::NewHashField { part, .. } | EditTarget::NewZSetMember { part, .. } => {
                *part = FieldPart::Name;
            }
            EditTarget::Value
            | EditTarget::HashField { .. }
            | EditTarget::NewSetMember
            | EditTarget::ListElement { .. }
            | EditTarget::NewListElement { .. }
            | EditTarget::ZSetScore { .. }
            | EditTarget::Ttl { .. } => {}
        }
    }

    /// Flip which end `a` will push to (D6, ADR-0017) — `Tab` while the List
    /// add form is open. A no-op outside [`EditTarget::NewListElement`]:
    /// nothing else has an end to toggle, and a stray `Tab` elsewhere already
    /// has its own meaning (`insert_tab`), which the caller is responsible
    /// for choosing between — this only ever changes the target it owns.
    pub fn toggle_list_end(&mut self) {
        if let EditTarget::NewListElement { end } = &mut self.target {
            *end = match end {
                super::value::ListEnd::Head => super::value::ListEnd::Tail,
                super::value::ListEnd::Tail => super::value::ListEnd::Head,
            };
        }
    }

    /// The value part's cursor, `(line, column)` — used to tell whether `↑`
    /// actually moved anything (`crate::update::editor_key`).
    pub fn cursor(&self) -> (usize, usize) {
        let c = self.area.cursor();
        (c.0, c.1)
    }

    /// The exact bytes the value had when the buffer was opened.
    pub fn original(&self) -> &[u8] {
        &self.original
    }

    /// Whether the value being edited was JSON-classified.
    pub fn was_json(&self) -> bool {
        self.was_json
    }

    /// The current text, lines joined with `\n`.
    pub fn text(&self) -> Vec<u8> {
        self.area.lines().join("\n").into_bytes()
    }

    /// Whether the current text is still valid JSON, when the value being
    /// edited was JSON-classified to begin with. `None` when the question
    /// does not arise — a plain String edit never had JSON syntax to lose.
    pub fn json_valid(&self) -> Option<bool> {
        self.was_json
            .then(|| serde_json::from_slice::<serde_json::Value>(&self.text()).is_ok())
    }

    /// Whether the text has changed from what the buffer was opened with.
    ///
    /// A TTL buffer is asked about its own `text`, not the `TextArea`: its
    /// capture is the hand-painted single-line one (ADR-0019 D11), so the
    /// `TextArea` stays empty and comparing it would report every TTL buffer
    /// as dirty the moment it was seeded with anything.
    ///
    /// That distinction is load-bearing rather than tidy. `format_duration`
    /// seeds the field at two units' precision, so a key at `1d 2h 30m 10s`
    /// seeds `1d 2h` — and staging *that* unchanged would write `1d 2h`,
    /// silently shortening the key by forty minutes nobody asked to lose.
    /// Answering honestly here means an untouched TTL field stages nothing
    /// at all, exactly like every other editor, and the lossy seed can only
    /// ever reach the server after a reader has edited it into something
    /// they meant.
    pub fn is_dirty(&self) -> bool {
        match &self.target {
            EditTarget::Ttl { text } => text.as_bytes() != self.original,
            _ => self.text() != self.original,
        }
    }

    /// The lines to render, for the value pane's `&TextArea` widget.
    pub fn widget(&self) -> &TextArea<'static> {
        &self.area
    }

    pub fn insert_char(&mut self, c: char) {
        self.area.insert_char(c);
    }

    pub fn insert_newline(&mut self) {
        self.area.insert_newline();
    }

    pub fn insert_tab(&mut self) {
        self.area.insert_tab();
    }

    /// One paste, as a single undo step (`ratatui-textarea`'s `insert_str`
    /// already groups the whole insertion into one history entry).
    pub fn insert_str(&mut self, s: &str) {
        self.area.insert_str(s);
    }

    pub fn backspace(&mut self) {
        self.area.delete_char();
    }

    pub fn delete_forward(&mut self) {
        self.area.delete_next_char();
    }

    pub fn move_cursor(&mut self, m: CursorMove) {
        self.area.move_cursor(m);
    }

    pub fn undo(&mut self) {
        self.area.undo();
    }

    pub fn redo(&mut self) {
        self.area.redo();
    }
}

/// Whether `text` is a score `⌃S` may stage (PLAN M2 task 9, D4, ADR-0018).
///
/// Accepts what Redis's `ZADD` accepts: decimal and exponent floats, and
/// `inf`/`+inf`/`-inf` case-insensitively (`str::parse::<f64>()` already
/// covers all of that). **`nan` must be rejected explicitly, after
/// parsing** — `str::parse::<f64>()` accepts `"nan"`/`"NaN"`/`"NAN"`
/// case-insensitively, so a successful parse is not by itself proof Redis
/// will accept the value: it refuses `nan` with `ERR value is not a valid
/// float`, and finding that out after the confirm dialog is strictly worse
/// than being told while typing (R4.4).
pub fn is_valid_zset_score(text: &str) -> bool {
    match text.parse::<f64>() {
        Ok(value) => !value.is_nan(),
        Err(_) => false,
    }
}

/// Where the Viewer's `row` of a String starts in its raw text, as
/// `(line, char column)`.
///
/// `StringValue::lines` cuts every `\n`-separated line into width-sized
/// chunks, and an empty line still takes a row, so walking the raw lines while
/// consuming chunks recovers both the line and the column the row starts at.
fn string_position(s: &StringValue, row: usize) -> (usize, usize) {
    let mut display = 0;
    for (line, raw) in s.raw.split('\n').enumerate() {
        let len = raw.chars().count();
        let mut col = 0;
        loop {
            if display == row {
                return (line, col);
            }
            let chunk = s.lines.get(display).map_or(0, |l| l.chars().count());
            display += 1;
            col += chunk;
            if chunk == 0 || col >= len {
                break;
            }
        }
    }
    (usize::MAX, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::value::{JsonValue, PairValue, StringValue};

    #[test]
    fn a_string_opens_on_the_line_and_column_the_viewer_row_starts_at() {
        // Width 8: "abcdefghij" draws as "abcdefgh" / "ij", then "" and "xyz".
        let value = Value::Str(StringValue::new("abcdefghij\n\nxyz", 8));
        let at = |row| EditBuffer::from_value(&value, row).unwrap().area.cursor();
        assert_eq!(at(0), (0, 0));
        assert_eq!(at(1), (0, 8), "the second chunk of a wrapped line");
        assert_eq!(at(2), (1, 0), "an empty line still has a row");
        assert_eq!(at(3), (2, 0));
    }

    #[test]
    fn json_opens_on_the_viewer_row_as_its_line() {
        let value = Value::Json(JsonValue::parse(r#"{"a":1,"b":2}"#));
        let buf = EditBuffer::from_value(&value, 2).unwrap();
        assert_eq!(buf.area.cursor(), (2, 0));
    }

    #[test]
    fn a_row_past_the_end_lands_on_the_last_line() {
        let value = Value::Str(StringValue::new("one\ntwo", 40));
        let buf = EditBuffer::from_value(&value, 99).unwrap();
        assert_eq!(buf.area.cursor().0, 1);
    }

    #[test]
    fn a_string_opens_with_the_raw_text_not_wrapped_display_lines() {
        let text = "x".repeat(200);
        let value = Value::Str(StringValue::new(&text, 40));
        let buf = EditBuffer::from_value(&value, 0).unwrap();
        assert_eq!(buf.text(), text.into_bytes());
        assert!(!buf.was_json());
    }

    #[test]
    fn json_opens_pretty_printed_with_was_json_set() {
        let value = Value::Json(JsonValue::parse(r#"{"a":1}"#));
        let buf = EditBuffer::from_value(&value, 0).unwrap();
        assert_eq!(buf.text(), b"{\n  \"a\": 1\n}");
        assert!(buf.was_json());
    }

    #[test]
    fn typing_undo_and_redo_change_the_text() {
        let value = Value::Str(StringValue::new("old", 40));
        let mut buf = EditBuffer::from_value(&value, 0).unwrap();
        buf.move_cursor(CursorMove::End);
        buf.insert_char('!');
        assert_eq!(buf.text(), b"old!");
        buf.undo();
        assert_eq!(buf.text(), b"old");
        buf.redo();
        assert_eq!(buf.text(), b"old!");
    }

    #[test]
    fn a_value_with_no_spaces_still_wraps_so_the_cursor_stays_on_screen() {
        use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
        let token = "x".repeat(100);
        let value = Value::Str(StringValue::new(&token, 40));
        let editor = EditBuffer::from_value(&value, 0).unwrap();
        let area = Rect::new(0, 0, 20, 10);
        let mut out = Buffer::empty(area);
        editor.widget().render(area, &mut out);
        let rows = (0..10).filter(|y| out[(0, *y)].symbol() == "x").count();
        assert_eq!(rows, 5, "100 characters at width 20 is five rows");
    }

    #[test]
    fn a_value_exactly_at_the_limit_is_accepted() {
        let text = "a".repeat(MAX_EDIT_BYTES);
        let value = Value::Str(StringValue::new(&text, 40));
        assert!(EditBuffer::from_value(&value, 0).is_ok());
    }

    #[test]
    fn a_value_one_byte_over_the_limit_is_refused_with_a_notice() {
        let text = "a".repeat(MAX_EDIT_BYTES + 1);
        let value = Value::Str(StringValue::new(&text, 40));
        let err = EditBuffer::from_value(&value, 0).unwrap_err();
        assert!(err.contains("too large to edit inline"));
        assert!(
            err.contains("external-editor escape hatch"),
            "must not promise a keybinding that doesn't exist yet: {err}"
        );
    }

    #[test]
    fn a_collection_refuses_with_a_notice() {
        let value = Value::Hash(PairValue {
            pairs: vec![("f".into(), "v".into())],
            total: 1,
        });
        let err = EditBuffer::from_value(&value, 0).unwrap_err();
        assert_eq!(err, "only string values are editable so far");
    }

    #[test]
    fn binary_refuses_with_a_notice() {
        use crate::state::value::BinaryValue;
        let value = Value::Binary(BinaryValue {
            bytes: vec![0, 1, 2],
        });
        let err = EditBuffer::from_value(&value, 0).unwrap_err();
        assert_eq!(err, "binary values aren't editable here yet");
    }

    #[test]
    fn a_hash_field_that_is_not_utf8_refuses_with_a_notice() {
        let refused = "binary fields aren't editable here yet";
        assert_eq!(
            EditBuffer::for_hash_field(b"\xff", b"v").unwrap_err(),
            refused
        );
        assert_eq!(
            EditBuffer::for_hash_field(b"f", b"\x80").unwrap_err(),
            refused
        );
    }

    #[test]
    fn new_set_member_opens_a_single_part_form_with_no_field() {
        let buf = EditBuffer::new_set_member();
        assert_eq!(buf.target(), &EditTarget::NewSetMember);
        assert_eq!(buf.field_name(), None, "a member has no field name");
        assert_eq!(buf.active_part(), None, "no FIELD/VALUE split for a member");
        assert_eq!(buf.text(), b"");
        assert!(!buf.was_json());
    }

    #[test]
    fn list_element_opens_on_the_raw_value_with_the_index() {
        let buf = EditBuffer::list_element(3, b"beta").unwrap();
        assert_eq!(buf.target(), &EditTarget::ListElement { index: 3 });
        assert_eq!(buf.text(), b"beta");
        assert_eq!(buf.field_name(), None, "a list element has no field name");
        assert_eq!(
            buf.active_part(),
            None,
            "no FIELD/VALUE split for an element"
        );
        assert!(!buf.was_json());
    }

    #[test]
    fn a_non_utf8_list_element_refuses_with_a_notice() {
        let err = EditBuffer::list_element(0, b"\xff\x80").unwrap_err();
        assert_eq!(err, "binary elements aren't editable here yet");
    }

    #[test]
    fn new_list_element_opens_a_single_part_form_defaulting_to_tail() {
        let buf = EditBuffer::new_list_element();
        assert_eq!(
            buf.target(),
            &EditTarget::NewListElement {
                end: crate::state::value::ListEnd::Tail
            }
        );
        assert_eq!(buf.field_name(), None, "a list element has no field name");
        assert_eq!(buf.active_part(), None, "no FIELD/VALUE split for an add");
        assert_eq!(buf.text(), b"");
        assert!(!buf.was_json());
    }

    #[test]
    fn toggle_list_end_flips_head_and_tail_and_is_a_no_op_elsewhere() {
        let mut buf = EditBuffer::new_list_element();
        assert_eq!(
            buf.target(),
            &EditTarget::NewListElement {
                end: crate::state::value::ListEnd::Tail
            }
        );
        buf.toggle_list_end();
        assert_eq!(
            buf.target(),
            &EditTarget::NewListElement {
                end: crate::state::value::ListEnd::Head
            }
        );
        buf.toggle_list_end();
        assert_eq!(
            buf.target(),
            &EditTarget::NewListElement {
                end: crate::state::value::ListEnd::Tail
            }
        );

        // No end to toggle on any other target — a no-op, not a panic.
        let mut existing = EditBuffer::list_element(0, b"beta").unwrap();
        let before = existing.target().clone();
        existing.toggle_list_end();
        assert_eq!(existing.target(), &before);
    }

    #[test]
    fn is_dirty_reflects_a_real_change_only() {
        let value = Value::Str(StringValue::new("old", 40));
        let mut buf = EditBuffer::from_value(&value, 0).unwrap();
        assert!(!buf.is_dirty());
        buf.move_cursor(CursorMove::End);
        buf.insert_char('!');
        assert!(buf.is_dirty());
        buf.undo();
        assert!(!buf.is_dirty());
    }

    #[test]
    fn zset_score_opens_seeded_from_format_score_with_the_member_fixed() {
        let buf = EditBuffer::zset_score(b"alpha", 3.5);
        assert_eq!(
            buf.target(),
            &EditTarget::ZSetScore {
                member: b"alpha".to_vec()
            }
        );
        assert_eq!(buf.text(), b"3.5");
        assert_eq!(buf.field_name(), None, "a score edit has no field name");
        assert_eq!(buf.active_part(), None, "no MEMBER/SCORE split for an edit");
        assert!(!buf.was_json());
    }

    #[test]
    fn zset_score_seeds_integral_scores_without_a_decimal_point() {
        let buf = EditBuffer::zset_score(b"alpha", 3.0);
        assert_eq!(buf.text(), b"3");
    }

    #[test]
    fn a_binary_member_does_not_block_a_zset_score_edit() {
        // D5: unlike every other row-level edit, a non-UTF-8 member is not
        // refused — the score edit never touches the member's bytes.
        let buf = EditBuffer::zset_score(b"m\xff\x80", 1.0);
        assert_eq!(
            buf.target(),
            &EditTarget::ZSetScore {
                member: b"m\xff\x80".to_vec()
            }
        );
    }

    #[test]
    fn new_zset_member_opens_a_two_part_form_on_the_member_part() {
        let buf = EditBuffer::new_zset_member();
        assert_eq!(
            buf.target(),
            &EditTarget::NewZSetMember {
                member: String::new(),
                part: FieldPart::Name,
            }
        );
        assert_eq!(buf.field_name(), Some(""));
        assert_eq!(
            buf.active_part(),
            Some(FieldPart::Name),
            "D6: the M3 inventory's predicted case — a real Some(..), not the old wildcard's None"
        );
        assert_eq!(buf.text(), b"");
        assert!(!buf.was_json());
    }

    #[test]
    fn is_valid_zset_score_accepts_what_redis_accepts() {
        for text in [
            "1",
            "-1",
            "0",
            "0.1",
            "-3.5",
            "1e10",
            "-1e10",
            "1.0000000000000002",
            "inf",
            "+inf",
            "-inf",
            "INF",
            "Infinity",
            "+Infinity",
            "-infinity",
        ] {
            assert!(is_valid_zset_score(text), "{text:?} should be accepted");
        }
    }

    #[test]
    fn ttl_seeds_from_format_duration_of_the_raw_ttl() {
        let buf = EditBuffer::ttl(2_520); // 42m
        assert_eq!(
            buf.target(),
            &EditTarget::Ttl {
                text: "42m".to_string()
            }
        );
        assert_eq!(buf.field_name(), None, "must not feed the duplicate checks");
        assert_eq!(buf.active_part(), None, "no FIELD/VALUE split for a TTL");
        assert!(buf.is_single_line_capture());
        assert_eq!(buf.ttl_text(), Some("42m"));
    }

    #[test]
    fn ttl_seeds_empty_for_a_key_with_no_expiry() {
        let buf = EditBuffer::ttl(crate::state::loaded::TTL_NONE);
        assert_eq!(
            buf.target(),
            &EditTarget::Ttl {
                text: String::new()
            }
        );
        assert_eq!(buf.ttl_text(), Some(""));
    }

    #[test]
    fn ttl_seeds_the_two_unit_form_for_a_multi_unit_duration() {
        let buf = EditBuffer::ttl(4_320); // 1h 12m
        assert_eq!(buf.ttl_text(), Some("1h 12m"));
    }

    /// An untouched TTL field is not dirty, so `⌃S` closes it without writing.
    ///
    /// This is what stops the two-unit seed from silently shortening a key.
    /// `format_duration` shows `1d 2h 30m 10s` as `1d 2h`, so staging an
    /// untouched buffer would write forty minutes less than the key has. The
    /// answer has to come from the target's own text: a TTL buffer's
    /// `TextArea` is always empty, so the ordinary `text() != original` check
    /// would call every seeded TTL field dirty.
    #[test]
    fn an_untouched_ttl_buffer_is_not_dirty_however_lossy_its_seed() {
        for seconds in [2_520, 4_320, 95_410, 1, i32::MAX] {
            let buf = EditBuffer::ttl(seconds);
            assert!(
                !buf.is_dirty(),
                "{seconds}s seeded {:?} and must not read as dirty",
                buf.ttl_text()
            );
        }
        // A key with no expiry seeds empty, and is equally untouched.
        assert!(!EditBuffer::ttl(crate::state::loaded::TTL_NONE).is_dirty());
    }

    #[test]
    fn a_ttl_buffer_becomes_dirty_the_moment_it_is_typed_into() {
        let mut buf = EditBuffer::ttl(95_410); // seeds "1d 2h"
        assert!(!buf.is_dirty());
        buf.name_push('5');
        assert!(buf.is_dirty(), "a typed character must register");

        // And back again: retyping the seed exactly is not a change.
        buf.name_pop();
        assert!(!buf.is_dirty(), "undoing the edit returns it to clean");

        // Clearing it entirely is a real edit — that is how persist is asked
        // for on a key that currently has an expiry.
        while buf.ttl_text().is_some_and(|t| !t.is_empty()) {
            buf.name_pop();
        }
        assert!(
            buf.is_dirty(),
            "cleared-to-persist is a change, not a no-op"
        );
    }

    #[test]
    fn ttl_typing_mutates_the_hand_painted_text_not_the_text_area() {
        let mut buf = EditBuffer::ttl(2_520);
        buf.name_pop();
        buf.name_pop();
        buf.name_pop();
        buf.name_push_str("1h");
        buf.name_push('5');
        buf.name_push('m');
        assert_eq!(buf.ttl_text(), Some("1h5m"));
        // The `TextArea`-backed `text()`/`widget()` machinery is untouched —
        // this target never routes typing through it (D11).
        assert_eq!(buf.text(), b"");
    }

    #[test]
    fn ttl_is_a_no_op_for_every_field_part_shaped_operation() {
        let mut buf = EditBuffer::ttl(2_520);
        let before = buf.target().clone();
        buf.advance_to_value();
        assert_eq!(buf.target(), &before, "no value part to advance to");
        buf.return_to_name();
        assert_eq!(buf.target(), &before);
        buf.toggle_list_end();
        assert_eq!(buf.target(), &before, "no end to toggle");
    }

    #[test]
    fn only_ttl_and_the_add_forms_name_part_are_single_line_captures() {
        assert!(EditBuffer::ttl(60).is_single_line_capture());
        assert!(EditBuffer::new_hash_field().is_single_line_capture());
        assert!(EditBuffer::new_zset_member().is_single_line_capture());
        assert!(!EditBuffer::new_set_member().is_single_line_capture());
        assert!(!EditBuffer::new_list_element().is_single_line_capture());
        let value = Value::Str(StringValue::new("v", 40));
        assert!(
            !EditBuffer::from_value(&value, 0)
                .unwrap()
                .is_single_line_capture()
        );
    }

    #[test]
    fn is_valid_zset_score_rejects_nan_and_garbage() {
        for text in [
            "nan", "NaN", "NAN", "+nan", "-nan", "", " ", "  ", "abc", "1.2.3", "1,5", "1_000",
            "0x10", "--1", "1e", "inf inf",
        ] {
            assert!(!is_valid_zset_score(text), "{text:?} should be rejected");
        }
    }
}
