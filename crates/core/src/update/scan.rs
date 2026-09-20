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
