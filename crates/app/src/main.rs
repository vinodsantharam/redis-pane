//! `redis-pane` — a terminal UI for Redis.
//!
//! This binary is the imperative shell (ADR-0011). It owns the terminal, the
//! Redis connection, the filesystem and the clock; `redis-pane-core` owns
//! everything else and cannot reach any of them.

mod config_io;
mod redis;
mod state_file;
mod terminal;

/// Exit codes. A target that cannot be reached exits non-zero with a diagnostic
/// naming the target, its Source, and the failure (R1.14, ADR-0009).
pub mod exit {
    /// Everything worked.
    pub const OK: i32 = 0;
    /// The resolved target could not be reached, or refused us.
    pub const CONNECTION: i32 = 2;
    /// The config file is malformed, or refused for being too readable.
    pub const CONFIG: i32 = 3;
    /// The server is below the floor: RESP3 and Redis 6.0 (R1.13, ADR-0007).
    pub const UNSUPPORTED_SERVER: i32 = 4;
}

fn main() {
    let state = redis_pane_core::State::default();
    let (_state, _cmds) = redis_pane_core::update(state, redis_pane_core::Msg::Quit);
    println!(
        "redis-pane {} — scaffold only, no UI yet",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(exit::OK);
}
