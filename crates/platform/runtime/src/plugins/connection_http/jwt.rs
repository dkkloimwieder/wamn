//! RFC 7523 client assertions for host-owned token requests.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{EncodingKey, Header};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{ConnectionError, Oauth2Request};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameters {
    client_id: String,
    token_url: String,
    token_authority: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    key_id: String,
    algorithm: Algorithm,
    #[serde(default)]
    scope_separator: ScopeSeparator,
}

#[derive(Clone, Copy, Deserialize)]
enum Algorithm {
    PS256,
    ES256,
    RS256,
}

#[derive(Default, Deserialize)]
enum ScopeSeparator {
    #[default]
    #[serde(rename = " ")]
    Space,
    #[serde(rename = ",")]
    Comma,
}

/// Build and sign before dispatch, with a clock supplied by the host.
pub(super) fn request(
    parameters: &serde_json::Value,
    material: Option<&str>,
    now: i64,
) -> Result<Oauth2Request, ConnectionError> {
    build_request(parameters, material, now).map_err(|_| ConnectionError::CredentialUnavailable)
}

// Fixed source text keeps private values out of error diagnostics.
#[derive(Debug)]
struct Error {
    operation: &'static str,
    source: &'static str,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "OAuth assertion {} failed: {}",
            self.operation, self.source
        )
    }
}

impl std::error::Error for Error {}

fn failure(operation: &'static str, source: &'static str) -> Error {
    Error { operation, source }
}

fn build_request(
    parameters: &serde_json::Value,
    material: Option<&str>,
    now: i64,
) -> Result<Oauth2Request, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Credential {
        private_key: String,
    }
    let parameters: Parameters = serde_json::from_value(parameters.clone())
        .map_err(|_| failure("parameters", "invalid shape"))?;
    let credential: Credential =
        serde_json::from_str(material.ok_or_else(|| failure("credential", "missing"))?)
            .map_err(|_| failure("credential", "invalid shape"))?;
    let pem = Zeroizing::new(credential.private_key);
    let scope = (!parameters.scopes.is_empty()).then(|| {
        parameters.scopes.join(match parameters.scope_separator {
            ScopeSeparator::Space => " ",
            ScopeSeparator::Comma => ",",
        })
    });
    let algorithm = match parameters.algorithm {
        Algorithm::RS256 => jsonwebtoken::Algorithm::RS256,
        Algorithm::PS256 => jsonwebtoken::Algorithm::PS256,
        Algorithm::ES256 => jsonwebtoken::Algorithm::ES256,
    };
    let mut header = Header::new(algorithm);
    header.kid = Some(parameters.key_id);
    let random = SystemRandom::new();
    // A fresh 256-bit identifier keeps assertions distinct even within one second.
    let mut nonce = [0_u8; 32];
    random
        .fill(&mut nonce)
        .map_err(|_| failure("jti", "random source unavailable"))?;
    let mut claims = serde_json::json!({
        "iss": parameters.client_id, "aud": parameters.token_url,
        "iat": now, "exp": now.checked_add(300).ok_or_else(|| failure("expiry", "clock out of range"))?,
        "jti": URL_SAFE_NO_PAD.encode(nonce),
    });
    if let Some(scope) = &scope {
        claims["scope"] = scope.clone().into();
    }
    let key = match parameters.algorithm {
        Algorithm::RS256 | Algorithm::PS256 => EncodingKey::from_rsa_pem(pem.as_bytes()),
        Algorithm::ES256 => EncodingKey::from_ec_pem(pem.as_bytes()),
    }
    .map_err(|_| failure("key", "invalid PEM or algorithm mismatch"))?;
    let assertion = jsonwebtoken::encode(&header, &claims, &key)
        .map_err(|_| failure("signature", "key or signing refused"))?;
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("grant_type", "client_credentials");
    form.append_pair(
        "client_assertion_type",
        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
    );
    form.append_pair("client_assertion", &assertion);
    if let Some(scope) = scope {
        form.append_pair("scope", &scope);
    }
    Ok(Oauth2Request {
        token_url: parameters.token_url,
        token_authority: parameters.token_authority,
        body: form.finish(),
        authorization: None,
    })
}

#[cfg(test)]
pub(super) mod tests;
