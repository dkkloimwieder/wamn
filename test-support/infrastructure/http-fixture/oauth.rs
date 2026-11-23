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
