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
                    retry_in_ms: None,
                },
                ..base()
            },
        ),
        (
            "connection lost, a retry scheduled",
            State {
                link: Link::Reconnecting {
                    attempt: 2,
                    retry_in_ms: Some(4_000),
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

    // A dropped link with nothing scheduled — the state the app actually
    // reaches today, since `Command::Reconnect` is not wired until M2. It shows
    // the Read age, which is what ADR-0009 asks for and what is true. This test
    // used to assert `retry 4s` here, from a state the running app could not
    // produce: a fixture keeping a promise the code never made.
    let dropped = readout(&State {
        link: Link::Reconnecting {
            attempt: 1,
            retry_in_ms: None,
        },
        ..base()
    });
    assert!(dropped.contains("read 14s ago"), "{dropped}");
    assert!(
        !dropped.contains("retry"),
        "no countdown for a retry nobody scheduled: {dropped}"
    );
    assert!(
        dropped.trim_end().ends_with("ago"),
        "nothing after the age — no Refetch key offered, since it would only \
         read a dead client: {dropped}"
    );

    // Once a retry really is scheduled the countdown is the useful fact
    // (ADR-0009): a backoff nobody can see is a freeze wearing a different
    // name. Only `Msg::ReconnectScheduled` can reach this.
    let retrying = readout(&State {
        link: Link::Reconnecting {
            attempt: 2,
            retry_in_ms: Some(4_000),
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
    let mut state = State {
        keys,
        scan: redis_pane_core::state::ScanState::Running {
            scanned: 41_203,
            estimated_total: 180_000,
        },
        link: up(Tk::Armed),
        ..base()
    };
    // The list renders through an index vector, which has to be built from the
    // store before anything is on screen.
    state.rebuild_list();
    state
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
    let mut big = State {
        keys,
        link: up(Tk::Armed),
        ..base()
    };
    big.rebuild_list();

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
    let mut state = State { keys, ..browsing() };
    state.rebuild_list();
    state
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

    // `str::find` returns a *byte* offset, and the dot marker preceding a key
    // name can be "●" (3 bytes, a known type) or the pending placeholder "·"
    // (2 bytes) — both occupy exactly one terminal column, so a byte offset
    // would report a false shift between them. Column position is measured in
    // characters instead, which is what actually appears on screen.
    let column_of = |frame: &str, needle: &str| -> Vec<usize> {
        frame
            .lines()
            .filter_map(|l| l.find(needle).map(|byte_idx| l[..byte_idx].chars().count()))
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

// ── a key that vanished while the list was on screen (DESIGN §9) ────────────

/// `browsing()` with one key deleted underneath the reader.
fn with_a_gone_key() -> State {
    let mut state = browsing();
    // `cart:91af3c9d2e`, in the middle of the list — the position is the point:
    // the row must stay where it is rather than renumbering its neighbours.
    state.keys.set_gone(4);
    state.rebuild_list();
    state
}

#[test]
fn golden_browser_with_a_deleted_key() {
    assert_golden("browser_gone", &draw(&with_a_gone_key(), 130, 26));
}

#[test]
fn a_deleted_key_keeps_its_row_and_says_so() {
    let frame = draw(&with_a_gone_key(), 130, 26);
    let row = frame
        .lines()
        .find(|l| l.contains("cart:91af3c9d2e"))
        .expect("the row must survive its key's deletion");

    assert!(row.contains('✕'), "the deletion must be marked: {row}");
    assert!(
        row.contains("gone"),
        "the TYPE column must say what happened rather than fall back to the \
         pending placeholder, which would claim the fetch had not happened yet"
    );
    // The fetch that found it missing is the same fetch that had already
    // reported its size, and during an incident that figure is the answer to
    // the only question worth asking about a key that is no longer there.
    assert!(
        row.contains("1.1 MB") || row.contains("1.1MB"),
        "last-known size is kept: {row}"
    );
    // But not the TTL: "expires in 12m" is a claim about a key that is not
    // there to expire, where "it held 1.1 MB" stays true after the deletion.
    assert!(
        !row.contains("12m"),
        "a gone key must not count down toward an expiry that cannot happen: {row}"
    );
}

#[test]
fn a_deleted_row_shifts_nothing_around_it() {
    let before = draw(&browsing(), 130, 26);
    let after = draw(&with_a_gone_key(), 130, 26);

    let column_of = |frame: &str, needle: &str| -> Vec<usize> {
        frame
            .lines()
            .filter_map(|l| l.find(needle).map(|byte_idx| l[..byte_idx].chars().count()))
            .collect::<Vec<_>>()
    };
    // `✕` and `●` are both one column wide but different byte lengths, so this
    // is measured in characters for the same reason the M1.4 proof above is.
    for anchor in ["feed:global:hot", "stream:orders", "cart:91af3c9d2e"] {
        assert_eq!(
            column_of(&before, anchor),
            column_of(&after, anchor),
            "{anchor} moved when a key above it was deleted"
        );
    }
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

// ── M1.5 / M1.6 / M1.7 — filter, tree, sort ─────────────────────────────────

use redis_pane_core::state::{FilterMode, SortBy};

fn many_keys() -> State {
    let mut keys = LoadedSet::default();
    let rows: &[(&str, KeyKind, i32, u32)] = &[
        ("user:8812:cart", KeyKind::ZSet, 720, 412),
        ("user:8812:profile", KeyKind::Json, TTL_NONE, 880),
        ("user:8812:session", KeyKind::Hash, 2_537, 2_150),
        ("user:8813:session", KeyKind::Hash, 3_400, 1_980),
        ("cart:91af3c9d2e", KeyKind::ZSet, 720, 1_153_434),
        ("feed:global:hot", KeyKind::List, TTL_NONE, 64_512),
    ];
    for (i, (name, kind, ttl, size)) in rows.iter().enumerate() {
        keys.push(name.as_bytes());
        keys.set_kind(i, *kind);
        keys.set_ttl(i, *ttl);
        keys.set_size(i, *size);
    }
    let mut state = State {
        keys,
        link: up(Tk::Armed),
        ..base()
    };
    state.rebuild_list();
    state
}

#[test]
fn golden_filtered_list() {
    let mut state = many_keys();
    state.list.filter = "user:*:session".into();
    state.rebuild_list();
    assert_golden("browser_filtered", &draw(&state, 130, 20));
}

#[test]
fn golden_tree_mode() {
    let mut state = many_keys();
    state.tree_mode = true;
    state.rebuild_list();
    assert_golden("browser_tree", &draw(&state, 130, 20));
}

#[test]
fn golden_sorted_by_size_partially_known() {
    let mut state = many_keys();
    // Two rows never got their size, which is the ordinary case while a list is
    // still filling.
    state.keys.set_size(0, u32::MAX - 1);
    let mut keys = LoadedSet::default();
    for (i, name) in [
        "user:8812:cart",
        "user:8812:profile",
        "unknown:a",
        "unknown:b",
    ]
    .iter()
    .enumerate()
    {
        keys.push(name.as_bytes());
        if i < 2 {
            keys.set_kind(i, KeyKind::ZSet);
            keys.set_size(i, [412u32, 880][i]);
            keys.set_ttl(i, 720);
        }
    }
    state.keys = keys;
    state.list.sort = SortBy::Size;
    state.rebuild_list();
    assert_golden("browser_sorted_partial", &draw(&state, 130, 20));
}

#[test]
fn the_filter_line_states_how_much_it_matched() {
    let mut state = many_keys();
    state.list.filter = "user:*".into();
    state.rebuild_list();
    let frame = draw(&state, 130, 20);
    assert!(frame.contains("/ user:*"), "{frame}");
    assert!(
        frame.contains("4 of 6"),
        "the reader is told the scope: {frame}"
    );
}

#[test]
fn a_partial_sort_says_so_on_screen() {
    // R2.5: ordering what arrived is fine; not saying so is not.
    let mut state = many_keys();
    let mut keys = LoadedSet::default();
    keys.push(b"known");
    keys.set_size(0, 100);
    keys.push(b"unknown");
    state.keys = keys;
    state.list.sort = SortBy::Size;
    state.rebuild_list();

    let frame = draw(&state, 130, 20);
    assert!(
        frame.contains("1 of 2 known"),
        "a partial sort must state its scope: {frame}"
    );
}

#[test]
fn tree_mode_shows_group_counts_and_leaf_names_only() {
    let mut state = many_keys();
    state.tree_mode = true;
    state.rebuild_list();
    let frame = draw(&state, 130, 20);
    assert!(frame.contains("▾ user:"), "{frame}");
    assert!(frame.contains("▾ 8812:"), "nested groups fold too: {frame}");
    // A leaf under `user:8812:` shows as `session`, not the whole path — the
    // ancestors are already on screen above it.
    // Leaf rows now carry a type-coloured dot ahead of the name (the UI task
    // this docstring predates), so the line starts with the dot, not the text.
    assert!(
        frame.lines().any(|l| l.trim_start().contains("● session")),
        "{frame}"
    );
}

#[test]
fn filtering_narrows_the_tree_as_well_as_the_flat_list() {
    let mut state = many_keys();
    state.tree_mode = true;
    state.list.filter = "cart".into();
    state.rebuild_list();
    let frame = draw(&state, 130, 20);
    assert!(!frame.contains("session"), "filtered out: {frame}");
    assert!(frame.contains("cart"), "{frame}");
}

#[test]
fn fuzzy_mode_matches_characters_in_order() {
    let mut state = many_keys();
    state.list.mode = FilterMode::Fuzzy;
    // `u88ses` would match 8812 *and* 8813 — fuzzy is deliberately generous.
    state.list.filter = "u8812ses".into();
    state.rebuild_list();
    assert_eq!(state.list.len(), 1);
    assert_eq!(
        state
            .keys
            .name_str(state.list.index_at(0).unwrap())
            .unwrap(),
        "user:8812:session"
    );
}

// ── M1.8 / M1.9 — one frame, eight bodies ───────────────────────────────────

use redis_pane_core::state::OpenKey;
use redis_pane_core::state::value::{
    BinaryValue, IndexedValue, JsonValue, MemberValue, PairValue, ScoredValue, StreamValue,
    StringValue, Value,
};

/// A key open, with the cursor on its row — the Attached case, which is what
/// these viewer fixtures are about.
///
/// It opens *the row that actually holds this name*, adding it to the Loaded
/// set if it is not already there. The earlier version opened index 0 whatever
/// the name was, which was invisible while nothing tied the panes together and
/// is a self-contradicting frame now that something does: a header naming one
/// key over a mark pointing at another is precisely the confusion these fixtures
/// exist to catch.
fn opened(name: &str, value: Value, ttl: i32) -> State {
    let mut state = many_keys();
    let index = (0..state.keys.len())
        .find(|&i| state.keys.name_str(i).as_deref() == Some(name))
        .unwrap_or_else(|| {
            state.keys.push(name.as_bytes());
            state.keys.len() - 1
        });
    state.open = Some(OpenKey::new(
        Some(index),
        name.into(),
        value,
        ttl,
        2_150,
        60_000,
    ));
    state.rebuild_list();
    state.view.selected = state.open.as_ref().and_then(|open| open.row).unwrap_or(0);
    state
}

fn hash_value() -> Value {
    Value::Hash(PairValue {
        pairs: vec![
            ("id".into(), "8812".into()),
            ("device".into(), "ios/17.2".into()),
            ("region".into(), "eu-west-1".into()),
            ("plan".into(), "pro".into()),
            ("locale".into(), "fr-FR".into()),
        ],
    })
}

#[test]
fn golden_viewer_hash() {
    assert_golden(
        "viewer_hash",
        &draw(&opened("user:8812:session", hash_value(), 2_537), 130, 22),
    );
}

#[test]
fn golden_viewer_zset() {
    let v = Value::ZSet(ScoredValue {
        entries: vec![
            ("sku-100".into(), 1.0),
            ("sku-221".into(), 2.0),
            ("sku-874".into(), 3.5),
        ],
        total: 3,
    });
    assert_golden(
        "viewer_zset",
        &draw(&opened("user:8812:cart", v, 720), 130, 22),
    );
}

#[test]
fn golden_viewer_stream() {
    // Newest first (XREVRANGE), and close enough to CLOCK (74_000) for the AGE
    // column to show something real rather than saturating to "just now" —
    // real stream IDs carry genuine epoch millis, which this fixed test clock
    // deliberately does not use anywhere else in this file.
    let v = Value::Stream(StreamValue {
        entries: vec![
            (
                "72000-0".into(),
                vec![
                    ("order".into(), "1001".into()),
                    ("amount".into(), "17".into()),
                ],
            ),
            (
                "10000-0".into(),
                vec![
                    ("order".into(), "1000".into()),
                    ("amount".into(), "42".into()),
                ],
            ),
        ],
        total: 500,
    });
    assert_golden(
        "viewer_stream",
        &draw(&opened("stream:orders", v, TTL_NONE), 130, 22),
    );
}

// ── focus is visible, because `r` means different things per pane ───────────

#[test]
fn which_pane_has_focus_is_visible_without_pressing_anything() {
    use redis_pane_core::render::layout::Pane;
    let keys_focused = State {
        focus: Pane::Keys,
        ..opened("user:8812:session", hash_value(), 2_537)
    };
    let value_focused = State {
        focus: Pane::Value,
        ..opened("user:8812:session", hash_value(), 2_537)
    };
    let a = render::to_golden(&render::frame(
        &keys_focused,
        &Theme::new(ColorDepth::TrueColor),
        &CLOCK,
        Rect::new(0, 0, 130, 22),
    ));
    let b = render::to_golden(&render::frame(
        &value_focused,
        &Theme::new(ColorDepth::TrueColor),
        &CLOCK,
        Rect::new(0, 0, 130, 22),
    ));
    let text_of = |g: &str| g.split("--- styles ---").next().unwrap().to_string();
    assert_eq!(
        text_of(&a),
        text_of(&b),
        "focus is emphasis, not content — no glyph, no reflow, no width change"
    );
    assert_ne!(a, b, "but it must be visible in the styles");
}

#[test]
fn focus_survives_the_loss_of_colour() {
    // It says which pane `r` will act on, so it is information rather than
    // decoration, and this module's rule is that losing colour loses emphasis
    // and never information. Muted is DIM in monochrome where Text is plain.
    let theme = Theme::new(ColorDepth::Monochrome);
    assert_ne!(
        theme.style(redis_pane_core::theme::Token::Text),
        theme.style(redis_pane_core::theme::Token::Muted),
        "the focused and unfocused pane headers would be indistinguishable"
    );
}

// ── the header must not pass a window off as the whole value ────────────────

#[test]
fn a_windowed_value_says_how_much_of_it_is_on_screen() {
    // `XLEN` says 500; the read brought back 2. Stating only the first turns
    // "the newest 2 of these" into "this is all of it" — the scan-cap defect
    // one level down, and the reason a search over these rows was cut rather
    // than built on top of a header that lies about its own scope.
    let v = Value::Stream(StreamValue {
        entries: vec![("72000-0".into(), vec![("order".into(), "1001".into())])],
        total: 500,
    });
    let frame = draw(&opened("stream:orders", v, TTL_NONE), 130, 22);
    let header = frame
        .lines()
        .find(|l| l.contains("stream ·"))
        .expect("the viewer header names the type");
    assert!(header.contains("500 entries"), "the real length: {header}");
    assert!(
        header.contains("1 shown"),
        "and what is on screen: {header}"
    );
}

#[test]
fn a_value_fetched_whole_says_nothing_extra() {
    // A hash comes back complete, so there is no window to disclose and the
    // header must not grow a phrase that would read as a caveat where none
    // applies.
    let frame = draw(&opened("user:8812:session", hash_value(), 2_537), 130, 22);
    let header = frame
        .lines()
        .find(|l| l.contains("hash ·"))
        .expect("the viewer header names the type");
    assert!(!header.contains("shown"), "nothing is withheld: {header}");
}

#[test]
fn golden_viewer_binary() {
    let v = Value::Binary(BinaryValue {
        bytes: (0u8..48).collect(),
    });
    assert_golden(
        "viewer_binary",
        &draw(&opened("blob:thumb", v, TTL_NONE), 130, 22),
    );
}

#[test]
fn golden_viewer_json() {
    let v = Value::Json(JsonValue::parse(
        r#"{"newnav":true,"ab_checkout":"B","rollout":0.25}"#,
    ));
    assert_golden(
        "viewer_json",
        &draw(&opened("config:feature-flags", v, TTL_NONE), 130, 22),
    );
}

/// R3.1's proof, stated as a frame comparison rather than a claim: the header
/// and footer are byte-identical across types, so navigation can be shared.
#[test]
fn the_frame_around_the_body_is_identical_for_every_type() {
    let cases = vec![
        Value::Str(StringValue::new("hello world", 60)),
        hash_value(),
        Value::List(IndexedValue {
            items: vec!["x".into()],
            total: 1,
        }),
        Value::Set(MemberValue {
            members: vec!["m".into()],
            total: 1,
        }),
        Value::ZSet(ScoredValue {
            entries: vec![("m".into(), 1.0)],
            total: 1,
        }),
        Value::Stream(StreamValue {
            entries: vec![],
            total: 0,
        }),
        Value::Json(JsonValue::parse("{}")),
        Value::Binary(BinaryValue { bytes: vec![1] }),
    ];

    let mut first_header: Option<String> = None;
    for value in cases {
        let frame = draw(&opened("k", value, 600), 130, 22);
        let lines: Vec<&str> = frame.lines().collect();
        // Row 0 is the title bar; the value pane's key name and ttl rows are
        // shared chrome and must not vary with the type.
        let key_row = lines[2].split('│').nth(1).unwrap_or("").to_string();
        let ttl_row = lines[4].split('│').nth(1).unwrap_or("").to_string();
        let combined = format!("{key_row}|{ttl_row}");
        match &first_header {
            None => first_header = Some(combined),
            Some(expected) => assert_eq!(
                &combined, expected,
                "the shared frame differed between types"
            ),
        }
    }
}

// ── M1.10 — the liveness states, on screen ──────────────────────────────────

#[test]
fn golden_viewer_update_held_while_scrolled() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    let open = state.open.as_mut().unwrap();
    open.offset = 2;
    open.at_rest = false;
    open.absorb(hash_value(), 2_400, 2_200, 72_000);
    assert_golden("viewer_changed_held", &draw(&state, 130, 22));
}

#[test]
fn golden_viewer_deleted() {
    let mut state = opened("lock:checkout:8812", hash_value(), 12);
    state.open.as_mut().unwrap().deleted_at_ms = Some(71_000);
    // The row learns it too. `Msg::ValueGone` does both together (958b311); a
    // fixture that badged only the Viewer would picture the two panes
    // disagreeing about one key, which is the state that commit removed.
    let index = state.open.as_ref().unwrap().index.unwrap();
    state.keys.set_gone(index);
    assert_golden("viewer_deleted", &draw(&state, 130, 22));
}

#[test]
fn an_update_at_rest_lands_with_no_keypress() {
    // The whole point of ADR-0006, asserted at the frame level.
    let mut state = opened("k", hash_value(), 600);
    let before = draw(&state, 130, 22);

    let changed = Value::Hash(PairValue {
        pairs: vec![("plan".into(), "enterprise".into())],
    });
    state
        .open
        .as_mut()
        .unwrap()
        .absorb(changed, 600, 2_150, 61_000);

    let after = draw(&state, 130, 22);
    assert_ne!(before, after, "the new value must be on screen already");
    assert!(after.contains("enterprise"));
    assert!(
        !after.contains("r to load"),
        "nothing was asked of the reader"
    );
}

#[test]
fn an_update_while_scrolled_is_announced_and_offers_the_effective_key() {
    let mut state = opened("k", hash_value(), 600);
    let open = state.open.as_mut().unwrap();
    open.offset = 3;
    open.at_rest = false;
    open.absorb(
        Value::Hash(PairValue {
            pairs: vec![("plan".into(), "enterprise".into())],
        }),
        600,
        2_150,
        // CLOCK is 74_000, so this arrived two seconds ago.
        72_000,
    );

    let frame = draw(&state, 130, 22);
    assert!(frame.contains("changed 2s ago"), "{frame}");
    assert!(frame.contains("r to load"), "{frame}");
    assert!(
        !frame.contains("enterprise"),
        "nothing moved under the reader"
    );
}

#[test]
fn a_deleted_key_keeps_its_value_on_screen() {
    let mut state = opened("k", hash_value(), 600);
    state.open.as_mut().unwrap().deleted_at_ms = Some(63_000);
    let frame = draw(&state, 130, 22);
    assert!(frame.contains("✕ deleted"), "{frame}");
    assert!(frame.contains("ios/17.2"), "the evidence survives: {frame}");
}

#[test]
fn the_ttl_counts_down_between_frames_without_any_fetch() {
    let state = opened("k", hash_value(), 600);
    let at_open = render::frame(
        &state,
        &Theme::new(ColorDepth::Monochrome),
        &FixedClock(60_000),
        Rect::new(0, 0, 130, 22),
    );
    let a_minute_later = render::frame(
        &state,
        &Theme::new(ColorDepth::Monochrome),
        &FixedClock(120_000),
        Rect::new(0, 0, 130, 22),
    );
    let text = |b: &ratatui::buffer::Buffer| render::to_text(b);
    assert!(text(&at_open).contains("ttl 10m"));
    assert!(
        text(&a_minute_later).contains("ttl 9m"),
        "{}",
        text(&a_minute_later)
    );
}

// ── M1.12 — copy ────────────────────────────────────────────────────────────

use redis_pane_core::keymap::{Action as Act, Keymap as Km};
use redis_pane_core::msg::KeyCode as KC;
use redis_pane_core::state::copy::{redis_cli_command, value_text};
use redis_pane_core::{Command, Msg, update};

fn press(state: State, c: char) -> (State, Vec<Command>) {
    update(state, Msg::Key(KeyPress::plain(KC::Char(c))))
}

#[test]
fn y_then_y_copies_the_key_name() {
    let state = opened("user:8812:session", hash_value(), 600);
    let (state, cmds) = press(state, 'y');
    assert!(cmds.is_empty(), "the chord waits for its second key");
    assert!(state.copy_pending);

    let (state, cmds) = press(state, 'y');
    assert!(!state.copy_pending);
    match cmds.first() {
        Some(Command::CopyToClipboard { text, label }) => {
            assert_eq!(text, "user:8812:session");
            assert_eq!(*label, "key");
        }
        other => panic!("expected a copy, got {other:?}"),
    }
}

#[test]
fn y_then_v_copies_the_whole_value() {
    let state = opened("k", hash_value(), 600);
    let (state, _) = press(state, 'y');
    let (_, cmds) = press(state, 'v');
    match cmds.first() {
        Some(Command::CopyToClipboard { text, .. }) => {
            assert_eq!(
                text.lines().count(),
                5,
                "all five fields, not the visible ones"
            );
            assert!(text.contains("device\tios/17.2"));
        }
        other => panic!("expected a copy, got {other:?}"),
    }
}

#[test]
fn y_then_c_copies_a_command_that_would_actually_run() {
    let state = opened("user:8812:session", hash_value(), 600);
    let (state, _) = press(state, 'y');
    let (_, cmds) = press(state, 'c');
    match cmds.first() {
        Some(Command::CopyToClipboard { text, .. }) => {
            assert_eq!(
                text,
                "redis-cli -h cache-01 -p 6379 -n 0 HGETALL user:8812:session"
            );
        }
        other => panic!("expected a copy, got {other:?}"),
    }
}

#[test]
fn an_unrecognised_second_key_cancels_rather_than_guessing() {
    // The clipboard is somewhere the user cannot see, so guessing is worse
    // than doing nothing.
    let state = opened("k", hash_value(), 600);
    let (state, _) = press(state, 'y');
    let (state, cmds) = press(state, 'z');
    assert!(cmds.is_empty());
    assert!(!state.copy_pending, "and the chord does not stay armed");
}

#[test]
fn the_key_name_is_copyable_from_the_list_with_nothing_open() {
    let mut state = many_keys();
    state.open = None;
    let (state, _) = press(state, 'y');
    let (_, cmds) = press(state, 'y');
    assert!(
        matches!(cmds.first(), Some(Command::CopyToClipboard { .. })),
        "a key name needs no open value"
    );
}

#[test]
fn copying_a_value_with_nothing_open_says_so_instead_of_copying_nothing() {
    let mut state = many_keys();
    state.open = None;
    let (state, _) = press(state, 'y');
    let (state, cmds) = press(state, 'v');
    assert!(cmds.is_empty());
    assert_eq!(state.notice_now(0), Some("nothing open to copy"));
}

#[test]
fn the_confirmation_fades_on_its_own() {
    // A notice you must dismiss is a modal dialog wearing a smaller hat.
    let (state, _) = update(
        opened("k", hash_value(), 600),
        Msg::Copied {
            label: "key",
            at_ms: 70_000,
        },
    );
    assert_eq!(state.notice_now(70_500), Some("copied key"));
    assert_eq!(state.notice_now(74_000), None, "gone by 4 seconds");
}

#[test]
fn golden_copy_notice() {
    let (state, _) = update(
        opened("user:8812:session", hash_value(), 2_537),
        Msg::Copied {
            label: "redis-cli command",
            at_ms: 73_000,
        },
    );
    assert_golden("copy_notice", &draw(&state, 130, 22));
}

#[test]
fn the_copy_binding_appears_in_the_help_overlay() {
    // Keybindings are data, so the overlay follows automatically (R7.5).
    let mut keymap = Km::default();
    assert!(keymap.hint(Act::Copy).is_some());
    keymap.bind(Act::Copy, KeyPress::ctrl(KC::Char('y')));
    let state = State { keymap, ..base() };
    assert!(help_lines(&state).iter().any(|l| l.starts_with("⌃Y")));
}

#[test]
fn the_command_uses_the_target_the_title_bar_is_showing() {
    // A copied command that points at a different server than the one on
    // screen would be actively dangerous.
    let state = opened("k", hash_value(), 600);
    let cmd = redis_cli_command(&state.connection.target, state.open.as_ref().unwrap());
    assert!(cmd.contains("cache-01"), "{cmd}");
    assert_eq!(state.connection.target, "cache-01:6379/0");
}

#[test]
fn copying_a_value_is_not_affected_by_where_the_viewer_is_scrolled() {
    let mut state = opened("k", hash_value(), 600);
    state.open.as_mut().unwrap().offset = 3;
    let full = value_text(&state.open.as_ref().unwrap().value, 0);
    assert_eq!(full.lines().count(), 5);
}

// ── severity-4: editing indicator in the value pane header ──────────────────

#[test]
fn golden_viewer_editing_with_nothing_pending() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.open.as_mut().unwrap().editing = true;
    assert_golden("viewer_editing", &draw(&state, 130, 22));
}

#[test]
fn editing_is_visibly_distinct_from_plain_live_in_monochrome_too() {
    // Colour is never the only carrier of meaning (DESIGN §5): the word
    // "editing" must appear even with hue gone entirely.
    let mut state = opened("k", hash_value(), 600);
    state.open.as_mut().unwrap().editing = true;
    let mono = render::frame(
        &state,
        &Theme::new(ColorDepth::Monochrome),
        &CLOCK,
        Rect::new(0, 0, 130, 22),
    );
    assert!(render::to_text(&mono).contains("editing"));
}

// ── UI task: type colour dots and the selection bar (style-verified) ────────

use redis_pane_core::theme::Token;

#[test]
fn golden_browser_style_selection_and_type_colours() {
    let state = many_keys();
    let frame = render::frame(
        &state,
        &Theme::new(ColorDepth::TrueColor),
        &CLOCK,
        Rect::new(0, 0, 90, 12),
    );
    assert_golden("browser_style_truecolor", &render::to_golden(&frame));
}

/// Find the y coordinate of the rendered row containing `needle`, by reading
/// the text a real frame produces rather than guessing chrome-row arithmetic.
fn row_of(frame: &ratatui::buffer::Buffer, needle: &str) -> u16 {
    render::to_text(frame)
        .lines()
        .position(|l| l.contains(needle))
        .expect("row not found") as u16
}

#[test]
fn the_selected_row_carries_a_background_all_the_way_across_not_just_on_the_name() {
    // The tricky part of this feature: put() resets style before applying its
    // own, so a background painted once and then written over by later cells
    // would leave holes rather than one continuous bar.
    let state = many_keys(); // selection defaults to row 0: user:8812:cart
    let theme = Theme::new(ColorDepth::TrueColor);
    let frame = render::frame(&state, &theme, &CLOCK, Rect::new(0, 0, 130, 12));
    let y = row_of(&frame, "user:8812:cart");

    let selected_bg = theme.style(Token::Selected).bg;
    assert!(selected_bg.is_some());

    // The bar spans exactly the keys pane, not the full 130-column frame —
    // there is a value pane to the right of it, correctly unpainted.
    let keys_pane_width = redis_pane_core::render::layout::layout(
        Rect::new(0, 0, 130, 12),
        redis_pane_core::render::layout::Pane::Keys,
    )
    .keys
    .width;

    let mut gaps = Vec::new();
    for x in 1..keys_pane_width {
        match frame.cell((x, y)) {
            Some(cell) if cell.style().bg == selected_bg => {}
            other => gaps.push((x, other.map(|c| c.style().bg))),
        }
    }
    assert!(gaps.is_empty(), "the selection bar has gaps: {gaps:?}");
}

#[test]
fn an_unselected_row_carries_no_background_at_all() {
    // The selection bar must not bleed into neighbouring rows.
    let state = many_keys();
    let theme = Theme::new(ColorDepth::TrueColor);
    let frame = render::frame(&state, &theme, &CLOCK, Rect::new(0, 0, 130, 12));
    let y = row_of(&frame, "user:8812:profile"); // row 1, not selected

    let keys_pane_width = redis_pane_core::render::layout::layout(
        Rect::new(0, 0, 130, 12),
        redis_pane_core::render::layout::Pane::Keys,
    )
    .keys
    .width;
    for x in 1..keys_pane_width {
        let bg = frame.cell((x, y)).map(|c| c.style().bg);
        assert!(
            matches!(bg, Some(None) | Some(Some(ratatui::style::Color::Reset))),
            "row 1 should carry no background, found {bg:?} at column {x}"
        );
    }
}

#[test]
fn distinct_types_render_with_distinct_dot_colours_in_a_real_frame() {
    // many_keys(): cart(zset) profile(json) session(hash) session(hash)
    // cart(zset) hot(list) — enough variety to prove the dots are not all one
    // colour, without hard-coding the exact hue table here. Row 0 (cart) is
    // selected, so its dot is overridden to the selection colour; the rest
    // show their real per-type hue.
    let state = many_keys();
    let theme = Theme::new(ColorDepth::TrueColor);
    let frame = render::frame(&state, &theme, &CLOCK, Rect::new(0, 0, 130, 12));

    let rows = [
        "user:8812:profile",
        "user:8812:session",
        "cart:91af3c9d2e",
        "feed:global:hot",
    ];
    let dot_colors: Vec<_> = rows
        .iter()
        .map(|needle| {
            let y = row_of(&frame, needle);
            frame.cell((1, y)).map(|c| c.style().fg)
        })
        .collect();
    let unique: std::collections::HashSet<_> = dot_colors.iter().collect();
    assert!(
        unique.len() > 1,
        "every row's dot rendered the same colour: {dot_colors:?}"
    );
}

// ── severity-3 #8: stack navigation below 70 columns ────────────────────────

use redis_pane_core::render::layout::Pane;

#[test]
fn golden_single_pane_value_view_60_cols() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.focus = Pane::Value;
    assert_golden("single_pane_value_60", &draw(&state, 60, 24));
}

#[test]
fn the_default_single_pane_state_still_renders_the_key_list_unchanged() {
    // Regression guard: adding Value mode must not disturb the existing,
    // already-shipped Keys mode at the same width.
    let state = many_keys();
    assert_eq!(state.focus, Pane::Keys);
    let frame = draw(&state, 60, 24);
    assert!(
        frame.contains("KEY"),
        "the flat/tree list header must still be there"
    );
}

#[test]
fn standalone_value_view_has_no_stray_border_character_at_the_left_edge() {
    // The two-pane layouts draw a "│" one column left of the value pane; at
    // full width there is no adjacent pane to separate from, and that
    // character must not appear over the content instead.
    let mut state = opened("k", hash_value(), 600);
    state.focus = Pane::Value;
    let frame = draw(&state, 60, 24);
    for line in frame.lines() {
        assert!(
            !line.starts_with('│'),
            "stray separator at the left edge: {line:?}"
        );
    }
}

#[test]
fn the_breadcrumb_names_the_key_and_the_effective_back_binding() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.focus = Pane::Value;
    let frame = draw(&state, 60, 24);
    assert!(frame.contains("back"), "{frame}");
    assert!(frame.contains("user:8812:session"), "{frame}");

    // R7.5: the breadcrumb must follow a rebinding, not a hard-coded "Esc".
    let mut keymap = redis_pane_core::keymap::Keymap::default();
    keymap.bind(
        redis_pane_core::keymap::Action::Cancel,
        KeyPress::plain(KeyCode::Char('h')),
    );
    state.keymap = keymap;
    let rebound = draw(&state, 60, 24);
    assert!(rebound.contains("h back"), "{rebound}");
}

#[test]
fn a_key_opened_narrow_shows_its_real_value_not_a_placeholder() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.focus = Pane::Value;
    let frame = draw(&state, 60, 24);
    assert!(frame.contains("device"), "{frame}");
    assert!(frame.contains("ios/17.2"), "{frame}");
}

// ── severity-3 #7: the scan cap gets a persistent banner, not a status line ─

fn capped_state() -> State {
    let mut keys = LoadedSet::with_cap(3);
    for name in ["a:1", "a:2", "a:3", "a:4"] {
        keys.push(name.as_bytes());
    }
    assert!(
        keys.is_capped(),
        "the fixture must actually be capped, or this proves nothing"
    );
    let mut state = State {
        keys,
        scan: redis_pane_core::state::ScanState::Capped { at: 3 },
        link: up(Tk::Armed),
        ..base()
    };
    state.rebuild_list();
    state
}

#[test]
fn golden_capped_keyspace() {
    assert_golden("browser_capped", &draw(&capped_state(), 130, 14));
}

#[test]
fn the_banner_names_the_cap_and_what_to_do_about_it() {
    let frame = draw(&capped_state(), 130, 14);
    assert!(frame.contains('⚠'), "{frame}");
    assert!(frame.contains("3"), "{frame}");
    assert!(frame.contains("narrow the filter"), "{frame}");
}

#[test]
fn an_uncapped_keyspace_shows_no_banner_and_spends_no_row_on_it() {
    let state = many_keys();
    assert!(!state.keys.is_capped());
    let capped_frame = draw(&capped_state(), 130, 14);
    let plain_frame = draw(&state, 130, 14);
    assert!(!plain_frame.contains('⚠'), "{plain_frame}");
    // The uncapped list's column header must sit one row higher than the
    // capped one's — proof the banner row is genuinely not reserved when it
    // is not needed, not just left blank.
    let header_row = |f: &str| {
        f.lines()
            .position(|l| l.contains("KEY") && l.contains("TYPE"))
    };
    assert!(
        header_row(&plain_frame) < header_row(&capped_frame),
        "plain={:?} capped={:?}",
        header_row(&plain_frame),
        header_row(&capped_frame)
    );
}

#[test]
fn the_banner_survives_filtering_because_the_underlying_set_is_still_incomplete() {
    // Filtering narrows what is shown, not what was actually scanned — the
    // set stays capped, and hiding that fact behind a short match list would
    // be worse: a filtered "no results" would look identical to "we never
    // got that far."
    let mut state = capped_state();
    state.list.filter = "a:1".into();
    state.rebuild_list();
    assert!(state.keys.is_capped());
    let frame = draw(&state, 130, 14);
    assert!(frame.contains('⚠'), "{frame}");
    assert!(
        frame.contains('/'),
        "the filter line must still be there too: {frame}"
    );
}

#[test]
fn the_banner_is_not_displaced_by_a_copy_confirmation() {
    // The entire point: today's status-bar line can be overwritten by
    // anything else that wants that row for a few seconds. This one cannot.
    let mut state = capped_state();
    let (next, _) = update(
        state.clone(),
        Msg::Copied {
            label: "key",
            at_ms: 73_000,
        },
    );
    state = next;
    let frame = draw(&state, 130, 14);
    assert!(
        frame.contains('⚠'),
        "the cap banner must survive a transient notice: {frame}"
    );
    assert!(
        frame.contains("copied key"),
        "the notice itself should still show too: {frame}"
    );
}

#[test]
fn the_banner_coexists_with_the_filter_line_in_the_documented_order() {
    let mut state = capped_state();
    state.list.filter = "a:".into();
    state.rebuild_list();
    let frame = draw(&state, 130, 14);
    let lines: Vec<&str> = frame.lines().collect();
    let banner_row = lines.iter().position(|l| l.contains('⚠')).unwrap();
    let filter_row = lines.iter().position(|l| l.starts_with(" /")).unwrap();
    assert!(
        banner_row < filter_row,
        "the cap banner should sit above the filter line"
    );
}

// ── The Open key vs the Selected key (CONTEXT.md; UI task severity 1) ───────
//
// The reported defect: the cursor on one key, the value pane showing another,
// with nothing on screen relating the two. The value was never wrong — it was a
// live tracked read of a different key — so these fixtures are about identity,
// not freshness.

/// The Attached case is the quiet one: no wash, a solid divider, no chip. It is
/// pinned here so that "nothing happens when the panes agree" is a tested
/// property rather than an assumption.
#[test]
fn golden_viewer_attached_says_nothing_extra() {
    let state = opened("user:8812:session", hash_value(), 2_537);
    let frame = draw(&state, 130, 22);
    assert!(!frame.contains('┊'), "no dashed divider while attached");
    assert!(!frame.contains('⊘'), "no chip while attached");
    assert!(frame.contains('├'), "but the row is still tied to the pane");
}

/// The cursor moves off the Open key. This is the frame the bug report was
/// about, and it is the one that has to be unmistakable.
#[test]
fn golden_viewer_detached() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.view.selected = 0; // `user:8812:cart`, two rows above the Open key
    assert_golden("viewer_detached", &draw(&state, 130, 22));
}

/// Filtered out: the Open key has no row, so the keys pane has nothing to mark
/// and the Viewer has to carry the whole signal on its own.
#[test]
fn golden_viewer_detached_off_list() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.list.filter = "cart".into();
    state.rebuild_list();
    assert_golden("viewer_detached_off_list", &draw(&state, 130, 22));
}

/// Scrolled past: the Open key is real and has a row, but not one on screen.
/// The divider points the way rather than leaving it to be hunted for.
#[test]
fn golden_viewer_detached_scrolled_out_of_view() {
    let mut state = opened("user:8812:cart", hash_value(), 2_537);
    // Enough rows that the window cannot hold them all, which is the only way
    // the Open key can have a row and still not be on screen.
    for i in 0..40 {
        state.keys.push(format!("filler:{i:03}").as_bytes());
    }
    state.rebuild_list();
    state.view.selected = state.row_count() - 1;
    let frame = draw(&state, 130, 22);
    assert!(
        frame.contains('▲'),
        "the Open key is above the window:\n{frame}"
    );
    assert_golden("viewer_detached_scrolled_out", &frame);
}

/// The chip gives way before the key name does, following the title bar's rule:
/// the name is the pane's identity, the chip is a qualifier on it.
#[test]
fn the_chip_shortens_and_then_goes_rather_than_crowding_the_key_name() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.view.selected = 0;
    let wide = draw(&state, 130, 22);
    assert!(wide.contains("⊘ not the selected key"), "{wide}");

    let narrow = draw(&state, 80, 22);
    assert!(
        narrow.contains("user:8812:session"),
        "the key name always survives:\n{narrow}"
    );
    assert!(
        narrow.contains('⊘'),
        "and the chip is still stated:\n{narrow}"
    );
    assert!(
        !narrow.contains("⊘ not the selected key"),
        "but not at full length in a pane this narrow:\n{narrow}"
    );
}

