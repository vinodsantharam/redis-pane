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

    /// How many rows were fetched, when that is fewer than [`Viewer::measure`]
    /// counts — `None` when the whole value is on screen.
    ///
    /// `measure` states the value's real length, from `LLEN`/`ZCARD`/`XLEN`,
    /// while the body can only render what the read actually brought back. For
    /// the windowed types those are different numbers, and a header that gives
    /// only the first of them turns "you are looking at the newest 500 of
    /// these" into "this is all of it" — the same defect the scan cap has in
    /// the keys pane, one level down. Types fetched whole return `None` and say
    /// nothing extra.
    fn window(&self) -> Option<usize> {
        None
    }

    /// One row of cells. Called only for visible rows: render cost is a
    /// function of viewport size, not value size.
    ///
    /// `now_ms` comes from the injected clock (ADR-0011), not read directly —
    /// only Stream's age column uses it, but every type takes it, so no
    /// special-casing is needed at the one call site that draws a row.
    fn row(&self, i: usize, now_ms: u64) -> Vec<String>;
}

// ── string ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringValue {
    pub lines: Vec<String>,
    pub bytes: usize,
    /// The text exactly as read, before wrapping.
    ///
    /// `lines` is a *display* artifact — wrapped at whatever the pane's width
    /// happened to be when this was built, with wrap-inserted breaks and real
    /// `\n`s flattened into the same `Vec<String>` and therefore no longer
    /// distinguishable from each other. That makes `lines` a one-way
    /// transform: there is no rejoining it back into the original text
    /// without either guessing wrong at word boundaries or inventing
    /// newlines the value never had. Editing (R4.1) needs the real bytes, not
    /// a reconstruction of them, so they are kept here untouched.
    pub raw: String,
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
            raw: text.to_string(),
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
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
        vec![self.lines.get(i).cloned().unwrap_or_default()]
    }
}

// ── hash ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PairValue {
    pub pairs: Vec<(String, String)>,
    /// The hash's real field count, which may exceed what was fetched.
    ///
    /// `HGETALL` used to bring back the whole hash regardless of size, into a
    /// 250MB budget on the same connection the scan is using — the one type,
    /// with Set, that stayed unbounded after List/ZSet/Stream were windowed.
    /// A million-field hash now reads like every other large collection: the
    /// newest `WINDOW` fields via `HSCAN`, with `total` from `HLEN` so the
    /// header can say what fraction that is.
    pub total: usize,
}

impl Viewer for PairValue {
    fn measure(&self) -> String {
        plural(self.total, "field")
    }
    fn columns(&self) -> &'static [&'static str] {
        &["FIELD", "VALUE"]
    }
    fn row_count(&self) -> usize {
        self.pairs.len()
    }
    fn window(&self) -> Option<usize> {
        (self.pairs.len() < self.total).then_some(self.pairs.len())
    }
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
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
    fn window(&self) -> Option<usize> {
        (self.items.len() < self.total).then_some(self.items.len())
    }
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
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
    fn window(&self) -> Option<usize> {
        (self.members.len() < self.total).then_some(self.members.len())
    }
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
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
    fn window(&self) -> Option<usize> {
        (self.entries.len() < self.total).then_some(self.entries.len())
    }
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
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
        &["ID", "AGE", "FIELDS"]
    }
    fn row_count(&self) -> usize {
        self.entries.len()
    }
    fn window(&self) -> Option<usize> {
        (self.entries.len() < self.total).then_some(self.entries.len())
    }
    fn row(&self, i: usize, now_ms: u64) -> Vec<String> {
        self.entries
            .get(i)
            .map(|(id, fields)| {
                let rendered = fields
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                vec![id.clone(), stream_entry_age(id, now_ms), rendered]
            })
            .unwrap_or_default()
    }
}

