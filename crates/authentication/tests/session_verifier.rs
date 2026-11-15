//! Signed tokens cross a real local HTTPS cache, not a deployed host boundary.
#![cfg(feature = "test-util")]

use std::time::Duration;

use serde_json::json;
use wamn_identity_protocol::verifier::SessionVerifier;

#[path = "support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "Key-cache and SQL tests also use this issuer fixture."
)]
mod session_fixture;
use session_fixture::{AUDIENCE, ORG, Server, claims, header, signed};

#[tokio::test]
async fn pinned_profile_refuses_before_fetch_and_preserves_signed_roles() {
    let server = Server::start().await;
    let (keys, _) = server.cache();
    assert!(SessionVerifier::new(keys.clone(), "", AUDIENCE).is_err());
    assert!(SessionVerifier::new(keys, ORG, " ").is_err());
    let (verifier, _, _) = server.verifier();
    for (field, value) in [
        ("alg", "Ed25519"),
        ("alg", "none"),
        ("typ", "JWT"),
        ("kid", ""),
    ] {
        let mut bad = header();
        bad[field] = json!(value);
        assert!(verifier.verify(&signed(&bad, &claims())).await.is_err());
    }
    let mut discovered = header();
    discovered["jku"] = json!("https://attacker.invalid/keys");
    assert!(
        verifier
            .verify(&signed(&discovered, &claims()))
            .await
            .is_err()
    );
    assert_eq!(server.count(), 0, "invalid profiles never discover keys");
    for roles in [
        json!([]),
        json!(["unknown-role"]),
        json!(["purchase-reader", "purchase-writer"]),
    ] {
        let mut body = claims();
        body["roles"] = roles.clone();
        let verified_claims = verifier
            .verify(&signed(&header(), &body))
            .await
            .expect("signed role evidence");
        assert_eq!(
            serde_json::to_value(&verified_claims.claims().roles).unwrap(),
            roles
        );
        verified_claims.require_admission().expect("fresh evidence");
    }
    assert_eq!(server.count(), 1, "all warm verification remains offline");
}

#[tokio::test]
async fn pending_admission_requires_fresh_token_and_key_after_permission_work() {
    let server = Server::start().await;
    let token = signed(&header(), &claims());
    let (verifier, token_clock, key_clock) = server.verifier();
    let pending = verifier.verify(&token).await.expect("initial verification");
    token_clock.set(1929);
    pending
        .require_admission()
        .expect("last second within tolerance");
    token_clock.set(1930);
    assert!(
        pending.require_admission().is_err(),
        "permission delay crosses token expiry"
    );
    token_clock.set(1000);
    key_clock.advance(Duration::from_secs(299));
    pending.require_admission().expect("key remains fresh");
    key_clock.advance(Duration::from_secs(1));
    assert!(
        pending.require_admission().is_err(),
        "permission delay crosses key deadline"
    );
    assert_eq!(
        server.count(),
        1,
        "final admission never restarts key freshness"
    );
}

#[tokio::test]
async fn token_age_is_read_after_a_delayed_key_fetch_not_before_it() {
    let server = Server::start().await;
    let (verifier, token_clock, _) = server.verifier();
    let (reply, entered, release) = session_fixture::Reply::keys(&["key-one"]).held();
    server.queue(reply);
    let token = signed(&header(), &claims());
    let fetch = tokio::spawn(async move { verifier.verify(&token).await });
    tokio::time::timeout(Duration::from_secs(2), entered)
        .await
        .expect("fetch reached fixture")
        .expect("fetch barrier");
    token_clock.set(1930);
    release.send(()).expect("release JWKS");
    assert!(
        fetch.await.expect("verification task").is_err(),
        "fetch delay cannot extend token age"
    );
}
