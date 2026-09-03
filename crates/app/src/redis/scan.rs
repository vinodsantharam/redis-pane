//! The keyspace source: a stream of keys with progress (R2.1, PLAN M1.2).
//!
//! **This abstracts over a stream of keys, not over a cursor.** v1 has exactly
//! one cursor behind it; Cluster will have N, one per master node, merged
//! (ADR-0008). Nothing above this module may learn how many there are — that is
//! the whole reason the abstraction exists before it is needed.
//!
//! `SCAN` only, never `KEYS`: cursor-based, streaming, resumable, and
//! cancellable at any page boundary.

use fred::prelude::*;
use fred::types::scan::Scanner;
use futures::StreamExt;
use redis_pane_core::Msg;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// How many keys to ask for per page.
///
/// `COUNT` is a hint, not a guarantee. Small enough that a page arrives
/// promptly and the list fills visibly; large enough not to pay a round trip
/// per handful of keys.
const PAGE: u32 = 500;

/// Traverse the keyspace, sending batches as they arrive.
///
/// Returns when the traversal finishes, fails, or is cancelled — having sent a
/// terminal [`Msg`] in each case, so the core always learns how it ended.
pub async fn stream_keys(
    client: &Client,
    pattern: Option<&str>,
    tx: Sender<Msg>,
    cancel: CancellationToken,
) {
    // DBSIZE is the denominator in "41,203 of ~180,000". It is an estimate by
    // nature — the keyspace moves while we walk it — which is why the readout
    // says `~` and never a percentage of something exact.
    let estimated_total: u64 = client.dbsize().await.unwrap_or(0);
    if tx.send(Msg::ScanStarted { estimated_total }).await.is_err() {
        return;
    }

    let pattern = pattern.unwrap_or("*");
    let mut pages = client.scan(pattern, Some(PAGE), None);

    loop {
        let page = tokio::select! {
            biased;
            // Cancellation is checked first so `Esc` is answered at the next
            // page boundary rather than after the whole keyspace.
            _ = cancel.cancelled() => {
                let _ = tx.send(Msg::ScanCancelled).await;
                return;
            }
            page = pages.next() => page,
        };

        let Some(page) = page else {
            let _ = tx.send(Msg::ScanComplete).await;
            return;
        };

        let mut page = match page {
            Ok(page) => page,
            Err(e) => {
                let _ = tx
                    .send(Msg::ScanFailed {
                        error: describe_scan_error(&e),
                    })
                    .await;
                return;
            }
        };

        if let Some(keys) = page.take_results() {
            let batch: Vec<Vec<u8>> = keys.into_iter().map(|k| k.into_bytes().to_vec()).collect();
            if !batch.is_empty() && tx.send(Msg::ScanBatch { keys: batch }).await.is_err() {
                return; // the UI is gone
            }
        }

        if !page.has_more() {
            let _ = tx.send(Msg::ScanComplete).await;
            return;
        }

        // Ask for the next page only now, which is what keeps memory bounded by
        // the Loaded set rather than by however fast the server can talk. If
        // this is never called the scan continues on drop, which would be the
        // unbounded behaviour we are avoiding.
        page.next();
    }
}

/// Server states that are not really errors deserve their own words (ADR-0009).
fn describe_scan_error(e: &Error) -> String {
    let details = e.details();
    if details.starts_with("LOADING") {
        "server is loading its dataset".into()
    } else if details.starts_with("BUSY") {
        "server is busy running a script".into()
    } else if details.is_empty() {
        format!("{e}")
    } else {
        details.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_and_busy_are_explained_rather_than_echoed() {
        let loading = Error::new(ErrorKind::Unknown, "LOADING Redis is loading the dataset");
        assert_eq!(
            describe_scan_error(&loading),
            "server is loading its dataset"
        );

        let busy = Error::new(ErrorKind::Unknown, "BUSY Redis is busy running a script");
        assert_eq!(
            describe_scan_error(&busy),
            "server is busy running a script"
        );
    }

    #[test]
    fn an_ordinary_error_is_passed_through() {
        let other = Error::new(ErrorKind::Unknown, "NOPERM this user has no permissions");
        assert!(describe_scan_error(&other).contains("NOPERM"));
    }
}
