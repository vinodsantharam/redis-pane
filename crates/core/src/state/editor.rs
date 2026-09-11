//! The inline value editor's unsaved text (R3.8, PLAN M2 task 4 rework,
//! ADR-0014).
//!
//! [`EditBuffer`] is the reader's *unsaved* text, not a cache of the server
//! value — the distinction CONTEXT.md draws between the two. It never reaches
//! the server on its own; `EditorStage` turns it into a
//! [`crate::state::PendingMutation::SetString`], which goes through the one
//! mutation chokepoint like every other write.

use ratatui_textarea::{CursorMove, TextArea, WrapMode};

use super::value::Value;

/// Values whose raw byte length exceeds this are refused inline (ADR-0014).
///
/// Set from Phase 0's spike measurements (`crates/core/examples/editor_spike.rs`,
/// throwaway): a single unwrapped line's p99 keystroke-plus-render time was
/// clean up to 200KB (≤8.5ms, comfortably inside the 16ms frame budget) on
/// every run, while 300KB was noisy across runs. The cost is dominated by
/// re-wrapping one long logical line, not by the edit operation itself.
pub const MAX_EDIT_BYTES: usize = 200 * 1024;

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
}

impl PartialEq for EditBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.area.lines() == other.area.lines()
            && self.area.cursor() == other.area.cursor()
            && self.original == other.original
            && self.was_json == other.was_json
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
    pub fn from_value(value: &Value) -> Result<EditBuffer, &'static str> {
        let (text, was_json) = match value {
            // The text exactly as read — never the wrapped display `lines`,
            // which cannot be losslessly turned back into the original bytes
            // (see `StringValue::raw`'s doc comment).
            Value::Str(s) => (s.raw.clone(), false),
            // Already pretty-printed at read time (`JsonValue::parse`) —
            // handed to the editor in that form on purpose. A JSON-looking
            // string is still a string underneath: whatever comes back is
            // staged as a plain `SET`, reformatting included.
            Value::Json(j) => (j.lines.join("\n"), true),
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
        area.set_wrap_mode(WrapMode::Word);
        // The cursor opens at the end of the value, not the start — a reader
        // asked to edit a value most often wants to append or fix the tail
        // of it, and `u16::MAX, u16::MAX` is the idiom the crate itself uses
        // for "clamp to the end of the buffer" (see `TextArea::select_all`).
        area.move_cursor(CursorMove::Jump(u16::MAX, u16::MAX));
        Ok(EditBuffer {
            area,
            original: text.into_bytes(),
            was_json,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::value::{JsonValue, PairValue, StringValue};

    #[test]
    fn a_string_opens_with_the_raw_text_not_wrapped_display_lines() {
        let text = "x".repeat(200);
        let value = Value::Str(StringValue::new(&text, 40));
        let buf = EditBuffer::from_value(&value).unwrap();
        assert_eq!(buf.text(), text.into_bytes());
        assert!(!buf.was_json());
    }

    #[test]
    fn json_opens_pretty_printed_with_was_json_set() {
        let value = Value::Json(JsonValue::parse(r#"{"a":1}"#));
        let buf = EditBuffer::from_value(&value).unwrap();
        assert_eq!(buf.text(), b"{\n  \"a\": 1\n}");
        assert!(buf.was_json());
    }

    #[test]
    fn typing_undo_and_redo_change_the_text() {
        let value = Value::Str(StringValue::new("old", 40));
        let mut buf = EditBuffer::from_value(&value).unwrap();
        buf.move_cursor(CursorMove::End);
        buf.insert_char('!');
        assert_eq!(buf.text(), b"old!");
        buf.undo();
        assert_eq!(buf.text(), b"old");
        buf.redo();
        assert_eq!(buf.text(), b"old!");
    }

    #[test]
    fn a_value_exactly_at_the_limit_is_accepted() {
        let text = "a".repeat(MAX_EDIT_BYTES);
        let value = Value::Str(StringValue::new(&text, 40));
        assert!(EditBuffer::from_value(&value).is_ok());
    }

    #[test]
    fn a_value_one_byte_over_the_limit_is_refused_with_a_notice() {
        let text = "a".repeat(MAX_EDIT_BYTES + 1);
        let value = Value::Str(StringValue::new(&text, 40));
        let err = EditBuffer::from_value(&value).unwrap_err();
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
        let err = EditBuffer::from_value(&value).unwrap_err();
        assert_eq!(err, "only string values are editable so far");
    }

    #[test]
    fn binary_refuses_with_a_notice() {
        use crate::state::value::BinaryValue;
        let value = Value::Binary(BinaryValue {
            bytes: vec![0, 1, 2],
        });
        let err = EditBuffer::from_value(&value).unwrap_err();
        assert_eq!(err, "binary values aren't editable here yet");
    }

    #[test]
    fn is_dirty_reflects_a_real_change_only() {
        let value = Value::Str(StringValue::new("old", 40));
        let mut buf = EditBuffer::from_value(&value).unwrap();
        assert!(!buf.is_dirty());
        buf.move_cursor(CursorMove::End);
        buf.insert_char('!');
        assert!(buf.is_dirty());
        buf.undo();
        assert!(!buf.is_dirty());
    }
}
