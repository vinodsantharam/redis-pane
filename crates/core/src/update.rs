//! The single entry point into the core (PLAN M0.4).

use crate::keymap::Action;
use crate::msg::KeyPress;
use crate::state::{Link, ReadOnlyReason, ScanState, Tracking};
use crate::{Command, Msg, State};

/// Takes a message, returns new state plus commands for a shell to execute.
///
/// Pure: no I/O, no clock, no randomness. Time arrives inside the message
/// (see [`Msg::ReadCompleted`]) rather than being read here, which is what
/// keeps a frame a function of state alone (ADR-0011).
pub fn update(mut state: State, msg: Msg) -> (State, Vec<Command>) {
    match msg {
        Msg::Key(key) => key_press(state, key),
        Msg::Resized { cols, rows } => {
            state.cols = cols;
            state.rows = rows;
            (state, Vec::new())
        }
        Msg::ReadCompleted { at_ms } => {
            state.last_read_ms = Some(at_ms);
            (state, Vec::new())
        }
        Msg::Connected {
            version,
            tracking_supported,
        } => {
            // Connected is never live on its own. Tracking starts Available at
            // best, and only an accepted arming moves it to Armed — which is
            // ADR-0009's invariant expressed as a state transition rather than
            // as a comment somebody has to remember.
            state.link = Link::Up {
                version,
                tracking: if tracking_supported {
                    Tracking::Available
                } else {
                    Tracking::Unsupported
                },
            };
            let commands = if tracking_supported {
                vec![Command::RefetchOpenKey]
            } else {
                Vec::new()
            };
            (state, commands)
        }
        Msg::ConnectionLost => {
            state.link = Link::Reconnecting {
                attempt: 1,
                retry_in_ms: 0,
            };
            (state, vec![Command::Reconnect { after_ms: 0 }])
        }
        Msg::ReconnectScheduled {
            attempt,
            retry_in_ms,
        } => {
            state.link = Link::Reconnecting {
                attempt,
                retry_in_ms,
            };
            (
                state,
                vec![Command::Reconnect {
                    after_ms: retry_in_ms,
                }],
            )
        }
        Msg::TrackingArmed => {
            if let Link::Up { tracking, .. } = &mut state.link
                && *tracking != Tracking::Unsupported
            {
                *tracking = Tracking::Armed;
            }
            (state, Vec::new())
        }
        Msg::Invalidated => {
            // The push consumed the arming. Refetching is what re-arms, and
            // there is only one command that can do it.
            if let Link::Up { tracking, .. } = &mut state.link
                && *tracking == Tracking::Armed
            {
                *tracking = Tracking::Consumed;
            }
            (state, vec![Command::RefetchOpenKey])
        }
        Msg::ScanStarted { estimated_total } => {
            state.keys.clear();
            state.scan = ScanState::Running {
                scanned: 0,
                estimated_total,
            };
            (state, Vec::new())
        }
        Msg::ScanBatch { keys } => scan_batch(state, keys),
        Msg::ScanComplete => {
            // A cap reached mid-scan already told its own story; completing
            // afterwards must not overwrite it with a smaller truth.
            if !matches!(state.scan, ScanState::Capped { .. }) {
                state.scan = ScanState::Complete {
                    total: state.keys.len() as u64,
                };
            }
            (state, Vec::new())
        }
        Msg::ScanCancelled => {
            if !matches!(state.scan, ScanState::Capped { .. }) {
                state.scan = ScanState::Cancelled {
                    scanned: state.keys.len() as u64,
                };
            }
            (state, Vec::new())
        }
        Msg::ScanFailed { error } => {
            state.scan = ScanState::Failed { error };
            (state, Vec::new())
        }
        Msg::Quit => quit(state),
    }
}

/// Absorb a page of keys, stopping the traversal if the cap is reached.
///
/// This is the single place the cap is enforced, so there is no path that grows
/// the Loaded set past it (ADR-0010).
fn scan_batch(mut state: State, keys: Vec<Vec<u8>>) -> (State, Vec<Command>) {
    for key in keys {
        if !state.keys.push(&key) {
            state.scan = ScanState::Capped {
                at: state.keys.len(),
            };
            return (state, vec![Command::CancelScan]);
        }
    }
    if let ScanState::Running {
        estimated_total, ..
    } = state.scan
    {
        state.scan = ScanState::Running {
            scanned: state.keys.len() as u64,
            estimated_total,
        };
    }
    (state, Vec::new())
}

