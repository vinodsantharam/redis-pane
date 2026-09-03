//! Golden-frame tests (ADR-0011).
//!
//! A frame is compared against a committed fixture. When a change is
//! intentional, re-record with:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p redis-pane-core
//! ```
//!
//! and review the diff — a rendered terminal is the most reviewable artifact
//! this project produces, so accepting a change should be cheap but never
//! automatic.

use std::path::PathBuf;

use ratatui::layout::Rect;
use redis_pane_core::clock::{Clock, FixedClock};
use redis_pane_core::render;
use redis_pane_core::state::{Connection, Environment, Link, Source, State, Tracking};
use redis_pane_core::theme::{ColorDepth, Theme};

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"))
}

/// Compare `actual` against the committed fixture for `name`.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing fixture {}\n\nrecord it with:\n  UPDATE_GOLDEN=1 cargo test -p redis-pane-core\n\nwould have written:\n{actual}", path.display())
    });
    if expected != actual {
        let side_by_side = expected
            .lines()
            .zip(actual.lines())
            .filter(|(e, a)| e != a)
            .map(|(e, a)| format!("  expected | {e}\n  actual   | {a}"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "golden frame {name} changed:\n{side_by_side}\n\nif intended: UPDATE_GOLDEN=1 cargo test -p redis-pane-core"
        );
    }
}

fn staging_state() -> State {
    State {
        cols: 100,
        rows: 2,
        connection: Connection {
            target: "cache-01:6379/0".into(),
            environment: Environment::Staging,
            source: Source::Profile("staging".into()),
        },
        last_read_ms: Some(60_000),
        link: Link::Up {
            version: "8.4.0".into(),
            tracking: Tracking::Armed,
        },
        ..State::default()
    }
}

fn render_at(depth: ColorDepth, clock: &dyn Clock) -> String {
    let buf = render::frame(
        &staging_state(),
        &Theme::new(depth),
        clock,
        Rect::new(0, 0, 100, 2),
    );
    render::to_golden(&buf)
}

// ── M0.3 — theme tokens degrade across colour depths ────────────────────────

#[test]
fn golden_title_bar_truecolor() {
    assert_golden(
        "title_bar_truecolor",
        &render_at(ColorDepth::TrueColor, &FixedClock(74_000)),
    );
}

#[test]
fn golden_title_bar_ansi256() {
    assert_golden(
        "title_bar_ansi256",
        &render_at(ColorDepth::Ansi256, &FixedClock(74_000)),
    );
}

#[test]
fn golden_title_bar_monochrome() {
    assert_golden(
        "title_bar_monochrome",
        &render_at(ColorDepth::Monochrome, &FixedClock(74_000)),
    );
}

#[test]
fn colour_depth_changes_the_frame_but_never_the_text() {
    let t = render_at(ColorDepth::TrueColor, &FixedClock(74_000));
    let m = render_at(ColorDepth::Monochrome, &FixedClock(74_000));
    assert_ne!(t, m, "styles must differ across depths");

    let text_of = |g: &str| g.split("--- styles ---").next().unwrap().to_string();
    assert_eq!(
        text_of(&t),
        text_of(&m),
        "losing colour must lose emphasis, never information — the words \
         `staging` and the target must survive into monochrome"
    );
}

// ── M0.2 — the clock is injected, so a frame is a function of state alone ───

/// Render in a state where the clock is actually consulted.
///
/// A `live` header states no read age — the server will say when the value
/// changes, so how long ago it was read is not the reader's problem. The clock
/// only reaches the frame when liveness is degraded, so that is the state these
/// tests must use.
fn render_manual_at(clock: &dyn Clock) -> String {
    let state = State {
        link: Link::Up {
            version: "8.4.0".into(),
            tracking: Tracking::Unsupported,
        },
        ..staging_state()
    };
    let buf = render::frame(
        &state,
        &Theme::new(ColorDepth::TrueColor),
        clock,
        Rect::new(0, 0, 100, 2),
    );
    render::to_golden(&buf)
}

#[test]
fn same_clock_reading_gives_an_identical_frame_whenever_it_is_rendered() {
    let first = render_manual_at(&FixedClock(74_000));
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let second = render_manual_at(&FixedClock(74_000));
    assert_eq!(
        first, second,
        "wall-clock time passed between these two renders; the frame must not notice"
    );
}

#[test]
fn a_different_clock_reading_does_change_the_frame() {
    // Without this, the test above would pass trivially for a frame that simply
    // ignores the clock.
    let at_14s = render_manual_at(&FixedClock(74_000));
    let at_5m = render_manual_at(&FixedClock(360_000));
    assert_ne!(
        at_14s, at_5m,
        "the read age is clock-derived and must move when the clock does"
    );
}

