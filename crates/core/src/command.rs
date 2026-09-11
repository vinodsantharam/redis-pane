//! `Command` — everything the shells must do (PLAN M0.4).

/// Which read a reply belongs to.
///
/// Reads are asynchronous and there can be more than one in flight, so a reply
/// has to say *which question it answers*. Without that, the last reply to
/// arrive wins regardless of what the user asked for last: opening a slow key
/// and then a fast one left the slow one's reply to land second and replace the
/// Open key — the value pane showing a key the user was not on.
///
/// A name would not do the job. Open A, open B, open A again, and the first
/// A's reply matches by name while answering a question two reads out of date.
/// Only an identity that changes on *every* read is sufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadToken(pub u64);

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
    RefetchOpenKey { token: ReadToken },
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
    OpenKey {
        index: usize,
        name: Vec<u8>,
        token: ReadToken,
    },
    /// Delete a key outright (`DEL`).
    ///
    /// Only ever issued once the reader has confirmed the preview
    /// [`crate::state::PendingMutation::DeleteKey`] described — this is the
    /// one point where the chokepoint's decision (allowed, or refused by
    /// Read-only Mode) becomes a command a shell will actually run (R4.4).
    DeleteKey { index: usize, name: Vec<u8> },
    /// Open the value in `$EDITOR` on a temp file (R3.2, R4.1).
    ///
    /// Handled synchronously by the terminal shell — not spawned onto a task
    /// — because nothing else may draw over the terminal while the editor
    /// owns it. `is_json` picks the temp file's extension, so the reader's
    /// own editor applies JSON syntax highlighting automatically for a value
    /// that reads as JSON.
    EditInEditor {
        name: Vec<u8>,
        current: Vec<u8>,
        is_json: bool,
    },
    /// Overwrite a String value (`SET`).
    ///
    /// Only ever issued once the reader has confirmed the preview
    /// [`crate::state::PendingMutation::SetString`] described — the same
    /// chokepoint [`Command::DeleteKey`] goes through (R4.4). On success the
    /// shell must send [`crate::Msg::ValueSet`], **never** a value read
    /// straight off this call's own reply — the core's only path for a value
    /// to reach the Viewer is a real read (ADR-0006), and `ValueSet`'s job is
    /// only to ask for one.
    SetValue { name: Vec<u8>, new: Vec<u8> },
    /// Put text on the clipboard.
    ///
    /// The core builds the text; how it reaches a clipboard is the shell's
    /// problem, and over SSH a harder one than it appears.
    CopyToClipboard { text: String, label: String },
    /// Raise a notice, dated by the shell's clock.
    ///
    /// The core has no clock of its own (ADR-0011), so it cannot date a notice
    /// it raises. This carries the words; the shell supplies the moment.
    Notify { text: String },
}
