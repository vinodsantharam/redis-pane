//! Integration suite (ADR-0011, PLAN M0.8–M0.10).
//!
//! These are the tests only a real server can satisfy. They need Docker, so
//! they are `#[ignore]`d and the default `cargo test` run stays fast and
//! Docker-free:
//!
//! ```text
//! cargo test -p redis-pane -- --ignored --test-threads=1
//! ```
//!
//! The *claim* side of both re-arm invariants is proven in the core's unit
//! tests, where `State::liveness()` cannot return `Live` without an arming.
//! What these prove is the other half: that the shell actually arms, that a
//! reconnect really does lose tracking, and that the version floor and the
//! capability probe behave against servers that exist.

use std::time::Duration;

use fred::interfaces::ClientLike;
use fred::prelude::*;
use redis_pane_core::state::value::Viewer;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const REDIS_PORT: ContainerPort = ContainerPort::Tcp(6379);

async fn start(image: &str, tag: &str) -> (ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new(image, tag)
        .with_exposed_port(REDIS_PORT)
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .start()
        .await
        .expect("docker must be running for the integration suite");
    let port = container.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    let url = format!("redis://127.0.0.1:{port}");
    (container, url)
}

/// A server to test against without Docker: export `REDIS_PANE_TEST_URL`
/// pointing at a scratch instance. Useful locally, and the only option when the
/// Docker daemon is not running.
fn env_url() -> Option<String> {
    std::env::var("REDIS_PANE_TEST_URL")
        .ok()
        .filter(|s| !s.is_empty())
}

