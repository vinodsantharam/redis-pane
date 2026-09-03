//! The terminal shell: raw mode, the event loop, and resize handling (PLAN M0.5).
//!
//! The render loop never does I/O. Crossterm events are translated into
//! [`Msg`]s, the core decides what they mean, and the resulting state is drawn.
//! A keystroke is answered within one frame regardless of what the network is
//! doing, because nothing here waits on the network.

use std::io::{Stdout, stdout};

use crossterm::event::{self, Event, KeyCode as XKeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use fred::prelude::Client;
use redis_pane_core::clock::Clock;
use redis_pane_core::msg::{KeyCode, KeyPress};
use redis_pane_core::theme::{ColorDepth, Theme};
use redis_pane_core::{Command, Msg, State, render, update};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Restores the terminal on drop, including when the process is unwinding.
/// Leaving a terminal in raw mode after a panic is the rudest thing a TUI can
/// do to someone at 3am.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), terminal::LeaveAlternateScreen);
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

/// Run the event loop until the core says to quit.
///
/// The render loop never does I/O. Redis work happens on tokio tasks that send
/// [`Msg`]s into this loop; the loop reads state and draws. A keystroke is
/// therefore answerable in one frame regardless of what the network is doing.
pub async fn run(
    mut state: State,
    theme: Theme,
    clock: &dyn Clock,
    client: Client,
    tracking: bool,
) -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(stdout(), terminal::EnterAlternateScreen)?;
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

    let mut scan_cancel: Option<CancellationToken> = None;
    start_scan(&client, None, &tx, &mut scan_cancel);

    // Invalidation pushes arrive on their own task and become messages like
    // everything else. This is what makes a value update with no keypress —
    // and it is the whole reason this project exists (ADR-0006).
    if tracking {
        let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(&client);
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

    loop {
        term.draw(|f| {
            let buf = render::frame(&state, &theme, clock, f.area());
            *f.buffer_mut() = buf;
        })?;

        let Some(msg) = rx.recv().await else {
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
                        if let Ok(entries) = crate::redis::fetch_metadata(&client, &window).await
                            && !entries.is_empty()
                        {
                            let _ = tx.send(Msg::MetadataBatch { entries }).await;
                        }
                    });
                }
                Command::OpenKey { index, name } => {
                    open_key(&client, index, name, &tx, area_width(&term))
                }
                Command::RefetchOpenKey => {
                    if let Some(open) = &state.open {
                        open_key(
                            &client,
                            open.index,
                            open.name.as_bytes().to_vec(),
                            &tx,
                            area_width(&term),
                        );
                    }
                }
                // Reconnection wiring lands with M2.
                Command::Reconnect { .. } => {}
            }
        }
    }
}

/// Read a key and send the result in. The one read path: it always re-arms,
/// so there is no branch on which liveness can be silently lost (ADR-0006).
fn open_key(
    client: &Client,
    index: usize,
    name: Vec<u8>,
    tx: &mpsc::Sender<Msg>,
    pane_width: usize,
) {
    let client = client.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let msg = match crate::redis::read::read_value(&client, &name, pane_width).await {
            Ok(Some(read)) => Msg::ValueLoaded {
                index,
                name: String::from_utf8_lossy(&name).into_owned(),
                value: read.value,
                ttl_seconds: read.ttl_seconds,
                size_bytes: read.size_bytes,
                at_ms,
            },
            Ok(None) => Msg::ValueGone { at_ms },
            Err(_) => return,
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

/// What the terminal can display. A real probe belongs in M0.3's follow-up;
/// `COLORTERM` is the part that is both cheap and reliable.
pub fn detect_color_depth() -> ColorDepth {
    match std::env::var("COLORTERM").as_deref() {
        Ok("truecolor") | Ok("24bit") => ColorDepth::TrueColor,
        _ => match std::env::var("TERM").as_deref() {
            Ok(t) if t.contains("256") => ColorDepth::Ansi256,
            Ok("dumb") | Err(_) => ColorDepth::Monochrome,
            _ => ColorDepth::Ansi256,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
