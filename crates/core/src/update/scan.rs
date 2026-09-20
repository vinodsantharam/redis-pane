//! The keyspace traversal: `SCAN` progress, the Loaded set's one cap
//! (ADR-0010), and the lazily-fetched metadata that fills in the keys
//! pane's columns (R2.4).

use super::*;

pub(super) fn scan_started(mut state: State, estimated_total: u64) -> (State, Vec<Command>) {
    state.keys.clear();
    // The Open key survives a rescan; its *index* must not. `SCAN` has
    // no stable order, so the same number will address a different key
    // once the set refills, and every use of it — the tombstone
    // writeback, the row marker in the keys pane — would then be
    // confidently wrong rather than merely absent. It is re-resolved by
    // name in `scan_batch` when the key comes back.
    if let Some(open) = &mut state.open {
        open.index = None;
    }
    state.rebuild_list();
    state.scan = ScanState::Running {
        scanned: 0,
        estimated_total,
    };
    (state, Vec::new())
}

/// Absorb a page of keys, stopping the traversal if the cap is reached.
///
/// This is the single place the cap is enforced, so there is no path that grows
/// the Loaded set past it (ADR-0010).
pub(super) fn scan_batch(mut state: State, keys: Vec<Vec<u8>>) -> (State, Vec<Command>) {
    // A rescan took the Open key's index away (`ScanStarted`). Watch for the
    // name coming back so it can be restored — here rather than by searching
    // the arena afterwards, because the index is simply the length before the
    // push, and this is the one place that knows it.
    let looking_for = match &state.open {
        Some(open) if open.index.is_none() => Some(open.name.clone()),
        _ => None,
    };
    for key in keys {
        let index = state.keys.len();
        if !state.keys.push(&key) {
            state.scan = ScanState::Capped {
                at: state.keys.len(),
            };
            return (state, vec![Command::CancelScan]);
        }
        // Only after the push succeeded: a key the cap refused has no index,
        // and claiming one would point the Open key at a row that is not there.
        if looking_for.as_ref().map(KeyName::as_bytes) == Some(key.as_slice())
            && let Some(open) = &mut state.open
        {
            open.index = Some(index);
        }
    }
    if let ScanState::Running {
        estimated_total, ..
    } = state.scan
    {
        state.scan = ScanState::Running {
            scanned: state.keys.len() as u64,
            estimated_total,
        };
    }
    state.rebuild_list();
    // Keys render as they arrive; their metadata should follow, but only for
    // the rows a reader can actually see.
    let indices = state.rows_needing_metadata();
    if indices.is_empty() {
        (state, Vec::new())
    } else {
        (state, vec![Command::FetchMetadata { indices }])
    }
}

pub(super) fn metadata_batch(
    mut state: State,
    entries: Vec<crate::msg::MetadataEntry>,
    gone: Vec<usize>,
    at_ms: u64,
) -> (State, Vec<Command>) {
    let read_at_s = epoch_secs(at_ms);
    let open_index = state.open.as_ref().and_then(|open| open.index);
    let mut row_says_alive = false;
    for e in entries {
        state.keys.set_kind(e.index, e.kind);
        state.keys.set_ttl(e.index, e.ttl_seconds, read_at_s);
        state.keys.set_size(e.index, e.size_bytes);
        row_says_alive |= Some(e.index) == open_index;
    }
    // A deleted key keeps its row and its last-known TTL and size; only
    // the type byte gives way to the tombstone. Removing the row would
    // renumber everything below the cursor between one frame and the
    // next, which is a worse lie than a row that says it is gone.
    let mut row_says_gone = false;
    for index in gone {
        state.keys.set_gone(index);
        row_says_gone |= Some(index) == open_index;
    }

    // 958b311 taught the row what the Viewer knew. This is the other
    // direction, which it did not cover: a tombstoned row has no type,
    // so it matches `rows_needing_metadata` forever and the next cursor
    // move refetches it — and a key that was deleted and then written
    // again came back to `● string 64 B ∞` in the list while the Viewer
    // still read `✕ deleted 40s ago`. With tracking off nothing ever
    // corrected it, and the list is the more believable of the two
    // because it is the one that looks untouched.
    //
    // Both directions of the disagreement are resolved the same way,
    // and not by copying one pane's opinion onto the other: ask the
    // server. The read path already dates its own answer and re-arms
    // tracking (ADR-0006), so whichever pane was wrong is corrected by
    // evidence rather than by inference.
    let viewer_says_gone = state
        .open
        .as_ref()
        .is_some_and(|open| open.deleted_at_ms.is_some());
    if (row_says_alive && viewer_says_gone) || (row_says_gone && !viewer_says_gone) {
        let commands = refetch(&mut state);
        return (state, commands);
    }
    (state, Vec::new())
}

pub(super) fn scan_complete(mut state: State) -> (State, Vec<Command>) {
    // A cap reached mid-scan already told its own story; completing
    // afterwards must not overwrite it with a smaller truth.
    if !matches!(state.scan, ScanState::Capped { .. }) {
        state.scan = ScanState::Complete {
            total: state.keys.len() as u64,
        };
    }
    (state, Vec::new())
}

