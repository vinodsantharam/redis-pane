//! The terminal shell: raw mode, the event loop, and resize handling (PLAN M0.5).
//!
//! The render loop never does I/O. Crossterm events are translated into
//! [`Msg`]s, the core decides what they mean, and the resulting state is drawn.
//! A keystroke is answered within one frame regardless of what the network is
//! doing, because nothing here waits on the network.

use std::io::{Stdout, stdout};

use crossterm::event::{
    self, Event, KeyCode as XKeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use fred::interfaces::EventInterface;
use fred::prelude::Client;
use redis_pane_core::clock::Clock;
use redis_pane_core::command::ReadToken;
use redis_pane_core::msg::{KeyCode, KeyPress, MouseAction};
use redis_pane_core::resolve::Credentials;
use redis_pane_core::theme::{ColorDepth, Theme};
use redis_pane_core::{Command, Msg, State, render, update};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::redis::Established;

/// Restores the terminal on drop, including when the process is unwinding.
/// Leaving a terminal in raw mode after a panic is the rudest thing a TUI can
/// do to someone at 3am.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            stdout(),
            event::DisableMouseCapture,
            terminal::LeaveAlternateScreen
        );
    }
}

/// Translate a crossterm key into the core's terminal-free representation.
///
/// This function is the boundary: everything above it speaks crossterm,
/// everything below it does not.
pub fn translate(key: KeyEvent) -> Option<Msg> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let code = match key.code {
        XKeyCode::Char(c) => KeyCode::Char(c),
        XKeyCode::Enter => KeyCode::Enter,
        XKeyCode::Esc => KeyCode::Esc,
        XKeyCode::Tab => KeyCode::Tab,
        XKeyCode::Backspace => KeyCode::Backspace,
        XKeyCode::Up => KeyCode::Up,
        XKeyCode::Down => KeyCode::Down,
        XKeyCode::Left => KeyCode::Left,
        XKeyCode::Right => KeyCode::Right,
        XKeyCode::Home => KeyCode::Home,
        XKeyCode::End => KeyCode::End,
        XKeyCode::PageUp => KeyCode::PageUp,
        XKeyCode::PageDown => KeyCode::PageDown,
        _ => return None,
    };
    Some(Msg::Key(KeyPress {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
    }))
}

/// Translate a crossterm mouse event the same way [`translate`] does for the
/// keyboard (R7.3). Only the left button is modelled — see
/// [`redis_pane_core::msg::MouseAction`] for why.
pub fn translate_mouse(mouse: MouseEvent) -> Option<Msg> {
    let (col, row) = (mouse.column, mouse.row);
    let action = match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => MouseAction::Down { col, row },
        MouseEventKind::Up(MouseButton::Left) => MouseAction::Up,
        MouseEventKind::Drag(MouseButton::Left) => MouseAction::Drag { col, row },
        MouseEventKind::ScrollUp => MouseAction::ScrollUp { col, row },
        MouseEventKind::ScrollDown => MouseAction::ScrollDown { col, row },
        _ => return None,
    };
    Some(Msg::Mouse(action))
}

