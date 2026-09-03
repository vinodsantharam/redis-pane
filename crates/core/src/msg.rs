//! `Msg` — everything that can happen (PLAN M0.4).

/// Every input to the core: keystrokes, resizes, and replies from the shells.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Msg {
    /// The terminal was resized. Drives the layout breakpoints in DESIGN §2.
    Resized { cols: u16, rows: u16 },
    /// The user asked to leave.
    Quit,
}
