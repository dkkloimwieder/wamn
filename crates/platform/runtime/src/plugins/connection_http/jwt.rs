//! RFC 7523 client assertions for host-owned token requests.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::signature;
use rustls::pki_types::{PrivateKeyDer, pem::PemObject as _};
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

#[derive(Clone, Copy, Deserialize, serde::Serialize)]
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
    let key = Zeroizing::new(
        PrivateKeyDer::from_pem_slice(pem.as_bytes()).map_err(|_| failure("key", "invalid PEM"))?,
    );
    let scope = (!parameters.scopes.is_empty()).then(|| {
        parameters.scopes.join(match parameters.scope_separator {
            ScopeSeparator::Space => " ",
            ScopeSeparator::Comma => ",",
        })
    });
    let header = serde_json::json!({
        "alg": parameters.algorithm, "kid": parameters.key_id, "typ": "JWT",
    });
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
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&header).map_err(|_| failure("header", "serialization refused"))?
        ),
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&claims).map_err(|_| failure("claims", "serialization refused"))?
        ),
    );
    let signature = match parameters.algorithm {
        Algorithm::PS256 | Algorithm::RS256 => {
            let key = match &*key {
                PrivateKeyDer::Pkcs8(der) => {
                    signature::RsaKeyPair::from_pkcs8(der.secret_pkcs8_der())
                }
                PrivateKeyDer::Pkcs1(der) => {
                    signature::RsaKeyPair::from_der(der.secret_pkcs1_der())
                }
                _ => return Err(failure("key", "algorithm mismatch")),
            }
            .map_err(|_| failure("signature", "key or signing refused"))?;
            let encoding: &dyn signature::RsaEncoding = match parameters.algorithm {
                Algorithm::PS256 => &signature::RSA_PSS_SHA256,
                Algorithm::RS256 => &signature::RSA_PKCS1_SHA256,
                Algorithm::ES256 => unreachable!(),
            };
            let mut signed = vec![0; key.public().modulus_len()];
            key.sign(encoding, &random, input.as_bytes(), &mut signed)
                .map_err(|_| failure("signature", "key or signing refused"))?;
            signed
        }
        Algorithm::ES256 => {
            let PrivateKeyDer::Pkcs8(der) = &*key else {
                return Err(failure("key", "algorithm mismatch"));
            };
            let key = signature::EcdsaKeyPair::from_pkcs8(
                &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                der.secret_pkcs8_der(),
                &random,
            )
            .map_err(|_| failure("signature", "key or signing refused"))?;
            key.sign(&random, input.as_bytes())
                .map_err(|_| failure("signature", "key or signing refused"))?
                .as_ref()
                .to_vec()
        }
    };
    let assertion = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature));
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