// ── M0.11 — every readout in DESIGN §6.8 ────────────────────────────────────

use redis_pane_core::keymap::{Action, Keymap};
use redis_pane_core::msg::{KeyCode, KeyPress};
use redis_pane_core::render::{help_lines, hint_bar, status_readout};
use redis_pane_core::state::{ReadOnlyReason, ServerCondition, Tracking as Tk};

const CLOCK: FixedClock = FixedClock(74_000);

fn base() -> State {
    State {
        cols: 100,
        rows: 2,
        connection: Connection {
            target: "cache-01:6379/0".into(),
            environment: Environment::Staging,
            source: Source::Profile("staging".into()),
        },
        last_read_ms: Some(60_000),
        ..State::default()
    }
}

fn up(tracking: Tk) -> Link {
    Link::Up {
        version: "8.4.0".into(),
        tracking,
    }
}

/// The readout as a plain string, which is what a reader actually sees.
fn readout(state: &State) -> String {
    status_readout(state, &CLOCK)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

#[test]
fn golden_title_bar_readouts() {
    let cases: Vec<(&str, State)> = vec![
        (
            "healthy and live",
            State {
                link: up(Tk::Armed),
                ..base()
            },
        ),
        (
            "tracking refused, degraded to manual",
            State {
                link: up(Tk::Unsupported),
                ..base()
            },
        ),
        (
            "connected but not yet armed",
            State {
                link: up(Tk::Available),
                ..base()
            },
        ),
        (
            "connection lost",
            State {
                link: Link::Reconnecting {
                    attempt: 2,
                    retry_in_ms: 4_000,
                },
                ..base()
            },
        ),
        (
            "read-only by Environment",
            State {
                link: up(Tk::Armed),
                read_only: Some(ReadOnlyReason::Environment),
                ..base()
            },
        ),
        (
            "read-only because the target is a replica",
            State {
                link: up(Tk::Armed),
                read_only: Some(ReadOnlyReason::Replica),
                ..base()
            },
        ),
        (
            "read-only by choice",
            State {
                link: up(Tk::Armed),
                read_only: Some(ReadOnlyReason::User),
                ..base()
            },
        ),
        (
            "maxmemory reached",
            State {
                link: up(Tk::Armed),
                condition: Some(ServerCondition::Oom),
                read_only: Some(ReadOnlyReason::Environment),
                ..base()
            },
        ),
        (
            "server restarting",
            State {
                link: up(Tk::Available),
                condition: Some(ServerCondition::Loading { percent: 43 }),
                ..base()
            },
        ),
    ];

    let rendered = cases
        .iter()
        .map(|(why, state)| format!("{why:<44}{}", readout(state)))
        .collect::<Vec<_>>()
        .join("\n");
    assert_golden("title_bar_readouts", &rendered);
}

#[test]
fn a_replica_is_locked_rather_than_offered_a_key_that_cannot_work() {
    let replica = State {
        link: up(Tk::Armed),
        read_only: Some(ReadOnlyReason::Replica),
        ..base()
    };
    let text = readout(&replica);
    assert!(text.contains("READ-ONLY replica"), "{text}");
    assert!(text.contains("locked"), "{text}");
    assert!(
        !text.contains("⌃R"),
        "a replica must not be offered a toggle: {text}"
    );

    let env = State {
        read_only: Some(ReadOnlyReason::Environment),
        ..replica.clone()
    };
    assert!(
        readout(&env).contains("⌃R"),
        "an Environment guard is liftable"
    );
}

#[test]
fn live_states_no_age_manual_states_one_and_reconnecting_states_the_countdown() {
    // A live header owes no age: the server will say when the value changes.
    let live = readout(&State {
        link: up(Tk::Armed),
        ..base()
    });
    assert!(!live.contains("read "), "{live}");

    // Degraded to manual, the age is the whole point.
    let manual = readout(&State {
        link: up(Tk::Unsupported),
        ..base()
    });
    assert!(manual.contains("read 14s ago"), "{manual}");

    // Reconnecting, the useful fact is when the next attempt lands (ADR-0009):
    // a backoff nobody can see is a freeze wearing a different name.
    let retrying = readout(&State {
        link: Link::Reconnecting {
            attempt: 2,
            retry_in_ms: 4_000,
        },
        ..base()
    });
    assert!(retrying.contains("retry 4s"), "{retrying}");
    assert!(
        !retrying.contains("read "),
        "the countdown replaces the age: {retrying}"
    );
}

#[test]
fn golden_help_overlay_frame() {
    let state = State {
        link: up(Tk::Armed),
        help_open: true,
        rows: 14,
        ..base()
    };
    let buf = render::frame(
        &state,
        &Theme::new(ColorDepth::Monochrome),
        &CLOCK,
        Rect::new(0, 0, 100, 14),
    );
    assert_golden("help_overlay_frame", &render::to_text(&buf));
}

// ── M0.12 — hints follow the binding, not a hard-coded label ────────────────

#[test]
fn golden_hint_bar_default_bindings() {
    assert_golden("hint_bar_default", &hint_bar(&base()));
}

#[test]
fn golden_help_overlay() {
    assert_golden("help_overlay", &help_lines(&base()).join("\n"));
}

/// R7.5's proof: rebinding an action changes what the screen says.
#[test]
fn rebinding_quit_changes_the_hint_bar_and_the_help_overlay() {
    let before = base();
    assert!(
        hint_bar(&before).contains("q quit"),
        "{}",
        hint_bar(&before)
    );

    let mut keymap = Keymap::default();
    keymap.bind(Action::Quit, KeyPress::ctrl(KeyCode::Char('x')));
    let after = State { keymap, ..base() };

    assert!(hint_bar(&after).contains("⌃X quit"), "{}", hint_bar(&after));
    assert!(
        !hint_bar(&after).contains("q quit"),
        "the stale label must be gone"
    );
    assert!(help_lines(&after).iter().any(|l| l.starts_with("⌃X")));
}

/// The same rule, applied to the readout: a rebound Refetch must show the new
/// key where the degraded header offers one.
#[test]
fn rebinding_refetch_changes_the_degraded_readout() {
    let mut keymap = Keymap::default();
    keymap.bind(Action::Refetch, KeyPress::plain(KeyCode::Char('u')));
    let state = State {
        link: up(Tk::Unsupported),
        keymap,
        ..base()
    };
    let text = readout(&state);
    assert!(text.ends_with("  u"), "{text}");
}

// ── M1.3 — the keyspace browser at every breakpoint ─────────────────────────

use redis_pane_core::state::LoadedSet;
use redis_pane_core::state::loaded::{KeyKind, TTL_NONE};

/// A browser with realistic keys, all metadata fetched.
fn browsing() -> State {
    let mut keys = LoadedSet::default();
    let rows: &[(&str, KeyKind, i32, u32)] = &[
        ("user:8812:session", KeyKind::Hash, 2_537, 2_150),
        ("user:8812:profile", KeyKind::Json, TTL_NONE, 880),
        ("user:8812:cart", KeyKind::ZSet, 720, 412),
        ("user:8813:session", KeyKind::Hash, 3_400, 1_980),
        ("cart:91af3c9d2e", KeyKind::ZSet, 720, 1_153_434),
        ("feed:global:hot", KeyKind::List, TTL_NONE, 64_512),
        ("lock:checkout:8812", KeyKind::String, 12, 41),
        ("stream:orders", KeyKind::Stream, TTL_NONE, 8_400_000),
    ];
    for (i, (name, kind, ttl, size)) in rows.iter().enumerate() {
        keys.push(name.as_bytes());
        keys.set_kind(i, *kind);
        keys.set_ttl(i, *ttl);
        keys.set_size(i, *size);
    }
    State {
        keys,
        scan: redis_pane_core::state::ScanState::Running {
            scanned: 41_203,
            estimated_total: 180_000,
        },
        link: up(Tk::Armed),
        ..base()
    }
}

fn draw(state: &State, w: u16, h: u16) -> String {
    let buf = render::frame(
        state,
        &Theme::new(ColorDepth::Monochrome),
        &CLOCK,
        Rect::new(0, 0, w, h),
    );
    render::to_text(&buf)
}

/// The full four-column layout begins at 120 (DESIGN §2), not at the 118 the
/// planning mockups happened to be drawn at.
#[test]
fn golden_browser_130_cols_full_density() {
    assert_golden("browser_130", &draw(&browsing(), 130, 26));
}

#[test]
fn golden_browser_119_cols_sheds_size() {
    assert_golden("browser_119", &draw(&browsing(), 119, 26));
}

#[test]
fn golden_browser_100_cols() {
    assert_golden("browser_100", &draw(&browsing(), 100, 26));
}

#[test]
fn golden_browser_80_cols() {
    assert_golden("browser_80", &draw(&browsing(), 80, 26));
}

#[test]
fn golden_browser_70_cols() {
    assert_golden("browser_70", &draw(&browsing(), 70, 26));
}

#[test]
fn golden_browser_single_pane_60_cols() {
    assert_golden("browser_60", &draw(&browsing(), 60, 26));
}

#[test]
fn rendering_cost_does_not_grow_with_the_keyspace() {
    // R2.6: a million-key Loaded set must draw like a ten-key one. If this ever
    // becomes false the list has stopped being virtualized.
    let mut keys = LoadedSet::default();
    for i in 0..200_000 {
        keys.push(format!("user:{i:08}:session").as_bytes());
    }
    let big = State {
        keys,
        link: up(Tk::Armed),
        ..base()
    };

    let started = std::time::Instant::now();
    for _ in 0..50 {
        let _ = draw(&big, 130, 26);
    }
    let per_frame = started.elapsed() / 50;
    assert!(
        per_frame < std::time::Duration::from_millis(16),
        "a frame took {per_frame:?}; a keystroke must be answerable in one frame"
    );
}

// ── M1.4 — placeholders hold their column ───────────────────────────────────

/// The same keys with nothing fetched yet.
fn pending() -> State {
    let mut keys = LoadedSet::default();
    for name in [
        "user:8812:session",
        "user:8812:profile",
        "user:8812:cart",
        "user:8813:session",
        "cart:91af3c9d2e",
        "feed:global:hot",
        "lock:checkout:8812",
        "stream:orders",
    ] {
        keys.push(name.as_bytes());
    }
    State { keys, ..browsing() }
}

#[test]
fn golden_browser_metadata_pending() {
    assert_golden("browser_pending", &draw(&pending(), 130, 26));
}

/// M1.4's proof: a value arriving must land exactly where its placeholder was.
#[test]
fn metadata_arriving_does_not_shift_a_single_column() {
    let pending_frame = draw(&pending(), 130, 26);
    let filled_frame = draw(&browsing(), 130, 26);

    let column_of = |frame: &str, needle: &str| -> Vec<usize> {
        frame
            .lines()
            .filter_map(|l| l.find(needle))
            .collect::<Vec<_>>()
    };

    // The key names are the anchor: they must occupy identical columns whether
    // or not the metadata beside them has arrived.
    assert_eq!(
        column_of(&pending_frame, "user:8812:session"),
        column_of(&filled_frame, "user:8812:session"),
        "a key name moved when metadata arrived"
    );

    // And the header row is geometry, not data — it cannot move at all.
    let header_of = |frame: &str| {
        frame
            .lines()
            .find(|l| l.contains("KEY") && l.contains("TYPE"))
            .unwrap()
            .to_string()
    };
    assert_eq!(
        header_of(&pending_frame),
        header_of(&filled_frame),
        "the column header shifted, so every cell under it did too"
    );
}

#[test]
fn a_pending_cell_is_visibly_waiting_rather_than_blank() {
    // Blank would read as "there is nothing here", which is a different claim.
    let frame = draw(&pending(), 130, 26);
    assert!(frame.contains('·'), "pending cells must show a placeholder");
    assert!(
        !frame.contains("∞"),
        "nothing is known yet, so no TTL facts"
    );

    let filled = draw(&browsing(), 130, 26);
    assert!(
        filled.contains('∞'),
        "a key with no expiry states that fact"
    );
}

/// DESIGN §2: as the terminal narrows the target is truncated from the left,
/// but the Environment and the Source are never sacrificed.
///
/// That readout is the entire mitigation for resolving a Connection silently
/// (ADR-0001), so it losing a race with the width is not a cosmetic bug.
#[test]
fn narrowing_never_costs_the_environment_or_the_source() {
    let state = State {
        connection: Connection {
            target: "redis-primary.eu-west-1.internal:6379/0".into(),
            environment: Environment::Prod,
            source: Source::Profile("prod".into()),
        },
        link: up(Tk::Armed),
        read_only: Some(ReadOnlyReason::Environment),
        ..browsing()
    };

    for width in [70u16, 80, 100, 119, 130, 200] {
        let title = draw(&state, width, 26).lines().next().unwrap().to_string();
        assert!(
            title.contains("prod"),
            "Environment lost at {width}: {title}"
        );
        assert!(
            title.contains("from profile prod"),
            "Source lost at {width}: {title}"
        );
        assert!(
            title.contains("READ-ONLY"),
            "the safety readout was overwritten at {width}: {title}"
        );
        assert_eq!(
            title.chars().count(),
            width as usize,
            "the title bar must fill exactly the width at {width}"
        );
    }
}
