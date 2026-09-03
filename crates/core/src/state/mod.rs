//! Application state: the Loaded set, Viewer state, and Connection state.
//!
//! The key list is columnar and capped (ADR-0010): key names live in one byte
//! arena addressed by `(offset, len)`, metadata lives in parallel arrays, and
//! sorting permutes an index vector rather than moving data — see
//! [`loaded::LoadedSet`].

pub mod loaded;
pub mod open;
pub mod scan;
pub mod tree;
pub mod value;
pub mod view;

pub use loaded::{KeyKind, LoadedSet};
pub use open::OpenKey;
pub use scan::ScanState;
pub use tree::Tree;
pub use value::{Value, Viewer};
pub use view::{FilterMode, KeyView, SortBy};

/// Where a Connection's target came from (ADR-0001).
///
/// Resolution never prompts, so this readout is the mitigation for resolving
/// silently. It is shown in the title bar at all times and is not optional
/// chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An explicit command-line flag.
    Flag,
    /// A named Profile from the config file.
    Profile(String),
    /// `REDIS_URL`, or the discrete `REDIS_HOST`/`PORT`/`USER`/`PASSWORD` set.
    Environment,
    /// Nothing was specified: `127.0.0.1:6379`.
    Default,
}

impl Source {
    /// The phrase shown in the title bar after the target.
    pub fn label(&self) -> String {
        match self {
            Source::Flag => "from flag".into(),
            Source::Profile(name) => format!("from profile {name}"),
            Source::Environment => "from environment".into(),
            Source::Default => "from default".into(),
        }
    }
}

/// There are four Environments, not three (ADR-0004).
///
/// `Unknown` is a real one: anything that is not loopback or a unix socket and
/// was not tagged gets it, and starts in Read-only Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Local,
    Staging,
    Prod,
    Unknown,
}

impl Environment {
    pub fn label(&self) -> &'static str {
        match self {
            Environment::Local => "local",
            Environment::Staging => "staging",
            Environment::Prod => "prod",
            Environment::Unknown => "unknown",
        }
    }

    /// Whether Read-only Mode is on by default here (R4.5, ADR-0004).
    pub fn read_only_by_default(&self) -> bool {
        matches!(self, Environment::Prod | Environment::Unknown)
    }
}

/// What the title bar says we are connected to, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// `host:port/db`, already formatted for display.
    pub target: String,
    pub environment: Environment,
    pub source: Source,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            target: "127.0.0.1:6379/0".into(),
            environment: Environment::Local,
            source: Source::Default,
        }
    }
}

/// Whether the server is tracking the open key for us.
///
/// This enum is the reason [`Liveness::Live`] cannot be claimed by accident.
/// `Live` is derivable only from [`Tracking::Armed`] or [`Tracking::Consumed`],
/// and the only way into `Armed` is a [`crate::Msg::TrackingArmed`] that the
/// shell sends *after* the server has actually accepted the arming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    /// The server refused `CLIENT TRACKING`. Managed platforms do this
    /// independently of the version they report, so this is production
    /// infrastructure, not a legacy path (ADR-0007).
    Unsupported,
    /// The server supports tracking, but nothing is armed right now — we have
    /// just connected, or just reconnected. **Not live.**
    Available,
    /// Armed for the open key. The server will push an invalidation.
    Armed,
    /// An invalidation arrived and consumed the arming (verified against Redis
    /// 8.4.0 — five writes produce one push, and nothing after it). A Refetch
    /// is in flight to re-arm. This state must be transient; if it persists,
    /// the Viewer has gone dark.
    Consumed,
}

/// The state of the link to the server (ADR-0009).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Link {
    /// Before the first successful connect.
    #[default]
    Connecting,
    Up {
        version: String,
        tracking: Tracking,
    },
    /// Dropped mid-session. The UI stays interactive and the Viewer keeps its
    /// last read value; it never exits.
    Reconnecting {
        attempt: u32,
        retry_in_ms: u64,
    },
}

/// What the Viewer header says about how current it is (ADR-0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// `● live` — the server will tell us when this key changes.
    Live,
    /// `○ manual` — Read age plus an explicit Refetch.
    Manual,
    /// `✕ disconnected` — the last read value is still on screen, badged.
    Disconnected,
}

impl Liveness {
    pub fn readout(&self) -> &'static str {
        match self {
            Liveness::Live => "● live",
            Liveness::Manual => "○ manual",
            Liveness::Disconnected => "✕ disconnected",
        }
    }
}

/// Why Read-only Mode is on (ADR-0009, R4.5).
///
/// The reason is displayed, because only some of them can be lifted. A guard
/// whose origin is invisible is a guard the user will misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// The Environment is `prod` or `unknown`.
    Environment,
    /// The server reported `role:slave`. **Not liftable** — the server will
    /// refuse writes whatever the app believes, so offering `⌃R` here would be
    /// a toggle that lies.
    Replica,
    /// The user asked for it.
    User,
}

impl ReadOnlyReason {
    pub fn label(&self) -> &'static str {
        match self {
            ReadOnlyReason::Environment => "environment",
            ReadOnlyReason::Replica => "replica",
            ReadOnlyReason::User => "user",
        }
    }

    /// Whether `⌃R` can turn this off.
    pub fn liftable(&self) -> bool {
        !matches!(self, ReadOnlyReason::Replica)
    }
}