/// Run the event loop until the core says to quit.
///
/// The render loop never does I/O. Redis work happens on tokio tasks that send
/// [`Msg`]s into this loop; the loop reads state and draws. A keystroke is
/// therefore answerable in one frame regardless of what the network is doing.
pub async fn run(
    mut state: State,
    theme: Theme,
    clock: &dyn Clock,
    mut client: Client,
    mut tracking: bool,
    dial: String,
    credentials: Credentials,
) -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(
        stdout(),
        terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    )?;
    let _guard = Guard;

    let mut term: Terminal<CrosstermBackend<Stdout>> =
        Terminal::new(CrosstermBackend::new(stdout()))?;

    let (tx, mut rx) = mpsc::channel::<Msg>(256);

    // Keyboard reads block, so they live on their own thread and arrive as
    // messages like everything else.
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(Event::Key(k)) => {
                    if let Some(msg) = translate(k)
                        && input_tx.blocking_send(msg).is_err()
                    {
                        return;
                    }
                }
                Ok(Event::Mouse(m)) => {
                    if let Some(msg) = translate_mouse(m)
                        && input_tx.blocking_send(msg).is_err()
                    {
                        return;
                    }
                }
                Ok(Event::Resize(cols, rows)) => {
                    if input_tx.blocking_send(Msg::Resized { cols, rows }).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let size = term.size()?;
    (state, _) = update(
        state,
        Msg::Resized {
            cols: size.width,
            rows: size.height,
        },
    );
    (state, _) = update(
        state,
        Msg::Connected {
            version: String::new(),
            tracking_supported: tracking,
        },
    );

    // Driven by the capability probe, never by preference (ADR-0007).
    // Mutable: a reconnect re-probes tracking (a fresh connection may find a
    // different server, or the same server in a different mood), and every
    // read after that must arm — or not — accordingly.
    let mut arming = if tracking {
        crate::redis::read::Arming::Enabled
    } else {
        crate::redis::read::Arming::Unsupported
    };

    let mut scan_cancel: Option<CancellationToken> = None;
    // Reads take turns, and superseded ones never reach the wire. See
    // `redis::read::ReadGate` for why that is a liveness invariant and not a
    // tidiness preference.
    let mut read_gate = crate::redis::read::ReadGate::default();
    start_scan(&client, None, &tx, &mut scan_cancel);

    // Superseded exactly like `scan_cancel`/`read_gate`: a manual retry
    // (`r` while disconnected, ADR-0009) must cancel whatever backoff sleep
    // or in-flight attempt was already running rather than race it.
    let mut reconnect_cancel: Option<CancellationToken> = None;
    // How many attempts this outage has cost, purely for the backoff curve
    // and the header's countdown (`Msg::ReconnectScheduled`) — reset to 0 the
    // moment a reconnect actually lands. `Command::Reconnect` carries no
    // attempt number of its own; this is the one place that counts.
    let mut reconnect_attempt: u32 = 0;
    // A successful reconnect swaps in a brand new `Client` (`connect_with`
    // redoes the whole startup ritual — version floor, tracking probe, server
    // conditions — exactly as it should: a server that vanished and came back
    // is not guaranteed to still be the same server). `Client` cannot travel
    // through `tx`/`Msg`: the core stays free of `fred` by construction
    // (ADR-0011's boundary), so a fresh client can only reach `run`'s own
    // locals through a side channel the main loop polls directly, never
    // through `update()`.
    let (reconnected_tx, mut reconnected_rx) = mpsc::channel::<(Client, Established)>(1);

    // fred reconnects underneath us, and the server on the other side remembers
    // nothing about what we were watching. Telling the core lets it drop the
    // liveness claim and re-arm — the invariant ADR-0009 exists for, which
    // until now was enforced only where it was tested (PLAN M0.10). Re-run
    // after every successful reconnect (below), against the new client, since
    // subscriptions are tied to the `Client` instance they were opened on —
    // one that has been replaced no longer delivers anything.
    spawn_link_watchers(&client, &tx, tracking);

    loop {
        term.draw(|f| {
            let buf = render::frame(&state, &theme, clock, f.area());
            *f.buffer_mut() = buf;
        })?;

        // A tick with no message of its own: `update` is never called on it,
        // only the redraw above runs again with a fresher clock reading. This
        // is the whole of what makes a TTL countdown actually move on an
        // otherwise idle screen — the number was already computed correctly
        // per frame (R3.9), it just had nothing asking for a new frame once a
        // second. Ratatui diffs the buffer before writing to the terminal, so
        // a tick where only a few digits changed writes only those cells.
        let msg = tokio::select! {
            biased;
            msg = rx.recv() => msg,
            // A reconnect that landed. Swapped in here, in the same task as
            // everything else in this loop, rather than inside the spawned
            // task that found it — so `client`/`arming` are current *before*
            // the `Msg::Connected` this constructs is ever handed to
            // `update()`, and so `Command::RefetchOpenKey` (which
            // `Msg::Connected` already asks for when tracking is supported)
            // reads the new connection rather than the dead one. Ordering
            // here is not incidental: the same guarantee through `tx`/`rx`
            // would need `Msg::Connected` to arrive only after the client was
            // already swapped, and two independent channels give no such
            // promise about which is drained first.
            Some((new_client, established)) = reconnected_rx.recv() => {
                client = new_client;
                tracking = established.tracking_supported;
                arming = if tracking {
                    crate::redis::read::Arming::Enabled
                } else {
                    crate::redis::read::Arming::Unsupported
                };
                reconnect_attempt = 0;
                spawn_link_watchers(&client, &tx, tracking);
                Some(Msg::Connected {
                    version: established.version.to_string(),
                    tracking_supported: tracking,
                })
            },
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => continue,
        };
        let Some(msg) = msg else {
            return Ok(());
        };
        let commands;
        (state, commands) = update(state, msg);

        for command in commands {
            match command {
                Command::Quit => return Ok(()),
                Command::StartScan { pattern } => {
                    start_scan(&client, pattern, &tx, &mut scan_cancel)
                }
                Command::CancelScan => {
                    if let Some(token) = scan_cancel.take() {
                        token.cancel();
                    }
                }
                Command::FetchMetadata { indices } => {
                    // Resolve names here, on the UI side, so the task owns no
                    // reference into state.
                    let window: Vec<(usize, Vec<u8>)> = indices
                        .iter()
                        .filter_map(|i| state.keys.name(*i).map(|n| (*i, n.to_vec())))
                        .collect();
                    if window.is_empty() {
                        continue;
                    }
                    let client = client.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let at_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        match crate::redis::fetch_metadata(&client, &window).await {
                            Ok((entries, gone)) if !entries.is_empty() || !gone.is_empty() => {
                                let _ = tx
                                    .send(Msg::MetadataBatch {
                                        entries,
                                        gone,
                                        at_ms,
                                    })
                                    .await;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                let _ = tx
                                    .send(Msg::Failed {
                                        command: "fetching metadata".into(),
                                        detail: e.details().to_string(),
                                        at_ms: 0,
                                    })
                                    .await;
                            }
                        }
                    });
                }
                Command::OpenKey { index, name, token } => open_key(
                    &client,
                    Some(index),
                    name,
                    token,
                    &tx,
                    area_width(&term),
                    arming,
                    &mut read_gate,
                ),
                Command::RefetchOpenKey { token } => {
                    if let Some(open) = &state.open {
                        open_key(
                            &client,
                            open.index,
                            open.name.as_bytes().to_vec(),
                            token,
                            &tx,
                            area_width(&term),
                            arming,
                            &mut read_gate,
                        );
                    }
                }
                Command::CopyToClipboard { text, label } => {
                    let method = crate::clipboard::detect_method();
                    // Truncation is an OSC-52-only concern (native has no
                    // comparable cap), and only OSC 52 writes to our own
                    // stdout, so only it needs a redraw after.
                    let truncated = method == crate::clipboard::Method::Osc52
                        && crate::clipboard::was_truncated(&text);
                    let result = crate::clipboard::copy(&text, method);
                    if method == crate::clipboard::Method::Osc52 {
                        let _ = term.clear();
                    }
                    let at_ms = clock.now_ms();
                    match result {
                        Ok(()) => {
                            let label = if truncated {
                                format!("{label}, truncated")
                            } else {
                                label
                            };
                            let _ = tx.send(Msg::Copied { label, at_ms }).await;
                        }
                        // A copy that failed used to produce nothing at all —
                        // no notice, no error — which is indistinguishable from
                        // one that worked, and leaves the reader pasting
                        // whatever was on the clipboard before (R7.4). OSC 52
                        // can still be dropped by the terminal without telling
                        // anyone; that is a limit of the protocol. This is the
                        // half we can see.
                        Err(e) => {
                            let _ = tx
                                .send(Msg::Failed {
                                    command: "copying to the clipboard".into(),
                                    detail: e.to_string(),
                                    at_ms,
                                })
                                .await;
                        }
                    }
                }
                Command::Notify { text } => {
                    let at_ms = clock.now_ms();
                    let _ = tx.send(Msg::Noticed { text, at_ms }).await;
                }
                Command::Reconnect { after_ms } => {
                    reconnect_attempt += 1;
                    spawn_reconnect_attempt(
                        dial.clone(),
                        credentials.clone(),
                        after_ms,
                        reconnect_attempt,
                        tx.clone(),
                        reconnected_tx.clone(),
                        &mut reconnect_cancel,
                    );
                }
            }
        }
    }
}

