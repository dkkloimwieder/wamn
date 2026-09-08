//! Exact fixed-profile tests; these are not route-admission or cache proofs.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::{Value, json};
use wamn_platform_identity::{
    IdentityErrorKind,
    session_keys::{PublicSessionKey, SessionJwks, decode_public_key},
    session_token::{
        SessionClaims, SessionScope, minting_times, session_key_id, validate_session_age,
        verify_session_token,
    },
};

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
        alg: "Ed25519".into(),
        r#use: "sig".into(),
        x: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    };
    let header = json!({"alg":"Ed25519", "typ":"wamn-session+jwt", "kid":"key-one"});
    let claims = json!({
        "iss":"https://identity.internal", "sub":"ed7056a9-5639-455f-9640-4678458794c0",
        "org":"org-a", "aud":"environment-id-a-dev", "roles":["purchase-reader"],
        "iat":1000, "exp":1900, "jti":"token-one"
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

fn scope() -> SessionScope<'static> {
    SessionScope {
        issuer: "https://identity.internal",
        org: "org-a",
        audience: "environment-id-a-dev",
    }
}

#[test]
fn public_jwk_wire_is_exact_and_rejects_private_or_missing_fields() {
    let (_, key, _, _) = fixture();
    let bytes = decode_public_key(&key).unwrap();
    assert_eq!(
        hex::encode(bytes),
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
    let wire = serde_json::to_value(SessionJwks {
        keys: vec![key.clone()],
    })
    .unwrap();
    assert_eq!(
        wire,
        json!({"keys":[{
            "kid":"key-one", "kty":"OKP", "crv":"Ed25519", "alg":"Ed25519", "use":"sig",
            "x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"
        }]})
    );
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

#[test]
fn public_key_decoding_pins_every_algorithm_field_and_canonical_bytes() {
    let (_, key, _, _) = fixture();
    for (field, value) in [
        ("kid", ""),
        ("kty", "RSA"),
        ("crv", "Ed448"),
        ("alg", "EdDSA"),
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

#[test]
fn fixed_ed25519_token_verifies_exact_scope_and_signature() {
    let (pair, key, header, claims) = fixture();
    let token = signed(&pair, &header, &claims);
    let verified = verify_session_token(&token, &key, scope(), 1000).unwrap();
    assert_eq!(serde_json::to_value(verified).unwrap(), claims);
    assert_eq!(session_key_id(&token).unwrap(), "key-one");
    for wrong_scope in [
        SessionScope {
            issuer: "https://other.internal",
            ..scope()
        },
        SessionScope {
            org: "org-b",
            ..scope()
        },
        SessionScope {
            audience: "environment-id-a-prod",
            ..scope()
        },
        SessionScope {
            org: "org-b",
            audience: "environment-id-b-dev",
            ..scope()
        },
    ] {
        assert!(verify_session_token(&token, &key, wrong_scope, 1000).is_err());
    }
    let different_key = PublicSessionKey {
        x: URL_SAFE_NO_PAD.encode([3; 32]),
        ..key.clone()
    };
    assert!(verify_session_token(&token, &different_key, scope(), 1000).is_err());
    let different_kid = PublicSessionKey {
        kid: "key-two".into(),
        ..key.clone()
    };
    assert!(verify_session_token(&token, &different_kid, scope(), 1000).is_err());
    let mut tampered = token.into_bytes();
    let final_byte = tampered.len() - 2;
    tampered[final_byte] = if tampered[final_byte] == b'A' {
        b'B'
    } else {
        b'A'
    };
    assert!(
        verify_session_token(&String::from_utf8(tampered).unwrap(), &key, scope(), 1000).is_err()
    );
}

#[test]
fn authenticated_header_and_claim_variants_refuse_indistinguishably() {
    let (pair, key, header, claims) = fixture();
    for (field, value) in [
        ("alg", "EdDSA"),
        ("alg", "none"),
        ("typ", "JWT"),
        ("kid", "key-two"),
    ] {
        let mut malformed = header.clone();
        malformed[field] = json!(value);
        let error = verify_session_token(&signed(&pair, &malformed, &claims), &key, scope(), 1000)
            .unwrap_err();
        assert_eq!(error.to_string(), "session token refused");
        assert_eq!(error.kind(), IdentityErrorKind::InvalidInput);
    }
    for field in ["jku", "jwk", "b64", "crit"] {
        let mut malformed = header.clone();
        malformed[field] = json!(false);
        assert!(
            verify_session_token(&signed(&pair, &malformed, &claims), &key, scope(), 1000).is_err()
        );
    }
    for field in ["iss", "sub", "org", "aud", "roles", "exp", "iat", "jti"] {
        let mut malformed = claims.clone();
        malformed.as_object_mut().unwrap().remove(field);
        assert!(
            verify_session_token(&signed(&pair, &header, &malformed), &key, scope(), 1000).is_err(),
            "missing {field}"
        );
    }
    for (field, value) in [
        ("sub", json!("not-a-uuid")),
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
            verify_session_token(&signed(&pair, &header, &malformed), &key, scope(), 1000).is_err(),
            "invalid {field}"
        );
    }
    let duplicate =
        br#"{"alg":"Ed25519","alg":"Ed25519","typ":"wamn-session+jwt","kid":"key-one"}"#;
    assert!(
        verify_session_token(
            &signed_bytes(&pair, duplicate, &serde_json::to_vec(&claims).unwrap()),
            &key,
            scope(),
            1000
        )
        .is_err()
    );
    let mut no_roles = claims;
    no_roles["roles"] = json!([]);
    let verified =
        verify_session_token(&signed(&pair, &header, &no_roles), &key, scope(), 1000).unwrap();
    assert!(
        verified.roles.is_empty(),
        "empty roles must not invent authority"
    );
}

#[test]
fn age_rules_cover_exact_boundaries_and_checked_arithmetic() {
    for (iat, exp, now) in [
        (1000, 1900, 1000),
        (1030, 1900, 1000),
        (1000, 1900, 1929),
        (1000, 1001, 1000),
    ] {
        validate_session_age(iat, exp, now).unwrap();
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
            validate_session_age(iat, exp, now).is_err(),
            "accepted ({iat},{exp},{now})"
        );
    }
}

#[test]
fn delayed_minting_keeps_original_deadline_and_refuses_late_results() {
    assert_eq!(minting_times(1000, 1000).unwrap(), (1000, 1900));
    assert_eq!(minting_times(1000, 1800).unwrap(), (1800, 1900));
    assert_eq!(minting_times(1000, 1899).unwrap(), (1899, 1900));
    for (started, now) in [
        (1000, 1900),
        (1000, 1901),
        (1000, 999),
        (i64::MAX, i64::MAX),
        (-1, 0),
    ] {
        assert!(minting_times(started, now).is_err());
    }
}

#[test]
fn token_serialization_has_all_required_claims_and_rejects_duplicate_claims() {
    let (pair, key, header, claims) = fixture();
    let typed: SessionClaims = serde_json::from_value(claims.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), claims);
    let mut duplicated = serde_json::to_string(&claims).unwrap();
    duplicated.pop();
    duplicated.push_str(",\"exp\":1900}");
    let token = signed_bytes(
        &pair,
        &serde_json::to_vec(&header).unwrap(),
        duplicated.as_bytes(),
    );
    assert!(verify_session_token(&token, &key, scope(), 1000).is_err());
    for token in ["", "a.b", "a.b.c.d", ".b.c", "a..c", "a.b."] {
        assert!(session_key_id(token).is_err());
        assert!(verify_session_token(token, &key, scope(), 1000).is_err());
    }
}
