//! Exact fixed-profile tests; these are not route-admission or cache tests.

use crate::keys::{PublicSessionKey, SessionJwks, decode_public_key};
use crate::test_util::{Audience as SessionAudience, verify};
use crate::token::SessionClaims;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::{Value, json};

fn fixture() -> (Ed25519KeyPair, PublicSessionKey, Value, Value) {
    // RFC 8032 test-one seed and public key, used only as deterministic test data.
    let seed =
        hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60").unwrap();
    let public =
        hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
    let pair = Ed25519KeyPair::from_seed_and_public_key(&seed, &public).unwrap();
    let key = PublicSessionKey {
        kid: "key-one".into(),
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        alg: "EdDSA".into(),
        r#use: "sig".into(),
        x: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    };
    let header = json!({"alg":"EdDSA", "typ":"wamn-session+jwt", "kid":"key-one"});
    let claims = json!({
        "iss":"https://identity.internal", "sub":"ed7056a9-5639-455f-9640-4678458794c0",
        "org":"org-a", "aud":"environment-id-a-dev", "roles":["purchase-reader"],
        "iat":1000, "exp":1900, "authority": {"login": "ed7056a9-5639-455f-9640-4678458794c0"}, "jti":"token-one"
    });
    (pair, key, header, claims)
}

fn signed_bytes(pair: &Ed25519KeyPair, header: &[u8], body: &[u8]) -> String {
    let message = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(body)
    );
    let signature = pair.sign(message.as_bytes());
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
}

fn signed(pair: &Ed25519KeyPair, header: &Value, claims: &Value) -> String {
    signed_bytes(
        pair,
        &serde_json::to_vec(header).unwrap(),
        &serde_json::to_vec(claims).unwrap(),
    )
}

fn audience() -> SessionAudience<'static> {
    SessionAudience {
        issuer: "https://identity.internal",
        org: "org-a",
        audience: "environment-id-a-dev",
    }
}

