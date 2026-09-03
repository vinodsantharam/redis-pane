//! # redis-pane-core
//!
//! The functional core. It takes a [`Msg`] and returns new [`State`] plus a list
//! of [`Command`]s for a shell to execute. It performs no I/O, and it does not
//! read the clock — see [ADR-0011].
//!
//! The module layout mirrors `docs/PLAN.md` §2.
//!
//! [ADR-0011]: https://github.com/vinodsantharam/redis-pane/blob/main/docs/adr/0011-functional-core-golden-frames.md

pub mod clock;
pub mod command;
pub mod config;
pub mod keymap;
pub mod msg;
pub mod render;
pub mod state;
pub mod theme;

pub use command::Command;
pub use msg::Msg;
pub use state::State;

/// The single entry point into the core.
///
/// Everything that can happen arrives as a `Msg`; everything the shells must do
/// leaves as a `Command`. No other path in or out exists, which is what makes a
/// frame a function of state alone.
pub fn update(state: State, msg: Msg) -> (State, Vec<Command>) {
    let _ = msg;
    (state, Vec::new())
}
