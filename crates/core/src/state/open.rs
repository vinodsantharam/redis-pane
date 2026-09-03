//! The open key and its currency (ADR-0006, PLAN M1.10).
//!
//! The whole of the product's founding complaint lives here. There is no cached
//! value: [`OpenKey`] holds what the server last said, and the only way to
//! change it is a message reporting a fresh read.
//!
//! What arrives depends on where the reader is. At rest an update simply lands;
//! scrolled into a large value it is announced and held; mid-edit it is held
//! entirely. That is one piece of state — [`OpenKey::at_rest`] — and it is the
//! only new state apply-if-idle costs.

use super::value::Value;

/// A value that arrived while the reader was not at rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub value: Value,
    pub ttl_seconds: i32,
    pub size_bytes: u32,
    /// Clock reading when it arrived, for `changed 2s ago`.
    pub at_ms: u64,
}

/// The key currently in the Viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenKey {
    /// Index into the Loaded set.
    pub index: usize,
    pub name: String,
    pub value: Value,
    pub ttl_seconds: i32,
    pub size_bytes: u32,
    /// Clock reading at the last completed read. Drives the TTL countdown and
    /// the Read age, both of which are computed locally (R3.9).
    pub read_at_ms: u64,
    /// Body scroll. Shared across every type, because the frame is shared.
    pub offset: usize,
    /// Whether the body is scrolled to the top and no editor is open.
    pub at_rest: bool,
    /// Set while an editor holds an unsaved buffer. An arriving update never
    /// touches it — that is a bug, not a trade-off.
    pub editing: bool,
    /// An update that arrived while the reader was not at rest.
    pub pending: Option<Pending>,
    /// The server said this key was deleted, expired or evicted. The last read
    /// value stays on screen: during an incident the question is almost always
    /// *what was in it*, and that is the moment it becomes unrecoverable.
    pub deleted_at_ms: Option<u64>,
}

impl OpenKey {
    pub fn new(
        index: usize,
        name: String,
        value: Value,
        ttl_seconds: i32,
        size_bytes: u32,
        at_ms: u64,
    ) -> Self {
        Self {
            index,
            name,
            value,
            ttl_seconds,
            size_bytes,
            read_at_ms: at_ms,
            offset: 0,
            at_rest: true,
            editing: false,
            pending: None,
            deleted_at_ms: None,
        }
    }

    /// Whether an arriving update may be applied without asking.
    ///
    /// Never while editing: clobbering someone's half-typed value is not a
    /// trade-off. Never while scrolled: pulling a row out from under a cursor
    /// in a 200-field hash is its own kind of broken.
    pub fn may_apply(&self) -> bool {
        self.at_rest && !self.editing && self.offset == 0
    }

    /// Take an update, applying it or holding it.
    pub fn absorb(&mut self, value: Value, ttl_seconds: i32, size_bytes: u32, at_ms: u64) {
        if self.may_apply() {
            self.value = value;
            self.ttl_seconds = ttl_seconds;
            self.size_bytes = size_bytes;
            self.read_at_ms = at_ms;
            self.pending = None;
            self.deleted_at_ms = None;
        } else {
            self.pending = Some(Pending {
                value,
                ttl_seconds,
                size_bytes,
                at_ms,
            });
        }
    }

    /// Apply a held update, when the reader asks for it.
    pub fn take_pending(&mut self) {
        if let Some(p) = self.pending.take() {
            self.value = p.value;
            self.ttl_seconds = p.ttl_seconds;
            self.size_bytes = p.size_bytes;
            self.read_at_ms = p.at_ms;
            self.deleted_at_ms = None;
        }
    }

    /// TTL now, counted down locally from the value read at fetch time (R3.9).
    ///
    /// The most time-sensitive figure on screen costs no round trip.
    pub fn ttl_now(&self, now_ms: u64) -> i32 {
        if self.ttl_seconds < 0 {
            return self.ttl_seconds;
        }
        let elapsed = now_ms.saturating_sub(self.read_at_ms) / 1000;
        (self.ttl_seconds as i64 - elapsed as i64).max(0) as i32
    }

    /// The header phrase describing currency, given liveness and the clock.
    pub fn currency(&self, live: bool, now_ms: u64) -> String {
        if let Some(gone) = self.deleted_at_ms {
            return format!("✕ deleted {}", ago(now_ms, gone));
        }
        if self.editing && self.pending.is_some() {
            return "✎ editing · changed · held".into();
        }
        if let Some(p) = &self.pending {
            return format!("● live · changed {}", ago(now_ms, p.at_ms));
        }
        // No update is waiting, but the reader has an unsaved buffer open.
        // Without this branch the header would read a plain "● live" while
        // editing, which says nothing about the one fact that matters most
        // right now: what is on screen is not what is saved. There is no
        // editor UI yet (M2), so `editing` is never set by shipped code today —
        // this exists so the header is correct the day one lands, rather than
        // silently wrong from the first edit built.
        if self.editing {
            return "✎ editing".into();
        }
        if live {
            "● live".into()
        } else {
            format!("○ manual · read {}", ago(now_ms, self.read_at_ms))
        }
    }
}

