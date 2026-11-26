//! NATS v2 JWT and credentials encoding.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use data_encoding::BASE32_NOPAD;
use nkeys::{KeyPair, KeyPairType};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha512_256};
use zeroize::Zeroizing;

use super::{Error, Limits};

const HEADER: &[u8] = br#"{"typ":"JWT","alg":"ed25519-nkey"}"#;
const UNLIMITED: i64 = -1;

#[derive(Serialize)]
struct Claims<'a, T> {
    #[serde(flatten)]
    common: Common<'a>,
    nats: T,
}

#[derive(Serialize)]
struct Common<'a> {
    #[serde(skip_serializing_if = "String::is_empty")]
    jti: String,
    iat: i64,
    iss: &'a str,
    name: &'a str,
    sub: &'a str,
}

#[derive(Serialize)]
struct Operator<'a> {
    #[serde(rename = "type")]
    claim_type: &'static str,
    version: u8,
    system_account: &'a str,
    signing_keys: [&'a str; 1],
    strict_signing_key_usage: bool,
}

#[derive(Serialize)]
struct Account<'a> {
    #[serde(rename = "type")]
    claim_type: &'static str,
    version: u8,
    limits: AccountLimits,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    signing_keys: &'a [String],
}

#[derive(Serialize)]
struct AccountLimits {
    #[serde(flatten)]
    adjustable: Limits,
    data: i64,
    leaf: i64,
    imports: i64,
    exports: i64,
    disallow_bearer: bool,
    mem_storage: i64,
}

pub(super) const PLATFORM_LIMITS: Limits = Limits {
    conn: UNLIMITED,
    subs: UNLIMITED,
    payload: UNLIMITED,
    consumer: 0,
    streams: 0,
    disk_storage: 0,
};

#[derive(Deserialize)]
pub(crate) struct AccountRevision {
    pub iat: i64,
    nats: RevisionClaims,
}

impl AccountRevision {
    pub(crate) fn limits(&self) -> Limits {
        self.nats.limits
    }
}

#[derive(Deserialize)]
struct RevisionClaims {
    limits: Limits,
}

pub(crate) fn account_revision(jwt: &[u8]) -> Result<Option<AccountRevision>, Error> {
    if jwt.is_empty() {
        return Ok(None);
    }
    let mut parts = jwt.splitn(3, |&byte| byte == b'.');
    let (Some(_), Some(payload), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Error::new(
            "decode NATS resolver account JWT",
            std::io::Error::other("expected a compact JWT"),
        ));
    };
    // The full resolver has already validated the signature before storing the JWT.
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|source| Error::new("decode NATS resolver account JWT payload", source))?;
    serde_json::from_slice(&decoded)
        .map(Some)
        .map_err(|source| Error::new("decode NATS resolver account claims", source))
}

#[derive(Serialize)]
struct User<'a> {
    #[serde(rename = "type")]
    claim_type: &'static str,
    version: u8,
    subs: i64,
    data: i64,
    payload: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    issuer_account: Option<&'a str>,
    #[serde(flatten)]
    permissions: &'a Permissions,
}

