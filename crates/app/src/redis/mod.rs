//! The Redis shell: connection, capability probe, and tracking (PLAN M0.8–M0.10).
//!
//! The server floor is RESP3 and Redis 6.0 (ADR-0007). Liveness is gated by
//! *capability*, never by version — managed platforms refuse `CLIENT TRACKING`
//! independently of the version they report, so ElastiCache Serverless will
//! answer `unknown subcommand 'tracking'` on an otherwise-current server.
//!
//! Two re-arm invariants hold here, both verified against Redis 8.4.0 (ADR-0006):
//!
//! 1. Every Refetch re-arms, because tracking is consumed by the invalidation
//!    it produces.
//! 2. Every reconnect re-arms before anything claims to be live.
//!
//! The core enforces the *claim* side of both — [`redis_pane_core::State::liveness`]
//! cannot return `Live` without an arming having been reported. This module is
//! responsible for the other half: actually doing it.

use std::time::Duration;

use fred::interfaces::{ClientInterface, TrackingInterface};
use fred::prelude::*;
use fred::types::{InfoKind, RespVersion};
use redis_pane_core::server::{FLOOR, Version};

/// Why a connection could not be established or kept.
#[derive(Debug)]
pub enum ConnectError {
    /// The target could not be reached, or refused our credentials.
    Unreachable(String),
    /// Connected, but the server is below the floor (ADR-0007).
    BelowFloor { found: Version },
    /// Connected, but `INFO server` did not report a parseable version.
    UnknownVersion(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unreachable(why) => write!(f, "{why}"),
            ConnectError::BelowFloor { found } => write!(
                f,
                "server is Redis {found}; redis-pane needs {FLOOR} or newer.\n\
                 RESP3 is required and is not available before {FLOOR}."
            ),
            ConnectError::UnknownVersion(raw) => {
                write!(
                    f,
                    "could not read the server version (INFO server said {raw:?})"
                )
            }
        }
    }
}

/// What a successful connect established.
#[derive(Debug, Clone)]
pub struct Established {
    pub version: Version,
    /// The result of *attempting* `CLIENT TRACKING`, never an inference.
    pub tracking_supported: bool,
}

/// Connect over RESP3, check the floor, then probe for tracking.
pub async fn connect(url: &str) -> Result<(Client, Established), ConnectError> {
    let mut config =
        Config::from_url(url).map_err(|e| ConnectError::Unreachable(format!("{url}: {e}")))?;
    // RESP2 is not spoken at all (ADR-0007): one reply shape per command, and
    // one code path per Viewer.
    config.version = RespVersion::RESP3;

    let client = Builder::from_config(config)
        .build()
        .map_err(|e| ConnectError::Unreachable(e.to_string()))?;
    client
        .init()
        .await
        .map_err(|e| ConnectError::Unreachable(describe(&e)))?;

    let version = server_version(&client).await?;
    if !version.meets_floor() {
        return Err(ConnectError::BelowFloor { found: version });
    }

    let tracking_supported = probe_tracking(&client).await;
    Ok((
        client,
        Established {
            version,
            tracking_supported,
        },
    ))
}

async fn server_version(client: &Client) -> Result<Version, ConnectError> {
    let info: String = client
        .info(Some(InfoKind::Server))
        .await
        .map_err(|e| ConnectError::Unreachable(describe(&e)))?;
    info.lines()
        .find_map(|l| l.strip_prefix("redis_version:"))
        .and_then(Version::parse)
        .ok_or_else(|| {
            ConnectError::UnknownVersion(info.lines().take(3).collect::<Vec<_>>().join("; "))
        })
}

/// Ask the server whether it will track for us, by trying.
///
/// A version check is not a substitute: managed platforms disable `CLIENT`
/// subcommands on their own schedule, so the only reliable question is the one
/// the server answers (ADR-0007).
async fn probe_tracking(client: &Client) -> bool {
    // OPTIN, no prefixes, no broadcast: tracking applies only to reads we
    // explicitly arm, which is what scopes it to one key rather than to every
    // key a keyspace browse happens to touch.
    client
        .start_tracking(Vec::<String>::new(), false, true, false, false)
        .await
        .is_ok()
}

/// Read the open key, **re-arming tracking in the same breath**.
///
/// There is no sibling function that reads without arming, and there should
/// never be one: tracking is consumed by the invalidation it produces, so a
/// read that skipped arming would leave the Viewer dark while the header still
/// said live (ADR-0006).
pub async fn refetch_and_rearm(client: &Client, key: &str) -> Result<Option<String>, Error> {
    // `CLIENT CACHING YES` applies to the next read-only command on this
    // connection, which under OPTIN is what arms exactly this one read.
    //
    // Note: fred's `Options { caching: Some(true) }` looks like it should do
    // this, and it compiles — but in fred 10.1.0 that field is copied onto the
    // command struct and never read by the router, so nothing reaches the wire.
    // Using it yields a connection that reports tracking as enabled while
    // silently arming nothing: this project's characteristic bug wearing a
    // library's clothes. The explicit call is deliberate; do not "simplify" it.
    let _: () = client.client_caching(true).await?;
    client.get(key).await
}

/// Exponential backoff with a ceiling, so a long outage does not turn into a
/// long silence. The countdown is shown; a silent wait is a freeze wearing a
/// different name (ADR-0009).
pub fn backoff_for(attempt: u32) -> Duration {
    let ms = 250u64.saturating_mul(1 << attempt.min(6));
    Duration::from_millis(ms.min(8_000))
}

fn describe(e: &Error) -> String {
    match e.details().is_empty() {
        true => format!("{e}"),
        false => format!("{:?}: {}", e.kind(), e.details()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_settles_at_a_ceiling() {
        assert_eq!(backoff_for(0), Duration::from_millis(250));
        assert_eq!(backoff_for(1), Duration::from_millis(500));
        assert_eq!(backoff_for(3), Duration::from_millis(2_000));
        assert_eq!(backoff_for(20), Duration::from_millis(8_000));
    }

    #[test]
    fn the_below_floor_message_names_both_versions() {
        let msg = ConnectError::BelowFloor {
            found: Version::parse("5.0.14").unwrap(),
        }
        .to_string();
        assert!(msg.contains("5.0.14"), "{msg}");
        assert!(msg.contains("6.0.0"), "{msg}");
    }
}