/// The wash is hue and nothing else, so monochrome must lose it — and must
/// still say the same thing. This is the test that stops the loud treatment
/// from becoming the *only* treatment.
#[test]
fn detachment_survives_the_loss_of_colour() {
    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.view.selected = 0;
    let area = Rect::new(0, 0, 130, 22);

    // `put` writes `Style::reset()`, which leaves `bg` as `Some(Color::Reset)`.
    // That is the *absence* of a background, so testing `is_some()` would pass
    // on every cell in the frame and prove nothing.
    let washed_at = |buf: &ratatui::buffer::Buffer, x: u16, y: u16| {
        !matches!(
            buf.cell((x, y)).map(|c| c.style().bg),
            None | Some(None) | Some(Some(ratatui::style::Color::Reset))
        )
    };

    let color = render::frame(&state, &Theme::new(ColorDepth::TrueColor), &CLOCK, area);
    // The pane's own rows are exactly the ones the dashed divider runs down, so
    // the two signals are checked against each other rather than against a
    // hand-counted range of chrome rows.
    let pane_rows: Vec<u16> = render::to_text(&color)
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains('┊'))
        .map(|(y, _)| y as u16)
        .collect();
    assert!(pane_rows.len() > 10, "sanity: {pane_rows:?}");
    assert!(
        pane_rows.iter().all(|y| washed_at(&color, 100, *y)),
        "in colour the whole value pane is washed, empty rows included"
    );
    assert!(
        !washed_at(&color, 20, 4),
        "and the keys pane is not — the wash is a statement about one pane"
    );

    let mono = render::frame(&state, &Theme::new(ColorDepth::Monochrome), &CLOCK, area);
    assert!(
        (0..area.height).all(|y| !washed_at(&mono, 100, y)),
        "in monochrome there is no wash at all"
    );
    let text = render::to_text(&mono);
    assert!(text.contains('┊'), "the dashed divider carries it instead");
    assert!(
        text.contains("⊘ not the selected key"),
        "and so does the chip"
    );
}

