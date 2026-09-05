//! `Msg` — everything that can happen (PLAN M0.4).

/// Metadata for one key, as fetched.
///
/// `ttl_seconds` uses the store's convention: `-1` means the key has no expiry,
/// which is a fact about the key rather than a gap in what we know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataEntry {
    pub index: usize,
    pub kind: crate::state::KeyKind,
    pub ttl_seconds: i32,
    pub size_bytes: u32,
}

/// A key, described without reference to any terminal library.
///
/// The shell translates crossterm's events into these. That translation is the
/// boundary doing its job: `crossterm` is not reachable from the core, so the
/// core cannot accidentally grow a dependency on how input arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// A keypress with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyPress {
    /// An unmodified key.
    pub fn plain(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
        }
    }

    /// A key held with Control.
    pub fn ctrl(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: true,
            alt: false,
        }
    }

    /// Whether this is the character `c` with no modifiers.
    pub fn is_char(&self, c: char) -> bool {
        self.code == KeyCode::Char(c) && !self.ctrl && !self.alt
    }
}

/// Every input to the core: keystrokes, resizes, and replies from the shells.
///
/// The core has no other way in. A shell that wants to tell the core something
/// adds a variant here rather than reaching into state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Msg {
    /// A key was pressed.
    Key(KeyPress),
    /// The terminal was resized. Drives the layout breakpoints in DESIGN §2.
    Resized {
        cols: u16,
        rows: u16,
    },
    /// A read of the open key completed at this clock reading.
    ReadCompleted {
        at_ms: u64,
    },
    /// The shell connected. `tracking_supported` is the result of *attempting*
    /// `CLIENT TRACKING`, never an inference from the version (ADR-0007).
    Connected {
        version: String,
        tracking_supported: bool,
    },
    /// The link dropped mid-session.
    ConnectionLost,
    /// A reconnect attempt is scheduled. Backoff is visible, never a silent wait.
    ReconnectScheduled {
        attempt: u32,
        retry_in_ms: u64,
    },
    /// The server accepted `CLIENT CACHING YES` for the open key. Only this
    /// message can make the header say `live`.
    TrackingArmed,
    /// An invalidation push arrived for the open key. It consumed the arming.
    Invalidated,
    /// A traversal began. `estimated_total` is `DBSIZE` at that moment.
    ScanStarted {
        estimated_total: u64,
    },
    /// A page of keys arrived. The core never sees the cursor that produced it,
    /// so one cursor can become N without the browser noticing (ADR-0008).
    ScanBatch {
        keys: Vec<Vec<u8>>,
    },
    /// The traversal finished on its own.
    ScanComplete,
    /// The traversal stopped early because the shell was asked to stop.
    ScanCancelled,
    ScanFailed {
        error: String,
    },
    /// Lazily-fetched metadata arrived for some rows (R2.4).
    ///
    /// `gone` carries the rows whose key had vanished by the time the fetch
    /// reached it. They are indices rather than `MetadataEntry` values with a
    /// flag: every field of a `MetadataEntry` describes metadata, and a key that
    /// is not there has none, so a flagged entry would have to invent a type.
    MetadataBatch {
        entries: Vec<MetadataEntry>,
        gone: Vec<usize>,
    },
    /// A read of the open key completed, carrying what the server said.
    ///
    /// The only way a value enters the Viewer. There is no other path, which is
    /// what makes a stale value unrepresentable (ADR-0006).
    ValueLoaded {
        /// Which read this answers. A reply from a superseded read is dropped
        /// rather than applied — see [`crate::command::ReadToken`].
        token: crate::command::ReadToken,
        /// The Loaded set row this key was read from, if one was known. `None`
        /// after a rescan took it away — the value is still the value, but
        /// there is no row it may be written back to.
        index: Option<usize>,
        name: String,
        value: crate::state::Value,
        ttl_seconds: i32,
        size_bytes: u32,
        at_ms: u64,
    },
    /// The key that was read is gone: deleted, expired, or evicted.
    ///
    /// Carries a token for the same reason [`Msg::ValueLoaded`] does, and with
    /// more at stake: this message tombstones the Open key *and* its row in the
    /// keys pane, so an unidentified one badges whichever key happens to be
    /// open now. A healthy key marked `✕ deleted` in both panes is worse than
    /// the wrong value, because nothing afterwards corrects it — metadata is
    /// refetched only for rows whose type is unknown, and this row's would be
    /// known and wrong.
    ValueGone {
        token: crate::command::ReadToken,
        at_ms: u64,
    },
    /// Something was copied. Drives a notice that fades on its own.
    Copied {
        label: &'static str,
        at_ms: u64,
    },
    /// Conditions the server reported: replica status, and anything currently
    /// rejecting writes (R1.15). Sent at connect and on every reconnect,
    /// because a failover can change the answer.
    ServerState {
        read_only: Option<crate::state::ReadOnlyReason>,
        condition: Option<crate::state::ServerCondition>,
    },
    /// An operation failed. Carries the command that failed (R7.4).
    ///
    /// Errors are never swallowed: a Redis error that produces no visible
    /// effect is indistinguishable from the app deciding to do nothing, which
    /// is the class of defect this project exists to remove.
    Failed {
        command: String,
        detail: String,
        at_ms: u64,
    },
    /// The user asked to leave.
    Quit,
}
