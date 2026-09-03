//! Type-aware value viewing (R3.1, R3.2, PLAN M1.8 and M1.9).
//!
//! **Type-awareness is one abstraction, not a `match` scattered through the
//! UI.** Every Redis type implements [`Viewer`], which supplies a summary line
//! and a table of rows. The frame around it — header, body, footer — and the
//! navigation through it are written once and are identical for every type, so
//! `↑↓` and `PgDn` behave the same in a hash as in a stream.
//!
//! Note what a viewer cannot do: it holds a value it was *given*, and has no
//! way to fetch one. Reads always hit the server (ADR-0006), so there is
//! nowhere here for a cache to accrete.

use super::loaded::KeyKind;

/// A fetched value, ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(StringValue),
    Hash(PairValue),
    List(IndexedValue),
    Set(MemberValue),
    ZSet(ScoredValue),
    Stream(StreamValue),
    Json(JsonValue),
    Binary(BinaryValue),
}

impl Value {
    /// The one place a type maps to its viewer.
    pub fn viewer(&self) -> &dyn Viewer {
        match self {
            Value::Str(v) => v,
            Value::Hash(v) => v,
            Value::List(v) => v,
            Value::Set(v) => v,
            Value::ZSet(v) => v,
            Value::Stream(v) => v,
            Value::Json(v) => v,
            Value::Binary(v) => v,
        }
    }

    pub fn kind(&self) -> KeyKind {
        match self {
            Value::Str(_) => KeyKind::String,
            Value::Hash(_) => KeyKind::Hash,
            Value::List(_) => KeyKind::List,
            Value::Set(_) => KeyKind::Set,
            Value::ZSet(_) => KeyKind::ZSet,
            Value::Stream(_) => KeyKind::Stream,
            Value::Json(_) => KeyKind::Json,
            Value::Binary(_) => KeyKind::Other,
        }
    }
}

/// What every type must supply. Everything else about the Viewer is shared.
pub trait Viewer {
    /// The count phrase for the header, e.g. `14 fields`.
    ///
    /// This is where element count lives (R2.4): stated the moment a key is
    /// opened, rather than costing a column in the key list.
    fn measure(&self) -> String;

    /// Column headings for the body. Empty for types with no table.
    fn columns(&self) -> &'static [&'static str];

    /// How many rows the body has. Drives scrolling, which is shared.
    fn row_count(&self) -> usize;

    /// One row of cells. Called only for visible rows: render cost is a
    /// function of viewport size, not value size.
    fn row(&self, i: usize) -> Vec<String>;
}

// ── string ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringValue {
    pub lines: Vec<String>,
    pub bytes: usize,
}

impl StringValue {
    /// Wrap at the pane width. Long values are common and horizontal scrolling
    /// is worse than wrapping for something you are reading rather than editing.
    pub fn new(text: &str, width: usize) -> Self {
        let width = width.max(8);
        let mut lines = Vec::new();
        for raw in text.split('\n') {
            if raw.is_empty() {
                lines.push(String::new());
            }
            let mut rest: Vec<char> = raw.chars().collect();
            while !rest.is_empty() {
                let take = width.min(rest.len());
                lines.push(rest[..take].iter().collect());
                rest.drain(..take);
            }
        }
        Self {
            lines,
            bytes: text.len(),
        }
    }
}

impl Viewer for StringValue {
    fn measure(&self) -> String {
        format!("{} bytes", self.bytes)
    }
    fn columns(&self) -> &'static [&'static str] {
        &[]
    }
    fn row_count(&self) -> usize {
        self.lines.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        vec![self.lines.get(i).cloned().unwrap_or_default()]
    }
}

// ── hash ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PairValue {
    pub pairs: Vec<(String, String)>,
}

impl Viewer for PairValue {
    fn measure(&self) -> String {
        plural(self.pairs.len(), "field")
    }
    fn columns(&self) -> &'static [&'static str] {
        &["FIELD", "VALUE"]
    }
    fn row_count(&self) -> usize {
        self.pairs.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        self.pairs
            .get(i)
            .map(|(k, v)| vec![k.clone(), v.clone()])
            .unwrap_or_default()
    }
}

// ── list ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexedValue {
    pub items: Vec<String>,
    /// The list's real length, which may exceed what was fetched.
    pub total: usize,
}