/// Two marks in one list only work if one is obviously the junior partner. The
/// cursor keeps reverse video; the Open key's row gets underline, which is the
/// one modifier still free once the selection has taken the other.
#[test]
fn the_open_row_is_underlined_and_the_cursor_row_is_not_merely_that() {
    use ratatui::style::Modifier;

    let mut state = opened("user:8812:session", hash_value(), 2_537);
    state.view.selected = 0;
    let area = Rect::new(0, 0, 130, 22);
    let mono = render::frame(&state, &Theme::new(ColorDepth::Monochrome), &CLOCK, area);

    // The bare name also appears in the value pane's header, which is above the
    // list; the dot is what makes this a key *row*.
    let open_y = row_of(&mono, "● user:8812:session");
    let cursor_y = row_of(&mono, "● user:8812:cart");
    assert_ne!(open_y, cursor_y, "this fixture is only meaningful detached");

    let style_at = |x: u16, y: u16| {
        mono.cell((x, y))
            .map(|c| c.style())
            .expect("cell in bounds")
    };
    // Column 3 is inside the key name, past the leading space and the dot.
    assert!(
        style_at(3, open_y)
            .add_modifier
            .contains(Modifier::UNDERLINED),
        "the Open key's name is underlined"
    );
    assert!(
        style_at(3, cursor_y)
            .add_modifier
            .contains(Modifier::REVERSED),
        "the cursor's row keeps the full-bar highlight"
    );
    assert!(
        !style_at(3, cursor_y)
            .add_modifier
            .contains(Modifier::UNDERLINED),
        "and the two marks are not the same mark"
    );
}
