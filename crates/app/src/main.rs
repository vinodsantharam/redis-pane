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

use clap::Parser;
use redis_pane_core::clock::Clock;
use redis_pane_core::resolve::{EnvVars, Flags, resolve};
use redis_pane_core::state::State;
use redis_pane_core::theme::Theme;

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

/// A terminal UI for Redis.
///
/// With no arguments, resolves a target from the default Profile, then the
/// environment, then `127.0.0.1:6379`. It never prompts; the title bar always
/// shows what was chosen and why (ADR-0001).
#[derive(Debug, Parser)]
#[command(name = "redis-pane", version, about, long_about = None)]
struct Cli {
    /// Profile to use, by name. A bare positional name works too.
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
    /// Profile name, positionally.
    #[arg(value_name = "PROFILE")]
    positional_profile: Option<String>,
    /// Connect to this URL, used wholesale.
    #[arg(long, value_name = "URL")]
    url: Option<String>,
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// Database index. Fixed at launch; there is no in-app switcher (ADR-0005).
    #[arg(long, value_name = "N")]
    db: Option<u8>,
    /// Resolve and print the target, then exit without connecting.
    #[arg(long)]
    print_target: bool,
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

fn env_vars() -> EnvVars {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    EnvVars {
        redis_url: get("REDIS_URL"),
        redis_host: get("REDIS_HOST"),
        redis_port: get("REDIS_PORT"),
        redis_user: get("REDIS_USER"),
        redis_password: get("REDIS_PASSWORD"),
    }
}

fn main() {
    let cli = Cli::parse();

    let config = match config_io::default_path() {
        Some(path) => match config_io::load(&path) {
            Ok(config) => config,
            Err(err) => {
                // A config that cannot be trusted is fatal, and says why.
                eprintln!("redis-pane: {err}");
                std::process::exit(exit::CONFIG);
            }
        },
        None => None,
    };

    let flags = Flags {
        url: cli.url,
        host: cli.host,
        port: cli.port,
        db: cli.db,
        profile: cli.profile.or(cli.positional_profile),
    };
    let connection = resolve(&flags, config.as_ref(), &env_vars());

    if cli.print_target {
        println!(
            "{} · {} · {}",
            connection.target,
            connection.environment.label(),
            connection.source.label()
        );
        std::process::exit(exit::OK);
    }

    let clock = SystemClock;
    let state = State {
        connection,
        ..State::default()
    };
    let theme = Theme::new(terminal::detect_color_depth());

    if let Err(err) = terminal::run(state, theme, &clock) {
        eprintln!("redis-pane: {err}");
        std::process::exit(exit::CONNECTION);
    }
    std::process::exit(exit::OK);
}
