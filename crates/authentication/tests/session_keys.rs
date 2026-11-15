//! Real HTTPS transport with independently controlled cache-evidence clocks.
#![cfg(feature = "test-util")]

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use tokio::task::JoinSet;
use wamn_authentication::session_keys::{IssuerKeys, IssuerKeysConfig};

#[path = "support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "SQL and verifier tests also use this issuer fixture."
)]
mod session_fixture;
use session_fixture::{ISSUER, Reply, Server, entered, key};

#[tokio::test]
async fn configured_https_endpoint_and_exclusive_ca_are_the_only_trust_source() {
    let server = Server::start().await;
    let other = Server::start().await;
    for endpoint in [
        "http://127.0.0.1/jwks",
        "https://user:pass@example.test/jwks",
        "https://example.test/jwks#fragment",
    ] {
        assert!(IssuerKeysConfig::new(ISSUER, endpoint, &server.ca_pem).is_err());
    }
    assert!(
        IssuerKeysConfig::new(
            "http://issuer.example.test",
            &server.endpoint,
            &server.ca_pem
        )
        .is_err()
    );
    assert!(IssuerKeysConfig::new(ISSUER, &server.endpoint, b"").is_err());
    let wrong_ca = IssuerKeys::new(
        IssuerKeysConfig::new(ISSUER, &server.endpoint, &other.ca_pem)
            .expect("other CA configuration"),
    );
    assert!(wrong_ca.key("trusted").await.is_err());
    assert_eq!(server.count(), 0, "TLS refusal precedes HTTP");
    let wrong_name = server.endpoint.replace("127.0.0.1", "localhost");
    let wrong_name = IssuerKeys::new(
        IssuerKeysConfig::new(ISSUER, &wrong_name, &server.ca_pem).expect("name mismatch config"),
    );
    assert!(wrong_name.key("trusted").await.is_err());
    assert_eq!(server.count(), 0, "hostname verification precedes HTTP");
    let (cache, _) = server.cache();
    server.queue(Reply::keys(&["trusted"]));
    assert!(
        cache
            .key("https://untrusted.invalid/.well-known/jwks.json")
            .await
            .is_err()
    );
    let evidence = cache
        .key("trusted")
        .await
        .expect("key from configured endpoint");
    assert!(evidence.is_fresh());
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn delayed_in_window_age_is_anchored_at_start_and_hits_do_not_extend_it() {
    let mut server = Server::start().await;
    let (cache, clock) = server.cache();
    let started = clock.now();
    let (reply, barrier, release) = Reply::keys(&["old"]).age("120").held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("old").await });
    entered(barrier).await;
    clock.advance(Duration::from_secs(2));
    release.send(()).expect("release body");
    let evidence = fetch
        .await
        .expect("fetch task")
        .expect("remaining evidence window");
    assert_eq!(evidence.deadline(), started + Duration::from_secs(180));
    // A hit outside the last thirty seconds does not refresh ahead of expiry.
    clock.advance(Duration::from_secs(147));
    assert!(evidence.is_fresh());
    assert_eq!(
        cache.key("old").await.expect("fresh hit").deadline(),
        evidence.deadline()
    );
    assert_eq!(server.count(), 1);
    server.stop().await;
    clock.advance(Duration::from_secs(31));
    assert!(!evidence.is_fresh(), "equality expires held evidence too");
    assert!(
        cache.key("old").await.is_err(),
        "known key cannot outlive evidence"
    );
}

#[tokio::test]
async fn late_results_are_discarded_at_and_after_their_age_adjusted_deadline() {
    for elapsed in [4, 5] {
        let server = Server::start().await;
        let (cache, clock) = server.cache();
        let (reply, barrier, release) = Reply::keys(&["late"]).age("296").held();
        server.queue(reply);
        let caller = cache.clone();
        let fetch = tokio::spawn(async move { caller.key("late").await });
        entered(barrier).await;
        clock.advance(Duration::from_secs(elapsed));
        release.send(()).expect("release late body");
        assert!(fetch.await.expect("fetch task").is_err());
        server.queue(Reply::keys(&[]));
        assert!(cache.key("late").await.is_err());
        assert_eq!(server.count(), 2, "late response was not installed");
    }
}