/// Read a key and send the result in. The one read path: it always re-arms,
/// so there is no branch on which liveness can be silently lost (ADR-0006).
///
/// Both `Command::OpenKey` and `Command::RefetchOpenKey` — including the one
/// an invalidation push triggers — call this, which is what unifies both
/// re-arm invariants (ADR-0006, ADR-0009) into one place instead of two.
///
/// **Found by testing against real managed servers, not by any test in the
/// suite:** `CLIENT CACHING YES` was sent and awaited inside `read_value`, but
/// nothing ever told the core it had succeeded. `State::liveness()` correctly
/// refuses to report `Live` without a `Msg::TrackingArmed` — that guard is the
/// whole point of ADR-0009 — but nothing on this path ever sent one. The header
/// read `○ manual` forever, on every server, including local Redis with
/// tracking fully working. The core's invariant was airtight; the shell simply
/// never told it the truth.
#[allow(clippy::too_many_arguments)]
fn open_key(
    client: &Client,
    index: Option<usize>,
    name: Vec<u8>,
    token: ReadToken,
    tx: &mpsc::Sender<Msg>,
    pane_width: usize,
    arming: crate::redis::read::Arming,
    gate: &mut crate::redis::read::ReadGate,
) {
    // Supersede whatever was in flight. The core would ignore its reply anyway
    // — every reply carries the token of the read it answers — but ignoring a
    // reply does not un-send the `CLIENT CACHING YES` that came with it, and
    // that is the half which decides what the server tracks.
    let permit = gate.begin();
    let client = client.clone();
    let tx = tx.clone();

    // Stamps the loading indicator's delay gate (`PendingRead::APPEAR_DELAY_MS`,
    // `crates/core/src/state/open.rs`). `update()` has no clock of its own
    // (ADR-0011); this is the one moment the shell actually knows when the
    // read was dispatched. Best-effort: a dropped send (the channel full)
    // just leaves the read unstamped, which the render layer already treats
    // as "not yet worth mentioning" — never a crash, never a wrong timestamp.
    let issued_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let _ = tx.try_send(Msg::ReadIssued {
        token,
        at_ms: issued_at_ms,
    });

    tokio::spawn(async move {
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let Some(result) = permit
            .run(crate::redis::read::read_value(
                &client, &name, pane_width, arming,
            ))
            .await
        else {
            return;
        };

        // `read_value` awaits `CLIENT CACHING YES` with `?` before doing
        // anything else, so any Ok(_) here means arming already succeeded on
        // the wire — this message is what makes that fact reach the core.
        if arming == crate::redis::read::Arming::Enabled && result.is_ok() {
            let _ = tx.send(Msg::TrackingArmed).await;
        }

        let msg = match result {
            Ok(Some(read)) => Msg::ValueLoaded {
                token,
                index,
                name: String::from_utf8_lossy(&name).into_owned(),
                value: read.value,
                ttl_seconds: read.ttl_seconds,
                size_bytes: read.size_bytes,
                at_ms,
            },
            Ok(None) => Msg::ValueGone {
                token,
                index,
                name: String::from_utf8_lossy(&name).into_owned(),
                at_ms,
            },
            // Never swallowed: a Redis error that produces no visible effect is
            // indistinguishable from the app deciding to do nothing (R7.4).
            //
            // Deliberately carries no token, so it is never dropped as stale. A
            // read that failed, failed — the key it names is in the message, and
            // suppressing it because the reader has moved on would be swallowing
            // an error on a technicality (R7.4).
            Err(e) => Msg::Failed {
                command: format!("reading {}", String::from_utf8_lossy(&name)),
                detail: e.details().to_string(),
                at_ms,
            },
        };
        let _ = tx.send(msg).await;
    });
}

