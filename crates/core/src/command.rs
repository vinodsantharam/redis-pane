//! `Command` — everything the shells must do (PLAN M0.4).

/// Work the core cannot perform itself. A shell executes these and reports back
/// as a [`crate::Msg`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    /// Tear down the terminal and exit.
    Quit,
}
