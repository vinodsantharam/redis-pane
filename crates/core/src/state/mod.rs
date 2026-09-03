//! Application state: the Loaded set, Viewer state, and Connection state.
//!
//! The key list is columnar and capped (ADR-0010): key names live in one byte
//! arena addressed by `(offset, len)`, metadata lives in parallel arrays, and
//! sorting permutes an index vector rather than moving data. None of that
//! exists yet — see `docs/PLAN.md` M1.1.

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
}