fn area_width(term: &Terminal<CrosstermBackend<Stdout>>) -> usize {
    term.size()
        .map(|s| (s.width / 2).max(20) as usize)
        .unwrap_or(40)
}

fn start_scan(
    client: &Client,
    pattern: Option<String>,
    tx: &mpsc::Sender<Msg>,
    slot: &mut Option<CancellationToken>,
) {
    if let Some(previous) = slot.take() {
        previous.cancel();
    }
    let token = CancellationToken::new();
    *slot = Some(token.clone());
    let client = client.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        crate::redis::scan::stream_keys(&client, pattern.as_deref(), tx, token).await;
    });
}

/// Subscribe to a client's link-level streams: reconnect notifications, wire
/// errors, and (while tracking is supported) invalidation pushes. Tied to the
/// `Client` instance passed in, so this has to be called again after every
/// successful reconnect — a subscription opened on the old, now-dead client
/// delivers nothing about the new one.
fn spawn_link_watchers(client: &Client, tx: &mpsc::Sender<Msg>, tracking: bool) {
    // fred reconnects underneath us on its own schedule if it has a
    // `ReconnectPolicy` (this app sets none, so in practice this fires only
    // if that ever changes) — and the server on the other side remembers
    // nothing about what we were watching. Telling the core lets it drop the
    // liveness claim and re-arm, the invariant ADR-0009 exists for.
    {
        let mut reconnects = client.reconnect_rx();
        let tx = tx.clone();
        let probe = client.clone();
        tokio::spawn(async move {
            while reconnects.recv().await.is_ok() {
                // A fresh connection tracks nothing, so capability must be
                // re-probed rather than remembered.
                let tracking = crate::redis::probe_tracking_public(&probe).await;
                let (read_only, condition) = crate::redis::server_conditions(&probe).await;
                let _ = tx
                    .send(Msg::ServerState {
                        read_only,
                        condition,
                    })
                    .await;
                if tx
                    .send(Msg::Connected {
                        version: String::new(),
                        tracking_supported: tracking,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
    {
        let mut errors = client.error_rx();
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Ok((error, _server)) = errors.recv().await {
                // A connection-level error means the link is gone; anything
                // else is a command failure and belongs in a notification.
                let msg = if matches!(error.kind(), fred::error::ErrorKind::IO) {
                    Msg::ConnectionLost
                } else {
                    Msg::Failed {
                        command: "connection".into(),
                        detail: error.details().to_string(),
                        at_ms: 0,
                    }
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
        });
    }

    // Invalidation pushes arrive on their own task and become messages like
    // everything else. This is what makes a value update with no keypress —
    // and it is the whole reason this project exists (ADR-0006).
    if tracking {
        let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(client);
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Ok(_invalidation) = invalidations.recv().await {
                // The server has told us the open key changed, which also
                // consumed the arming. Refetching is what re-arms, and the core
                // decides whether the result lands or is announced.
                if tx.send(Msg::Invalidated).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// Attempt one reconnect after `after_ms`, cancellable by a fresh call — `r`
/// while disconnected retries immediately (ADR-0009) by cancelling whatever
/// backoff sleep or in-flight attempt was already running rather than racing
/// it, the same discipline `start_scan` applies to `scan_cancel`.
///
/// Success reaches `run`'s own locals through `reconnected`, never through a
/// `Msg`: a fresh `Client` cannot travel through the core's message type
/// without giving the core a dependency on `fred`, which the core/shell
/// boundary (ADR-0011) forbids. Failure is ordinary shell-reported state —
/// `Msg::Failed` for what went wrong, `Msg::ReconnectScheduled` for when to
/// try again — and needs no special channel.
#[allow(clippy::too_many_arguments)]
fn spawn_reconnect_attempt(
    dial: String,
    credentials: Credentials,
    after_ms: u64,
    attempt: u32,
    tx: mpsc::Sender<Msg>,
    reconnected: mpsc::Sender<(Client, Established)>,
    slot: &mut Option<CancellationToken>,
) {
    if let Some(previous) = slot.take() {
        previous.cancel();
    }
    let token = CancellationToken::new();
    *slot = Some(token.clone());
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = token.cancelled() => {}
            () = async {
                tokio::time::sleep(std::time::Duration::from_millis(after_ms)).await;
                match crate::redis::connect_with(&dial, &credentials).await {
                    Ok((client, established)) => {
                        let _ = tx
                            .send(Msg::ServerState {
                                read_only: established.read_only,
                                condition: established.condition,
                            })
                            .await;
                        let _ = reconnected.send((client, established)).await;
                    }
                    Err(err) => {
                        let next = attempt + 1;
                        let retry_in_ms = crate::redis::backoff_for(next).as_millis() as u64;
                        let at_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        let _ = tx
                            .send(Msg::Failed {
                                command: "reconnecting".into(),
                                detail: err.to_string(),
                                at_ms,
                            })
                            .await;
                        let _ = tx
                            .send(Msg::ReconnectScheduled {
                                attempt: next,
                                retry_in_ms,
                            })
                            .await;
                    }
                }
            } => {}
        }
    });
}

/// What the terminal can display. A real probe belongs in M0.3's follow-up;
/// `COLORTERM` is the part that is both cheap and reliable.
pub fn detect_color_depth() -> ColorDepth {
    resolve_color_depth(
        std::env::var("NO_COLOR").ok(),
        std::env::var("COLORTERM").ok(),
        std::env::var("TERM").ok(),
    )
}

/// The pure decision, taken out of [`detect_color_depth`] so it can be tested
/// without mutating the process environment — this workspace forbids
/// `unsafe`, which `std::env::set_var` requires in edition 2024, so a function
/// that reads the environment directly cannot be unit tested at all.
///
/// Severity-4 UI task: this had no dedicated tests before, only the golden
/// frames' assertion that each `ColorDepth` renders distinctly once selected —
/// nothing pinned *which* depth a given environment resolves to. `NO_COLOR`
/// (<https://no-color.org>) is now honoured too: a user who has set it wants
/// monochrome regardless of what the terminal claims to support, and ignoring
/// it was the one real gap here.
fn resolve_color_depth(
    no_color: Option<String>,
    colorterm: Option<String>,
    term: Option<String>,
) -> ColorDepth {
    if no_color.is_some() {
        return ColorDepth::Monochrome;
    }
    match colorterm.as_deref() {
        Some("truecolor") | Some("24bit") => ColorDepth::TrueColor,
        _ => match term.as_deref() {
            Some(t) if t.contains("256") => ColorDepth::Ansi256,
            Some("dumb") | None => ColorDepth::Monochrome,
            _ => ColorDepth::Ansi256,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth(no_color: Option<&str>, colorterm: Option<&str>, term: Option<&str>) -> ColorDepth {
        resolve_color_depth(
            no_color.map(String::from),
            colorterm.map(String::from),
            term.map(String::from),
        )
    }

    #[test]
    fn truecolor_from_colorterm() {
        assert_eq!(depth(None, Some("truecolor"), None), ColorDepth::TrueColor);
        assert_eq!(
            depth(None, Some("24bit"), Some("xterm")),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn ansi256_when_term_says_so() {
        assert_eq!(
            depth(None, None, Some("xterm-256color")),
            ColorDepth::Ansi256
        );
        assert_eq!(
            depth(None, None, Some("screen-256color")),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn monochrome_for_a_dumb_terminal_or_no_term_at_all() {
        assert_eq!(depth(None, None, Some("dumb")), ColorDepth::Monochrome);
        assert_eq!(depth(None, None, None), ColorDepth::Monochrome);
    }

    #[test]
    fn an_unrecognised_but_present_term_defaults_to_ansi256() {
        // A terminal that sets TERM to something we do not recognise is more
        // likely to support some colour than none; monochrome is reserved for
        // the terminal actively declaring "dumb" or declaring nothing.
        assert_eq!(depth(None, None, Some("screen")), ColorDepth::Ansi256);
        assert_eq!(depth(None, None, Some("vt100")), ColorDepth::Ansi256);
    }

    #[test]
    fn no_color_forces_monochrome_regardless_of_everything_else() {
        // https://no-color.org — a user who sets this wants monochrome even if
        // the terminal is fully capable of truecolor.
        assert_eq!(
            depth(Some("1"), Some("truecolor"), Some("xterm-256color")),
            ColorDepth::Monochrome
        );
        assert_eq!(
            depth(Some(""), None, None),
            ColorDepth::Monochrome,
            "presence is what matters, not the value"
        );
    }

    #[test]
    fn colorterm_wins_over_term_when_both_are_present() {
        assert_eq!(
            depth(None, Some("truecolor"), Some("dumb")),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn a_plain_char_translates() {
        let ev = KeyEvent::new(XKeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(
            translate(ev),
            Some(Msg::Key(KeyPress::plain(KeyCode::Char('q'))))
        );
    }

    #[test]
    fn ctrl_is_carried_across_the_boundary() {
        let ev = KeyEvent::new(XKeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            translate(ev),
            Some(Msg::Key(KeyPress::ctrl(KeyCode::Char('c'))))
        );
    }

    #[test]
    fn key_releases_are_ignored_so_a_press_is_not_counted_twice() {
        let mut ev = KeyEvent::new(XKeyCode::Char('q'), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Release;
        assert_eq!(translate(ev), None);
    }

    #[test]
    fn an_untranslatable_key_is_dropped_rather_than_guessed_at() {
        let ev = KeyEvent::new(XKeyCode::F(7), KeyModifiers::NONE);
        assert_eq!(translate(ev), None);
    }

    /// M0.5's proof: synthetic events drive the core with no terminal attached.
    #[test]
    fn synthetic_events_drive_the_core_to_quit_without_a_terminal() {
        let mut state = State::default();
        let mut commands = Vec::new();
        for ev in [
            KeyEvent::new(XKeyCode::Char('x'), KeyModifiers::NONE),
            KeyEvent::new(XKeyCode::Char('q'), KeyModifiers::NONE),
        ] {
            if let Some(msg) = translate(ev) {
                (state, commands) = update(state, msg);
            }
        }
        assert!(state.quitting);
        assert_eq!(commands, vec![Command::Quit]);
    }
}
