//! `Command` — everything the shells must do (PLAN M0.4).

use crate::key::KeyName;
use crate::mutation::Mutation;

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
    /// Read a key from the server: opening it, or refetching the one already
    /// open. The one read command (ADR-0006).
    ///
    /// `key` is the exact bytes the server knows the key by, carried here
    /// rather than looked up by the shell from the Open key. The Open key's
    /// name used to be display text, so a key that was not valid UTF-8
    /// refetched a *different* key, came back `✕ deleted`, and armed tracking
    /// on that other key (review C1).
    ///
    /// `arm` says whether this read sends `CLIENT CACHING YES` with it, and the
    /// core decides it — from the same [`crate::state::Link`] the header's
    /// liveness is derived from — so the shell holds no second copy of the
    /// tracking capability to fall out of step with it (review H3). Tracking
    /// is consumed by the invalidation it produces (verified against Redis
    /// 8.4.0), so every read on a tracking connection arms; there is no way to
    /// issue one that does not.
    ReadKey {
        key: KeyName,
        /// The Loaded set row this key was read from, if one is known.
        index: Option<usize>,
        token: ReadToken,
        arm: bool,
    },
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
    /// Execute a confirmed write (R4.4).
    ///
    /// Only ever issued by confirming a [`crate::state::PendingMutation`]: the
    /// one point where the chokepoint's decision (allowed, or refused by
    /// Read-only Mode) becomes work a shell will actually do. The shell answers
    /// with [`crate::Msg::MutationSettled`], never with a value read off the
    /// write's own reply — the core's only path for a value to reach the Viewer
    /// is a real read (ADR-0006).
    ///
    /// One variant for every write (review H1): adding a mutation adds a
    /// [`Mutation`] variant, not a command, a shell arm and a reply message.
    Execute {
        mutation: Mutation,
        /// The Loaded set row a `DeleteKey` was staged from, echoed back in
        /// the reply so the tombstone lands on it. `None` for every other write.
        index: Option<usize>,
    },
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