impl Viewer for IndexedValue {
    fn measure(&self) -> String {
        plural(self.total, "item")
    }
    fn columns(&self) -> &'static [&'static str] {
        &["#", "VALUE"]
    }
    fn row_count(&self) -> usize {
        self.items.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        self.items
            .get(i)
            .map(|v| vec![i.to_string(), v.clone()])
            .unwrap_or_default()
    }
}

// ── set ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemberValue {
    pub members: Vec<String>,
    pub total: usize,
}

impl Viewer for MemberValue {
    fn measure(&self) -> String {
        plural(self.total, "member")
    }
    fn columns(&self) -> &'static [&'static str] {
        &["MEMBER"]
    }
    fn row_count(&self) -> usize {
        self.members.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        self.members
            .get(i)
            .map(|m| vec![m.clone()])
            .unwrap_or_default()
    }
}

// ── sorted set ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScoredValue {
    pub entries: Vec<(String, f64)>,
    pub total: usize,
}

impl Eq for ScoredValue {}

impl Viewer for ScoredValue {
    fn measure(&self) -> String {
        plural(self.total, "member")
    }
    fn columns(&self) -> &'static [&'static str] {
        &["SCORE", "MEMBER"]
    }
    fn row_count(&self) -> usize {
        self.entries.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        self.entries
            .get(i)
            .map(|(m, s)| vec![format_score(*s), m.clone()])
            .unwrap_or_default()
    }
}

/// Scores are f64 but are usually integers; printing `1` beats printing `1.0`.
fn format_score(s: f64) -> String {
    if s.fract() == 0.0 && s.abs() < 1e15 {
        format!("{}", s as i64)
    } else {
        format!("{s}")
    }
}

// ── stream ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamValue {
    pub entries: Vec<(String, Vec<(String, String)>)>,
    pub total: usize,
}

impl Viewer for StreamValue {
    fn measure(&self) -> String {
        plural(self.total, "entry")
    }
    fn columns(&self) -> &'static [&'static str] {
        &["ID", "FIELDS"]
    }
    fn row_count(&self) -> usize {
        self.entries.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        self.entries
            .get(i)
            .map(|(id, fields)| {
                let rendered = fields
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                vec![id.clone(), rendered]
            })
            .unwrap_or_default()
    }
}

// ── JSON ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JsonValue {
    pub lines: Vec<String>,
}

impl JsonValue {
    /// Pretty-print, falling back to the raw text when it will not parse.
    ///
    /// A value that claims to be JSON and is not should still be readable —
    /// showing an error instead of the bytes helps nobody debugging it.
    pub fn parse(text: &str) -> Self {
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => Self {
                lines: serde_json::to_string_pretty(&v)
                    .unwrap_or_else(|_| text.to_string())
                    .lines()
                    .map(str::to_string)
                    .collect(),
            },
            Err(_) => Self {
                lines: text.lines().map(str::to_string).collect(),
            },
        }
    }
}

impl Viewer for JsonValue {
    fn measure(&self) -> String {
        plural(self.lines.len(), "line")
    }
    fn columns(&self) -> &'static [&'static str] {
        &[]
    }
    fn row_count(&self) -> usize {
        self.lines.len()
    }
    fn row(&self, i: usize) -> Vec<String> {
        vec![self.lines.get(i).cloned().unwrap_or_default()]
    }
}

// ── binary ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BinaryValue {
    pub bytes: Vec<u8>,
}

const HEX_WIDTH: usize = 16;

impl Viewer for BinaryValue {
    fn measure(&self) -> String {
        format!("{} bytes", self.bytes.len())
    }
    fn columns(&self) -> &'static [&'static str] {
        &["OFFSET", "HEX", "ASCII"]
    }
    fn row_count(&self) -> usize {
        self.bytes.len().div_ceil(HEX_WIDTH)
    }
    fn row(&self, i: usize) -> Vec<String> {
        let start = i * HEX_WIDTH;
        let chunk =
            &self.bytes[start.min(self.bytes.len())..(start + HEX_WIDTH).min(self.bytes.len())];
        let hex = chunk
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        vec![format!("{start:08x}"), hex, ascii]
    }
}