/// Resolve a keypress through the keymap, never against hard-coded keys.
///
/// The hint bar and help overlay read the same map, so what is shown is always
/// the effective binding after user overrides (R7.5).
fn key_press(mut state: State, key: KeyPress) -> (State, Vec<Command>) {
    let Some(action) = state.keymap.action_for(&key) else {
        return (state, Vec::new());
    };
    match action {
        Action::Quit => quit(state),
        Action::Help => {
            state.help_open = !state.help_open;
            (state, Vec::new())
        }
        Action::Cancel => {
            if state.help_open {
                state.help_open = false;
                return (state, Vec::new());
            }
            // Every in-flight operation is cancellable (PRD R7.3).
            if state.scan.is_running() {
                return (state, vec![Command::CancelScan]);
            }
            (state, Vec::new())
        }
        Action::Refetch => (state, vec![Command::RefetchOpenKey]),
        Action::ToggleReadOnly => {
            // A replica will refuse writes whatever we believe, so this is not
            // a toggle the user gets to win (ADR-0009).
            match state.read_only {
                Some(ReadOnlyReason::Replica) => {}
                Some(_) => state.read_only = None,
                None => state.read_only = Some(ReadOnlyReason::User),
            }
            (state, Vec::new())
        }
    }
}

fn quit(mut state: State) -> (State, Vec<Command>) {
    state.quitting = true;
    (state, vec![Command::Quit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::KeyCode;
    use crate::state::{Environment, Source};

    #[test]
    fn resize_updates_dimensions_and_emits_nothing() {
        let (s, cmds) = update(
            State::default(),
            Msg::Resized {
                cols: 118,
                rows: 29,
            },
        );
        assert_eq!((s.cols, s.rows), (118, 29));
        assert!(cmds.is_empty());
    }

    #[test]
    fn quit_sets_the_flag_and_asks_the_shell_to_exit() {
        let (s, cmds) = update(State::default(), Msg::Quit);
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn read_completion_records_the_clock_reading_it_was_given() {
        let (s, _) = update(State::default(), Msg::ReadCompleted { at_ms: 9_000 });
        assert_eq!(s.last_read_ms, Some(9_000));
    }

    #[test]
    fn update_is_pure_same_input_same_output() {
        let a = update(State::default(), Msg::Resized { cols: 80, rows: 24 });
        let b = update(State::default(), Msg::Resized { cols: 80, rows: 24 });
        assert_eq!(a, b);
    }

    // ── M0.5: synthetic events drive the core with no terminal attached ─────

    #[test]
    fn q_quits() {
        let (s, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::plain(KeyCode::Char('q'))),
        );
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn ctrl_c_quits() {
        let (s, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('c'))),
        );
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn ctrl_q_is_not_q() {
        let (s, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('q'))),
        );
        assert!(!s.quitting);
        assert!(cmds.is_empty());
    }

    #[test]
    fn an_unbound_key_changes_nothing() {
        let before = State::default();
        let (after, cmds) = update(
            before.clone(),
            Msg::Key(KeyPress::plain(KeyCode::Char('z'))),
        );
        assert_eq!(before, after);
        assert!(cmds.is_empty());
    }

    #[test]
    fn question_mark_toggles_the_help_overlay_and_esc_closes_it() {
        let (s, _) = update(
            State::default(),
            Msg::Key(KeyPress::plain(KeyCode::Char('?'))),
        );
        assert!(s.help_open);
        let (s, _) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!s.help_open);
    }

    #[test]
    fn r_asks_for_a_refetch_which_is_the_only_read_path() {
        let (_, cmds) = update(
            State::default(),
            Msg::Key(KeyPress::plain(KeyCode::Char('r'))),
        );
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);
    }

    #[test]
    fn ctrl_r_toggles_read_only_but_a_replica_cannot_be_lifted() {
        // Off -> user-imposed.
        let (s, _) = update(
            State::default(),
            Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))),
        );
        assert_eq!(s.read_only, Some(ReadOnlyReason::User));
        // And back off again.
        let (s, _) = update(s, Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))));
        assert_eq!(s.read_only, None);

        // A replica refuses to be lifted, because the server would refuse the
        // write regardless and a toggle that does nothing is a toggle that lies.
        let replica = State {
            read_only: Some(ReadOnlyReason::Replica),
            ..State::default()
        };
        let (s, _) = update(replica, Msg::Key(KeyPress::ctrl(KeyCode::Char('r'))));
        assert_eq!(s.read_only, Some(ReadOnlyReason::Replica));
    }

    #[test]
    fn a_rebound_key_takes_effect_in_update_not_just_in_the_hint() {
        let mut keymap = crate::keymap::Keymap::default();
        keymap.bind(Action::Quit, KeyPress::ctrl(KeyCode::Char('x')));
        let state = State {
            keymap,
            ..State::default()
        };
        let (s, cmds) = update(state, Msg::Key(KeyPress::ctrl(KeyCode::Char('x'))));
        assert!(s.quitting);
        assert_eq!(cmds, vec![Command::Quit]);
    }

    #[test]
    fn prod_and_unknown_are_read_only_by_default_local_and_staging_are_not() {
        assert!(Environment::Prod.read_only_by_default());
        assert!(Environment::Unknown.read_only_by_default());
        assert!(!Environment::Local.read_only_by_default());
        assert!(!Environment::Staging.read_only_by_default());
    }

    #[test]
    fn source_labels_name_where_the_target_came_from() {
        assert_eq!(Source::Default.label(), "from default");
        assert_eq!(
            Source::Profile("staging".into()).label(),
            "from profile staging"
        );
    }
}

