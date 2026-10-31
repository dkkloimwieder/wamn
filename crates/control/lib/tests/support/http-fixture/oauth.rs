//! NetSuite token assertions use the independent public key from the runtime fixture.

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature;
use serde_json::{Value, json};

pub const TOKEN_PATH: &str = "/services/rest/auth/oauth2/v1/token";
pub const BEARER: &str = "Bearer fixture-netsuite-token";

pub fn verify_assertion(body: &[u8], audience: &str, now: u64) -> bool {
    let verify = || -> Option<()> {
        let pairs: Vec<_> = url::form_urlencoded::parse(body).collect();
        let form: HashMap<_, _> = pairs
            .iter()
            .map(|(k, v)| (k.as_ref(), v.as_ref()))
            .collect();
        if pairs.len() != 4
            || form.len() != 4
            || form.get("grant_type") != Some(&"client_credentials")
            || form.get("client_assertion_type")
                != Some(&"urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
            || form.get("scope") != Some(&"rest_webservices,restlets")
        {
            return None;
        }
        let parts: Vec<_> = form.get("client_assertion")?.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).ok()?).ok()?;
        if header != json!({"alg":"ES256","kid":"certificate-id","typ":"JWT"}) {
            return None;
        }
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).ok()?).ok()?;
        let issued = claims["iat"].as_u64()?;
        let jti = claims["jti"].as_str()?;
        if issued > now
            || now - issued > 60
            || URL_SAFE_NO_PAD.decode(jti).ok()?.len() != 32
            || claims
                != json!({"iss":"fixture-client","aud":audience,"scope":"rest_webservices,restlets",
                "iat":issued,"exp":issued.checked_add(300)?,"jti":jti})
        {
            return None;
        }
        signature::UnparsedPublicKey::new(
            &signature::ECDSA_P256_SHA256_FIXED,
            include_bytes!("fixtures/p256-public.sec1"),
        )
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).ok()?,
        )
        .ok()
    };
    verify().is_some()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    pub fn token_form(audience: &str, change: impl FnOnce(&mut Value, &mut Value)) -> String {
        let mut header = json!({"alg":"ES256","kid":"certificate-id","typ":"JWT"});
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut claims = json!({"iss":"fixture-client","aud":audience,"scope":"rest_webservices,restlets",
            "iat":now,"exp":now+300,"jti":URL_SAFE_NO_PAD.encode([1_u8;32])});
        change(&mut header, &mut claims);
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
        );
        let pem: String = include_str!("fixtures/p256.pem")
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let random = ring::rand::SystemRandom::new();
        let key = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &STANDARD.decode(pem).unwrap(),
            &random,
        )
        .unwrap();
        let signed = key.sign(&random, input.as_bytes()).unwrap();
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair(
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            )
            .append_pair("scope", "rest_webservices,restlets")
            .append_pair(
                "client_assertion",
                &format!("{input}.{}", URL_SAFE_NO_PAD.encode(signed.as_ref())),
            )
            .finish()
    }

    #[test]
    fn signature_claims_and_netsuite_refusals() {
        let audience = "http://http-fixture:8080/services/rest/auth/oauth2/v1/token";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let good = token_form(audience, |_, _| {});
        assert!(verify_assertion(good.as_bytes(), audience, now + 1));
        assert!(!verify_assertion(
            good.as_bytes(),
            "http://foreign/token",
            now + 1
        ));
        for (field, value) in [
            ("iss", json!("foreign-client")),
            ("aud", json!("http://foreign/token")),
            ("scope", json!("rest_webservices restlets")),
            ("exp", json!(now + 3600)),
            ("iat", json!(now + 100)),
            ("jti", json!("short")),
        ] {
            let body = token_form(audience, |_, claims| claims[field] = value);
            assert!(
                !verify_assertion(body.as_bytes(), audience, now + 1),
                "{field}"
            );
        }
        for (field, value) in [("alg", "RS256"), ("kid", "foreign-certificate")] {
            let body = token_form(audience, |header, _| header[field] = value.into());
            assert!(
                !verify_assertion(body.as_bytes(), audience, now + 1),
                "{field}"
            );
        }
        let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(good.as_bytes())
            .into_owned()
            .collect();
        let assertion = &mut pairs
            .iter_mut()
            .find(|(k, _)| k == "client_assertion")
            .unwrap()
            .1;
        let start = assertion.rfind('.').unwrap() + 1;
        assertion.replace_range(
            start..start + 1,
            if &assertion[start..start + 1] == "A" {
                "B"
            } else {
                "A"
            },
        );
        let bad_signature = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&pairs)
            .finish();
        assert!(!verify_assertion(
            bad_signature.as_bytes(),
            audience,
            now + 1
        ));
        assert!(!verify_assertion(
            b"client_assertion=malformed",
            audience,
            now + 1
        ));
        let space_scope = good.replace("rest_webservices%2Crestlets", "rest_webservices+restlets");
        assert!(!verify_assertion(space_scope.as_bytes(), audience, now + 1));
    }
}
