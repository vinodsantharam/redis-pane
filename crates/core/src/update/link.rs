//! Connection lifecycle: connect/reconnect, tracking arming, and the
//! server-reported facts (`ServerState`) that decide Read-only Mode's
//! `replica` reason (ADR-0006, ADR-0009).

use super::*;

pub(super) fn connected(
    mut state: State,
    version: String,
    tracking_supported: bool,
) -> (State, Vec<Command>) {
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
        refetch(&mut state)
    } else {
        Vec::new()
    };
    (state, commands)
}

pub(super) fn connection_lost(mut state: State) -> (State, Vec<Command>) {
    // No countdown: nothing has scheduled a retry yet, and the shell
    // may never schedule one. The header shows the Read age instead,
    // which is the fact ADR-0009 actually asks for in this state and
    // the only one that is true.
    //
    // Whatever read was in flight will never answer over this link,
    // so its loading indicator is cleared here rather than left to
    // read `⟳ fetching…` until the process exits.
    state.open_pending = None;
    state.link = Link::Reconnecting {
        attempt: 1,
        retry_in_ms: None,
    };
    (state, vec![Command::Reconnect { after_ms: 0 }])
}

pub(super) fn reconnect_scheduled(
    mut state: State,
    attempt: u32,
    retry_in_ms: u64,
) -> (State, Vec<Command>) {
    state.link = Link::Reconnecting {
        attempt,
        retry_in_ms: Some(retry_in_ms),
    };
    (
        state,
        vec![Command::Reconnect {
            after_ms: retry_in_ms,
        }],
    )
}

pub(super) fn tracking_armed(mut state: State) -> (State, Vec<Command>) {
    if let Link::Up { tracking, .. } = &mut state.link
        && *tracking != Tracking::Unsupported
    {
        *tracking = Tracking::Armed;
    }
    (state, Vec::new())
}

pub(super) fn invalidated(mut state: State) -> (State, Vec<Command>) {
    // The push consumed the arming. Refetching is what re-arms, and
    // there is only one command that can do it.
    if let Link::Up { tracking, .. } = &mut state.link
        && *tracking == Tracking::Armed
    {
        *tracking = Tracking::Consumed;
    }
    let commands = refetch(&mut state);
    (state, commands)
}

pub(super) fn server_state(
    mut state: State,
    read_only: Option<ReadOnlyReason>,
    condition: Option<crate::state::ServerCondition>,
) -> (State, Vec<Command>) {
    // A replica outranks an Environment guard: that reason cannot be
    // lifted, so claiming the weaker one would offer a toggle the
    // server will refuse anyway (ADR-0009). A user-imposed guard is
    // never taken away by the server changing its mind.
    state.read_only = match (read_only, state.read_only) {
        (Some(reason), _) => Some(reason),
        (None, Some(ReadOnlyReason::Replica)) => state
            .connection
            .environment
            .read_only_by_default()
            .then_some(ReadOnlyReason::Environment),
        (None, existing) => existing,
    };
    state.condition = condition;
    (state, Vec::new())
}

#[cfg(test)]
mod liveness_invariants {
    //! The two re-arm invariants from ADR-0006 and ADR-0009, asserted against
    //! the state machine rather than against a server. A network test can show
    //! that the shell *did* re-arm once; these show that the header *cannot*
    //! claim liveness without it, on any path.

    use super::*;
    use crate::state::Liveness;

    /// A key open in the Viewer, so a Refetch has something to read. With
    /// nothing open there is nothing to refetch, and no read is issued.
    fn viewing() -> State {
        State {
            open: Some(OpenKey::new(
                Some(0),
                "k".into(),
                crate::state::value::Value::Str(crate::state::value::StringValue::new("v", 40)),
                -1,
                1,
                0,
            )),
            ..State::default()
        }
    }

    fn connected(tracking_supported: bool) -> State {
        let (s, _) = update(
            viewing(),
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
            viewing(),
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));
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
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));

        let (rearmed, _) = update(back, Msg::TrackingArmed);
        assert_eq!(rearmed.liveness(), Liveness::Live);
    }

    /// ADR-0006: tracking is consumed by its own invalidation, so every
    /// invalidation must produce a Refetch — and Refetch is the only read path,
    /// so it always re-arms.
    #[test]
    fn an_invalidation_consumes_the_arming_and_forces_a_refetch() {
        let (after, cmds) = update(armed(), Msg::Invalidated);
        assert!(matches!(cmds.as_slice(), [Command::ReadKey { .. }]));

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
                retry_in_ms: Some(4_000)
            }
        );
        assert_eq!(cmds, vec![Command::Reconnect { after_ms: 4_000 }]);
    }

    /// With nothing open there is nothing to refetch: connecting issues no
    /// read, and spends no token superseding one that might be in flight.
    #[test]
    fn connecting_with_nothing_open_issues_no_read() {
        let (s, cmds) = update(
            State::default(),
            Msg::Connected {
                version: "8.4.0".into(),
                tracking_supported: true,
            },
        );
        assert!(cmds.is_empty(), "{cmds:?}");
        assert_eq!(s.read_token, ReadToken::default());
    }

    /// Review H3: whether a read arms travels on the command, decided from the
    /// same link state the header's liveness is derived from — so a shell can
    /// no longer hold a stale copy of the capability.
    #[test]
    fn every_read_arms_exactly_when_the_connection_tracks() {
        let up = |tracking| Link::Up {
            version: "8.4.0".into(),
            tracking,
        };
        let cases = [
            (up(Tracking::Available), true),
            (up(Tracking::Armed), true),
            (up(Tracking::Consumed), true),
            (up(Tracking::Unsupported), false),
            (
                Link::Reconnecting {
                    attempt: 1,
                    retry_in_ms: None,
                },
                false,
            ),
            (Link::Connecting, false),
        ];
        for (link, arm) in cases {
            // A Refetch of the Open key…
            let state = State {
                link: link.clone(),
                ..viewing()
            };
            let (_, cmds) = update(state, Msg::Invalidated);
            assert!(
                matches!(cmds.as_slice(), [Command::ReadKey { arm: a, .. }] if *a == arm),
                "refetch under {link:?}: {cmds:?}"
            );
            // …and opening a key from the list.
            let mut state = State {
                cols: 130,
                rows: 40,
                link: link.clone(),
                ..State::default()
            };
            state.keys.push(b"other");
            state.rebuild_list();
            let (_, cmds) = update(state, Msg::Key(KeyPress::plain(KeyCode::Right)));
            assert!(
                matches!(cmds.as_slice(), [Command::ReadKey { arm: a, .. }] if *a == arm),
                "open under {link:?}: {cmds:?}"
            );
        }
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
                    retry_in_ms: None,
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