#[cfg(test)]
mod liveness_invariants {
    //! The two re-arm invariants from ADR-0006 and ADR-0009, asserted against
    //! the state machine rather than against a server. A network test can show
    //! that the shell *did* re-arm once; these show that the header *cannot*
    //! claim liveness without it, on any path.

    use super::*;
    use crate::state::Liveness;

    fn connected(tracking_supported: bool) -> State {
        let (s, _) = update(
            State::default(),
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported,
            },
        );
        s
    }

    fn armed() -> State {
        let (s, _) = update(connected(true), Msg::TrackingArmed);
        s
    }

    #[test]
    fn a_fresh_connection_is_not_live_until_tracking_is_armed() {
        let s = connected(true);
        assert_eq!(
            s.liveness(),
            Liveness::Manual,
            "connected is not the same as live"
        );
        assert_eq!(armed().liveness(), Liveness::Live);
    }

    #[test]
    fn connecting_emits_a_refetch_which_is_the_only_thing_that_arms() {
        let (_, cmds) = update(
            State::default(),
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);
    }

    /// ADR-0009: a reconnect that does not re-arm must not present as live.
    #[test]
    fn a_reconnect_drops_liveness_and_does_not_get_it_back_for_free() {
        let live = armed();
        assert_eq!(live.liveness(), Liveness::Live);

        let (dropped, _) = update(live, Msg::ConnectionLost);
        assert_eq!(dropped.liveness(), Liveness::Disconnected);

        // The server comes back. Tracking does not come back with it: a fresh
        // connection tracks nothing, which the spike confirmed directly.
        let (back, cmds) = update(
            dropped,
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert_ne!(
            back.liveness(),
            Liveness::Live,
            "reconnected but not re-armed"
        );
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);

        let (rearmed, _) = update(back, Msg::TrackingArmed);
        assert_eq!(rearmed.liveness(), Liveness::Live);
    }

    /// ADR-0006: tracking is consumed by its own invalidation, so every
    /// invalidation must produce a Refetch — and Refetch is the only read path,
    /// so it always re-arms.
    #[test]
    fn an_invalidation_consumes_the_arming_and_forces_a_refetch() {
        let (after, cmds) = update(armed(), Msg::Invalidated);
        assert_eq!(cmds, vec![Command::RefetchOpenKey]);

        let (rearmed, _) = update(after, Msg::TrackingArmed);
        assert_eq!(rearmed.liveness(), Liveness::Live);
    }

    #[test]
    fn a_server_that_refuses_tracking_degrades_to_manual_and_never_claims_live() {
        let s = connected(false);
        assert_eq!(s.liveness(), Liveness::Manual);

        // Even an erroneous TrackingArmed cannot promote an unsupported server.
        let (s, _) = update(s, Msg::TrackingArmed);
        assert_eq!(s.liveness(), Liveness::Manual);
    }

    #[test]
    fn liveness_is_derived_and_disconnected_always_wins() {
        // Whatever the tracking state was, losing the link shows disconnected.
        for start in [connected(true), armed(), connected(false)] {
            let (s, _) = update(start, Msg::ConnectionLost);
            assert_eq!(s.liveness(), Liveness::Disconnected);
        }
    }

    #[test]
    fn backoff_is_scheduled_visibly_rather_than_waited_out_silently() {
        let (s, cmds) = update(armed(), Msg::ConnectionLost);
        assert!(matches!(s.link, Link::Reconnecting { .. }));
        assert_eq!(cmds, vec![Command::Reconnect { after_ms: 0 }]);

        let (s, cmds) = update(
            s,
            Msg::ReconnectScheduled {
                attempt: 3,
                retry_in_ms: 4_000,
            },
        );
        assert_eq!(
            s.link,
            Link::Reconnecting {
                attempt: 3,
                retry_in_ms: 4_000
            }
        );
        assert_eq!(cmds, vec![Command::Reconnect { after_ms: 4_000 }]);
    }

    /// The exhaustive statement of the rule: across every reachable link and
    /// tracking combination, `live` appears only where arming has happened.
    #[test]
    fn live_appears_only_where_tracking_is_armed_or_mid_refetch() {
        let cases = [
            (Link::Connecting, Liveness::Disconnected),
            (
                Link::Reconnecting {
                    attempt: 1,
                    retry_in_ms: 0,
                },
                Liveness::Disconnected,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Unsupported,
                },
                Liveness::Manual,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Available,
                },
                Liveness::Manual,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Armed,
                },
                Liveness::Live,
            ),
            (
                Link::Up {
                    version: "8.4.0".into(),
                    tracking: Tracking::Consumed,
                },
                Liveness::Live,
            ),
        ];
        for (link, expected) in cases {
            let state = State {
                link: link.clone(),
                ..State::default()
            };
            assert_eq!(state.liveness(), expected, "for {link:?}");
        }
    }
}

