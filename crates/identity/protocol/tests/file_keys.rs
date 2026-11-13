//! The edge key source: a key set read from a file, verified end to end.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::EncodingKey;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair as _},
};
use serde_json::json;
use wamn_identity_protocol::SessionErrorType;
use wamn_identity_protocol::file_keys::FileKeys;
use wamn_identity_protocol::keys::PublicSessionKey;
use wamn_identity_protocol::token::{SessionAuthority, SessionClaims, session_header};
use wamn_identity_protocol::verifier::SessionVerifier;

const ISSUER: &str = "https://identity.example.test";
const ORG: &str = "org-a";
const AUDIENCE: &str = "org-a/dev";
const PRINCIPAL: &str = "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b";

/// A key pair from a fixed seed, and its public JWK under `kid`.
fn key(kid: &str, _seed: u8) -> (EncodingKey, PublicSessionKey) {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap();
    let public = PublicSessionKey {
        kid: kid.into(),
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        alg: "EdDSA".into(),
        r#use: "sig".into(),
        x: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    };
    (EncodingKey::from_ed_der(document.as_ref()), public)
}

/// A new key file under Cargo's temporary directory for this test target.
fn key_file(document: &serde_json::Value) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "session-keys-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, serde_json::to_vec(document).expect("encode")).expect("write");
    path
}

fn sign(pair: &EncodingKey, kid: &str, issuer: &str) -> String {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    )
    .expect("the clock fits in i64");
    let claims = SessionClaims {
        iss: issuer.into(),
        sub: PRINCIPAL.parse().unwrap(),
        org: ORG.into(),
        aud: AUDIENCE.into(),
        roles: vec!["operator".parse().unwrap()],
        exp: now + 300,
        iat: now - 10,
        jti: "token-1".into(),
        authority: SessionAuthority::Login(PRINCIPAL.parse().unwrap()),
        csrf: None,
    };
    let header = session_header(kid);
    jsonwebtoken::encode(&header, &claims, pair).unwrap()
}

#[tokio::test]
async fn a_token_signed_by_a_file_key_verifies() {
    let (one, one_public) = key("key-one", 1);
    let (_, two_public) = key("key-two", 2);
    let path = key_file(&json!({"keys": [one_public, two_public]}));
    let keys = FileKeys::load(&path, ISSUER).expect("load");
    let verifier = SessionVerifier::new(keys, ORG, AUDIENCE).expect("verifier");

    let session = verifier
        .verify(&sign(&one, "key-one", ISSUER))
        .await
        .expect("a token signed by a loaded key verifies");
    assert_eq!(session.claims().sub, PRINCIPAL.parse().unwrap());
    assert_eq!(
        session.claims().roles,
        ["operator"
            .parse::<wamn_identity_protocol::RoleName>()
            .unwrap()]
    );
    session.check_admission().expect("a file key stays fresh");
}

#[tokio::test]
async fn an_unknown_kid_another_key_or_another_issuer_is_refused() {
    let (one, one_public) = key("key-one", 1);
    let (other, _) = key("key-other", 3);
    let path = key_file(&json!({"keys": [one_public]}));
    let verifier =
        SessionVerifier::new(FileKeys::load(&path, ISSUER).expect("load"), ORG, AUDIENCE)
            .expect("verifier");

    for token in [
        sign(&other, "key-other", ISSUER),
        sign(&other, "key-one", ISSUER),
        sign(&one, "key-one", "https://another.example.test"),
    ] {
        assert!(verifier.verify(&token).await.is_err());
    }
}

#[test]
fn a_repeated_kid_or_an_invalid_key_refuses_the_file() {
    let (_, one) = key("key-one", 1);
    let (_, again) = key("key-one", 2);
    let error = FileKeys::load(key_file(&json!({"keys": [one, again]})), ISSUER)
        .expect_err("a repeated key ID refuses the file");
    assert_eq!(error.error_type(), SessionErrorType::InvalidKey);

    let mut invalid = one.clone();
    invalid.alg = "Ed25519".into();
    let error = FileKeys::load(key_file(&json!({"keys": [invalid]})), ISSUER)
        .expect_err("a key off the Ed25519 profile refuses the file");
    assert_eq!(error.error_type(), SessionErrorType::InvalidKey);

    let error = FileKeys::load(key_file(&json!({"keys": [one], "extra": 1})), ISSUER)
        .expect_err("an unknown member refuses the file");
    assert_eq!(error.error_type(), SessionErrorType::KeyFile);

    let error = FileKeys::load(
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("missing.json"),
        ISSUER,
    )
    .expect_err("a missing file is refused");
    assert_eq!(error.error_type(), SessionErrorType::KeyFile);
}