fn plural(n: usize, noun: &str) -> String {
    // "entry" pluralises irregularly and is the only such noun here.
    let plural = match noun {
        "entry" => "entries".to_string(),
        other => format!("{other}s"),
    };
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R3.1's proof: every type answers the same four questions, so the frame
    /// and the navigation around them can be written once.
    #[test]
    fn every_type_satisfies_the_same_interface() {
        let values = vec![
            Value::Str(StringValue::new("hello", 40)),
            Value::Hash(PairValue {
                pairs: vec![("a".into(), "1".into())],
            }),
            Value::List(IndexedValue {
                items: vec!["x".into()],
                total: 1,
            }),
            Value::Set(MemberValue {
                members: vec!["m".into()],
                total: 1,
            }),
            Value::ZSet(ScoredValue {
                entries: vec![("m".into(), 1.0)],
                total: 1,
            }),
            Value::Stream(StreamValue {
                entries: vec![("1-0".into(), vec![("f".into(), "v".into())])],
                total: 1,
            }),
            Value::Json(JsonValue::parse("{\"a\":1}")),
            Value::Binary(BinaryValue {
                bytes: vec![0, 255],
            }),
        ];
        for value in &values {
            let v = value.viewer();
            assert!(!v.measure().is_empty(), "{:?} has no measure", value.kind());
            assert!(v.row_count() > 0, "{:?} has no rows", value.kind());
            assert!(!v.row(0).is_empty(), "{:?} row 0 is empty", value.kind());
            // Out of range is answered, not panicked on: scrolling shares one
            // implementation and must be safe for every type.
            let _ = v.row(9_999);
        }
    }

    #[test]
    fn the_element_count_lives_in_the_header_where_it_costs_no_column() {
        let hash = PairValue {
            pairs: (0..14).map(|i| (format!("f{i}"), "v".into())).collect(),
        };
        assert_eq!(hash.measure(), "14 fields");
        assert_eq!(
            PairValue {
                pairs: vec![("a".into(), "1".into())]
            }
            .measure(),
            "1 field"
        );
    }

    #[test]
    fn entries_pluralise_correctly_because_someone_will_notice() {
        let s = StreamValue {
            entries: vec![],
            total: 1,
        };
        assert_eq!(s.measure(), "1 entry");
        let s = StreamValue {
            entries: vec![],
            total: 2,
        };
        assert_eq!(s.measure(), "2 entries");
    }

    #[test]
    fn a_string_wraps_rather_than_scrolling_sideways() {
        let v = StringValue::new("abcdefghij", 8);
        assert_eq!(v.lines, ["abcdefgh", "ij"]);
        assert_eq!(v.measure(), "10 bytes");
    }

    #[test]
    fn wrapping_has_a_floor_so_a_narrow_pane_does_not_produce_one_char_lines() {
        let v = StringValue::new("abcdefghij", 2);
        assert_eq!(v.lines, ["abcdefgh", "ij"], "clamped to the 8-column floor");
    }

    #[test]
    fn newlines_in_a_string_are_kept() {
        let v = StringValue::new("one\ntwo", 40);
        assert_eq!(v.lines, ["one", "two"]);
    }

    #[test]
    fn json_is_pretty_printed_and_bad_json_still_shows_its_bytes() {
        let good = JsonValue::parse(r#"{"a":1,"b":[2,3]}"#);
        assert!(good.lines.len() > 1, "pretty-printed: {:?}", good.lines);

        let bad = JsonValue::parse("{not json");
        assert_eq!(
            bad.lines,
            ["{not json"],
            "a broken value must stay readable"
        );
    }

    #[test]
    fn binary_renders_as_a_hex_dump_with_an_ascii_gutter() {
        let v = BinaryValue {
            bytes: b"AB\x00\xff".to_vec(),
        };
        let row = v.row(0);
        assert_eq!(row[0], "00000000");
        assert_eq!(row[1], "41 42 00 ff");
        assert_eq!(row[2], "AB..", "unprintables become dots, not gaps");
    }

    #[test]
    fn scores_print_as_integers_when_they_are_integers() {
        assert_eq!(format_score(3.0), "3");
        assert_eq!(format_score(1.5), "1.5");
    }

    #[test]
    fn a_viewer_has_no_way_to_fetch_anything() {
        // ADR-0006, expressed as a type: `Viewer` takes `&self` and returns
        // strings. There is no client, no channel, and nowhere for a cached
        // value to accumulate.
        let v = PairValue::default();
        assert_eq!(v.row_count(), 0);
    }
}
