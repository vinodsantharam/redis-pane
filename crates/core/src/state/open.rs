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

/// Whether the Viewer is showing the Selected key.
///
/// The Open key and the Selected key are allowed to differ, and routinely do:
/// opening is explicit, so moving the cursor leaves the Viewer where it was.
/// That is useful — it is how you read one key while looking for another — but
/// it is only usable if the app says when it applies. Left unsaid it produces
/// the complaint this type exists to answer: the value pane appearing to show
/// the wrong key, when in fact it is showing the right value for a key the
/// reader is no longer on. Nothing here is about freshness; the value is a live
/// tracked read either way (ADR-0006). It is about *whose* value it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// The Open key is the Selected key. The quiet case, and the common one.
    Attached,
    /// The Open key is elsewhere in the list, this many rows from the cursor —
    /// negative above, positive below.
    Detached { rows: isize },
    /// The Open key is open but has no row to point at: filtered out, inside a
    /// collapsed group, or not yet re-resolved after a rescan. The keys pane
    /// has nothing to mark, so the Viewer has to carry the whole signal.
    DetachedOffList,
}

/// What the last completed read of this key found.
///
/// ADR-0006 opens by naming the ambiguity the product exists to remove: the
/// user *"cannot distinguish 'the refresh did nothing' from 'the value
/// genuinely did not change'"*. Without this the app reproduced it. A Refetch
/// that found nothing left every cell of the frame identical, which is also
/// exactly what a reply dropped as a superseded read looks like, and what a
/// read that failed and had its notice missed looks like. The value was right
/// either way; the reader had no way to know a read had happened at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The key was just opened — there was nothing on screen to compare to.
    Opened,
    /// The value that arrived differed from the one on screen.
    Updated { at_ms: u64 },
    /// The value that arrived was identical, byte for byte.
    Unchanged { at_ms: u64 },
}

impl ReadOutcome {
    /// When this was learned, if it was learned from a read.
    fn at_ms(&self) -> Option<u64> {
        match self {
            ReadOutcome::Opened => None,
            ReadOutcome::Updated { at_ms } | ReadOutcome::Unchanged { at_ms } => Some(*at_ms),
        }
    }
}

/// A value that arrived while the reader was not at rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub value: Value,
    pub ttl_seconds: i32,
    pub size_bytes: u32,
    /// Clock reading when it arrived, for `changed 2s ago`.
    pub at_ms: u64,
}

/// The Open key: the key currently in the Viewer.
///
/// It is frequently *not* the Selected key — opening is explicit, and moving
/// the cursor does not move the Viewer. Both panes state that relationship
/// rather than leaving two key names to be compared by eye; the state behind
/// that is [`crate::state::State::attachment`], computed from [`OpenKey::row`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenKey {
    /// Index into the Loaded set, while one is known to address this key.
    ///
    /// `None` after a rescan. `SCAN` order is not stable, so the Loaded set is
    /// renumbered and the old index may address a *different key* — writing
    /// this key's type or tombstone through it would corrupt an unrelated row,
    /// and marking that row as the Open key would point the user confidently at
    /// the wrong line. It is re-resolved by name when the key is scanned again.
    /// [`OpenKey::name`] is the identity that never goes stale.
    pub index: Option<usize>,
    /// Display row of this key, while it has one.
    ///
    /// `None` when the key has no row to be on: filtered out, inside a
    /// collapsed group, or not yet re-resolved after a rescan. Recomputed when
    /// the row list is rebuilt rather than searched for per frame — moving the
    /// cursor never changes it, so the render path needs no reverse lookup.
    pub row: Option<usize>,
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
    /// What the last completed read found, so the header can say so.
    pub last_read: ReadOutcome,
}