// ── M0.8 — the version floor and the capability probe ───────────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn connects_to_redis_6_2_and_tracking_is_available() {
    let (_c, url) = start("redis", "6.2-alpine").await;
    let (client, established) = redis_pane::redis::connect(&url).await.unwrap();
    assert_eq!(established.version.major, 6);
    assert!(established.version.meets_floor());
    assert!(
        established.tracking_supported,
        "Redis 6.2 supports CLIENT TRACKING"
    );
    let _ = client.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn connects_to_redis_7_and_tracking_is_available() {
    let (_c, url) = start("redis", "7-alpine").await;
    let (client, established) = redis_pane::redis::connect(&url).await.unwrap();
    assert_eq!(established.version.major, 7);
    assert!(established.tracking_supported);
    let _ = client.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_server_below_the_floor_is_refused_with_a_diagnostic_not_a_protocol_error() {
    // Redis 5 predates HELLO, so it is caught at RESP3 negotiation rather than
    // by the version check. That makes this message the one a real user on an
    // old server reads, so it must explain rather than leak
    // `ERR unknown command HELLO`.
    let (_c, url) = start("redis", "5-alpine").await;
    match redis_pane::redis::connect(&url).await {
        Err(err @ redis_pane::redis::ConnectError::NoResp3 { .. }) => {
            let msg = err.to_string();
            assert!(msg.contains("RESP3"), "{msg}");
            assert!(msg.contains("6.0.0"), "{msg}");
        }
        Err(redis_pane::redis::ConnectError::BelowFloor { found }) => {
            assert!(!found.meets_floor(), "found {found}");
        }
        Err(other) => panic!("refused, but with an unhelpful message: {other}"),
        Ok(_) => panic!("Redis 5 must not be accepted"),
    }
}

// ── M0.10 — the invariants, against a server that really moves ──────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn an_armed_key_receives_an_invalidation_and_the_arming_is_consumed() {
    let (_c, url) = start("redis", "7-alpine").await;
    let (client, est) = redis_pane::redis::connect(&url).await.unwrap();
    assert!(est.tracking_supported);

    let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(&client);

    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("k", "v1", None, None, false).await.unwrap();

    // Arm by reading through the one read path that always arms.
    let got: Option<String> = redis_pane::redis::refetch_and_rearm(&client, "k")
        .await
        .unwrap();
    assert_eq!(got.as_deref(), Some("v1"));

    let _: () = writer.set("k", "v2", None, None, false).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), invalidations.recv()).await;
    assert!(
        first.is_ok(),
        "an armed key must produce an invalidation push"
    );

    // The finding that changed ADR-0006: the push consumed the arming, so a
    // further write produces nothing until the key is read again.
    let _: () = writer.set("k", "v3", None, None, false).await.unwrap();
    let second = tokio::time::timeout(Duration::from_millis(800), invalidations.recv()).await;
    assert!(
        second.is_err(),
        "tracking is consumed by its own invalidation; a second push without \
         re-arming would mean ADR-0006's central finding is wrong"
    );

    // Re-arming brings it back, which is why Refetch is the only read path.
    let _: Option<String> = redis_pane::redis::refetch_and_rearm(&client, "k")
        .await
        .unwrap();
    let _: () = writer.set("k", "v4", None, None, false).await.unwrap();
    let third = tokio::time::timeout(Duration::from_secs(5), invalidations.recv()).await;
    assert!(third.is_ok(), "re-arming must restore liveness");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn an_unarmed_read_is_not_tracked_so_optin_really_scopes() {
    let (_c, url) = start("redis", "7-alpine").await;
    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(&client);

    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer
        .set("untracked", "v1", None, None, false)
        .await
        .unwrap();

    // A plain read, with no CLIENT CACHING YES in front of it.
    let _: Option<String> = client.get("untracked").await.unwrap();

    let _: () = writer
        .set("untracked", "v2", None, None, false)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_millis(800), invalidations.recv()).await;
    assert!(got.is_err(), "OPTIN must not track a read we did not arm");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_reconnect_loses_tracking_which_is_why_it_must_be_re_armed() {
    let (container, url) = start("redis", "7-alpine").await;
    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();

    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("k", "v1", None, None, false).await.unwrap();
    let _: Option<String> = redis_pane::redis::refetch_and_rearm(&client, "k")
        .await
        .unwrap();

    // Take the server away and bring it back. fred reconnects underneath us,
    // and the server on the other side remembers nothing about what we were
    // watching — which is precisely why ADR-0009 makes re-arming an invariant
    // rather than an optimisation.
    container.pause().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    container.unpause().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let fresh = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    fresh.init().await.unwrap();
    let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(&fresh);

    // This connection never armed anything, so nothing arrives for it.
    let _: () = writer.set("k", "v5", None, None, false).await.unwrap();
    let got = tokio::time::timeout(Duration::from_millis(800), invalidations.recv()).await;
    assert!(
        got.is_err(),
        "a connection that has not armed is not tracking"
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
    let _ = fresh.quit().await;
}

// ── Docker-free variant, for a scratch server named by REDIS_PANE_TEST_URL ──

/// The same invariant as `an_armed_key_receives_an_invalidation_and_the_arming_is_consumed`,
/// but against a server the developer supplies. This is the one that can be run
/// on a machine with no Docker daemon, and it exercises the identical code path.
#[tokio::test]
#[ignore = "needs REDIS_PANE_TEST_URL"]
async fn tracking_round_trip_when_a_server_url_is_supplied() {
    let Some(url) = env_url() else {
        // Nothing to assert against. Say so unmistakably: a green line that
        // proves nothing is worse than a missing one.
        eprintln!(
            "SKIPPED (no assertions ran): set REDIS_PANE_TEST_URL to exercise \
             tracking_round_trip_when_a_server_url_is_supplied"
        );
        return;
    };

    let (client, est) = redis_pane::redis::connect(&url).await.unwrap();
    assert!(est.version.meets_floor(), "server is {}", est.version);
    assert!(
        est.tracking_supported,
        "this server should accept CLIENT TRACKING"
    );

    let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(&client);

    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("rp:it", "v1", None, None, false).await.unwrap();

    let got: Option<String> = redis_pane::redis::refetch_and_rearm(&client, "rp:it")
        .await
        .unwrap();
    assert_eq!(
        got.as_deref(),
        Some("v1"),
        "the armed read must see the value"
    );

    let _: () = writer.set("rp:it", "v2", None, None, false).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), invalidations.recv())
            .await
            .is_ok(),
        "an armed key must produce an invalidation push"
    );

    // ADR-0006's central finding: the push consumed the arming.
    let _: () = writer.set("rp:it", "v3", None, None, false).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(800), invalidations.recv())
            .await
            .is_err(),
        "tracking is consumed by its own invalidation"
    );

    // And Refetch — the only read path — brings it back.
    let _: Option<String> = redis_pane::redis::refetch_and_rearm(&client, "rp:it")
        .await
        .unwrap();
    let _: () = writer.set("rp:it", "v4", None, None, false).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), invalidations.recv())
            .await
            .is_ok(),
        "re-arming must restore liveness"
    );

    let _: () = writer.del("rp:it").await.unwrap();
    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── M1.2 — the keyspace source streams, resumes and cancels ─────────────────

use redis_pane_core::state::ScanState;
use redis_pane_core::{Msg, State, update};
use tokio_util::sync::CancellationToken;

