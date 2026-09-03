//! `Command` — everything the shells must do (PLAN M0.4).

/// Work the core cannot perform itself. A shell executes these and reports back
/// as a [`crate::Msg`].
///
/// Every Redis read belongs here, and there is exactly one variant for it, so
/// the arming step that liveness depends on cannot be forgotten on one branch
/// of several (ADR-0006).
/// Deliberately **not** `#[non_exhaustive]`. That attribute exists to let a
/// crate add variants without breaking downstream matches — which is precisely
/// the opposite of what is wanted here. Every `Command` must be executed by a
/// shell, so adding one should fail the shell's `match` at compile time rather
/// than silently doing nothing at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Tear down the terminal and exit.
    Quit,
    /// Re-read the open key from the server.
    ///
    /// **This always re-arms tracking**, and there is deliberately no variant
    /// that reads without arming. Tracking is consumed by the invalidation it
    /// produces (verified against Redis 8.4.0), so a read path that skipped
    /// arming would leave the Viewer permanently dark while the header still
    /// said `live` — the original RedisInsight defect by another route. Making
    /// it one command is what stops that being possible on one branch of
    /// several (ADR-0006).
    RefetchOpenKey,
    /// Try to connect again after the given delay.
    Reconnect { after_ms: u64 },
    /// Begin traversing the keyspace. `SCAN` only, never `KEYS` — cursor-based,
    /// streaming and resumable (R2.1).
    StartScan { pattern: Option<String> },
    /// Stop an in-flight traversal. Every long operation is cancellable.
    CancelScan,
    /// Fetch type, TTL and memory usage for these rows of the Loaded set.
    ///
    /// Only ever the visible window (R2.4). Fetching metadata for a whole
    /// keyspace would be `KEYS *` with extra steps, and it would compete with
    /// `SCAN` for the connection while the list is still filling.
    FetchMetadata { indices: Vec<usize> },
    /// Open a key: read it, arming tracking in the same breath.
    ///
    /// Distinct from [`Command::RefetchOpenKey`] only in that it changes which
    /// key is open; both go through the one read path that always arms.
    OpenKey { index: usize, name: Vec<u8> },
    /// Put text on the clipboard.
    ///
    /// The core builds the text; how it reaches a clipboard is the shell's
    /// problem, and over SSH a harder one than it appears.
    CopyToClipboard { text: String, label: &'static str },
}
