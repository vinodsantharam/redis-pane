//! The inline value editor's unsaved text (R3.8, PLAN M2 task 4 rework,
//! ADR-0014).
//!
//! [`EditBuffer`] is the reader's *unsaved* text, not a cache of the server
//! value — the distinction CONTEXT.md draws between the two. It never reaches
//! the server on its own; `EditorStage` turns it into a
//! [`crate::state::PendingMutation::SetString`], which goes through the one
//! mutation chokepoint like every other write.

use ratatui_textarea::{CursorMove, TextArea, WrapMode};

use super::value::{StringValue, Value};

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
/// [`crate::state::PendingMutation`] out of, and what a `Msg::NotWritten`
/// reply is about when it asks the buffer what command it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    /// The Open value's whole body — a String or a JSON-classified String
    /// (M2 task 4's original behaviour, unchanged).
    Value,
    /// One field of the Open Hash, being overwritten (`HSET`, guarded).
    HashField { field: String },
    /// A brand-new field of the Open Hash, not yet on the server (`HSETNX`,
    /// guarded).
    NewHashField { field: String },
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
    /// Handed to the confirm dialog. Still drawn, so the pane shows what is
    /// about to be written rather than the value it replaces, but it takes no
    /// more keys.
    staged: bool,
    /// What this buffer writes back when staged (PLAN M2 task 6, D3).
    target: EditTarget,
}

impl PartialEq for EditBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.area.lines() == other.area.lines()
            && self.area.cursor() == other.area.cursor()
            && self.original == other.original
            && self.was_json == other.was_json
            && self.staged == other.staged
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
            staged: false,
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
    pub fn for_hash_field(field: String, value: &str) -> Result<EditBuffer, &'static str> {
        if value.len() > MAX_EDIT_BYTES {
            return Err(
                "too large to edit inline (over 200KB) — an external-editor escape hatch is planned",
            );
        }
        let was_json = serde_json::from_str::<serde_json::Value>(value).is_ok();
        let lines: Vec<String> = value.split('\n').map(str::to_string).collect();
        let mut area = TextArea::new(lines);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        Ok(EditBuffer {
            area,
            original: value.as_bytes().to_vec(),
            was_json,
            staged: false,
            target: EditTarget::HashField { field },
        })
    }

    /// An empty buffer for a field that does not exist on the server yet
    /// (`a`, PLAN M2 task 6, D4). There is no "original" to compare against
    /// but an empty one — Redis allows an empty field value, so an
    /// unmodified empty buffer still stages an `HSETNX` with an empty value
    /// rather than being treated as "nothing to save".
    pub fn new_hash_field(field: String) -> EditBuffer {
        let mut area = TextArea::new(vec![String::new()]);
        area.set_wrap_mode(WrapMode::WordOrGlyph);
        area.set_cursor_line_style(ratatui::style::Style::default());
        EditBuffer {
            area,
            original: Vec::new(),
            was_json: false,
            staged: false,
            target: EditTarget::NewHashField { field },
        }
    }

    /// What this buffer writes back when staged.
    pub fn target(&self) -> &EditTarget {
        &self.target
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

    /// Hand the buffer to the confirm dialog: it stays on screen but takes no
    /// more keys.
    pub fn stage(&mut self) {
        self.staged = true;
    }

    /// Take the buffer back from the confirm dialog, to be typed into again.
    pub fn unstage(&mut self) {
        self.staged = false;
    }

    pub fn is_staged(&self) -> bool {
        self.staged
    }

    /// Whether the text has changed from what the buffer was opened with.
    pub fn is_dirty(&self) -> bool {
        self.text() != self.original
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
}
