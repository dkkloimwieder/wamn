//! Signed tokens cross a real local HTTPS cache, not a deployed host boundary.
#![cfg(feature = "test-util")]

use std::time::Duration;

use serde_json::json;
use wamn_runtime::session_verifier::SessionVerifier;

#[path = "support/session_fixture.rs"]
mod session_fixture;
use session_fixture::{AUDIENCE, ISSUER, ORG, Server, claims, header, signed};

#[tokio::test]
async fn exact_org_and_environment_matrix_uses_one_configured_issuer_cache() {
    let server = Server::start().await;
    let (keys, _) = server.cache();
    let scopes = [
        (
            "org-a",
            "urn:wamn:project-env:org-a:project:dev:instance-one",
        ),
        (
            "org-a",
            "urn:wamn:project-env:org-a:project:prod:instance-one",
        ),
        (
            "org-b",
            "urn:wamn:project-env:org-b:project:dev:instance-one",
        ),
        (
            "org-b",
            "urn:wamn:project-env:org-b:project:prod:instance-one",
        ),
        (
            "org-a",
            "urn:wamn:project-env:org-a:project:dev:instance-two",
        ),
    ];
    for (host_org, host_audience) in scopes {
        let (verifier, _) =
            SessionVerifier::with_test_clock(keys.clone(), host_org, host_audience, 1000)
                .expect("configured host scope");
        for (token_org, token_audience) in scopes {
            let mut body = claims();
            body["org"] = json!(token_org);
            body["aud"] = json!(token_audience);
            assert_eq!(
                verifier.verify(&signed(&header(), &body)).await.is_ok(),
                host_org == token_org && host_audience == token_audience,
                "host={host_audience}, token={token_audience}",
            );
        }
        let mut wrong_issuer = claims();
        wrong_issuer["org"] = json!(host_org);
        wrong_issuer["aud"] = json!(host_audience);
        wrong_issuer["iss"] = json!("https://attacker.invalid");
        assert!(
            verifier
                .verify(&signed(&header(), &wrong_issuer))
                .await
                .is_err()
        );
        let mut wrong_org = wrong_issuer;
        wrong_org["iss"] = json!(ISSUER);
        wrong_org["org"] = json!("unconfigured-org");
        assert!(
            verifier
                .verify(&signed(&header(), &wrong_org))
                .await
                .is_err()
        );
    }
    assert_eq!(
        server.count(),
        1,
        "configured scopes share one issuer budget"
    );
    assert!(SessionVerifier::new(keys.clone(), "", AUDIENCE).is_err());
    assert!(SessionVerifier::new(keys, ORG, " ").is_err());
}

#[tokio::test]
async fn pinned_profile_refuses_before_fetch_and_claim_misses_never_gain_authority() {
    let server = Server::start().await;
    let (verifier, _, _) = server.verifier();
    for (field, value) in [
        ("alg", "EdDSA"),
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
    for field in ["iss", "sub", "org", "aud", "roles", "iat", "exp", "jti"] {
        let mut missing = claims();
        missing.as_object_mut().expect("claim object").remove(field);
        assert!(verifier.verify(&signed(&header(), &missing)).await.is_err());
    }
    let mut token = signed(&header(), &claims());
    let final_character = token.pop().expect("signature character");
    token.push(if final_character == 'A' { 'B' } else { 'A' });
    assert!(
        verifier.verify(&token).await.is_err(),
        "signature mutation refuses"
    );
    for roles in [
        json!([]),
        json!(["unknown-role"]),
        json!(["purchase-reader", "purchase-writer"]),
    ] {
        let mut body = claims();
        body["roles"] = roles.clone();
        let verified = verifier
            .verify(&signed(&header(), &body))
            .await
            .expect("signed role evidence");
        assert_eq!(
            serde_json::to_value(&verified.claims().roles).unwrap(),
            roles
        );
        verified.check_admission().expect("fresh evidence");
    }
    assert_eq!(server.count(), 1, "all warm verification remains offline");
}

#[tokio::test]
async fn age_rules_remain_separate_at_their_exact_boundaries() {
    let server = Server::start().await;
    let (verifier, clock, _) = server.verifier();
    for (iat, exp, now, accepted) in [
        (1000, 1900, 1000, true),
        (1000, 1901, 1000, false),
        (1000, 1000, 1000, false),
        (1030, 1900, 1000, true),
        (1031, 1900, 1000, false),
        (1000, 1900, 1929, true),
        (1000, 1900, 1930, false),
        (1000, 1900, 1931, false),
        (-1, 899, 0, false),
        (i64::MAX - 1, i64::MAX, i64::MAX, false),
    ] {
        let mut body = claims();
        body["iat"] = json!(iat);
        body["exp"] = json!(exp);
        clock.set(now);
        assert_eq!(
            verifier.verify(&signed(&header(), &body)).await.is_ok(),
            accepted,
            "iat={iat}, exp={exp}, now={now}"
        );
    }
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn unknown_key_ids_across_clones_share_the_issuer_refresh_budget() {
    let server = Server::start().await;
    let (verifier, _, _) = server.verifier();
    verifier
        .verify(&signed(&header(), &claims()))
        .await
        .expect("warm trusted key");
    for index in 0..32 {
        let mut unknown = header();
        unknown["kid"] = json!(format!("https://untrusted.invalid/keys/{index}"));
        assert!(
            verifier
                .clone()
                .verify(&signed(&unknown, &claims()))
                .await
                .is_err()
        );
    }
    assert_eq!(
        server.count(),
        1,
        "key selectors cannot create refresh budgets"
    );
}

#[tokio::test]
async fn pending_admission_rechecks_token_and_key_deadlines_after_permission_work() {
    let server = Server::start().await;
    let token = signed(&header(), &claims());
    let (verifier, token_clock, key_clock) = server.verifier();
    let pending = verifier.verify(&token).await.expect("initial verification");
    token_clock.set(1929);
    pending
        .check_admission()
        .expect("last second within tolerance");
    token_clock.set(1930);
    assert!(
        pending.check_admission().is_err(),
        "permission delay crosses token expiry"
    );
    token_clock.set(1000);
    key_clock.advance(Duration::from_secs(299));
    pending.check_admission().expect("key remains fresh");
    key_clock.advance(Duration::from_secs(1));
    assert!(
        pending.check_admission().is_err(),
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
    let (entered, release) = server.hold();
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

#[tokio::test]
async fn two_local_verifiers_refuse_removed_key_by_deadline_with_and_without_jwks() {
    for available in [true, false] {
        let mut server = Server::start().await;
        let (first, _, first_clock) = server.verifier();
        let (second, _, second_clock) = server.verifier();
        let token = signed(&header(), &claims());
        for verifier in [&first, &second] {
            verifier
                .verify(&token)
                .await
                .expect("warm signed-token verification");
        }
        server.remove_keys();
        if !available {
            server.stop().await;
        }
        first_clock.advance(Duration::from_secs(299));
        second_clock.advance(Duration::from_secs(299));
        for verifier in [&first, &second] {
            verifier
                .verify(&token)
                .await
                .expect("unexpired key evidence");
        }
        first_clock.advance(Duration::from_secs(1));
        second_clock.advance(Duration::from_secs(1));
        for verifier in [&first, &second] {
            assert!(
                verifier.verify(&token).await.is_err(),
                "JWKS available={available}"
            );
        }
        assert_eq!(server.count(), if available { 4 } else { 2 });
    }
}