pub(super) fn scan_cancelled(mut state: State) -> (State, Vec<Command>) {
    if !matches!(state.scan, ScanState::Capped { .. }) {
        state.scan = ScanState::Cancelled {
            scanned: state.keys.len() as u64,
        };
    }
    (state, Vec::new())
}

pub(super) fn scan_failed(mut state: State, error: String) -> (State, Vec<Command>) {
    state.scan = ScanState::Failed { error };
    (state, Vec::new())
}

#[cfg(test)]
mod scan_tests {
    //! The keyspace traversal, as seen by the core: batches arrive, the cap is
    //! enforced in one place, and cancellation is always available.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::LoadedSet;

    /// A scan on a realistically sized terminal, so `visible_rows` is sane.
    fn started(estimated_total: u64) -> State {
        let base = State {
            rows: 30,
            ..State::default()
        };
        let (s, _) = update(base, Msg::ScanStarted { estimated_total });
        s
    }

    fn batch(n: usize, from: usize) -> Vec<Vec<u8>> {
        (from..from + n)
            .map(|i| format!("k:{i}").into_bytes())
            .collect()
    }

    #[test]
    fn a_scan_starts_empty_and_reports_progress_against_an_estimate() {
        let s = started(180_000);
        assert!(s.keys.is_empty());
        assert_eq!(
            s.scan,
            ScanState::Running {
                scanned: 0,
                estimated_total: 180_000
            }
        );

        let (s, cmds) = update(
            s,
            Msg::ScanBatch {
                keys: batch(500, 0),
            },
        );
        assert_eq!(s.keys.len(), 500);
        assert_eq!(s.scan.readout(), "scanning 500 of ~180,000");

        // The keys render immediately; their metadata is requested separately,
        // and only for the rows on screen (R2.4).
        let Some(Command::FetchMetadata { indices }) = cmds.first() else {
            panic!("expected a metadata request, got {cmds:?}");
        };
        assert_eq!(
            indices.len(),
            s.visible_rows(),
            "500 keys arrived, but only the visible window is fetched"
        );
    }

    #[test]
    fn batches_accumulate_so_results_render_as_they_arrive() {
        let mut s = started(1_000);
        for i in 0..4 {
            (s, _) = update(
                s,
                Msg::ScanBatch {
                    keys: batch(250, i * 250),
                },
            );
        }
        assert_eq!(s.keys.len(), 1_000);
        assert_eq!(s.keys.name_str(0).unwrap(), "k:0");
        assert_eq!(s.keys.name_str(999).unwrap(), "k:999");
    }

    #[test]
    fn starting_a_new_scan_discards_the_previous_one() {
        let (s, _) = update(started(10), Msg::ScanBatch { keys: batch(5, 0) });
        assert_eq!(s.keys.len(), 5);
        let (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 10,
            },
        );
        assert!(
            s.keys.is_empty(),
            "a filter change must not append to stale results"
        );
    }

    #[test]
    fn reaching_the_cap_stops_the_scan_and_says_so() {
        let mut s = State {
            keys: LoadedSet::with_cap(600),
            ..State::default()
        };
        (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 10_000,
            },
        );
        let (s, cmds) = update(
            s,
            Msg::ScanBatch {
                keys: batch(1_000, 0),
            },
        );

        assert_eq!(
            cmds,
            vec![Command::CancelScan],
            "the shell must be told to stop"
        );
        assert_eq!(s.keys.len(), 600);
        assert!(s.keys.is_capped());
        assert_eq!(s.scan, ScanState::Capped { at: 600 });
        assert!(s.scan.readout().contains("narrow the filter"));
    }

    #[test]
    fn a_completion_arriving_after_the_cap_does_not_erase_it() {
        // The shell's CancelScan will be answered by ScanComplete or
        // ScanCancelled. Neither may overwrite the more important fact.
        let mut s = State {
            keys: LoadedSet::with_cap(10),
            ..State::default()
        };
        (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 100,
            },
        );
        (s, _) = update(s, Msg::ScanBatch { keys: batch(50, 0) });
        assert!(matches!(s.scan, ScanState::Capped { .. }));

        let (s, _) = update(s, Msg::ScanComplete);
        assert!(
            matches!(s.scan, ScanState::Capped { .. }),
            "the cap is the story"
        );

        let mut s2 = State {
            keys: LoadedSet::with_cap(10),
            ..State::default()
        };
        (s2, _) = update(
            s2,
            Msg::ScanStarted {
                estimated_total: 100,
            },
        );
        (s2, _) = update(s2, Msg::ScanBatch { keys: batch(50, 0) });
        let (s2, _) = update(s2, Msg::ScanCancelled);
        assert!(matches!(s2.scan, ScanState::Capped { .. }));
    }

    #[test]
    fn completing_normally_reports_the_total() {
        let (s, _) = update(
            started(700),
            Msg::ScanBatch {
                keys: batch(700, 0),
            },
        );
        let (s, _) = update(s, Msg::ScanComplete);
        assert_eq!(s.scan, ScanState::Complete { total: 700 });
        assert_eq!(s.scan.readout(), "700 keys");
    }

    #[test]
    fn esc_cancels_an_in_flight_scan() {
        let s = started(10_000);
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(cmds, vec![Command::CancelScan]);
    }

    #[test]
    fn esc_closes_help_first_and_leaves_the_scan_alone() {
        // Help is the nearer thing to back out of. Cancelling the scan as well
        // would make one keypress do two unrelated things.
        let mut s = started(10_000);
        s.help_open = true;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!s.help_open);
        assert!(cmds.is_empty());
        assert!(s.scan.is_running());
    }

    #[test]
    fn esc_with_nothing_in_flight_does_nothing() {
        let (_, cmds) = update(State::default(), Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(cmds.is_empty());
    }

    #[test]
    fn a_failed_scan_keeps_what_it_had_and_names_the_error() {
        let (s, _) = update(started(100), Msg::ScanBatch { keys: batch(30, 0) });
        let (s, _) = update(
            s,
            Msg::ScanFailed {
                error: "LOADING".into(),
            },
        );
        assert_eq!(s.keys.len(), 30, "partial results are still worth showing");
        assert!(s.scan.readout().contains("LOADING"));
    }
}