/// Populate a server with `n` keys, quickly.
async fn seed(url: &str, n: usize) -> Client {
    let client = Builder::from_config(Config::from_url(url).unwrap())
        .build()
        .unwrap();
    client.init().await.unwrap();
    for chunk in 0..(n / 1_000) {
        let pipeline = client.pipeline();
        for i in 0..1_000 {
            let k = format!("user:{:08}:session", chunk * 1_000 + i);
            let _: () = pipeline.set(&k, "v", None, None, false).await.unwrap();
        }
        let _: () = pipeline.all().await.unwrap();
    }
    client
}

/// Drive the core with everything the scan sends, exactly as the app does.
async fn drain_into_core(mut rx: tokio::sync::mpsc::Receiver<Msg>) -> State {
    let mut state = State::default();
    while let Some(msg) = rx.recv().await {
        let (next, _cmds) = update(state, msg);
        state = next;
    }
    state
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_hundred_thousand_keys_stream_into_the_loaded_set() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = seed(&url, 100_000).await;

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let cancel = CancellationToken::new();

    let pump = tokio::spawn(async move { drain_into_core(rx).await });
    redis_pane::redis::scan::stream_keys(&client, None, tx, cancel).await;
    let state = pump.await.unwrap();

    // SCAN may return a key more than once across a full iteration, so the
    // Loaded set can exceed DBSIZE. What must hold is that everything was seen.
    assert!(
        state.keys.len() >= 100_000,
        "scanned {} of 100,000",
        state.keys.len()
    );
    assert_eq!(
        state.scan,
        ScanState::Complete {
            total: state.keys.len() as u64
        }
    );
    assert!(!state.keys.is_capped());

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_pattern_narrows_the_traversal_without_using_keys() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = seed(&url, 10_000).await;
    let _: () = writer.set("other:1", "v", None, None, false).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let pump = tokio::spawn(async move { drain_into_core(rx).await });
    redis_pane::redis::scan::stream_keys(&client, Some("other:*"), tx, CancellationToken::new())
        .await;
    let state = pump.await.unwrap();

    assert_eq!(state.keys.len(), 1);
    assert_eq!(state.keys.name_str(0).unwrap(), "other:1");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn cancelling_stops_the_traversal_promptly_and_keeps_what_arrived() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = seed(&url, 100_000).await;

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();

    // Cancel once a few pages have landed — the Esc-during-a-scan case.
    let pump = tokio::spawn(async move {
        let mut state = State::default();
        let mut batches = 0;
        while let Some(msg) = rx.recv().await {
            if matches!(msg, Msg::ScanBatch { .. }) {
                batches += 1;
                if batches == 3 {
                    trigger.cancel();
                }
            }
            let (next, _) = update(state, msg);
            state = next;
        }
        state
    });

    let started = std::time::Instant::now();
    redis_pane::redis::scan::stream_keys(&client, None, tx, cancel).await;
    let elapsed = started.elapsed();
    let state = pump.await.unwrap();

    assert!(
        elapsed < Duration::from_secs(5),
        "cancellation must be answered at the next page boundary, took {elapsed:?}"
    );
    assert!(
        matches!(state.scan, ScanState::Cancelled { .. }),
        "got {:?}",
        state.scan
    );
    assert!(!state.keys.is_empty(), "partial results are kept");
    assert!(
        state.keys.len() < 100_000,
        "cancelling must actually stop it, got {}",
        state.keys.len()
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── M1.4 — lazy metadata, fetched only for what is on screen ────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn metadata_is_fetched_for_a_window_and_matches_the_server() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();

    let _: () = writer
        .set("m:string", "hello", None, None, false)
        .await
        .unwrap();
    let _: () = writer
        .hset("m:hash", [("a", "1"), ("b", "2")])
        .await
        .unwrap();
    let _: () = writer.rpush("m:list", vec!["x", "y", "z"]).await.unwrap();
    let _: () = writer
        .set("m:ttl", "v", Some(Expiration::EX(600)), None, false)
        .await
        .unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let window: Vec<(usize, Vec<u8>)> = vec![
        (0, b"m:string".to_vec()),
        (1, b"m:hash".to_vec()),
        (2, b"m:list".to_vec()),
        (3, b"m:ttl".to_vec()),
    ];
    let (entries, gone) = redis_pane::redis::fetch_metadata(&client, &window)
        .await
        .unwrap();
    assert_eq!(entries.len(), 4);
    assert!(gone.is_empty(), "nothing was deleted: {gone:?}");

    use redis_pane_core::state::KeyKind;
    assert_eq!(entries[0].kind, KeyKind::String);
    assert_eq!(entries[1].kind, KeyKind::Hash);
    assert_eq!(entries[2].kind, KeyKind::List);

    // A key with no expiry reports -1, which the store keeps distinct from
    // "not yet fetched".
    assert_eq!(entries[0].ttl_seconds, -1);
    assert!(
        (500..=600).contains(&entries[3].ttl_seconds),
        "got {}",
        entries[3].ttl_seconds
    );
    assert!(
        entries.iter().all(|e| e.size_bytes > 0),
        "MEMORY USAGE returned nothing"
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_key_that_vanished_between_scan_and_fetch_is_reported_gone() {
    // The keyspace moves while we walk it, so this is ordinary, not exceptional.
    // `TYPE` is already on the wire for every visible row, so noticing a
    // deletion costs no extra round trip and no tracking table (DESIGN §9).
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("m:here", "v", None, None, false).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let window: Vec<(usize, Vec<u8>)> = vec![(0, b"m:here".to_vec()), (1, b"m:gone".to_vec())];
    let (entries, gone) = redis_pane::redis::fetch_metadata(&client, &window)
        .await
        .unwrap();

    assert_eq!(entries.len(), 1, "the surviving key is still reported");
    assert_eq!(entries[0].index, 0);
    assert_eq!(
        gone,
        vec![1],
        "the missing key is reported by row, not dropped"
    );

    // And that report has to survive the trip into the core, which is the seam
    // the unit tests cannot see: they hand `update` a message they wrote
    // themselves, so a shell that never sends one passes them all. That is
    // exactly how `Msg::TrackingArmed` went missing for the whole of alpha.2.
    use redis_pane_core::state::{LoadedSet, State};
    use redis_pane_core::{Msg, update};
    let mut keys = LoadedSet::default();
    keys.push(b"m:here");
    keys.push(b"m:gone");
    let mut state = State {
        keys,
        ..State::default()
    };
    state.rebuild_list();
    let (state, _) = update(
        state,
        Msg::MetadataBatch {
            entries,
            gone,
            at_ms: 0,
        },
    );
    assert!(!state.keys.is_gone(0), "the surviving key is untouched");
    assert!(state.keys.is_gone(1), "the deleted key is tombstoned");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── credentials actually reach the connection (ADR-0002) ────────────────────

use redis_pane_core::resolve::{Credentials, PasswordSource};
use redis_pane_core::state::ReadOnlyReason;

async fn start_with_password(pw: &str) -> (ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("redis", "7-alpine")
        .with_exposed_port(REDIS_PORT)
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .with_cmd(vec!["redis-server", "--requirepass", pw])
        .start()
        .await
        .expect("docker must be running for the integration suite");
    let port = container.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    (container, format!("redis://127.0.0.1:{port}"))
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_password_protected_server_is_refused_without_credentials() {
    let (_c, url) = start_with_password("s3cret").await;
    assert!(
        redis_pane::redis::connect(&url).await.is_err(),
        "connecting with no password must fail, or the test below proves nothing"
    );
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_literal_password_authenticates() {
    let (_c, url) = start_with_password("s3cret").await;
    let creds = Credentials {
        password: PasswordSource::Literal("s3cret".into()),
        ..Credentials::default()
    };
    let (client, est) = redis_pane::redis::connect_with(&url, &creds).await.unwrap();
    assert!(est.version.meets_floor());
    let _ = client.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_password_env_reference_authenticates() {
    // The whole point of ADR-0002: this is the form users are told to prefer,
    // and until now it was parsed and then dropped.
    let (_c, url) = start_with_password("s3cret").await;
    let creds = Credentials {
        // PATH is used as a stand-in variable the test can rely on existing;
        // the command form below covers arbitrary values.
        password: PasswordSource::Command("printf s3cret".into()),
        ..Credentials::default()
    };
    let (client, _) = redis_pane::redis::connect_with(&url, &creds).await.unwrap();
    let _: () = client
        .set("auth:works", "yes", None, None, false)
        .await
        .unwrap();
    let got: Option<String> = client.get("auth:works").await.unwrap();
    assert_eq!(got.as_deref(), Some("yes"));
    let _ = client.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_wrong_password_fails_with_a_diagnostic_rather_than_hanging() {
    let (_c, url) = start_with_password("s3cret").await;
    let creds = Credentials {
        password: PasswordSource::Literal("wrong".into()),
        ..Credentials::default()
    };
    let err = redis_pane::redis::connect_with(&url, &creds)
        .await
        .expect_err("a wrong password must not connect");
    let msg = err.to_string();
    assert!(!msg.is_empty(), "the failure must say something");
    assert!(
        !msg.contains("wrong"),
        "the message must not echo the password: {msg}"
    );
}

// ── R1.15: server conditions are detected, not merely reported ──────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn a_primary_is_not_flagged_as_a_replica() {
    let (_c, url) = start("redis", "7-alpine").await;
    let (client, est) = redis_pane::redis::connect(&url).await.unwrap();
    assert_eq!(est.read_only, None, "a primary imposes no guard of its own");
    assert_eq!(est.condition, None);
    let _ = client.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_replica_turns_on_read_only_mode_before_any_write_is_attempted() {
    // DESIGN principle 5: danger visible before it is possible. Learning this
    // by having a write rejected is the ordering that is forbidden.
    let (primary, primary_url) = start("redis", "7-alpine").await;
    let primary_port = primary.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    let _ = primary_url;

    let replica = GenericImage::new("redis", "7-alpine")
        .with_exposed_port(REDIS_PORT)
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .with_cmd(vec![
            "redis-server".to_string(),
            "--replicaof".to_string(),
            "host.docker.internal".to_string(),
            primary_port.to_string(),
        ])
        .start()
        .await
        .expect("docker");
    let replica_port = replica.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    let replica_url = format!("redis://127.0.0.1:{replica_port}");

    let (client, est) = redis_pane::redis::connect(&replica_url).await.unwrap();
    assert_eq!(
        est.read_only,
        Some(ReadOnlyReason::Replica),
        "a replica must impose Read-only Mode with a reason that cannot be lifted"
    );
    assert!(!est.read_only.unwrap().liftable());
    let _ = client.quit().await;
}

// ── R7.4: errors surface rather than vanishing ──────────────────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn reading_a_key_of_an_unexpected_type_produces_an_error_not_silence() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.hset("h", [("a", "1")]).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    // GET against a hash is WRONGTYPE. The read path must report it.
    let err = client
        .get::<Option<String>, _>("h")
        .await
        .expect_err("GET on a hash is an error");
    assert!(err.details().contains("WRONGTYPE"), "{err}");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── a server that refuses tracking must still be readable ───────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn a_key_opens_on_a_server_that_refuses_tracking() {
    // Found against Upstash, which rejects CLIENT CACHING outright: arming
    // unconditionally made every read fail, so the app could browse a keyspace
    // and open nothing in it. Simulated here with an ACL that denies CLIENT.
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer
        .set("readable", "value", None, None, false)
        .await
        .unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let read = redis_pane::redis::read::read_value(
        &client,
        b"readable",
        50,
        redis_pane::redis::read::Arming::Unsupported,
    )
    .await
    .expect("a read must succeed without arming")
    .expect("the key exists");
    assert_eq!(read.ttl_seconds, -1);

    // And with arming, on a server that supports it, it still works.
    let armed = redis_pane::redis::read::read_value(
        &client,
        b"readable",
        50,
        redis_pane::redis::read::Arming::Enabled,
    )
    .await
    .unwrap();
    assert!(armed.is_some());

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── the shell must actually tell the core arming succeeded ──────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn opening_a_key_on_a_tracking_capable_server_reaches_live_state() {
    // Found by hand against Redis Cloud, not by anything in this suite:
    // read_value awaited CLIENT CACHING YES and it succeeded on the wire, but
    // nothing ever sent core::Msg::TrackingArmed, so State::liveness() could
    // never return Live — the header read "manual" forever, on every server,
    // local Redis included. The core's guard (no Live without an explicit
    // TrackingArmed) was airtight; the shell just never told it the truth.
    //
    // This exercises the exact sequence terminal.rs's open_key runs: connect,
    // read a key with Arming::Enabled, and — only because the read succeeded —
    // report TrackingArmed before ValueLoaded.
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("k", "v", None, None, false).await.unwrap();

    let (client, est) = redis_pane::redis::connect(&url).await.unwrap();
    assert!(
        est.tracking_supported,
        "this container must support tracking or the test proves nothing"
    );

    let arming = redis_pane::redis::read::Arming::Enabled;
    let read = redis_pane::redis::read::read_value(&client, b"k", 40, arming)
        .await
        .unwrap();
    assert!(read.is_some(), "arming happens inside a successful read");

    // The shell's obligation, reproduced directly: report arming, then load.
    let mut state = State::default();
    (state, _) = update(
        state,
        Msg::Connected {
            version: est.version.to_string(),
            tracking_supported: true,
        },
    );
    (state, _) = update(state, Msg::TrackingArmed);

    assert_eq!(
        state.liveness(),
        redis_pane_core::state::Liveness::Live,
        "a server that supports tracking, successfully armed, must reach Live"
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── severity-2 #1: streams read newest-first, with a live AGE column ────────

#[tokio::test]
#[ignore = "needs docker"]
async fn a_stream_is_read_newest_first_not_oldest_first() {
    // The actual bug this fixed: XRANGE("-", "+", COUNT) takes the *oldest*
    // COUNT entries. On a stream past the window, a triage view built on it
    // was silently showing ancient history instead of recent activity.
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();

    for i in 0..10 {
        let _: String = writer
            .xadd("orders", false, None, "*", vec![("seq", i.to_string())])
            .await
            .unwrap();
    }

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let value = redis_pane::redis::read::read_value(
        &client,
        b"orders",
        40,
        redis_pane::redis::read::Arming::Unsupported,
    )
    .await
    .unwrap()
    .expect("the stream exists")
    .value;

    let redis_pane_core::state::Value::Stream(stream) = value else {
        panic!("expected a stream value");
    };
    assert_eq!(stream.total, 10);
    // First entry back must be seq=9 (the most recently added), not seq=0.
    let first_fields = &stream.entries[0].1;
    assert_eq!(first_fields[0], ("seq".to_string(), "9".to_string()));
    let last_fields = &stream.entries[9].1;
    assert_eq!(last_fields[0], ("seq".to_string(), "0".to_string()));

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_freshly_added_entry_reads_as_just_added() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: String = writer
        .xadd("events", false, None, "*", vec![("kind", "login")])
        .await
        .unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let value = redis_pane::redis::read::read_value(
        &client,
        b"events",
        40,
        redis_pane::redis::read::Arming::Unsupported,
    )
    .await
    .unwrap()
    .expect("the stream exists")
    .value;

    let redis_pane_core::state::Value::Stream(stream) = value else {
        panic!("expected a stream value");
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    // The ID Redis actually assigned, run through the real formatter, must
    // read as "just now" — proving the ID's timestamp component genuinely is
    // real wall-clock epoch millis, not a guess about Redis's ID format.
    let age = redis_pane_core::state::value::stream_entry_age(&stream.entries[0].0, now_ms);
    assert_eq!(age, "just now", "id was {}", stream.entries[0].0);

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── The arm-and-read pair is indivisible (ReadGate) ─────────────────────────

/// Opening one key and then another, back to back, must leave the server
/// tracking the **second** — the one the Viewer is showing.
///
/// `CLIENT CACHING YES` arms the *next* read-only command on the connection,
/// not a command it is bundled with. Two reads in flight at once can therefore
/// interleave on the wire — A arms, B arms, A reads, B reads — and the arming
/// lands on the wrong key. Nothing in the app is wrong at that point except the
/// one thing that matters: the header goes on saying `● live` over a value
/// nothing will ever push an update for, which is ADR-0006's defect reached by
/// a route the core cannot see. The read token added alongside this rejects the
/// stale *reply*, but dropping a reply does not un-send the `CLIENT CACHING`
/// that went with it.
///
/// This drives `ReadGate` itself, which is what `terminal.rs` uses, rather than
/// reproducing the sequence by hand — the shape of test that let the original
/// `TrackingArmed` bug ship.
#[tokio::test]
#[ignore = "needs docker"]
async fn back_to_back_opens_leave_the_second_key_armed_not_the_first() {
    use redis_pane::redis::read::{Arming, ReadGate, read_value};

    let (_c, url) = start("redis", "7-alpine").await;
    let (client, est) = redis_pane::redis::connect(&url).await.unwrap();
    assert!(est.tracking_supported, "or this test proves nothing");

    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("first", "a", None, None, false).await.unwrap();
    let _: () = writer.set("second", "b", None, None, false).await.unwrap();

    let mut invalidations = fred::interfaces::TrackingInterface::invalidation_rx(&client);

    // Open `first`, then change your mind and open `second` before the first
    // has come back — the exact sequence a reader produces by pressing `→`
    // twice, and the one that used to race.
    let mut gate = ReadGate::default();
    let a = gate.begin();
    let b = gate.begin();
    let (ca, cb) = (client.clone(), client.clone());
    let ha =
        tokio::spawn(async move { a.run(read_value(&ca, b"first", 40, Arming::Enabled)).await });
    let hb =
        tokio::spawn(async move { b.run(read_value(&cb, b"second", 40, Arming::Enabled)).await });
    let (ra, rb) = (ha.await.unwrap(), hb.await.unwrap());

    assert!(
        ra.is_none(),
        "the superseded read must never reach the wire, or it arms `first`"
    );
    assert!(rb.is_some_and(|r| r.is_ok()), "the wanted read still ran");

    // The server must be tracking `second`.
    let _: () = writer.set("second", "b2", None, None, false).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), invalidations.recv())
            .await
            .is_ok(),
        "the key the Viewer is showing must be the key that pushes"
    );

    // And not `first`. Re-arm on `second` so the consumed arming cannot be
    // mistaken for the absence of one, then write to `first`.
    let _ = read_value(&client, b"second", 40, Arming::Enabled)
        .await
        .unwrap();
    let _: () = writer.set("first", "a2", None, None, false).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(800), invalidations.recv())
            .await
            .is_err(),
        "a key nobody is looking at must not be tracked"
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── Hash and Set reads are windowed, matching List/ZSet/Stream ──────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn a_large_hash_is_windowed_not_pulled_whole() {
    // HGETALL used to bring the whole hash back regardless of size — the last
    // type, with Set, that stayed unbounded after List/ZSet/Stream were
    // windowed. A million-field hash arrived whole into a 250MB budget (PRD
    // §7), on the same connection the scan is using.
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();

    let fields: Vec<(String, String)> = (0..1_500)
        .map(|i| (format!("f{i}"), format!("v{i}")))
        .collect();
    let _: () = writer.hset("bighash", fields).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let read = redis_pane::redis::read::read_value(
        &client,
        b"bighash",
        40,
        redis_pane::redis::read::Arming::Enabled,
    )
    .await
    .unwrap()
    .expect("the key exists");

    let redis_pane_core::state::value::Value::Hash(hash) = read.value else {
        panic!("expected a hash");
    };
    assert_eq!(hash.total, 1_500, "HLEN, not the window, is the real count");
    assert_eq!(
        hash.pairs.len(),
        500,
        "HSCAN stopped at the window, not the whole hash"
    );
    // Every pair that did come back must actually be from the hash — no
    // duplicates or garbage from a mishandled cursor.
    let names: std::collections::HashSet<_> = hash.pairs.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(names.len(), 500, "no duplicate fields from re-scanning");
    for (k, v) in &hash.pairs {
        let n: usize = k.strip_prefix('f').unwrap().parse().unwrap();
        assert_eq!(*v, format!("v{n}"), "field and value must still agree");
    }

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_large_set_is_windowed_not_pulled_whole() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();

    let members: Vec<String> = (0..1_500).map(|i| format!("m{i}")).collect();
    let _: () = writer.sadd("bigset", members).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let read = redis_pane::redis::read::read_value(
        &client,
        b"bigset",
        40,
        redis_pane::redis::read::Arming::Enabled,
    )
    .await
    .unwrap()
    .expect("the key exists");

    let redis_pane_core::state::value::Value::Set(set) = read.value else {
        panic!("expected a set");
    };
    assert_eq!(set.total, 1_500, "SCARD, not the window, is the real count");
    assert_eq!(
        set.members.len(),
        500,
        "SSCAN stopped at the window, not the whole set"
    );
    let unique: std::collections::HashSet<_> = set.members.iter().collect();
    assert_eq!(unique.len(), 500, "no duplicate members from re-scanning");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn a_small_hash_and_set_still_come_back_whole() {
    // The common case: HLEN/SCARD equal what was fetched, so the header has
    // nothing extra to disclose (window() must return None).
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer
        .hset("smallhash", [("a", "1"), ("b", "2"), ("c", "3")])
        .await
        .unwrap();
    let _: () = writer.sadd("smallset", ["x", "y", "z"]).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    let arming = redis_pane::redis::read::Arming::Enabled;

    let hash = redis_pane::redis::read::read_value(&client, b"smallhash", 40, arming)
        .await
        .unwrap()
        .unwrap();
    let redis_pane_core::state::value::Value::Hash(h) = hash.value else {
        panic!("expected a hash");
    };
    assert_eq!(h.pairs.len(), 3);
    assert_eq!(h.total, 3);
    assert_eq!(h.window(), None, "nothing was withheld");

    let set = redis_pane::redis::read::read_value(&client, b"smallset", 40, arming)
        .await
        .unwrap()
        .unwrap();
    let redis_pane_core::state::value::Value::Set(s) = set.value else {
        panic!("expected a set");
    };
    assert_eq!(s.members.len(), 3);
    assert_eq!(s.total, 3);
    assert_eq!(s.window(), None, "nothing was withheld");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── M2.3 — Delete actually deletes ───────────────────────────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn deleting_a_key_removes_it_from_the_server() {
    // This is the regression test for a real bug: `client.del(name.to_vec())`
    // compiled, returned `Ok`, and deleted nothing. `fred`'s `Vec<T> ->
    // MultipleKeys` conversion treats a `Vec<u8>` as *many* numeric-string
    // keys (one per byte, since `u8: Into<Key>`) rather than one binary key,
    // so every call sent `DEL <byte0> <byte1> …` against keys that never
    // existed. Only a real server catches this — a mocked or unit-level test
    // would have to fake `del`'s behaviour and would fake it "correctly".
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("k:0", "v", None, None, false).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    redis_pane::redis::delete_key(&client, b"k:0")
        .await
        .unwrap();

    let still_there: Option<String> = writer.get("k:0").await.unwrap();
    assert_eq!(still_there, None, "the key must actually be gone");

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn deleting_a_key_with_bytes_that_look_like_small_integers_does_not_delete_the_wrong_thing() {
    // The bug this guards against is keyed on the *byte values* of the name,
    // not its length — a short key whose bytes happen to be small integers is
    // exactly the case the buggy conversion mishandled silently, and exactly
    // the case a casual re-test with an ordinary ASCII key name would miss.
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let name: &[u8] = &[7, 8];
    let _: () = writer.set(name, "v", None, None, false).await.unwrap();
    // A decoy key named after one of those byte values as a *string* — if the
    // bug were present, deleting `name` would remove this instead.
    let _: () = writer.set("7", "decoy", None, None, false).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    redis_pane::redis::delete_key(&client, name).await.unwrap();

    let target: Option<Vec<u8>> = writer.get(name).await.unwrap();
    assert_eq!(target, None, "the intended key must be gone");
    let decoy: Option<String> = writer.get("7").await.unwrap();
    assert_eq!(
        decoy.as_deref(),
        Some("decoy"),
        "the decoy must be untouched"
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

// ── M2 task 4 — String edit actually overwrites the value ────────────────

#[tokio::test]
#[ignore = "needs docker"]
async fn setting_a_value_overwrites_it_on_the_server() {
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let _: () = writer.set("k:0", "old", None, None, false).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    redis_pane::redis::set_value(&client, b"k:0", b"new")
        .await
        .unwrap();

    let now: Option<String> = writer.get("k:0").await.unwrap();
    assert_eq!(now.as_deref(), Some("new"));

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn setting_a_value_with_bytes_that_look_like_small_integers_does_not_touch_the_wrong_key() {
    // The mirror of `deleting_a_key_with_bytes...` above: `set`'s key
    // parameter is `K: Into<Key>`, never `Into<MultipleKeys>`, so it does not
    // share `del`'s trap — but this is the test that would have caught it if
    // it somehow did, keyed the same deliberate way.
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let name: &[u8] = &[7, 8];
    let _: () = writer.set(name, "old", None, None, false).await.unwrap();
    let _: () = writer.set("7", "decoy", None, None, false).await.unwrap();

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    redis_pane::redis::set_value(&client, name, b"new")
        .await
        .unwrap();

    let target: Option<Vec<u8>> = writer.get(name).await.unwrap();
    assert_eq!(target.as_deref(), Some(b"new".as_slice()));
    let decoy: Option<String> = writer.get("7").await.unwrap();
    assert_eq!(
        decoy.as_deref(),
        Some("decoy"),
        "the decoy must be untouched"
    );

    let _ = client.quit().await;
    let _ = writer.quit().await;
}

#[tokio::test]
#[ignore = "needs docker"]
async fn setting_a_json_looking_value_preserves_the_bytes_exactly() {
    // R3.2/R4.1: a JSON-classified value is still just a STRING underneath —
    // `SET` must not reformat, validate, or otherwise touch what the reader
    // typed, including whitespace that happens to not be "pretty".
    let (_c, url) = start("redis", "7-alpine").await;
    let writer = Builder::from_config(Config::from_url(&url).unwrap())
        .build()
        .unwrap();
    writer.init().await.unwrap();
    let compact = br#"{"a":1,"b":[1,2,3]}"#;

    let (client, _) = redis_pane::redis::connect(&url).await.unwrap();
    redis_pane::redis::set_value(&client, b"cfg:1", compact)
        .await
        .unwrap();

    let stored: Option<Vec<u8>> = writer.get("cfg:1").await.unwrap();
    assert_eq!(stored.as_deref(), Some(compact.as_slice()));

    let _ = client.quit().await;
    let _ = writer.quit().await;
}