#[tokio::test]
async fn expired_malformed_and_ambiguous_age_are_refused() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    for age in ["300", "301", "18446744073709551616", "-1", "", "1, 2"] {
        server.queue(Reply::keys(&["old"]).age(age));
        assert!(cache.key("old").await.is_err(), "Age={age:?}");
        clock.advance(Duration::from_secs(1));
    }
    server.queue(Reply::keys(&["old"]).age("0").age("0"));
    assert!(cache.key("old").await.is_err());
    assert_eq!(server.count(), 7);
}

#[tokio::test]
async fn complete_replacement_removes_absent_keys_including_an_empty_set() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    server.queue(Reply::keys(&["old"]));
    cache.key("old").await.expect("warm old key");
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&["new"]));
    cache.key("new").await.expect("new set");
    assert!(
        cache.key("old").await.is_err(),
        "old set is not merged back"
    );
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&[]));
    assert!(cache.key("unknown").await.is_err());
    assert!(
        cache.key("new").await.is_err(),
        "empty set removes every key"
    );
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn varied_unknown_ids_share_one_inflight_slot_and_one_attempt_start_interval() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    server.queue(Reply::keys(&["known"]));
    cache.key("known").await.expect("warm known key");
    for i in 0..32 {
        assert!(cache.key(&format!("unknown-{i}")).await.is_err());
    }
    assert_eq!(
        server.count(),
        1,
        "different IDs do not bypass the interval"
    );
    clock.advance(Duration::from_millis(999));
    assert!(cache.key("still-limited").await.is_err());
    assert_eq!(server.count(), 1);
    clock.advance(Duration::from_millis(1));
    let (reply, barrier, release) = Reply::keys(&["known", "added"]).held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("missing").await });
    entered(barrier).await;
    clock.advance(Duration::from_secs(2));
    let caller = cache.clone();
    let waiting = tokio::spawn(async move { caller.key("added").await });
    let mut contenders = JoinSet::new();
    for i in 0..32 {
        let caller = cache.clone();
        contenders.spawn(async move { caller.key(&format!("other-{i}")).await.is_err() });
    }
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(
        cache.key("known").await.is_ok(),
        "fresh hits do not wait for refresh"
    );
    assert_eq!(
        server.count(),
        2,
        "elapsed interval cannot overlap an active request"
    );
    assert_eq!(server.observed.peak.load(Ordering::SeqCst), 1);
    release.send(()).expect("release response");
    assert!(fetch.await.expect("refresh task").is_err());
    waiting
        .await
        .expect("waiting task")
        .expect("the key from the refresh it waited for");
    // Each miss waited for that refresh, read its result, and fetched nothing.
    while let Some(result) = contenders.join_next().await {
        assert!(result.expect("lookup task"));
    }
    assert_eq!(server.count(), 2);
    // The interval is measured from attempt start, not completion.
    server.queue(Reply::keys(&["known"]));
    assert!(cache.key("next-missing").await.is_err());
    assert_eq!(server.count(), 3);
    for i in 0..32 {
        assert!(cache.key(&format!("limited-{i}")).await.is_err());
    }
    assert_eq!(server.count(), 3);
}

#[tokio::test]
async fn a_hit_near_its_deadline_refreshes_ahead_so_evidence_crosses_the_old_deadline() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    server.queue(Reply::keys(&["signing"]));
    let first = cache.key("signing").await.expect("warm key");
    clock.advance(Duration::from_secs(269));
    assert_eq!(
        cache
            .key("signing")
            .await
            .expect("hit outside the window")
            .deadline(),
        first.deadline()
    );
    assert_eq!(server.count(), 1);
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&["signing"]));
    let renewed = cache.key("signing").await.expect("hit inside the window");
    assert!(renewed.deadline() > first.deadline());
    assert_eq!(server.count(), 2);
    clock.advance(Duration::from_secs(30));
    assert!(!first.is_fresh());
    assert!(
        renewed.is_fresh(),
        "evidence taken inside the window outlives the old deadline"
    );
}