#[cfg(test)]
mod metadata_tests {
    //! Metadata is fetched lazily and only for what is on screen (R2.4).

    use super::*;
    use crate::msg::{KeyCode, MetadataEntry};
    use crate::state::KeyKind;
    use crate::state::loaded::TTL_NONE;

    /// The two panes must never state different things about one key.
    ///
    /// Found in use: with a key open and then deleted, the Viewer badged
    /// `✕ deleted just now` while that key's row in the list still read
    /// `● string 64 B ∞`. Nothing would have corrected it — metadata is only
    /// refetched for rows whose type is *unknown*, and this row's was known
    /// and stale — so the list quietly kept the more believable of two
    /// contradictory claims.
    #[test]
    fn a_deleted_open_key_is_badged_in_the_list_as_well_as_the_viewer() {
        use crate::state::value::{StringValue, Value};

        let state = browsing(10);
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: ReadToken::default(),
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 64,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.keys.kind(3), Some(KeyKind::String));
        assert!(!state.keys.is_gone(3));

        let (state, _) = update(
            state,
            Msg::ValueGone {
                token: ReadToken::default(),
                index: Some(3),
                name: "k:3".into(),
                at_ms: 2_000,
            },
        );
        assert!(
            state.open.as_ref().unwrap().deleted_at_ms.is_some(),
            "the Viewer knows"
        );
        assert!(state.keys.is_gone(3), "and so must the row it came from");
        assert!(!state.keys.is_gone(2), "its neighbours are untouched");
    }

    #[test]
    fn a_key_that_comes_back_stops_saying_gone() {
        use crate::state::value::{PairValue, Value};

        let mut state = browsing(10);
        state.keys.set_gone(3);
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: ReadToken::default(),
                index: Some(3),
                name: "k:3".into(),
                value: Value::Hash(PairValue::default()),
                ttl_seconds: 90,
                size_bytes: 128,
                at_ms: 3_000,
            },
        );
        assert!(
            !state.keys.is_gone(3),
            "a value came back, so the key is there"
        );
        assert_eq!(
            state.keys.kind(3),
            Some(KeyKind::Hash),
            "and the row is right at once, not a placeholder until the next scroll"
        );
        assert_eq!(state.keys.ttl(3), Some(90));
        assert_eq!(state.keys.size(3), Some(128));
    }

    fn browsing(n: usize) -> State {
        let mut state = State {
            // Wide enough for two panes, so both are on screen and a
            // pane-scoped key is not silently gated out from under the test.
            cols: 130,
            rows: 30,
            ..State::default()
        };
        (state, _) = update(
            state,
            Msg::ScanStarted {
                estimated_total: n as u64,
            },
        );
        let keys = (0..n).map(|i| format!("k:{i}").into_bytes()).collect();
        let (state, _) = update(state, Msg::ScanBatch { keys });
        state
    }

    /// Review C1: a key that is not valid UTF-8 opened correctly once, then
    /// refetched a *different* key — its replacement-character display text —
    /// came back gone, and armed tracking on that other key.
    #[test]
    fn a_key_that_is_not_utf8_is_refetched_and_tombstoned_by_its_exact_bytes() {
        use crate::state::value::{StringValue, Value};
        const RAW: &[u8] = b"\xff\xfe:session";

        let mut state = State {
            cols: 130,
            rows: 30,
            link: Link::Up {
                version: "8.4.0".into(),
                tracking: Tracking::Available,
            },
            ..State::default()
        };
        (state, _) = update(state, Msg::ScanStarted { estimated_total: 1 });
        (state, _) = update(
            state,
            Msg::ScanBatch {
                keys: vec![RAW.to_vec()],
            },
        );

        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(Command::ReadKey {
            key, token, index, ..
        }) = cmds.into_iter().next()
        else {
            panic!("expected an open");
        };
        assert_eq!(key.as_bytes(), RAW);
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index,
                name: key,
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.open.as_ref().unwrap().name.as_bytes(), RAW);
        assert!(
            state.keys.kind(0).is_some(),
            "the row learned its type, so the name guard matched"
        );

        let (state, cmds) = update(state, Msg::Invalidated);
        let Some(Command::ReadKey {
            key,
            token,
            index,
            arm,
        }) = cmds.into_iter().next()
        else {
            panic!("expected a refetch");
        };
        assert_eq!(
            key.as_bytes(),
            RAW,
            "refetched by its bytes, not its display text"
        );
        assert!(arm, "and armed for that same key");

        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index,
                name: key,
                at_ms: 2_000,
            },
        );
        assert!(
            state.keys.is_gone(0),
            "the tombstone lands on the right row"
        );
    }

    use crate::state::Attachment;

    // ── Which key is the Viewer showing? (CONTEXT.md: Open key) ────────────
    //
    // The reported defect was the cursor on one key and the value pane showing
    // another. Three separate causes, two of them races, and a race that is
    // only *usually* won is a bug that gets reported once a quarter forever.

    /// Open a slow key, then a fast one. The slow reply lands last and must not
    /// win: it is a correct value answering a question the reader has left.
    #[test]
    fn a_reply_from_a_superseded_read_never_reaches_the_viewer() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        // Open row 3 …
        state.view.selected = 3;
        let (mut state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: slow, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        // … then change your mind and open row 5 before the first came back.
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: fast, .. }) = cmds.first() else {
            panic!("expected an open, got {cmds:?}");
        };
        assert_ne!(slow, fast, "two reads, two identities");

        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: fast,
                index: Some(5),
                name: "k:5".into(),
                value: Value::Str(StringValue::new("five", 40)),
                ttl_seconds: -1,
                size_bytes: 4,
                at_ms: 1_000,
            },
        );
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: slow,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("three", 40)),
                ttl_seconds: -1,
                size_bytes: 5,
                at_ms: 2_000,
            },
        );

        assert_eq!(
            state.open.as_ref().unwrap().name,
            "k:5",
            "the last key asked for is the key on screen"
        );
        assert!(
            state.keys.kind(3).is_none(),
            "and the superseded reply wrote nothing anywhere"
        );
    }

    /// The sharper half: `ValueGone` tombstones the Open key *and* its row, so
    /// a stale one marks a perfectly healthy key deleted in both panes — and
    /// nothing afterwards corrects it.
    #[test]
    fn a_superseded_gone_reply_does_not_bury_the_key_that_is_open_now() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (mut state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: doomed, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token: current, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token: current,
                index: Some(5),
                name: "k:5".into(),
                value: Value::Str(StringValue::new("five", 40)),
                ttl_seconds: -1,
                size_bytes: 4,
                at_ms: 1_000,
            },
        );

        // k:3 really was deleted — but that is news about k:3, not about k:5.
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token: doomed,
                index: Some(3),
                name: "k:3".into(),
                at_ms: 2_000,
            },
        );
        assert!(
            state.open.as_ref().unwrap().deleted_at_ms.is_none(),
            "the open key is alive and must not be badged as deleted"
        );
        assert!(!state.keys.is_gone(5), "nor may its row be tombstoned");
    }

    /// The bug reported from use, reproduced exactly: key A is open, the
    /// reader arrows onto a *different*, already-deleted key B — not a
    /// superseded read of A, a fresh, current, correctly-answered read of B.
    /// The old token-only `ValueGone` had no way to tell this apart from a
    /// reply about A, and badged A — alive, unrelated — as deleted, tombstoning
    /// its row too.
    #[test]
    fn opening_a_gone_key_never_misattributes_it_to_the_key_that_was_already_open() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        // k:3 (A) is open and alive. Now arrow onto k:5 (B) — a different key
        // — and B turns out to be gone.
        state.view.selected = 5;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index: Some(5),
                name: "k:5".into(),
                at_ms: 2_000,
            },
        );

        let open = state.open.as_ref().unwrap();
        assert_eq!(
            open.name, "k:5",
            "the pane must name the key just asked for"
        );
        assert!(open.value.is_none(), "nothing was ever read for it");
        assert!(open.deleted_at_ms.is_some());
        assert!(state.keys.is_gone(5), "B's own row is correctly tombstoned");
        assert!(
            !state.keys.is_gone(3),
            "A is alive and must not be falsely marked gone in the keys pane"
        );
    }

    /// The quieter half of the same bug: nothing was open at all, so the old
    /// handler's `if let Some(open) = &mut state.open` guard made a fresh gone
    /// key a complete no-op — the keypress produced no visible change
    /// whatsoever, indistinguishable from the app having done nothing.
    #[test]
    fn opening_a_gone_key_from_a_cold_start_is_not_a_silent_no_op() {
        let mut state = browsing(10);
        assert!(state.open.is_none());

        state.view.selected = 2;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index: Some(2),
                name: "k:2".into(),
                at_ms: 1_000,
            },
        );

        let open = state
            .open
            .as_ref()
            .expect("the keypress must produce something");
        assert_eq!(open.name, "k:2");
        assert!(open.value.is_none());
        assert!(state.keys.is_gone(2));
    }

    /// Refetching a key that is *already* the gone placeholder — `r` pressed
    /// again, and the server still says it does not exist — must not panic or
    /// disturb anything; it is Case A (the name matches what is already open)
    /// landing on a key with no value rather than one with a stale value.
    #[test]
    fn refetching_an_already_gone_key_that_is_still_gone_is_idempotent() {
        let mut state = browsing(10);
        state.view.selected = 4;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index: Some(4),
                name: "k:4".into(),
                at_ms: 1_000,
            },
        );
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('r'))));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected a refetch, got {cmds:?}");
        };
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index: Some(4),
                name: "k:4".into(),
                at_ms: 2_000,
            },
        );
        let open = state.open.unwrap();
        assert_eq!(open.name, "k:4");
        assert!(open.value.is_none());
        assert!(open.deleted_at_ms.is_some());
    }

    /// A key that was gone and has since been recreated: `absorb` must treat
    /// `None → Some` as unambiguously `Updated`, not a special case and not
    /// `Unchanged` — there was nothing before for the new value to match.
    #[test]
    fn a_gone_key_that_comes_back_is_reported_as_updated_not_unchanged() {
        use crate::state::open::{OpenKey, ReadOutcome};
        use crate::state::value::{StringValue, Value};

        let mut open = OpenKey::gone(Some(0), "k".into(), 0);
        assert!(open.value.is_none());
        open.absorb(Value::Str(StringValue::new("v", 40)), -1, 4, 1_000);

        assert_eq!(open.value, Some(Value::Str(StringValue::new("v", 40))));
        assert!(open.deleted_at_ms.is_none(), "no longer gone");
        assert!(matches!(open.last_read, ReadOutcome::Updated { .. }));
    }

    /// `SCAN` has no stable order, so an index outlives its meaning. Keeping it
    /// would put the row marker and the tombstone on whatever key inherited the
    /// number — confidently wrong, which is worse than absent.
    #[test]
    fn a_rescan_takes_the_open_keys_index_and_a_later_batch_gives_it_back() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.open.as_ref().unwrap().index, Some(3));
        assert_eq!(state.open.as_ref().unwrap().row, Some(3));

        let (state, _) = update(
            state,
            Msg::ScanStarted {
                estimated_total: 10,
            },
        );
        assert_eq!(
            state.open.as_ref().unwrap().index,
            None,
            "the number means nothing until the key is seen again"
        );
        assert_eq!(state.open.as_ref().unwrap().row, None);
        assert_eq!(
            state.attachment(),
            Some(Attachment::DetachedOffList),
            "and the Viewer says so rather than pointing at a row"
        );

        // The keyspace comes back in a different order, as it may.
        let keys = ["k:7", "k:3", "k:1"]
            .iter()
            .map(|k| k.as_bytes().to_vec())
            .collect();
        let (state, _) = update(state, Msg::ScanBatch { keys });
        assert_eq!(
            state.open.as_ref().unwrap().index,
            Some(1),
            "re-resolved by name, at wherever it landed this time"
        );
    }

    /// The metadata writeback is guarded by the name, not only by the token: a
    /// rescan can land between a read and its reply without either being stale.
    #[test]
    fn a_value_reply_never_writes_metadata_onto_a_renumbered_row() {
        use crate::state::value::{PairValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        // The rescan refills row 3 with a different key.
        let (state, _) = update(state, Msg::ScanStarted { estimated_total: 4 });
        let keys = ["a", "b", "c", "somebody-else"]
            .iter()
            .map(|k| k.as_bytes().to_vec())
            .collect();
        let (state, _) = update(state, Msg::ScanBatch { keys });

        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Hash(PairValue::default()),
                ttl_seconds: 90,
                size_bytes: 128,
                at_ms: 1_000,
            },
        );
        assert!(
            state.keys.kind(3).is_none(),
            "row 3 is `somebody-else` now and knows nothing about k:3"
        );
        assert_eq!(
            state.open.as_ref().unwrap().name,
            "k:3",
            "the Viewer still took the value, which was never in doubt"
        );
    }

    /// The three answers the panes are drawn from.
    #[test]
    fn attachment_distinguishes_on_the_cursor_from_elsewhere_from_nowhere() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        assert_eq!(state.attachment(), None, "nothing open, no question to ask");

        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        assert_eq!(state.attachment(), Some(Attachment::Attached));

        state.view.selected = 1;
        assert_eq!(
            state.attachment(),
            Some(Attachment::Detached { rows: 2 }),
            "two rows below the cursor"
        );

        state.list.filter = "k:9".into();
        state.rebuild_list();
        assert_eq!(
            state.attachment(),
            Some(Attachment::DetachedOffList),
            "filtered away, so there is no row to point at"
        );
    }

    /// The hint bar names the action for the focused pane; `r` has to perform
    /// that one. The held-update branch used to run first, so `r` in the keys
    /// pane applied a value in the *other* pane and did not rescan — while the
    /// hint said `r rescan`, the list did not move, and no scan readout
    /// appeared to explain it.
    #[test]
    fn r_in_the_keys_pane_rescans_even_with_an_update_waiting_in_the_viewer() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v1", 40)),
                ttl_seconds: -1,
                size_bytes: 2,
                at_ms: 1_000,
            },
        );
        // Scrolled, so the next value is held rather than applied.
        {
            let open = state.open.as_mut().unwrap();
            open.offset = 3;
            open.at_rest = false;
        }
        let token = state.read_token;
        let (mut state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v2", 40)),
                ttl_seconds: -1,
                size_bytes: 2,
                at_ms: 2_000,
            },
        );
        assert!(state.open.as_ref().unwrap().pending.is_some(), "held");

        state.focus = Pane::Keys;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('r'))));
        assert!(
            matches!(cmds.as_slice(), [Command::StartScan { .. }]),
            "the hint says `r rescan`, so `r` rescans: {cmds:?}"
        );
        assert!(
            state.open.as_ref().unwrap().pending.is_some(),
            "and the held update is still held, untouched by a keys-pane key"
        );
    }

    /// 958b311 taught the row what the Viewer knew. The other direction was
    /// open: a tombstoned row has no type, so it is refetched on the next
    /// cursor move, and a key deleted and then written again came back to
    /// `● string 64 B ∞` in the list while the Viewer still read `✕ deleted`.
    #[test]
    fn a_key_that_comes_back_makes_the_viewer_go_and_ask() {
        use crate::msg::MetadataEntry;
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        let token = state.read_token;
        let (state, _) = update(
            state,
            Msg::ValueGone {
                token,
                index: Some(3),
                name: "k:3".into(),
                at_ms: 2_000,
            },
        );
        assert!(state.keys.is_gone(3), "both panes agree it is gone");

        // The row is refetched, because a tombstone has no type, and the server
        // says the key is there again.
        let (state, cmds) = update(
            state,
            Msg::MetadataBatch {
                entries: vec![MetadataEntry {
                    index: 3,
                    kind: crate::state::KeyKind::String,
                    ttl_seconds: -1,
                    size_bytes: 64,
                }],
                gone: Vec::new(),
                at_ms: 1_000,
            },
        );
        assert!(
            matches!(cmds.as_slice(), [Command::ReadKey { .. }]),
            "the panes disagree, so ask the server rather than guess: {cmds:?}"
        );
        assert!(
            state.open.as_ref().unwrap().deleted_at_ms.is_some(),
            "and the badge stays until the answer arrives — no guessing either way"
        );
    }

    /// The same rule in the other direction: metadata noticed the deletion
    /// first, and the Viewer is still showing a live-looking value.
    #[test]
    fn a_row_that_goes_makes_the_viewer_go_and_ask_too() {
        use crate::state::value::{StringValue, Value};

        let mut state = browsing(10);
        state.view.selected = 3;
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        let Some(&Command::ReadKey { token, .. }) = cmds.first() else {
            panic!("expected an open");
        };
        let (state, _) = update(
            state,
            Msg::ValueLoaded {
                token,
                index: Some(3),
                name: "k:3".into(),
                value: Value::Str(StringValue::new("v", 40)),
                ttl_seconds: -1,
                size_bytes: 1,
                at_ms: 1_000,
            },
        );
        let (_, cmds) = update(
            state,
            Msg::MetadataBatch {
                entries: Vec::new(),
                gone: vec![3],
                at_ms: 1_000,
            },
        );
        assert!(
            matches!(cmds.as_slice(), [Command::ReadKey { .. }]),
            "{cmds:?}"
        );
    }

    // ── An action must not act on a pane that is not on screen ─────────────
    //
    // Below 70 columns one pane is drawn at a time (DESIGN §2). Aiming a
    // pane-scoped key at the other one produced no visible change whatsoever,
    // which is the single worst thing a TUI can look like: indistinguishable
    // from having stopped responding.

    fn narrow_viewing() -> State {
        let mut state = browsing(10);
        state.cols = 60;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert_eq!(state.focus, Pane::Value, "→ pushes onto the value");
        assert!(
            !state.keys_pane_visible(),
            "and the list is off screen there"
        );
        state
    }

    /// The sharpest case. `/` began capturing into a filter line rendered
    /// inside a pane of zero width: every keypress after it disappeared, `q`
    /// typed a `q` instead of quitting, and only `Esc` escaped.
    #[test]
    fn slash_cannot_open_an_invisible_keystroke_sink() {
        let state = narrow_viewing();
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(
            !state.filtering,
            "no capture may start while the pane that would show it is not drawn"
        );

        // And `q` still means quit, which is the part that mattered.
        let (_, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('q'))));
        assert_eq!(cmds, vec![Command::Quit]);
    }

    /// The cursor moved an arbitrary distance with nothing on screen changing,
    /// and `Esc` then returned to a list that had silently wandered.
    #[test]
    fn the_key_list_does_not_move_while_it_is_off_screen() {
        let state = narrow_viewing();
        let before = state.view.selected;
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::End)));
        assert_eq!(state.view.selected, before, "the cursor stayed put");
    }

    /// The same defect mirrored: on the list screen the value is not drawn, so
    /// its scroll keys were moving an offset nobody could see.
    #[test]
    fn the_value_does_not_scroll_while_it_is_off_screen() {
        let mut state = browsing(10);
        state.cols = 60;
        state.open = Some(OpenKey::new(
            Some(0),
            "k".into(),
            crate::state::value::Value::Str(crate::state::value::StringValue::new("v", 40)),
            -1,
            10,
            0,
        ));
        assert!(
            !state.value_pane_visible(),
            "the list is what is drawn here"
        );
        let (state, _) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Down)));
        assert_eq!(state.open.as_ref().unwrap().offset, 0);
    }

    /// Both panes are drawn above 70 columns, so the two-pane keymap is
    /// untouched: plain arrows drive the list from either pane and `⌃`-arrows
    /// drive the value, exactly as DESIGN §4 specifies.
    #[test]
    fn nothing_is_gated_while_both_panes_are_on_screen() {
        let state = browsing(10);
        let (mut state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        assert_eq!(state.focus, Pane::Value);
        assert!(state.keys_pane_visible() && state.value_pane_visible());

        let before = state.view.selected;
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_ne!(
            state.view.selected, before,
            "the list still moves from the Viewer at two-pane widths"
        );
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(state.filtering, "and `/` still filters");
    }

    /// Reached by dragging a window edge rather than by pressing a key: a
    /// filter being typed at two-pane width, then narrowed past the breakpoint
    /// with the Viewer focused, would leave the capture running off screen.
    #[test]
    fn narrowing_the_terminal_keeps_an_open_filter_where_it_can_be_seen() {
        let state = browsing(10);
        let (mut state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
        (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(state.filtering);

        let (state, _) = update(state, Msg::Resized { cols: 60, rows: 30 });
        assert!(state.filtering, "the filter is still being typed");
        assert!(
            state.keys_pane_visible(),
            "so the pane showing it must be the one on screen"
        );
    }

    #[test]
    fn only_the_visible_window_is_ever_requested() {
        let state = browsing(10_000);
        let indices = state.rows_needing_metadata();
        assert_eq!(indices.len(), state.visible_rows());
        assert!(
            indices.iter().all(|i| *i < state.visible_rows()),
            "requested a row nobody can see"
        );
    }

    #[test]
    fn arriving_metadata_lands_on_the_right_rows() {
        let state = browsing(100);
        let (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries: vec![MetadataEntry {
                    index: 2,
                    kind: KeyKind::Hash,
                    ttl_seconds: 2_537,
                    size_bytes: 2_150,
                }],
                gone: Vec::new(),
                at_ms: 1_000,
            },
        );
        assert_eq!(state.keys.kind(2), Some(KeyKind::Hash));
        assert_eq!(state.keys.ttl(2), Some(2_537));
        assert_eq!(state.keys.size(2), Some(2_150));
        assert_eq!(state.keys.kind(1), None, "its neighbours are untouched");
    }

    #[test]
    fn rows_that_already_have_metadata_are_not_requested_again() {
        let state = browsing(100);
        let entries = (0..state.visible_rows())
            .map(|i| MetadataEntry {
                index: i,
                kind: KeyKind::String,
                ttl_seconds: TTL_NONE,
                size_bytes: 10,
            })
            .collect();
        let (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries,
                gone: Vec::new(),
                at_ms: 1_000,
            },
        );
        assert!(
            state.rows_needing_metadata().is_empty(),
            "the visible window is fully known, so nothing more is owed"
        );
    }

    #[test]
    fn scrolling_asks_for_what_scrolling_revealed() {
        let state = browsing(10_000);
        let entries = (0..state.visible_rows())
            .map(|i| MetadataEntry {
                index: i,
                kind: KeyKind::String,
                ttl_seconds: TTL_NONE,
                size_bytes: 10,
            })
            .collect();
        let (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries,
                gone: Vec::new(),
                at_ms: 1_000,
            },
        );

        // Page down past the known rows.
        let (state, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::PageDown)));
        let Some(Command::FetchMetadata { indices }) = cmds.first() else {
            panic!("scrolling into unknown rows must ask for them, got {cmds:?}");
        };
        assert!(!indices.is_empty());
        assert!(
            indices.iter().all(|i| state.keys.kind(*i).is_none()),
            "already-known rows must not be re-fetched"
        );
    }

    #[test]
    fn navigation_is_clamped_to_the_loaded_set() {
        let state = browsing(5);
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::End)));
        assert_eq!(state.view.selected, 4);
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(state.view.selected, 4, "cannot walk off the end");
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Home)));
        assert_eq!(state.view.selected, 0);
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Up)));
        assert_eq!(state.view.selected, 0, "nor off the start");
    }

    #[test]
    fn navigating_an_empty_keyspace_does_nothing_rather_than_panicking() {
        let (state, cmds) = update(State::default(), Msg::Key(KeyPress::plain(KeyCode::Down)));
        assert_eq!(state.view.selected, 0);
        assert!(cmds.is_empty());
    }

    // ── Selection follows the key, not the row (search-filter cursor bug) ──
    //
    // `view.selected` is a row number, and rows are meaningless across a
    // filter change: the same number can point at an unrelated key once the
    // list has narrowed, widened or been cleared. The cursor must stick to
    // the key the reader was on, not the row it happened to occupy.

    fn selected_name(state: &State) -> Option<String> {
        state
            .selected_key()
            .and_then(|i| state.keys.name_str(i))
            .map(|s| s.into_owned())
    }

    fn open_filter(state: State) -> State {
        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char('/'))));
        assert!(state.filtering);
        state
    }

    fn type_filter(state: State, text: &str) -> State {
        let mut state = state;
        for c in text.chars() {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Char(c))));
        }
        state
    }

    /// `k:5` is the only key containing a `5`, so narrowing to it must keep
    /// the cursor on it rather than resetting to the top of the match.
    #[test]
    fn typing_a_narrower_filter_keeps_the_selected_key_selected() {
        let mut state = browsing(10);
        for _ in 0..5 {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        }
        assert_eq!(selected_name(&state).as_deref(), Some("k:5"));

        let state = type_filter(open_filter(state), "5");
        assert_eq!(
            selected_name(&state).as_deref(),
            Some("k:5"),
            "the only match is the key that was already selected"
        );
    }

    /// Typing past the selected key's last match has nothing to stick to, so
    /// it falls back to the top of the new list rather than an arbitrary row.
    #[test]
    fn typing_a_filter_the_selected_key_fails_falls_back_to_the_top() {
        let mut state = browsing(10);
        for _ in 0..5 {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        }
        assert_eq!(selected_name(&state).as_deref(), Some("k:5"));

        // "k:1" matches "k:1" only, excluding the selected "k:5".
        let state = type_filter(open_filter(state), "k:1");
        assert_eq!(state.view.selected, 0, "nothing to stick to but the top");
        assert_eq!(selected_name(&state).as_deref(), Some("k:1"));
    }

    /// Backspacing the filter back down widens the list again; the cursor
    /// must not silently become whatever key now falls on its old row.
    #[test]
    fn backspacing_the_filter_keeps_the_selected_key_selected() {
        let mut state = browsing(10);
        for _ in 0..5 {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        }
        let state = type_filter(open_filter(state), "5");
        assert_eq!(selected_name(&state).as_deref(), Some("k:5"));

        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Backspace)));
        assert_eq!(
            selected_name(&state).as_deref(),
            Some("k:5"),
            "widening the filter must not relocate the cursor to an unrelated row"
        );
    }

    /// This is the reported bug: clear the filter entirely with Esc, and the
    /// cursor used to land on whatever key the old row number now pointed at.
    #[test]
    fn clearing_the_filter_with_esc_keeps_the_selected_key_selected() {
        let mut state = browsing(10);
        for _ in 0..5 {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        }
        let state = type_filter(open_filter(state), "5");
        assert_eq!(selected_name(&state).as_deref(), Some("k:5"));

        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!state.filtering, "Esc also exits filter capture");
        assert_eq!(
            selected_name(&state).as_deref(),
            Some("k:5"),
            "clearing the filter must not relocate the cursor to an unrelated row"
        );
    }

    /// The same three transitions, but in tree mode, where rows can be group
    /// headers with no key behind them — `row_of`/`key_at` must still resolve
    /// the selected key correctly rather than only working for the flat list.
    #[test]
    fn the_selected_key_stays_selected_across_a_filter_change_in_tree_mode() {
        let mut state = browsing(10);
        state.tree_mode = true;
        state.rebuild_list();
        for _ in 0..5 {
            (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Down)));
        }
        // Tree mode may spend rows on group headers, so which key ends up
        // selected after five Downs is a tree-folding detail, not something
        // this test should assert — only that the fold landed on an actual
        // key, and that that key is the one that stays selected below.
        let name = selected_name(&state).expect("landed on a key, not a group header");
        let digit = name.strip_prefix("k:").expect("browsing() names are k:N");

        let state = type_filter(open_filter(state), digit);
        assert_eq!(selected_name(&state).as_deref(), Some(name.as_str()));

        let (state, _) = update(state, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(
            selected_name(&state).as_deref(),
            Some(name.as_str()),
            "tree mode must relocate the selected key too, not just the flat list"
        );
    }

    /// The filter-mode hint is the only on-screen surface reachable while
    /// filtering (the help overlay cannot be opened, since `?` is captured as
    /// text there) — it must say what Esc/Enter do, not repeat the generic
    /// five-action hint bar.
    #[test]
    fn the_hint_bar_explains_filter_capture_while_it_is_open() {
        let state = browsing(10);
        assert!(
            !crate::render::hint_bar(&state).contains("clear & exit"),
            "the generic hint bar applies outside filter capture"
        );

        let state = open_filter(state);
        assert!(
            crate::render::hint_bar(&state).contains("clear & exit"),
            "the filter hint must say what Esc does, since nothing else does"
        );
    }
}
