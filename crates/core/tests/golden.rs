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
use redis_pane_core::state::{Connection, Environment, Source, State};
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
        quitting: false,
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

#[test]
fn same_clock_reading_gives_an_identical_frame_whenever_it_is_rendered() {
    let first = render_at(ColorDepth::TrueColor, &FixedClock(74_000));
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let second = render_at(ColorDepth::TrueColor, &FixedClock(74_000));
    assert_eq!(
        first, second,
        "wall-clock time passed between these two renders; the frame must not notice"
    );
}

#[test]
fn a_different_clock_reading_does_change_the_frame() {
    // Without this, the test above would pass trivially for a frame that simply
    // ignores the clock.
    let at_14s = render_at(ColorDepth::TrueColor, &FixedClock(74_000));
    let at_5m = render_at(ColorDepth::TrueColor, &FixedClock(360_000));
    assert_ne!(
        at_14s, at_5m,
        "the read age is clock-derived and must move when the clock does"
    );
}