#[cfg(test)]
mod scan_tests {
    //! The keyspace traversal, as seen by the core: batches arrive, the cap is
    //! enforced in one place, and cancellation is always available.

    use super::*;
    use crate::msg::KeyCode;
    use crate::state::LoadedSet;

    fn started(estimated_total: u64) -> State {
        let (s, _) = update(State::default(), Msg::ScanStarted { estimated_total });
        s
    }

    fn batch(n: usize, from: usize) -> Vec<Vec<u8>> {
        (from..from + n)
            .map(|i| format!("k:{i}").into_bytes())
            .collect()
    }

    #[test]
    fn a_scan_starts_empty_and_reports_progress_against_an_estimate() {
        let s = started(180_000);
        assert!(s.keys.is_empty());
        assert_eq!(
            s.scan,
            ScanState::Running {
                scanned: 0,
                estimated_total: 180_000
            }
        );

        let (s, cmds) = update(
            s,
            Msg::ScanBatch {
                keys: batch(500, 0),
            },
        );
        assert!(cmds.is_empty());
        assert_eq!(s.keys.len(), 500);
        assert_eq!(s.scan.readout(), "scanning 500 of ~180,000");
    }

    #[test]
    fn batches_accumulate_so_results_render_as_they_arrive() {
        let mut s = started(1_000);
        for i in 0..4 {
            (s, _) = update(
                s,
                Msg::ScanBatch {
                    keys: batch(250, i * 250),
                },
            );
        }
        assert_eq!(s.keys.len(), 1_000);
        assert_eq!(s.keys.name_str(0).unwrap(), "k:0");
        assert_eq!(s.keys.name_str(999).unwrap(), "k:999");
    }

