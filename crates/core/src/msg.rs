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
    Delete,
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

/// A mouse action, described without reference to any terminal library — the
/// same boundary `KeyCode`/`KeyPress` draw for the keyboard (R7.3).
///
/// Deliberately not "everything a mouse can do": only the left button is
/// modelled, because nothing here is specified for the others, and a
/// right-click doing something the reader did not ask for is worse than a
/// right-click doing nothing. `col`/`row` are cell coordinates in the whole
/// terminal, the same space [`crate::render::layout::layout`] lays panes out
/// in — the core, not the shell, decides what a coordinate means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MouseAction {
    /// The left button went down at this cell.
    Down { col: u16, row: u16 },
    /// The left button was released, wherever it happened to be.
    Up,
    /// The cursor moved to this cell while the left button was held.
    Drag { col: u16, row: u16 },
    /// The wheel scrolled one notch toward the top, over this cell.
    ScrollUp { col: u16, row: u16 },
    /// The wheel scrolled one notch toward the bottom, over this cell.
    ScrollDown { col: u16, row: u16 },
}

/// Why a guarded write did not write anything (PLAN M2 task 6, D1, ADR-0015).
///
/// One enum for every guard a write can trip, whichever mutation it was —
/// `Msg::NotWritten` carries this rather than the mutation itself, so the
/// `update()` handler is one match on *why*, not one per command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotWritten {
    /// The key itself was already gone. Tombstoned, the same as a
    /// `ValueGone` — the key is never recreated (ADR-0014).
    KeyGone,
    /// A `SetHashField` found the field already gone.
    FieldGone,
    /// An `AddHashField` found the field already there.
    FieldExists,
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
    /// The mouse did something (R7.3).
    Mouse(MouseAction),
    /// The terminal was resized. Drives the layout breakpoints in DESIGN §2.
    Resized {
        cols: u16,
        rows: u16,
    },
    /// A read of the open key completed at this clock reading.
    ReadCompleted {
        at_ms: u64,
    },
    /// A read was just dispatched to the shell, at this clock reading.
    ///
    /// Exists only to timestamp `PendingRead` (`crate::state::PendingRead`)
    /// for the loading indicator's delay gate — `update()` has no clock of
    /// its own (ADR-0011), so the shell, which actually dispatches the read,
    /// supplies the one fact the core needs. A token that no longer names the
    /// outstanding read (superseded, or already answered) is ignored.
    ReadIssued {
        token: crate::command::ReadToken,
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
        /// When this batch was read, so the keys pane's TTL column can count
        /// down locally afterward (R3.9's rule, extended from the Viewer to
        /// here) instead of freezing at whatever it read until the next
        /// rescan.
        at_ms: u64,
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
    /// The key that was read is gone: deleted, expired, or evicted — or was
    /// never there to begin with.
    ///
    /// Carries the same identity `ValueLoaded` does, and for the same reason:
    /// the token alone only says *this is the most recent read*, not *which
    /// key it was a read of*. A token-only version of this message shipped
    /// once and had exactly the bug that identity exists to prevent —
    /// opening a gone key while a different one was already open badged the
    /// *previous*, perfectly alive key `✕ deleted` and tombstoned its row,
    /// because the handler had no way to tell the two apart and simply
    /// mutated whatever was open. `index`/`name` are what let the core ask
    /// "is this the key I already have open, or a different one" instead of
    /// assuming.
    ValueGone {
        token: crate::command::ReadToken,
        /// The Loaded set row this key was read from, if one was known —
        /// same meaning as [`Msg::ValueLoaded::index`].
        index: Option<usize>,
        name: String,
        at_ms: u64,
    },
    /// Something was copied. Drives a notice that fades on its own.
    ///
    /// The label is owned rather than `&'static str` because a copy that could
    /// only take part of a value has to say so, and how much it took is not
    /// known until the copy is built.
    Copied {
        label: String,
        at_ms: u64,
    },
    /// The core raised a notice and needs the shell's clock to date it.
    ///
    /// `update` is pure and has no clock (ADR-0011), so a notice it raises by
    /// itself cannot be timestamped where it is written. Two of them were built
    /// with `at_ms: 0` and were therefore invisible for the life of the
    /// process: `notice_now` shows a notice for 2.5 seconds, and the shell's
    /// clock reads epoch milliseconds. The message existed, explained itself,
    /// and could never appear. Round-tripping through the shell is how every
    /// other dated fact reaches the core, and it is how these do now.
    Noticed {
        text: String,
        at_ms: u64,
    },
    /// Conditions the server reported: replica status, and anything currently
    /// rejecting writes (R1.15). Sent at connect and on every reconnect,
    /// because a failover can change the answer.
    ServerState {
        read_only: Option<crate::state::ReadOnlyReason>,
        condition: Option<crate::state::ServerCondition>,
    },
    /// A staged `DeleteKey` completed — the key is gone, whether it still
    /// existed at the moment `DEL` ran or was already gone by then. Not a
    /// read, so it carries no [`crate::command::ReadToken`]; a delete only
    /// ever follows a confirm the reader just pressed, and there is at most
    /// one staged at a time (R4.3, R4.4).
    KeyDeleted {
        /// The Loaded set row this key was staged from, if one was known —
        /// same meaning as [`Msg::ValueGone::index`].
        index: Option<usize>,
        name: String,
        at_ms: u64,
    },
    /// The terminal reported a bracketed paste (ADR-0014).
    ///
    /// With the inline editor open this is one `insert_str`, staged as a
    /// single undo step; while the filter is capturing it is appended
    /// (newlines stripped); otherwise it is ignored.
    Paste(String),
    /// A `SET` from `Command::SetValue` completed (R4.1).
    ///
    /// Carries no value of its own: what follows is the same Refetch every
    /// other change to the open key goes through
    /// (`crate::update::issue_refetch`) — the reply is what actually reaches
    /// the Viewer, never the bytes this message's own sender already knew
    /// (ADR-0006: no value cache, not even a one-message-long one). Guarded
    /// by `name`, the same way `ValueGone`/`KeyDeleted` are: the reader may
    /// have moved on to a different key by the time this lands.
    ValueSet {
        name: String,
        at_ms: u64,
    },
    /// A guarded write refused to write, because its precondition was no
    /// longer true by the time it reached the server (R4.1, PLAN M2 task 6,
    /// D1, ADR-0014, ADR-0015).
    ///
    /// Not a `Failed` — the server did exactly as asked and nothing broke;
    /// the write is simply not there to reread. The edited text goes back
    /// into the buffer either way, since no read can recover it — what
    /// differs by `why` is whether the key itself is gone (tombstoned, same
    /// as a `ValueGone`) or only the precondition failed (the buffer is
    /// handed back with a Refetch in flight, held under R3.8 while it is
    /// open again). Guarded by `name`, like `ValueSet`: the reader may have
    /// moved on to a different key by the time this lands. The command this
    /// was — `SET`, `HSET key field`, or `HSETNX key field` — is not carried
    /// here: the buffer that is about to be handed back already knows, via
    /// `EditBuffer::target`, so the core derives it rather than trusting a
    /// second copy the shell could get out of sync with the first.
    NotWritten {
        name: String,
        why: NotWritten,
        at_ms: u64,
    },
    /// A staged `DeleteHashField` found the field already gone (PLAN M2 task
    /// 6, D1, D4).
    ///
    /// Not an error, and not routed through `NotWritten`: `HDEL` did exactly
    /// what was asked and found nothing to remove, and there is no buffer to
    /// hand anything back to — `Delete` never opens one. Reported as a
    /// notice, then a Refetch, the same way every other change to the open
    /// key is (ADR-0006).
    HashFieldAlreadyGone {
        name: String,
        field: String,
        at_ms: u64,
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
