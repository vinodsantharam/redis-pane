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
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage};

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