    #[test]
    fn starting_a_new_scan_discards_the_previous_one() {
        let (s, _) = update(started(10), Msg::ScanBatch { keys: batch(5, 0) });
        assert_eq!(s.keys.len(), 5);
        let (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 10,
            },
        );
        assert!(
            s.keys.is_empty(),
            "a filter change must not append to stale results"
        );
    }

    #[test]
    fn reaching_the_cap_stops_the_scan_and_says_so() {
        let mut s = State {
            keys: LoadedSet::with_cap(600),
            ..State::default()
        };
        (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 10_000,
            },
        );
        let (s, cmds) = update(
            s,
            Msg::ScanBatch {
                keys: batch(1_000, 0),
            },
        );

        assert_eq!(
            cmds,
            vec![Command::CancelScan],
            "the shell must be told to stop"
        );
        assert_eq!(s.keys.len(), 600);
        assert!(s.keys.is_capped());
        assert_eq!(s.scan, ScanState::Capped { at: 600 });
        assert!(s.scan.readout().contains("narrow the filter"));
    }

    #[test]
    fn a_completion_arriving_after_the_cap_does_not_erase_it() {
        // The shell's CancelScan will be answered by ScanComplete or
        // ScanCancelled. Neither may overwrite the more important fact.
        let mut s = State {
            keys: LoadedSet::with_cap(10),
            ..State::default()
        };
        (s, _) = update(
            s,
            Msg::ScanStarted {
                estimated_total: 100,
            },
        );
        (s, _) = update(s, Msg::ScanBatch { keys: batch(50, 0) });
        assert!(matches!(s.scan, ScanState::Capped { .. }));

        let (s, _) = update(s, Msg::ScanComplete);
        assert!(
            matches!(s.scan, ScanState::Capped { .. }),
            "the cap is the story"
        );

        let mut s2 = State {
            keys: LoadedSet::with_cap(10),
            ..State::default()
        };
        (s2, _) = update(
            s2,
            Msg::ScanStarted {
                estimated_total: 100,
            },
        );
        (s2, _) = update(s2, Msg::ScanBatch { keys: batch(50, 0) });
        let (s2, _) = update(s2, Msg::ScanCancelled);
        assert!(matches!(s2.scan, ScanState::Capped { .. }));
    }

    #[test]
    fn completing_normally_reports_the_total() {
        let (s, _) = update(
            started(700),
            Msg::ScanBatch {
                keys: batch(700, 0),
            },
        );
        let (s, _) = update(s, Msg::ScanComplete);
        assert_eq!(s.scan, ScanState::Complete { total: 700 });
        assert_eq!(s.scan.readout(), "700 keys");
    }

    #[test]
    fn esc_cancels_an_in_flight_scan() {
        let s = started(10_000);
        let (_, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert_eq!(cmds, vec![Command::CancelScan]);
    }

    #[test]
    fn esc_closes_help_first_and_leaves_the_scan_alone() {
        // Help is the nearer thing to back out of. Cancelling the scan as well
        // would make one keypress do two unrelated things.
        let mut s = started(10_000);
        s.help_open = true;
        let (s, cmds) = update(s, Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(!s.help_open);
        assert!(cmds.is_empty());
        assert!(s.scan.is_running());
    }

    #[test]
    fn esc_with_nothing_in_flight_does_nothing() {
        let (_, cmds) = update(State::default(), Msg::Key(KeyPress::plain(KeyCode::Esc)));
        assert!(cmds.is_empty());
    }

    #[test]
    fn a_failed_scan_keeps_what_it_had_and_names_the_error() {
        let (s, _) = update(started(100), Msg::ScanBatch { keys: batch(30, 0) });
        let (s, _) = update(
            s,
            Msg::ScanFailed {
                error: "LOADING".into(),
            },
        );
        assert_eq!(s.keys.len(), 30, "partial results are still worth showing");
        assert!(s.scan.readout().contains("LOADING"));
    }
}