/// A server state that rejects writes or defers them (ADR-0009).
///
/// Detected rather than merely reported, so danger is visible *before* it is
/// possible (DESIGN principle 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerCondition {
    /// `maxmemory` reached; writes are refused.
    Oom,
    /// RDB saves are failing; writes are refused.
    Misconf,
    /// The dataset is loading after a restart.
    Loading { percent: u8 },
}

impl ServerCondition {
    pub fn readout(&self) -> String {
        match self {
            ServerCondition::Oom => "✕ OOM · writes rejected".into(),
            ServerCondition::Misconf => "✕ MISCONF · writes rejected".into(),
            ServerCondition::Loading { percent } => format!("⟳ loading {percent}%"),
        }
    }
}

/// The whole of what the application knows.
///
/// Note what is absent: any cached value for the open key. Reads always hit the
/// server (ADR-0006), so there is no field here for one to live in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// Terminal size, from the shell. Layout is a function of this (DESIGN §2).
    pub cols: u16,
    pub rows: u16,
    pub connection: Connection,
    /// Clock reading at the last completed read, if any. Rendering an age from
    /// this is what makes the frame depend on the injected clock (ADR-0011).
    pub last_read_ms: Option<u64>,
    /// Set once the core has been told to shut down.
    pub quitting: bool,
    pub link: Link,
    /// Read-only Mode and why, if it is on (R4.5, ADR-0009).
    pub read_only: Option<ReadOnlyReason>,
    /// A server condition worth a banner, if there is one.
    pub condition: Option<ServerCondition>,
    /// Bindings in force. Hints read from here so they show the effective key.
    pub keymap: crate::keymap::Keymap,
    pub help_open: bool,
    /// Every key scanned so far, columnar and capped (ADR-0010).
    pub keys: LoadedSet,
    pub scan: ScanState,
    /// Which rows are on screen and which is selected. Scrolling changes this,
    /// never the Loaded set (R2.6).
    pub view: crate::render::keys::Viewport,
    /// The filtered, ordered index vector the list renders through.
    pub list: KeyView,
    /// Folded rows, when tree mode is on.
    pub tree: Tree,
    pub tree_mode: bool,
    /// Set while `/` is capturing a filter.
    pub filtering: bool,
    /// The key in the Viewer, if one is open. There is no cache behind this —
    /// it holds what the server last said and nothing more (ADR-0006).
    pub open: Option<OpenKey>,
}

impl State {
    /// What the header may claim about currency.
    ///
    /// There is deliberately no setter for this. Liveness is *derived* from the
    /// link and the tracking state, so no code path can set the header to
    /// `live` without the server having actually armed — which is the failure
    /// ADR-0009 exists to prevent, and the one most likely to rot silently.
    /// Whether Read-only Mode can be lifted right now. A replica cannot, so the
    /// hint must read `locked` rather than offering a key that will not work.
    pub fn read_only_liftable(&self) -> bool {
        self.read_only.is_some_and(|r| r.liftable())
    }

    /// How many key rows are visible, given the current terminal height.
    ///
    /// Title row, blank, column header, status bar and hint bar are chrome.
    pub fn visible_rows(&self) -> usize {
        (self.rows as usize).saturating_sub(6).max(1)
    }

    /// Visible rows whose metadata has not arrived yet (R2.4).
    ///
    /// Only ever the window on screen. Fetching metadata for the whole Loaded
    /// set would be `KEYS *` with extra steps, and it would compete with `SCAN`
    /// for the connection while the list is still filling.
    pub fn rows_needing_metadata(&self) -> Vec<usize> {
        let height = self.visible_rows();
        let start = self.view.scrolled_to_selection(height).offset;
        (start..(start + height).min(self.row_count()))
            .filter_map(|row| self.key_at(row))
            .filter(|i| self.keys.kind(*i).is_none())
            .collect()
    }

    /// How many rows the list currently shows — after filtering, and after
    /// folding if tree mode is on.
    pub fn row_count(&self) -> usize {
        if self.tree_mode {
            self.tree.len()
        } else {
            self.list.len()
        }
    }

    /// The Loaded set index shown at a display row, if that row is a key.
    ///
    /// In tree mode a row may be a group header, which has no key behind it.
    pub fn key_at(&self, row: usize) -> Option<usize> {
        if self.tree_mode {
            self.tree.key_index(row)
        } else {
            self.list.index_at(row)
        }
    }

    /// The key the selection is on, if any.
    pub fn selected_key(&self) -> Option<usize> {
        self.key_at(self.view.selected)
    }

    /// Recompute the list after the keys, the filter, the sort or the mode
    /// changed. Tree mode needs name order to fold in one pass.
    pub fn rebuild_list(&mut self) {
        if self.tree_mode && self.list.sort == SortBy::Scan {
            self.list.sort = SortBy::Name;
        }
        self.list.rebuild(&self.keys);
        if self.tree_mode {
            self.tree.rebuild(&self.keys, &self.list);
        }
        let last = self.row_count().saturating_sub(1);
        if self.view.selected > last {
            self.view.selected = last;
        }
    }

    pub fn liveness(&self) -> Liveness {
        match &self.link {
            Link::Connecting | Link::Reconnecting { .. } => Liveness::Disconnected,
            Link::Up { tracking, .. } => match tracking {
                Tracking::Armed | Tracking::Consumed => Liveness::Live,
                // Connected, and the server supports tracking — but nothing is
                // armed yet. Not live, and this is exactly the state a
                // reconnect lands in.
                Tracking::Available => Liveness::Manual,
                Tracking::Unsupported => Liveness::Manual,
            },
        }
    }
}
