//! Render real frames against a real server, without a TTY.
//!
//! Drives the same code path the TUI does — connect, scan, fetch metadata,
//! open a key — and prints the rendered frames to stdout. Useful for seeing
//! what the app shows against a server you cannot run a terminal against, and
//! for timing the phases over a real network.
//!
//! ```text
//! REDIS_PANE_URL=rediss://default@host:6379 REDIS_PANE_PW_ENV=TOKEN \
//!   cargo run --release -p redis-pane --example frame
//! ```

use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use redis_pane::SystemClock;
use redis_pane_core::clock::Clock;
use redis_pane_core::resolve::{Credentials, PasswordSource};
use redis_pane_core::state::{Connection, Environment, Source, State};
use redis_pane_core::theme::{ColorDepth, Theme};
use redis_pane_core::{Msg, render, update};
use tokio_util::sync::CancellationToken;

const W: u16 = 118;
const H: u16 = 26;

fn show(title: &str, state: &State, clock: &dyn Clock) {
    let buf = render::frame(
        state,
        &Theme::new(ColorDepth::Monochrome),
        clock,
        Rect::new(0, 0, W, H),
    );
    println!("\n\x1b[1m{title}\x1b[0m");
    println!("{}", render::to_text(&buf));
}

#[tokio::main]
async fn main() {
    let url = std::env::var("REDIS_PANE_URL").expect("set REDIS_PANE_URL");
    let credentials = match std::env::var("REDIS_PANE_PW_ENV") {
        Ok(var) => Credentials {
            password: PasswordSource::Env(var),
            ..Credentials::default()
        },
        Err(_) => Credentials::default(),
    };

    let clock = SystemClock;
    let display = redis_pane_core::resolve::redact(&url);

    let t0 = Instant::now();
    let (client, est) = redis_pane::redis::connect_with(&url, &credentials)
        .await
        .expect("connect");
    let connect_ms = t0.elapsed();

    let mut state = State {
        cols: W,
        rows: H,
        connection: Connection {
            target: format!("{display}/0"),
            environment: Environment::Staging,
            source: Source::Profile(
                std::env::var("REDIS_PANE_PROFILE_NAME").unwrap_or_else(|_| "profile".into()),
            ),
        },
        ..State::default()
    };
    (state, _) = update(state, Msg::Resized { cols: W, rows: H });
    (state, _) = update(
        state,
        Msg::Connected {
            version: est.version.to_string(),
            tracking_supported: est.tracking_supported,
        },
    );

    // ── scan, exactly as the app does ──────────────────────────────────────
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let scan_client = client.clone();
    let t1 = Instant::now();
    tokio::spawn(async move {
        redis_pane::redis::scan::stream_keys(&scan_client, None, tx, CancellationToken::new())
            .await;
    });

    let mut first_batch: Option<Duration> = None;
    let mut pending: Vec<usize> = Vec::new();
    while let Some(msg) = rx.recv().await {
        let done = matches!(msg, Msg::ScanComplete | Msg::ScanFailed { .. });
        if matches!(msg, Msg::ScanBatch { .. }) && first_batch.is_none() {
            first_batch = Some(t1.elapsed());
        }
        let cmds;
        (state, cmds) = update(state, msg);
        for c in cmds {
            if let redis_pane_core::Command::FetchMetadata { indices } = c {
                pending = indices;
            }
        }
        if done {
            break;
        }
    }
    let scan_ms = t1.elapsed();
    show("── after the scan ──", &state, &clock);

    // ── metadata for the visible window ────────────────────────────────────
    let window: Vec<(usize, Vec<u8>)> = pending
        .iter()
        .filter_map(|i| state.keys.name(*i).map(|n| (*i, n.to_vec())))
        .collect();
    let t2 = Instant::now();
    if let Ok((entries, gone)) = redis_pane::redis::fetch_metadata(&client, &window).await {
        (state, _) = update(
            state,
            Msg::MetadataBatch {
                entries,
                gone,
                at_ms: clock.now_ms(),
            },
        );
    }
    let meta_ms = t2.elapsed();
    show(
        "── metadata filled in for the visible window ──",
        &state,
        &clock,
    );

    // ── open a key: `REDIS_PANE_OPEN` if it names one, else the first hash ──
    // The override is what makes this useful for checking a *particular* key's
    // frame — a windowed list or a long stream, say, where the header has
    // something to disclose that a 5-byte string does not.
    let wanted = std::env::var("REDIS_PANE_OPEN").ok();
    let target = (0..state.keys.len())
        .find(|i| {
            state.keys.name_str(*i).is_some_and(|n| match &wanted {
                Some(w) => n == w.as_str(),
                None => n.ends_with(":session"),
            })
        })
        .unwrap_or(0);
    let name = state.keys.name(target).unwrap().to_vec();
    let t3 = Instant::now();
    let arming = if est.tracking_supported {
        redis_pane::redis::read::Arming::Enabled
    } else {
        redis_pane::redis::read::Arming::Unsupported
    };
    let read_result = redis_pane::redis::read::read_value(&client, &name, 50, arming).await;
    let read_ms = t3.elapsed();
    // This is exactly what terminal.rs's open_key must also do: Ok(_) after
    // Arming::Enabled means CLIENT CACHING YES already succeeded on the wire.
    if arming == redis_pane::redis::read::Arming::Enabled && read_result.is_ok() {
        (state, _) = update(state, Msg::TrackingArmed);
    }
    if let Ok(Some(read)) = read_result {
        let token = state.read_token;
        (state, _) = update(
            state,
            Msg::ValueLoaded {
                // This example drives `update` by hand rather than through the
                // command loop, so it stamps the token the core is holding.
                token,
                index: Some(target),
                name: String::from_utf8_lossy(&name).into_owned(),
                value: read.value,
                ttl_seconds: read.ttl_seconds,
                size_bytes: read.size_bytes,
                at_ms: clock.now_ms(),
            },
        );
    }
    show("── a key open ──", &state, &clock);
    println!(
        "\nliveness after opening a key: {:?} (tracking_supported={})",
        state.liveness(),
        est.tracking_supported
    );

    // ── a filter, applied to what is already loaded ────────────────────────
    state.list.filter = "user:*:session".into();
    state.rebuild_list();
    show("── filtered to user:*:session ──", &state, &clock);

    println!(
        "\ntimings over the network\n\
         connect + probe   {connect_ms:>8.0?}\n\
         first keys on screen {:>8.0?}\n\
         full scan of {} keys {:>7.0?}\n\
         metadata window   {meta_ms:>8.0?}\n\
         open a key        {read_ms:>8.0?}",
        first_batch.unwrap_or_default(),
        state.keys.len(),
        scan_ms,
    );
}
