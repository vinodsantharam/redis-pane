//! `redis-pane` — a terminal UI for Redis.
//!
//! The binary is a thin wrapper: it parses arguments, resolves a target, and
//! hands off to the shells in the library.

use clap::Parser;
use fred::interfaces::ClientLike;
use redis_pane_core::resolve::{Credentials, EnvVars, Flags, resolve};
use redis_pane_core::state::{Connection, ReadOnlyReason, State};
use redis_pane_core::theme::Theme;

use redis_pane::redis::ConnectError;
use redis_pane::{SystemClock, config_io, exit, redis, terminal};

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
    /// ACL username. Always wins over a Profile's or the environment's.
    #[arg(long, value_name = "NAME")]
    user: Option<String>,
    /// Password, given directly. Visible in shell history and to other users
    /// via `ps` — prefer a Profile's `passwordEnv`/`passwordCommand` for
    /// anything long-lived. Always wins over a Profile's or the environment's.
    #[arg(long, value_name = "SECRET")]
    password: Option<String>,
    /// Use TLS. Additive only — there is no --no-tls to downgrade a Profile or
    /// a `rediss://` URL that already wants it.
    #[arg(long)]
    tls: bool,
    /// Resolve and print the target, then exit without connecting.
    #[arg(long)]
    print_target: bool,
    /// Connect, report what the server supports, then exit.
    #[arg(long)]
    probe: bool,
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

/// The diagnostic printed when a target cannot be reached (R1.14, ADR-0009).
///
/// It names the target *and* its Source, because "connection refused" without
/// saying which server was tried, and why that server was chosen, sends the
/// reader off to guess at their own environment. This is the only UI some
/// users will ever see, so it is worth writing well.
fn startup_failure(connection: &Connection, err: &ConnectError) -> String {
    format!(
        "redis-pane: cannot connect to {} ({}, {})\n  {err}",
        connection.target,
        connection.environment.label(),
        connection.source.label(),
    )
}

/// Connect, report what the server supports, and exit. M0.8's proof, runnable
/// by hand as well as by the suite.
fn probe(connection: &Connection, credentials: &Credentials, dial: &str) -> i32 {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("redis-pane: {e}");
            return exit::CONNECTION;
        }
    };

    match runtime.block_on(redis::connect_with(dial, credentials)) {
        Ok((client, established)) => {
            println!(
                "{} · {} · {}",
                connection.target,
                connection.environment.label(),
                connection.source.label()
            );
            println!("redis {}", established.version);
            if let Some(reason) = established.read_only {
                println!("read-only: {} (not liftable)", reason.label());
            }
            if let Some(condition) = established.condition {
                println!("condition: {}", condition.readout());
            }
            println!(
                "liveness: {}",
                if established.tracking_supported {
                    "CLIENT TRACKING accepted — push-driven"
                } else {
                    "CLIENT TRACKING refused — degrading to manual"
                }
            );
            let _ = runtime.block_on(client.quit());
            exit::OK
        }
        Err(err @ (ConnectError::BelowFloor { .. } | ConnectError::NoResp3 { .. })) => {
            eprintln!("{}", startup_failure(connection, &err));
            exit::UNSUPPORTED_SERVER
        }
        Err(err) => {
            eprintln!("{}", startup_failure(connection, &err));
            exit::CONNECTION
        }
    }
}

fn main() {
    let cli = Cli::parse();

    if cli.password.is_some() {
        eprintln!(
            "redis-pane: warning: --password is visible in shell history and to other users \
             via `ps`; prefer passwordEnv/passwordCommand in a Profile for anything long-lived."
        );
    }

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
        user: cli.user,
        password: cli.password,
        tls: cli.tls,
    };
    let resolution = resolve(&flags, config.as_ref(), &env_vars());
    let connection = resolution.connection.clone();
    // The displayed target is redacted; connecting needs the original, so the
    // two are deliberately kept apart rather than reconstructed from the label.
    let dial = resolution.dial_url();

    if cli.print_target {
        println!(
            "{} · {} · {}",
            connection.target,
            connection.environment.label(),
            connection.source.label()
        );
        std::process::exit(exit::OK);
    }

    if cli.probe {
        std::process::exit(probe(&connection, &resolution.credentials, dial));
    }

    // Connect before taking over the terminal: a failure here is a diagnostic
    // in the shell, not an error box in a TUI (ADR-0009).
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("redis-pane: {e}");
            std::process::exit(exit::CONNECTION);
        }
    };
    let (client, established) =
        match runtime.block_on(redis::connect_with(dial, &resolution.credentials)) {
            Ok(pair) => pair,
            Err(err @ (ConnectError::BelowFloor { .. } | ConnectError::NoResp3 { .. })) => {
                eprintln!("{}", startup_failure(&connection, &err));
                std::process::exit(exit::UNSUPPORTED_SERVER);
            }
            Err(err) => {
                eprintln!("{}", startup_failure(&connection, &err));
                std::process::exit(exit::CONNECTION);
            }
        };

    let clock = SystemClock;
    // `prod` and `unknown` start guarded (R4.5, ADR-0004). A replica outranks
    // the Environment: that reason cannot be lifted, so claiming the weaker one
    // would offer a toggle the server will refuse (R1.15, ADR-0009).
    let read_only = established.read_only.or_else(|| {
        connection
            .environment
            .read_only_by_default()
            .then_some(ReadOnlyReason::Environment)
    });
    let state = State {
        connection,
        read_only,
        condition: established.condition,
        // Tree is the default view (DESIGN §9, resolved): it shows fewer rows
        // at rest, and `t` is one keypress from flat for anyone who wants it.
        // Set here rather than on State::default() — a great many tests use
        // that as a blank-slate baseline and rely on tree_mode being false
        // unless a test opts in explicitly.
        tree_mode: true,
        ..State::default()
    };
    let theme = Theme::new(terminal::detect_color_depth());

    let tracking = established.tracking_supported;
    if let Err(err) = runtime.block_on(terminal::run(
        state,
        theme,
        &clock,
        client,
        tracking,
        dial.to_string(),
        resolution.credentials.clone(),
    )) {
        eprintln!("redis-pane: {err}");
        std::process::exit(exit::CONNECTION);
    }
    std::process::exit(exit::OK);
}