fn ago(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    match secs {
        0 => "just now".into(),
        s if s < 60 => format!("{s}s ago"),
        s => format!("{}m ago", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::value::{PairValue, StringValue};

    fn pair(v: &str) -> Value {
        Value::Hash(PairValue {
            pairs: vec![("f".into(), v.into())],
        })
    }

    fn open() -> OpenKey {
        OpenKey::new(0, "k".into(), pair("v1"), 600, 100, 10_000)
    }

    #[test]
    fn at_rest_an_update_simply_lands() {
        let mut k = open();
        k.absorb(pair("v2"), 500, 120, 12_000);
        assert_eq!(k.value, pair("v2"));
        assert!(k.pending.is_none());
        assert_eq!(k.currency(true, 12_000), "● live");
    }

    #[test]
    fn scrolled_an_update_is_announced_and_held() {
        let mut k = open();
        k.offset = 40;
        k.at_rest = false;
        k.absorb(pair("v2"), 500, 120, 12_000);

        assert_eq!(k.value, pair("v1"), "nothing moved under the reader");
        assert!(k.pending.is_some());
        assert_eq!(k.currency(true, 14_000), "● live · changed 2s ago");

        k.take_pending();
        assert_eq!(k.value, pair("v2"), "and it lands when asked for");
    }

    #[test]
    fn mid_edit_an_update_never_touches_the_buffer() {
        // Not a trade-off. Clobbering a half-typed value is a bug.
        let mut k = open();
        k.editing = true;
        k.absorb(pair("v2"), 500, 120, 12_000);
        assert_eq!(k.value, pair("v1"));
        assert_eq!(k.currency(true, 12_000), "✎ editing · changed · held");
    }

    #[test]
    fn a_deleted_key_keeps_its_last_value_badged() {
        let mut k = open();
        k.deleted_at_ms = Some(12_000);
        assert_eq!(k.currency(true, 15_000), "✕ deleted 3s ago");
        assert_eq!(k.value, pair("v1"), "the evidence survives");
    }

    #[test]
    fn ttl_counts_down_locally_without_a_round_trip() {
        let k = open();
        assert_eq!(k.ttl_now(10_000), 600);
        assert_eq!(k.ttl_now(70_000), 540, "a minute later");
        assert_eq!(k.ttl_now(999_999_999), 0, "and never goes negative");
    }

    #[test]
    fn a_key_with_no_expiry_stays_that_way_however_long_you_watch() {
        let k = OpenKey::new(0, "k".into(), pair("v"), -1, 10, 0);
        assert_eq!(k.ttl_now(999_999_999), -1);
    }

    #[test]
    fn degraded_liveness_shows_a_read_age_where_live_does_not() {
        let k = open();
        assert_eq!(k.currency(true, 40_000), "● live");
        assert_eq!(k.currency(false, 40_000), "○ manual · read 30s ago");
    }

    #[test]
    fn scrolling_back_to_the_top_is_not_enough_on_its_own() {
        // `at_rest` is set by navigation, not inferred, so a reader who scrolled
        // and returned still gets to choose when the update lands.
        let mut k = open();
        k.at_rest = false;
        k.offset = 0;
        assert!(!k.may_apply());
    }

    #[test]
    fn the_viewer_holds_no_second_copy_of_anything() {
        let k = OpenKey::new(
            0,
            "k".into(),
            Value::Str(StringValue::new("x", 40)),
            -1,
            1,
            0,
        );
        // One value, replaced wholesale by a read. There is no map from key
        // name to value anywhere in this type (ADR-0006).
        assert_eq!(k.value.viewer().row_count(), 1);
    }
}

#[cfg(test)]
mod editing_indicator_tests {
    //! Severity-4 UI task: the header must say when there is an unsaved
    //! buffer open, not just when an update is waiting for one. No editor
    //! exists yet (M2), so nothing sets `editing` today — this is forward
    //! plumbing, tested now so it is correct on day one rather than
    //! discovered wrong.

    use super::*;
    use crate::state::value::PairValue;

    fn pair() -> Value {
        Value::Hash(PairValue {
            pairs: vec![("f".into(), "v".into())],
        })
    }

    #[test]
    fn editing_with_nothing_pending_says_so_rather_than_reading_as_plain_live() {
        let mut k = OpenKey::new(0, "k".into(), pair(), -1, 10, 0);
        k.editing = true;
        assert_eq!(k.currency(true, 0), "✎ editing");
        assert_ne!(
            k.currency(true, 0),
            "● live",
            "must not look like an ordinary live read"
        );
    }

    #[test]
    fn editing_with_a_pending_update_still_says_held() {
        let mut k = OpenKey::new(0, "k".into(), pair(), -1, 10, 0);
        k.editing = true;
        k.absorb(pair(), -1, 10, 1_000);
        assert_eq!(k.currency(true, 1_000), "✎ editing · changed · held");
    }

    #[test]
    fn not_editing_is_unaffected() {
        let k = OpenKey::new(0, "k".into(), pair(), -1, 10, 0);
        assert_eq!(k.currency(true, 0), "● live");
    }
}
