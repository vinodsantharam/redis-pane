//! `redis-pane` — the imperative shells around the core (ADR-0011).
//!
//! Everything that touches the outside world lives here: the terminal, the
//! Redis connection, the filesystem and the clock. `redis-pane-core` owns
//! everything else and cannot reach any of them.
//!
//! This is a library as well as a binary so the integration suite can drive the
//! shells directly rather than through a subprocess.

pub mod clipboard;
pub mod config_io;
pub mod redis;
pub mod secret;
pub mod state_file;
pub mod terminal;

use std::time::{SystemTime, UNIX_EPOCH};

use redis_pane_core::clock::Clock;

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
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}
