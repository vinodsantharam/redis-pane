//! `Msg` — everything that can happen (PLAN M0.4).

/// Every input to the core: keystrokes, resizes, and replies from the shells.
///
/// The core has no other way in. A shell that wants to tell the core something
/// adds a variant here rather than reaching into state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Msg {
    /// The terminal was resized. Drives the layout breakpoints in DESIGN §2.
    Resized { cols: u16, rows: u16 },
    /// A read of the open key completed at this clock reading.
    ReadCompleted { at_ms: u64 },
    /// The user asked to leave.
    Quit,
}
