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
