//! `Msg` — everything that can happen (PLAN M0.4).

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
    Resized { cols: u16, rows: u16 },
    /// A read of the open key completed at this clock reading.
    ReadCompleted { at_ms: u64 },
    /// The shell connected. `tracking_supported` is the result of *attempting*
    /// `CLIENT TRACKING`, never an inference from the version (ADR-0007).
    Connected {
        version: String,
        tracking_supported: bool,
    },
    /// The link dropped mid-session.
    ConnectionLost,
    /// A reconnect attempt is scheduled. Backoff is visible, never a silent wait.
    ReconnectScheduled { attempt: u32, retry_in_ms: u64 },
    /// The server accepted `CLIENT CACHING YES` for the open key. Only this
    /// message can make the header say `live`.
    TrackingArmed,
    /// An invalidation push arrived for the open key. It consumed the arming.
    Invalidated,
    /// The user asked to leave.
    Quit,
}
