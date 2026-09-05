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

pub mod read;
pub mod scan;

use std::time::Duration;

use fred::interfaces::{ClientInterface, TrackingInterface};
use fred::prelude::*;
use fred::types::config::TlsConnector;
use fred::types::{InfoKind, RespVersion};
use redis_pane_core::msg::MetadataEntry;
use redis_pane_core::resolve::Credentials;
use redis_pane_core::server::{FLOOR, Version};
use redis_pane_core::state::{ReadOnlyReason, ServerCondition};

/// Why a connection could not be established or kept.
#[derive(Debug)]
pub enum ConnectError {
    /// The target could not be reached, or refused our credentials.
    Unreachable(String),
    /// Connected, but the server is below the floor (ADR-0007).
    BelowFloor { found: Version },
    /// The server rejected `HELLO 3`, so it does not speak RESP3 at all.
    ///
    /// In practice this, not [`ConnectError::BelowFloor`], is how an old server
    /// is caught: `HELLO` arrived *in* Redis 6.0, so protocol negotiation
    /// already excludes everything under the floor. The version check remains
    /// as defence in depth for a server that speaks RESP3 and still reports an
    /// older version, but this is the variant a user on Redis 5 actually sees —
    /// which is why it must not leak the raw protocol error.
    NoResp3 { detail: String },
    /// Connected, but `INFO server` did not report a parseable version.
    UnknownVersion(String),
    /// A password reference could not be resolved (ADR-0002).
    Credentials(String),
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
            ConnectError::NoResp3 { detail } => write!(
                f,
                "server rejected RESP3 (HELLO 3), so it predates Redis {FLOOR}.\n\
                 redis-pane speaks RESP3 only and needs {FLOOR} or newer.\n\
                 The server said: {detail}"
            ),
            ConnectError::Credentials(why) => write!(f, "{why}"),
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
    /// Set when the server reported `role:slave`. Read-only Mode goes on with
    /// reason `replica`, and `⌃R` cannot lift it (R1.15, ADR-0009).
    pub read_only: Option<ReadOnlyReason>,
    /// A condition currently rejecting writes.
    pub condition: Option<ServerCondition>,
}

/// Connect over RESP3, check the floor, then probe for tracking.
pub async fn connect(url: &str) -> Result<(Client, Established), ConnectError> {
    connect_with(url, &Credentials::default()).await
}

/// Connect, authenticating with the credentials a Profile or the environment
/// supplied.
///
/// Without this, a Profile's `passwordEnv` is parsed, validated, and then
/// dropped — which makes the whole config file work on localhost and nowhere
/// else (ADR-0002).
pub async fn connect_with(
    url: &str,
    credentials: &Credentials,
) -> Result<(Client, Established), ConnectError> {
    // Redacted, because this string is printed. A URL that fails to parse is
    // exactly the one someone pasted by hand with a real password in it, and
    // the next thing they do with a startup diagnostic is paste it into a bug
    // report. Everywhere else the dial URL and the displayable target are kept
    // deliberately apart; this was the one place they met.
    let mut config = Config::from_url(url).map_err(|e| {
        ConnectError::Unreachable(format!("{}: {e}", redis_pane_core::resolve::redact(url)))
    })?;

    // A Profile's credentials are the more specific statement of intent, so
    // they win over anything embedded in the URL.
    let password = crate::secret::resolve(&credentials.password)
        .map_err(|e| ConnectError::Credentials(e.to_string()))?;
    if password.is_some() {
        config.password = password;
    }
    if credentials.username.is_some() {
        config.username = credentials.username.clone();
    }
    // Managed Redis is TLS-only in practice — Upstash, Redis Cloud, Azure, and
    // ElastiCache in transit-encryption mode all refuse plaintext. A `rediss://`
    // URL already sets this; a Profile's `tls: true` is the other way to ask.
    if credentials.tls && config.tls.is_none() {
        config.tls = Some(
            TlsConnector::default_rustls()
                .map_err(|e| ConnectError::Unreachable(format!("TLS unavailable: {e}")))?
                .into(),
        );
    }
    // RESP2 is not spoken at all (ADR-0007): one reply shape per command, and
    // one code path per Viewer.
    config.version = RespVersion::RESP3;

    let client = Builder::from_config(config)
        .build()
        .map_err(|e| ConnectError::Unreachable(e.to_string()))?;
    client.init().await.map_err(|e| {
        let detail = describe(&e);
        // Turn an unhelpful protocol error into the diagnostic the user needs.
        if rejected_hello(&detail) {
            ConnectError::NoResp3 { detail }
        } else {
            ConnectError::Unreachable(detail)
        }
    })?;

    let version = server_version(&client).await?;
    if !version.meets_floor() {
        return Err(ConnectError::BelowFloor { found: version });
    }

    let tracking_supported = probe_tracking(&client).await;
    let (read_only, condition) = server_conditions(&client).await;
    Ok((
        client,
        Established {
            version,
            tracking_supported,
            read_only,
            condition,
        },
    ))
}