/// Subjects a user may publish or subscribe to, as specified by NATS.
#[derive(Debug, Serialize)]
pub struct SubjectPermission {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

/// Permission to publish to a reply subject on a received request.
#[derive(Debug, Serialize)]
pub struct ResponsePermission {
    /// Maximum replies per received request.
    pub max: u32,
    /// Reply window in nanoseconds, the Go duration used by NATS.
    pub ttl: u64,
}

/// The publish, subscribe and response permissions of a NATS user.
#[derive(Debug, Serialize)]
pub struct Permissions {
    #[serde(rename = "pub")]
    pub pub_: SubjectPermission,
    pub sub: SubjectPermission,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resp: Option<ResponsePermission>,
}

pub(super) fn operator(
    root: &KeyPair,
    signing: &KeyPair,
    system_account: &str,
    issued_at: i64,
) -> Result<String, Error> {
    encode(
        root,
        &root.public_key(),
        "wamn",
        issued_at,
        Operator {
            claim_type: "operator",
            version: 2,
            system_account,
            signing_keys: [&signing.public_key()],
            strict_signing_key_usage: true,
        },
    )
}

pub(super) fn account(
    issuer: &KeyPair,
    subject: &str,
    name: &str,
    issued_at: i64,
    limits: Limits,
    signing_keys: &[String],
) -> Result<String, Error> {
    encode(
        issuer,
        subject,
        name,
        issued_at,
        Account {
            claim_type: "account",
            version: 2,
            limits: AccountLimits {
                adjustable: limits,
                data: UNLIMITED,
                leaf: 0,
                imports: 0,
                exports: 0,
                disallow_bearer: true,
                mem_storage: 0,
            },
            signing_keys,
        },
    )
}

pub(super) fn user(
    issuer: &KeyPair,
    issuer_account: Option<&str>,
    subject: &str,
    name: &str,
    issued_at: i64,
    permissions: &Permissions,
) -> Result<String, Error> {
    if !is_user_public_key(subject) {
        return Err(Error::invalid_user_key());
    }
    encode(
        issuer,
        subject,
        name,
        issued_at,
        User {
            claim_type: "user",
            version: 2,
            subs: UNLIMITED,
            data: UNLIMITED,
            payload: UNLIMITED,
            issuer_account,
            permissions,
        },
    )
}

/// Check that a caller provided the canonical public NKey of a NATS user.
pub fn is_user_public_key(subject: &str) -> bool {
    KeyPair::from_public_key(subject)
        .is_ok_and(|key| key.key_pair_type() == KeyPairType::User && key.public_key() == subject)
}

/// Encode a signed user JWT and its local key as a NATS credentials file.
pub fn credentials(jwt: &str, key: &KeyPair) -> Result<Zeroizing<String>, Error> {
    let seed = Zeroizing::new(
        key.seed()
            .map_err(|source| Error::new("encode NATS seed", source))?,
    );
    Ok(Zeroizing::new(format!(
        "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n         ************************* IMPORTANT *************************\n         NKEY Seed printed below can be used to sign and prove identity.\n         NKEYs are sensitive and should be treated as secrets.\n\n         -----BEGIN USER NKEY SEED-----\n{}\n------END USER NKEY SEED------\n\n         *************************************************************\n",
        seed.as_str()
    )))
}

pub(super) fn delete_account(
    key: &KeyPair,
    account: &str,
    issued_at: i64,
) -> Result<String, Error> {
    // The resolver requires a generic JWT self-signed by a trusted operator key.
    let subject = key.public_key();
    encode(
        key,
        &subject,
        "delete-account",
        issued_at,
        serde_json::json!({
            "accounts": [account],
            "version": 2
        }),
    )
}

fn encode<T: Serialize>(
    key: &KeyPair,
    subject: &str,
    name: &str,
    issued_at: i64,
    nats: T,
) -> Result<String, Error> {
    let issuer = key.public_key();
    let mut common = Common {
        jti: String::new(),
        iat: issued_at,
        iss: &issuer,
        name,
        sub: subject,
    };
    // The Go reference hashes ClaimsData, with the empty jti omitted.
    let hash_input = serde_json::to_vec(&common)
        .map_err(|source| Error::new("encode NATS common claims", source))?;
    common.jti = BASE32_NOPAD.encode(&Sha512_256::digest(hash_input));
    let claims = serde_json::to_vec(&Claims { common, nats })
        .map_err(|source| Error::new("encode NATS claims", source))?;
    let mut token = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(HEADER),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let signature = key
        .sign(token.as_bytes())
        .map_err(|source| Error::new("sign NATS claims", source))?;
    token.push('.');
    URL_SAFE_NO_PAD.encode_string(signature, &mut token);
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_streams_and_disk_scale_with_the_project_count() {
        let op = KeyPair::new_operator();
        let account = KeyPair::new_account();
        let configured = super::super::EnvLimits {
            connections: 52,
            consumers: 1400,
            subscriptions: 8000,
            payload_bytes: 512 * 1024,
            streams_per_project: 3,
            disk_bytes_per_project: 512 * 1024 * 1024,
        };
        let limits = super::super::env_limits(&configured, 3).unwrap();
        let jwt = super::account(&op, &account.public_key(), "env", 1, limits, &[]).unwrap();
        let revision = account_revision(jwt.as_bytes()).unwrap().unwrap();
        let payload = jwt.split('.').nth(1).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        assert_eq!(claims["nats"]["limits"]["data"], UNLIMITED);
        assert_eq!(
            revision.limits(),
            Limits {
                conn: configured.connections,
                consumer: configured.consumers,
                subs: configured.subscriptions,
                payload: configured.payload_bytes,
                streams: 9,
                disk_storage: 1536 * 1024 * 1024,
            }
        );
    }
}