#[tokio::test]
async fn public_jwk_wire_is_exact_and_rejects_private_or_missing_fields() {
    let (_, key, _, _) = fixture();
    let bytes = decode_public_key(&key).unwrap();
    assert_eq!(
        hex::encode(bytes),
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
    let wire = serde_json::to_value(SessionJwks { keys: vec![key] }).unwrap();
    for name in ["kid", "kty", "crv", "alg", "use", "x"] {
        let mut malformed = wire.clone();
        malformed["keys"][0].as_object_mut().unwrap().remove(name);
        assert!(
            serde_json::from_value::<SessionJwks>(malformed).is_err(),
            "missing {name}"
        );
    }
    for name in ["d", "private_pkcs8", "key_ops", "unknown"] {
        let mut malformed = wire.clone();
        malformed["keys"][0][name] = json!("private-marker");
        assert!(
            serde_json::from_value::<SessionJwks>(malformed).is_err(),
            "accepted {name}"
        );
    }
    let mut malformed = wire;
    malformed["private"] = json!("private-marker");
    assert!(serde_json::from_value::<SessionJwks>(malformed).is_err());
}

#[tokio::test]
async fn public_key_decoding_pins_every_algorithm_field_and_canonical_bytes() {
    let (_, key, _, _) = fixture();
    for (field, value) in [
        ("kid", ""),
        ("kty", "RSA"),
        ("crv", "Ed448"),
        ("alg", "Ed25519"),
        ("use", "enc"),
    ] {
        let mut wire = serde_json::to_value(&key).unwrap();
        wire[field] = json!(value);
        let malformed: PublicSessionKey = serde_json::from_value(wire).unwrap();
        assert!(
            decode_public_key(&malformed).is_err(),
            "accepted {field}={value}"
        );
    }
    for x in [
        URL_SAFE_NO_PAD.encode([0; 31]),
        URL_SAFE_NO_PAD.encode([0; 33]),
        format!("{}=", key.x),
        "not+url/safe".into(),
    ] {
        assert!(decode_public_key(&PublicSessionKey { x, ..key.clone() }).is_err());
    }
    let mut noncanonical = key.x.clone();
    noncanonical.pop();
    noncanonical.push('p'); // Same upper four bits, nonzero unused trailing bits.
    assert!(
        decode_public_key(&PublicSessionKey {
            x: noncanonical,
            ..key
        })
        .is_err()
    );
}

#[tokio::test]
async fn fixed_ed25519_token_verifies_exact_audience_and_signature() {
    let (pair, key, header, claims) = fixture();
    let token = signed(&pair, &header, &claims);
    let verified = verify(&token, &key, audience(), 1000).await.unwrap();
    assert_eq!(serde_json::to_value(verified).unwrap(), claims);
    for wrong_audience in [
        SessionAudience {
            issuer: "https://other.internal",
            ..audience()
        },
        SessionAudience {
            org: "org-b",
            ..audience()
        },
        SessionAudience {
            audience: "environment-id-a-prod",
            ..audience()
        },
        SessionAudience {
            org: "org-b",
            audience: "environment-id-b-dev",
            ..audience()
        },
    ] {
        assert!(verify(&token, &key, wrong_audience, 1000).await.is_err());
    }
    let different_key = PublicSessionKey {
        x: URL_SAFE_NO_PAD.encode([3; 32]),
        ..key.clone()
    };
    assert!(
        verify(&token, &different_key, audience(), 1000)
            .await
            .is_err()
    );
    let different_kid = PublicSessionKey {
        kid: "key-two".into(),
        ..key.clone()
    };
    assert!(
        verify(&token, &different_kid, audience(), 1000)
            .await
            .is_err()
    );
    let mut tampered = token.into_bytes();
    let final_byte = tampered.len() - 2;
    tampered[final_byte] = if tampered[final_byte] == b'A' {
        b'B'
    } else {
        b'A'
    };
    assert!(
        verify(
            &String::from_utf8(tampered).unwrap(),
            &key,
            audience(),
            1000
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn authenticated_header_and_claim_variants_refuse_indistinguishably() {
    let (pair, key, header, claims) = fixture();
    for (field, value) in [
        ("alg", "Ed25519"),
        ("alg", "none"),
        ("typ", "JWT"),
        ("kid", "key-two"),
    ] {
        let mut malformed = header.clone();
        malformed[field] = json!(value);
        verify(&signed(&pair, &malformed, &claims), &key, audience(), 1000)
            .await
            .unwrap_err();
    }
    for field in ["jku", "jwk", "b64", "crit"] {
        let mut malformed = header.clone();
        malformed[field] = json!(false);
        assert!(
            verify(&signed(&pair, &malformed, &claims), &key, audience(), 1000)
                .await
                .is_err()
        );
    }
    for field in [
        "iss",
        "sub",
        "org",
        "aud",
        "roles",
        "exp",
        "iat",
        "jti",
        "authority",
    ] {
        let mut malformed = claims.clone();
        malformed.as_object_mut().unwrap().remove(field);
        assert!(
            verify(&signed(&pair, &header, &malformed), &key, audience(), 1000)
                .await
                .is_err(),
            "missing {field}"
        );
    }
    for (field, value) in [
        ("sub", json!("not-a-uuid")),
        ("authority", json!({"login": "not-a-uuid"})),
        (
            "authority",
            json!({"unknown": "ed7056a9-5639-455f-9640-4678458794c0"}),
        ),
        (
            "authority",
            json!({"login": "ed7056a9-5639-455f-9640-4678458794c0", "pat": "ed7056a9-5639-455f-9640-4678458794c0"}),
        ),
        ("roles", json!(["Not A Role"])),
        ("roles", json!(["Purchase-reader"])),
        ("roles", json!([" purchase-reader "])),
        ("iat", json!(1000.5)),
        ("aud", json!(["environment-id-a-dev"])),
        ("exp", json!(1901)),
        ("iat", json!(1031)),
    ] {
        let mut malformed = claims.clone();
        malformed[field] = value;
        assert!(
            verify(&signed(&pair, &header, &malformed), &key, audience(), 1000)
                .await
                .is_err(),
            "invalid {field}"
        );
    }
    let duplicate = br#"{"alg":"EdDSA","alg":"EdDSA","typ":"wamn-session+jwt","kid":"key-one"}"#;
    assert!(
        verify(
            &signed_bytes(&pair, duplicate, &serde_json::to_vec(&claims).unwrap()),
            &key,
            audience(),
            1000
        )
        .await
        .is_err()
    );
    let mut no_roles = claims;
    no_roles["roles"] = json!([]);
    let verified = verify(&signed(&pair, &header, &no_roles), &key, audience(), 1000)
        .await
        .unwrap();
    assert!(
        verified.roles.is_empty(),
        "empty roles must not invent authority"
    );
}

#[tokio::test]
async fn age_rules_cover_exact_boundaries_and_overflow() {
    let (pair, key, header, mut claims) = fixture();
    for (iat, exp, now) in [
        (1000, 1900, 1000),
        (1030, 1900, 1000),
        (1000, 1900, 1929),
        (1000, 1001, 1000),
    ] {
        claims["iat"] = json!(iat);
        claims["exp"] = json!(exp);
        verify(&signed(&pair, &header, &claims), &key, audience(), now)
            .await
            .unwrap();
    }
    for (iat, exp, now) in [
        (1000, 1901, 1000),
        (1000, 1000, 1000),
        (1001, 1000, 1000),
        (1031, 1900, 1000),
        (1000, 1900, 1930),
        (1000, 1900, 1931),
        (i64::MAX - 1, i64::MAX, i64::MAX),
        (i64::MIN, i64::MAX, 0),
        (-1, 899, 0),
    ] {
        assert!(
            verify(
                &signed(&pair, &header, &{
                    claims["iat"] = json!(iat);
                    claims["exp"] = json!(exp);
                    claims.clone()
                }),
                &key,
                audience(),
                now
            )
            .await
            .is_err(),
            "accepted ({iat},{exp},{now})"
        );
    }
}

#[tokio::test]
async fn token_serialization_has_all_required_claims_and_rejects_duplicate_claims() {
    let (pair, key, header, claims) = fixture();
    let typed: SessionClaims = serde_json::from_value(claims.clone()).unwrap();
    assert!(typed.validate().is_ok());
    let mut duplicated = serde_json::to_string(&claims).unwrap();
    duplicated.pop();
    duplicated.push_str(",\"exp\":1900}");
    let token = signed_bytes(
        &pair,
        &serde_json::to_vec(&header).unwrap(),
        duplicated.as_bytes(),
    );
    assert!(verify(&token, &key, audience(), 1000).await.is_err());
    for token in ["", "a.b", "a.b.c.d", ".b.c", "a..c", "a.b."] {
        assert!(verify(token, &key, audience(), 1000).await.is_err());
    }
}

#[tokio::test]
async fn csrf_claim_is_optional_and_refuses_anything_but_a_sha256_hex_digest() {
    let (pair, key, header, mut claims) = fixture();
    let digest = "a".repeat(64);
    claims["csrf"] = json!(digest);
    let token = signed(&pair, &header, &claims);
    let verified = verify(&token, &key, audience(), 1000).await.unwrap();
    assert_eq!(verified.csrf.as_deref(), Some(digest.as_str()));
    assert_eq!(serde_json::to_value(verified).unwrap(), claims);
    for malformed in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        claims["csrf"] = json!(malformed);
        let token = signed(&pair, &header, &claims);
        assert!(verify(&token, &key, audience(), 1000).await.is_err());
    }
}