/// Detect the conditions that will reject writes (R1.15, ADR-0009).
///
/// Detected rather than merely reported, so danger is visible *before* it is
/// possible. Learning that a server is a replica by having a write rejected is
/// exactly the ordering DESIGN principle 5 forbids.
pub async fn server_conditions(
    client: &Client,
) -> (Option<ReadOnlyReason>, Option<ServerCondition>) {
    let info: String = client
        .info(Some(InfoKind::Default))
        .await
        .unwrap_or_default();

    let field = |name: &str| -> Option<String> {
        info.lines()
            .find_map(|l| l.strip_prefix(name))
            .map(|v| v.trim().to_string())
    };

    let read_only = match field("role:").as_deref() {
        Some("slave") | Some("replica") => Some(ReadOnlyReason::Replica),
        _ => None,
    };

    // A failing background save makes Redis refuse writes with -MISCONF, and it
    // stays broken until someone intervenes — worth a banner, not a surprise.
    let misconf = matches!(field("rdb_last_bgsave_status:").as_deref(), Some("err"));
    let used: u64 = field("used_memory:")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let max: u64 = field("maxmemory:")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let condition = if misconf {
        Some(ServerCondition::Misconf)
    } else if max > 0 && used >= max {
        Some(ServerCondition::Oom)
    } else if field("loading:").as_deref() == Some("1") {
        Some(ServerCondition::Loading { percent: 0 })
    } else {
        None
    };

    (read_only, condition)
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
pub async fn probe_tracking_public(client: &Client) -> bool {
    probe_tracking(client).await
}

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

/// Whether a connect failure was the server refusing `HELLO`.
fn rejected_hello(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("unknown command") && lower.contains("hello")
}

fn describe(e: &Error) -> String {
    match e.details().is_empty() {
        true => format!("{e}"),
        false => format!("{:?}: {}", e.kind(), e.details()),
    }
}

/// Fetch type, TTL and memory usage for a window of keys (R2.4, PLAN M1.4).
///
/// Three commands per key, pipelined so the whole window costs one round trip
/// rather than `3 × window`. Only ever the visible rows: fetching metadata for
/// a whole keyspace would be `KEYS *` with extra steps, and it would compete
/// with `SCAN` for the connection while the list is still filling.
///
/// A key that vanished between the scan and this fetch is reported as gone
/// rather than failing the batch — the keyspace moves while we walk it. That
/// second return value is free deletion detection: `TYPE` is already on the
/// wire for every visible row, so noticing costs no extra round trip and needs
/// no tracking table (DESIGN §9).
pub async fn fetch_metadata(
    client: &Client,
    keys: &[(usize, Vec<u8>)],
) -> Result<(Vec<MetadataEntry>, Vec<usize>), Error> {
    use redis_pane_core::state::KeyKind;

    let pipeline = client.pipeline();
    for (_, name) in keys {
        let key: Key = name.as_slice().into();
        let _: () = pipeline.r#type(key.clone()).await?;
        let _: () = pipeline.ttl(key.clone()).await?;
        let _: () = pipeline.memory_usage(key, None).await?;
    }
    let replies: Vec<Value> = pipeline.all().await?;

    let mut out = Vec::with_capacity(keys.len());
    let mut gone = Vec::new();
    for (slot, (index, _)) in keys.iter().enumerate() {
        let kind = replies.get(slot * 3).and_then(|v| v.as_str());
        let ttl = replies.get(slot * 3 + 1).and_then(|v| v.as_i64());
        let size = replies.get(slot * 3 + 2).and_then(|v| v.as_i64());

        // TYPE answers "none" for a key that no longer exists. A reply that is
        // absent entirely is a different thing — a short pipeline, not a
        // deleted key — and must not badge the row: one truncated reply would
        // otherwise mark every remaining row in the window as deleted.
        let Some(kind) = kind else { continue };
        if kind == "none" {
            gone.push(*index);
            continue;
        }
        out.push(MetadataEntry {
            index: *index,
            kind: KeyKind::from_redis(&kind),
            // Redis returns -1 for no expiry and -2 for a missing key; both map
            // to "no expiry" here, and the missing case was filtered above.
            ttl_seconds: ttl.unwrap_or(-1).max(-1) as i32,
            size_bytes: size.unwrap_or(0).clamp(0, u32::MAX as i64 - 1) as u32,
        });
    }
    Ok((out, gone))
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
    fn a_rejected_hello_is_recognised_as_an_old_server() {
        assert!(rejected_hello(
            "Unknown: ERR unknown command `HELLO`, with args beginning with: `3`"
        ));
        assert!(!rejected_hello("IO: Connection refused"));
        assert!(!rejected_hello(
            "Auth: WRONGPASS invalid username-password pair"
        ));
    }

    #[test]
    fn the_no_resp3_message_explains_rather_than_leaking_the_protocol_error() {
        let msg = ConnectError::NoResp3 {
            detail: "ERR unknown command `HELLO`".into(),
        }
        .to_string();
        assert!(msg.contains("predates Redis 6.0.0"), "{msg}");
        assert!(msg.contains("RESP3"), "{msg}");
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