#[tokio::test]
async fn cancelled_request_releases_slot_without_resetting_its_interval() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    let (reply, barrier, release) = Reply::keys(&["old"]).held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("old").await });
    entered(barrier).await;
    fetch.abort();
    assert!(fetch.await.expect_err("cancelled lookup").is_cancelled());
    assert!(cache.key("another").await.is_err());
    assert_eq!(server.count(), 1);
    release.send(()).expect("release abandoned response");
    clock.advance(Duration::from_secs(1));
    server.queue(Reply::keys(&["new"]));
    cache
        .key("new")
        .await
        .expect("slot reusable after cancellation");
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn warm_caches_refuse_removed_keys_by_deadline_with_and_without_the_endpoint() {
    for age in [0, 30] {
        for available in [true, false] {
            let mut server = Server::start().await;
            let (cache, clock) = server.cache();
            server.queue(Reply::keys(&["removed"]).age(&age.to_string()));
            let evidence = cache.key("removed").await.expect("warm cache");
            clock.advance(Duration::from_secs(299 - age));
            if available {
                server.queue(Reply::keys(&["replacement"]));
            } else {
                server.stop().await;
            }
            assert!(cache.key("missing").await.is_err());
            assert_eq!(
                cache.key("removed").await.is_ok(),
                !available,
                "a refresh removes absent keys but an outage preserves unexpired evidence"
            );
            assert!(evidence.is_fresh());
            clock.advance(Duration::from_secs(1));
            assert!(!evidence.is_fresh());
            if available {
                server.queue(Reply::keys(&["replacement"]));
            }
            assert!(cache.key("removed").await.is_err());
            if available {
                assert!(cache.key("replacement").await.is_ok());
            }
        }
    }
}

#[tokio::test]
async fn invalid_or_private_key_documents_are_not_installed() {
    let server = Server::start().await;
    let (cache, clock) = server.cache();
    let mut invalid = vec![
        json!({"keys": [key("old"), key("old")]}),
        json!({"keys": [key("old")], "private": "refuse"}),
    ];
    for (field, value) in [
        ("d", "private"),
        ("alg", "Ed25519"),
        ("kty", "RSA"),
        ("crv", "X25519"),
        ("use", "enc"),
        ("x", "short"),
        ("kid", ""),
    ] {
        let mut bad = key("old");
        bad[field] = json!(value);
        invalid.push(json!({"keys": [bad]}));
    }
    for body in invalid {
        let mut reply = Reply::keys(&[]);
        reply.body = serde_json::to_vec(&body).expect("invalid fixture bytes");
        server.queue(reply);
        assert!(cache.key("old").await.is_err());
        clock.advance(Duration::from_secs(1));
    }
    server.queue(Reply::keys(&["old"]));
    cache
        .key("old")
        .await
        .expect("valid replacement after refused documents");
}

#[tokio::test]
async fn redirects_and_server_failures_are_not_followed_or_retried() {
    let server = Server::start().await;
    let redirect_target = Server::start().await;
    let (cache, clock) = server.cache();
    let mut redirect = Reply::keys(&["old"]);
    redirect.status = 302;
    redirect
        .headers
        .push(("Location".into(), redirect_target.endpoint.clone()));
    server.queue(redirect);
    assert!(cache.key("old").await.is_err());
    assert_eq!(server.count(), 1);
    assert_eq!(redirect_target.count(), 0);
    clock.advance(Duration::from_secs(1));
    let mut unavailable = Reply::keys(&["old"]);
    unavailable.status = 503;
    server.queue(unavailable);
    assert!(cache.key("old").await.is_err());
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn streaming_body_limit_accepts_exact_bound_and_refuses_the_next_byte() {
    let server = Server::start().await;
    for (size, admitted) in [(65_536, true), (65_537, false)] {
        let (cache, _) = server.cache();
        let mut reply = Reply::keys(&["old"]);
        reply.body.resize(size, b' ');
        reply.chunked = true;
        server.queue(reply);
        assert_eq!(
            cache.key("old").await.is_ok(),
            admitted,
            "body bytes={size}"
        );
    }
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn body_stall_is_bounded_by_five_seconds_without_retry() {
    let server = Server::start().await;
    let (cache, _) = server.cache();
    let (reply, barrier, release) = Reply::keys(&["old"]).held();
    server.queue(reply);
    let caller = cache.clone();
    let fetch = tokio::spawn(async move { caller.key("old").await });
    entered(barrier).await;
    let result = tokio::time::timeout(Duration::from_secs(7), fetch)
        .await
        .expect("five-second total timeout bounds stalled body")
        .expect("lookup task");
    assert!(result.is_err());
    assert_eq!(server.count(), 1);
    drop(release);
}
