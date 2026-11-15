//! Assertion contracts use public keys independent of the signer.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;

pub(crate) const RSA: &str = include_str!("fixtures/rsa.pem");
pub(crate) const P256: &str = include_str!("fixtures/p256.pem");

pub(crate) fn parameters(algorithm: &str, url: &str) -> Value {
    json!({"client_id":"client", "token_url":url, "key_id":"certificate-id",
        "algorithm":algorithm, "scopes":["a","b"]})
}

pub(crate) fn material(algorithm: &str) -> String {
    json!({"private_key":if algorithm == "ES256" { P256 } else { RSA }}).to_string()
}

#[tokio::test]
async fn fixed_clock_headers_claims_and_scope_separators() {
    fn assert_assertion(
        body: &[u8],
        algorithm: &str,
        client: &str,
        url: &str,
        scope: Option<&str>,
        now: i64,
    ) -> String {
        let form: HashMap<_, _> = url::form_urlencoded::parse(body).into_owned().collect();
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("client_credentials")
        );
        assert_eq!(
            form.get("client_assertion_type").map(String::as_str),
            Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
        );
        assert_eq!(form.get("scope").map(String::as_str), scope);
        let algorithm: jsonwebtoken::Algorithm = algorithm.parse().unwrap();
        let key = match algorithm {
            jsonwebtoken::Algorithm::ES256 => {
                jsonwebtoken::DecodingKey::from_ec_der(include_bytes!("fixtures/p256-public.sec1"))
            }
            _ => jsonwebtoken::DecodingKey::from_rsa_der(include_bytes!("fixtures/rsa-public.der")),
        };
        let mut validation = jsonwebtoken::Validation::new(algorithm);
        validation.set_issuer(&[client]);
        validation.set_audience(&[url]);
        validation.validate_exp = false;
        let token =
            jsonwebtoken::decode::<Value>(&form["client_assertion"], &key, &validation).unwrap();
        assert_eq!(token.header.kid.as_deref(), Some("certificate-id"));
        assert_eq!(token.header.typ.as_deref(), Some("JWT"));
        let claims = token.claims;
        if let Some(scope) = scope {
            assert_eq!(claims["scope"].as_str(), Some(scope));
        } else {
            assert!(claims.get("scope").is_none());
        }
        let issued = claims["iat"].as_i64().unwrap();
        assert_eq!(issued, now);
        assert_eq!(claims["exp"].as_i64().unwrap() - issued, 300);
        claims["jti"].as_str().unwrap().into()
    }

    let url = "https://example.invalid/token";
    let now = 1_800_000_000;
    for algorithm in ["PS256", "ES256", "RS256"] {
        for (separator, scope) in [
            (None, Some("a b")),
            (Some(" "), Some("a b")),
            (Some(","), Some("a,b")),
            (None, None),
        ] {
            let mut parameters = parameters(algorithm, url);
            if let Some(separator) = separator {
                parameters["scope_separator"] = separator.into();
            }
            if scope.is_none() {
                parameters["scopes"] = json!([]);
            }
            let identifiers = std::sync::Mutex::new(std::collections::HashSet::new());
            for _ in 0..2 {
                let request = request(&parameters, Some(&material(algorithm)), now).unwrap();
                request
                    .exchange(&|request: hyper::Request<Vec<u8>>| {
                        assert_eq!(request.method(), hyper::Method::POST);
                        assert_eq!(
                            request.headers()["content-type"],
                            "application/x-www-form-urlencoded"
                        );
                        assert!(!request.headers().contains_key("authorization"));
                        let identifier =
                            assert_assertion(request.body(), algorithm, "client", url, scope, now);
                        assert!(identifiers.lock().unwrap().insert(identifier));
                        Box::pin(std::future::ready(Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .header("content-type", "application/json")
                                .body(br#"{"access_token":"token","token_type":"Bearer"}"#.to_vec())
                                .unwrap(),
                        )))
                    })
                    .await
                    .unwrap();
            }
        }
    }
}

#[test]
fn malformed_pem_and_algorithm_mismatch_refuse_before_dispatch() {
    for (algorithm, pem) in [
        ("PS256", P256),
        ("RS256", P256),
        ("ES256", RSA),
        ("PS256", "not PEM"),
    ] {
        let parameters = parameters(algorithm, "https://example.invalid/token");
        let material = json!({"private_key":pem}).to_string();
        assert!(matches!(
            request(&parameters, Some(&material), 0),
            Err(ConnectionError::CredentialUnavailable)
        ));
    }
}
