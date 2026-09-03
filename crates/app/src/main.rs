//! `redis-pane` — a terminal UI for Redis.
//!
//! This binary is the imperative shell (ADR-0011). It owns the terminal, the
//! Redis connection, the filesystem and the clock; `redis-pane-core` owns
//! everything else and cannot reach any of them.

mod config_io;
mod redis;
mod state_file;
mod terminal;

use std::time::{SystemTime, UNIX_EPOCH};

use redis_pane_core::clock::Clock;
use redis_pane_core::state::{Connection, Environment, Source, State};
use redis_pane_core::theme::{ColorDepth, Theme};
use redis_pane_core::{Msg, render, update};

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

/// The real clock. It lives here, in the shell, because the core must not be
/// able to read it (ADR-0011).
struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

fn main() {
    let clock = SystemClock;
    let now = clock.now_ms();

    // Placeholder until resolution lands in M0.6 — the shape is real, the
    // values are not yet resolved from flags, Profile, or environment.
    let state = State {
        connection: Connection {
            target: "127.0.0.1:6379/0".into(),
            environment: Environment::Local,
            source: Source::Default,
        },
        ..State::default()
    };
    let (state, _cmds) = update(state, Msg::Resized { cols: 100, rows: 2 });
    let (state, _cmds) = update(
        state,
        Msg::ReadCompleted {
            at_ms: now - 14_000,
        },
    );

    let area = ratatui::layout::Rect::new(0, 0, state.cols, state.rows);
    let buf = render::frame(&state, &Theme::new(ColorDepth::TrueColor), &clock, area);
    println!("{}", render::to_text(&buf));

    std::process::exit(exit::OK);
}
