//! `Command` — everything the shells must do (PLAN M0.4).

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
}
