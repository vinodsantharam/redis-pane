//! `redis-pane` — a terminal UI for Redis.
//!
//! The binary is a thin wrapper: it parses arguments, resolves a target, and
//! hands off to the shells in the library.

use clap::Parser;
use fred::interfaces::ClientLike;
use fred::prelude::Client;
use redis_pane_core::resolve::{Credentials, EnvVars, Flags, resolve};
use redis_pane_core::state::{Connection, Startup, State};
use redis_pane_core::theme::Theme;

use redis_pane::redis::{ConnectError, Established};
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

/// Target, Environment and Source on one line — what `--print-target` and
/// `--probe` both lead with, and what the title bar shows (ADR-0001).
fn readout(connection: &Connection) -> String {
    format!(
        "{} · {} · {}",
        connection.target,
        connection.environment.label(),
        connection.source.label()
    )
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

/// Connect, or exit with the diagnostic and the exit code for why not.
///
/// The one place a connect failure becomes an exit code, for `--probe` and a
/// session alike: the two used to repeat this, and exit codes are an interface
/// scripts depend on (review M8).
fn connect_or_exit(
    connection: &Connection,
    credentials: &Credentials,
    dial: &str,
) -> (tokio::runtime::Runtime, Client, Established) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("redis-pane: {e}");
        std::process::exit(exit::CONNECTION);
    });
    match runtime.block_on(redis::connect_with(dial, credentials)) {
        Ok((client, established)) => (runtime, client, established),
        Err(err) => {
            eprintln!("{}", startup_failure(connection, &err));
            std::process::exit(match err {
                ConnectError::BelowFloor { .. } | ConnectError::NoResp3 { .. } => {
                    exit::UNSUPPORTED_SERVER
                }
                _ => exit::CONNECTION,
            });
        }
    }
}

/// Report what the server supports. M0.8's proof, runnable by hand as well as
/// by the suite.
fn probe(
    runtime: &tokio::runtime::Runtime,
    connection: &Connection,
    client: &Client,
    established: &Established,
) -> i32 {
    println!("{}", readout(connection));
    println!("redis {}", established.version);
    if let Some(reason) = established.read_only {
        println!("read-only: {} (not liftable)", reason.label());
    }
    if let Some(condition) = established.condition {
        println!("condition: {}", condition.readout());
    }
    if let Some(e) = &established.server_state_error {
        println!("replica/condition checks failed: {e}");
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
        println!("{}", readout(&connection));
        std::process::exit(exit::OK);
    }

    // Connect before taking over the terminal: a failure here is a diagnostic
    // in the shell, not an error box in a TUI (ADR-0009).
    let (runtime, client, established) =
        connect_or_exit(&connection, &resolution.credentials, dial);

    if cli.probe {
        std::process::exit(probe(&runtime, &connection, &client, &established));
    }

    // Which Read-only reason a session starts with is the core's rule
    // (`State::new`): a replica outranks the Environment's default.
    let state = State::new(Startup {
        connection,
        server_read_only: established.read_only,
        condition: established.condition,
    });
    let theme = Theme::new(terminal::detect_color_depth());

    if let Err(err) = runtime.block_on(terminal::run(
        state,
        theme,
        std::sync::Arc::new(SystemClock),
        client,
        established,
        dial.to_string(),
        resolution.credentials.clone(),
    )) {
        eprintln!("redis-pane: {err}");
        std::process::exit(exit::CONNECTION);
    }
    std::process::exit(exit::OK);
}