impl OpenKey {
    pub fn new(
        index: Option<usize>,
        name: String,
        value: Value,
        ttl_seconds: i32,
        size_bytes: u32,
        at_ms: u64,
    ) -> Self {
        Self {
            index,
            row: None,
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
            last_read: ReadOutcome::Opened,
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

    /// How long the header states what the last read found before falling back
    /// to its resting phrase.
    ///
    /// Long enough to be read, short enough that it cannot be mistaken for a
    /// standing description of the key.
    pub const OUTCOME_MS: u64 = 2_500;

    /// Take an update, applying it or holding it.
    pub fn absorb(&mut self, value: Value, ttl_seconds: i32, size_bytes: u32, at_ms: u64) {
        if self.may_apply() {
            // Recorded before the move, and only where the value actually
            // reaches the screen — a held update has not changed anything the
            // reader can see, and `pending` is what speaks for it.
            self.last_read = if self.value == value {
                ReadOutcome::Unchanged { at_ms }
            } else {
                ReadOutcome::Updated { at_ms }
            };
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
            self.last_read = if self.value == p.value {
                ReadOutcome::Unchanged { at_ms: p.at_ms }
            } else {
                ReadOutcome::Updated { at_ms: p.at_ms }
            };
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
        // What the last read found, while it is still news. This is the half
        // ADR-0006 named and the app did not have: without it, a Refetch that
        // found nothing renders a frame identical in every cell to one where
        // the reply was dropped as superseded, or failed, or was never sent.
        // The reader is left doing exactly what the ADR describes — unable to
        // tell "the refresh did nothing" from "nothing changed".
        //
        // It fades, because it is an account of an event and not a description
        // of the key. After it does, the resting phrases below take over.
        if let Some(at) = self.last_read.at_ms()
            && now_ms.saturating_sub(at) < Self::OUTCOME_MS
        {
            return match (live, self.last_read) {
                (true, ReadOutcome::Updated { .. }) => "● live · updated now".into(),
                (true, ReadOutcome::Unchanged { .. }) => "● live · unchanged".into(),
                (false, ReadOutcome::Updated { .. }) => "○ manual · updated now".into(),
                (false, ReadOutcome::Unchanged { .. }) => "○ manual · unchanged".into(),
                // Unreachable: `at_ms()` is `None` for `Opened`.
                (_, ReadOutcome::Opened) => unreachable!(),
            };
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
        OpenKey::new(Some(0), "k".into(), pair("v1"), 600, 100, 10_000)
    }

    #[test]
    fn at_rest_an_update_simply_lands_and_the_header_says_it_did() {
        let mut k = open();
        k.absorb(pair("v2"), 500, 120, 12_000);
        assert_eq!(k.value, pair("v2"));
        assert!(k.pending.is_none());
        // This used to assert a bare `● live`, which is what made a landed
        // update and an hour of silence render identically.
        assert_eq!(k.currency(true, 12_000), "● live · updated now");
        assert_eq!(
            k.currency(true, 12_000 + OpenKey::OUTCOME_MS),
            "● live",
            "it is an account of an event, so it fades back"
        );
    }

    /// ADR-0006's founding complaint, at the level of one string: pressing `r`
    /// and learning nothing is the defect, not the absence of a change.
    #[test]
    fn a_read_that_found_nothing_says_so_rather_than_looking_like_no_read() {
        let mut k = open();
        let before = k.currency(false, 12_000);
        k.absorb(pair("v1"), 600, 100, 12_000);

        assert_eq!(k.value, pair("v1"), "nothing changed, correctly");
        assert_eq!(k.currency(false, 12_000), "○ manual · unchanged");
        assert_ne!(
            k.currency(false, 12_000),
            before,
            "and the frame is not identical to one where no read happened"
        );
        assert_eq!(
            k.currency(true, 12_000),
            "● live · unchanged",
            "the same question exists when live and `r` is pressed by hand"
        );
    }

    #[test]
    fn applying_a_held_update_reports_what_it_turned_out_to_be() {
        let mut k = open();
        k.offset = 40;
        k.at_rest = false;
        k.absorb(pair("v2"), 500, 120, 12_000);
        assert!(k.pending.is_some(), "held while scrolled");

        k.offset = 0;
        k.at_rest = true;
        k.take_pending();
        assert_eq!(k.currency(true, 12_000), "● live · updated now");
    }

    /// A held update speaks for itself through `pending`; the outcome line must
    /// not pre-empt it, because nothing has reached the screen yet.
    #[test]
    fn a_held_update_does_not_report_an_outcome_it_has_not_had() {
        let mut k = open();
        k.offset = 40;
        k.at_rest = false;
        k.absorb(pair("v2"), 500, 120, 12_000);
        assert_eq!(k.currency(true, 12_000), "● live · changed just now");
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
        let k = OpenKey::new(Some(0), "k".into(), pair("v"), -1, 10, 0);
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
            Some(0),
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
        let mut k = OpenKey::new(Some(0), "k".into(), pair(), -1, 10, 0);
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
        let mut k = OpenKey::new(Some(0), "k".into(), pair(), -1, 10, 0);
        k.editing = true;
        k.absorb(pair(), -1, 10, 1_000);
        assert_eq!(k.currency(true, 1_000), "✎ editing · changed · held");
    }

    #[test]
    fn not_editing_is_unaffected() {
        let k = OpenKey::new(Some(0), "k".into(), pair(), -1, 10, 0);
        assert_eq!(k.currency(true, 0), "● live");
    }
}