/// The relative age of a stream entry, computed from the millisecond
/// timestamp Redis embeds in every ID's leading component — no extra fetch,
/// no stored state, ticking correctly between frames exactly the way the TTL
/// countdown does (R3.9, ADR-0011): a pure function of the ID and the clock.
///
/// A custom ID (`XADD key 5-0 ...`) is syntactically identical to a real
/// timestamp — Redis does not distinguish them — so an implausibly small
/// value produces an implausibly large age rather than a special case. That
/// is honest, not wrong: the entry really was assigned that ID.
pub fn stream_entry_age(id: &str, now_ms: u64) -> String {
    let Some(ms) = id.split('-').next().and_then(|s| s.parse::<u64>().ok()) else {
        return "—".into();
    };
    let secs = now_ms.saturating_sub(ms) / 1000;
    match secs {
        0 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
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
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
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
    fn row(&self, i: usize, _now_ms: u64) -> Vec<String> {
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
                total: 1,
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
            assert!(!v.row(0, 0).is_empty(), "{:?} row 0 is empty", value.kind());
            // Out of range is answered, not panicked on: scrolling shares one
            // implementation and must be safe for every type.
            let _ = v.row(9_999, 0);
        }
    }

    #[test]
    fn the_element_count_lives_in_the_header_where_it_costs_no_column() {
        let hash = PairValue {
            pairs: (0..14).map(|i| (format!("f{i}"), "v".into())).collect(),
            total: 14,
        };
        assert_eq!(hash.measure(), "14 fields");
        assert_eq!(
            PairValue {
                pairs: vec![("a".into(), "1".into())],
                total: 1,
            }
            .measure(),
            "1 field"
        );
    }

    #[test]
    fn a_hash_windows_the_same_way_list_zset_and_stream_do() {
        // Hash — with Set — was the last type where `measure` (the real
        // length) and `row_count` (what was fetched) were always the same
        // number, because the read pulled the whole collection regardless of
        // size. `window()` now answers the same question for it the other
        // windowed types already did.
        let partial = PairValue {
            pairs: vec![("f".into(), "v".into())],
            total: 1_500,
        };
        assert_eq!(partial.window(), Some(1), "fetched fewer than the total");

        let whole = PairValue {
            pairs: vec![("a".into(), "1".into())],
            total: 1,
        };
        assert_eq!(whole.window(), None, "fetched the whole thing");
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
        let row = v.row(0, 0);
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

#[cfg(test)]
mod stream_timeline_tests {
    //! Severity-2 #1: the AGE column, computed live from the millisecond
    //! timestamp Redis embeds in every entry ID — no fetch, no stored state,
    //! same discipline as the TTL countdown (R3.9, ADR-0011).

    use super::*;

    #[test]
    fn each_unit_tier_renders_correctly() {
        assert_eq!(stream_entry_age("0-0", 0), "just now");
        assert_eq!(stream_entry_age("0-0", 30_000), "30s ago");
        assert_eq!(stream_entry_age("0-0", 120_000), "2m ago");
        assert_eq!(stream_entry_age("0-0", 7_200_000), "2h ago");
        assert_eq!(stream_entry_age("0-0", 172_800_000), "2d ago");
    }

    #[test]
    fn age_is_computed_from_the_leading_millisecond_component_only() {
        // The sequence number after the dash must never leak into the math.
        assert_eq!(stream_entry_age("1000-0", 2_000), "1s ago");
        assert_eq!(stream_entry_age("1000-999", 2_000), "1s ago");
    }

    #[test]
    fn a_malformed_id_shows_a_dash_rather_than_a_wrong_number_or_a_panic() {
        assert_eq!(stream_entry_age("not-an-id", 1_000), "—");
        assert_eq!(stream_entry_age("", 1_000), "—");
    }

    #[test]
    fn a_custom_low_id_is_shown_honestly_not_specially_cased() {
        // XADD key 5-0 ... is syntactically valid and indistinguishable from a
        // real timestamp; the large resulting age is correct, not a bug.
        let age = stream_entry_age("5-0", 999_999_999);
        assert!(age.ends_with("d ago"), "{age}");
    }

    #[test]
    fn the_age_column_sits_between_id_and_fields() {
        let v = StreamValue::default();
        assert_eq!(v.columns(), ["ID", "AGE", "FIELDS"]);
    }

    #[test]
    fn the_age_ticks_between_two_renders_of_the_same_entry_with_no_refetch() {
        // Exactly the TTL discipline: the same stored entry, two different
        // clock readings, two different ages — nothing about the value itself
        // needs to change for the display to be current.
        let v = StreamValue {
            entries: vec![("60000-0".into(), vec![("k".into(), "v".into())])],
            total: 1,
        };
        assert_eq!(v.row(0, 61_000)[1], "1s ago");
        assert_eq!(v.row(0, 120_000)[1], "1m ago");
    }
}
