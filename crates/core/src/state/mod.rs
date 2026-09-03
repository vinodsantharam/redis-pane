//! Application state: the Loaded set, Viewer state, and Connection state.
//!
//! The key list is columnar and capped (ADR-0010): key names live in one byte
//! arena addressed by `(offset, len)`, metadata lives in parallel arrays, and
//! sorting permutes an index vector rather than moving data.

/// The whole of what the application knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Terminal size, from the shell. Layout is a function of this (DESIGN §2).
    pub cols: u16,
    pub rows: u16,
    /// Set once the core has been told to shut down.
    pub quitting: bool,
}
