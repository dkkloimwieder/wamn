//! # wamn-node-invoke — the v0 custom-node invocation protocol (5.6 / wamn-bd5)
//!
//! §5.6's v0 dispatch of a *dynamically-loaded custom node* is a **boring,
//! debuggable in-cluster HTTP hop**: the trusted flow-runner POSTs an
//! invocation envelope to a `serve-node` host that owns the node's warm
//! instance, and the node runs under the REAL frozen `wamn:node` world
//! (`docs/archive/contracts/wamn-node.wit`). This crate is the pure heart of that path so it
//! cannot drift between the two ends:
//!
//! - the **wire envelope** ([`NodeInvokeRequest`] / [`NodeInvokeResponse`]) —
//!   ctx + input + the per-invocation credential grant on the way in, the
//!   node's emission or the frozen `node-error` taxonomy on the way out;
//! - the **grant derivation** ([`granted_credentials`]) — the runner declares
//!   EXACTLY the credentials the flow's node step declared, never the project's
//!   whole set (the cjv.3 grant the serve-node host installs before dispatch);
//!
//! PURE — serde + the HMAC signing primitives, no DB / clock / wasm / network —
//! so BOTH the flowrunner GUEST (wasm32-wasip2) and the serve-node HOST link the
//! identical bytes. Runner↔node authn (wamn-fqg.22) lives HERE too — the
//! canonical signed bytes ([`sign_envelope`] / [`verify_envelope`]) are shared
//! so signer and verifier cannot drift; mTLS remains the later infra upgrade.

use std::borrow::Cow;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

// ---------------------------------------------------------------------------
// Wire envelope: runner -> serve-node (request) and back (response)
// ---------------------------------------------------------------------------

/// The internal run context the runner sends to a node host.
///
/// The frozen `wamn:node/types.run-context` retains an ABI-only
/// `idempotency-key`; this envelope deliberately omits it, and the node host
/// supplies an empty value only when constructing that frozen ABI record.
/// Secrets are also absent: the node pulls its granted credential lazily
/// through the `wamn:node/credentials` import the serve-node host links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WireRunContext {
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: u32,
    pub node_id: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
    /// The node's JSON config document (template-expanded by the runner). A
    /// `json` string, exactly as the frozen contract types it.
    pub config: String,
    /// Durable per-run context as a JSON document. This is required on every
    /// invocation so custom nodes have the same universal read surface as
    /// in-process nodes.
    pub context: String,
}

/// A node input/output payload on the wire. v0 carries only the `inline` case
/// (the frozen contract's `streamed` variant waits for the payload store,
/// 5.10); the tagged shape leaves room for it without a wire break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WirePayload {
    /// A JSON-encoded string (the frozen `payload::inline(json)` case).
    Inline(String),
}

impl WirePayload {
    /// The inline JSON string, if this is an inline payload.
    pub fn inline(&self) -> Option<&str> {
        match self {
            WirePayload::Inline(s) => Some(s),
        }
    }
}

/// The runner -> serve-node invocation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NodeInvokeRequest {
    pub ctx: WireRunContext,
    pub input: WirePayload,
    /// The credential names granted to THIS invocation — exactly the flow's
    /// node step declared ([`granted_credentials`]). The serve-node host installs
    /// this as the node's cjv.3 grant before dispatch; a `get` for anything else
    /// is `not-granted` host-side. NEVER the project's whole credential set.
    #[serde(default)]
    pub grant: Vec<String>,
}

impl NodeInvokeRequest {
    /// Encode to the JSON body POSTed to `serve-node`.
    pub fn to_json(&self) -> String {
        // A plain data struct never fails to encode.
        serde_json::to_string(self).expect("NodeInvokeRequest serializes")
    }

    /// Decode a request body received by `serve-node`.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let request: Self = serde_json::from_str(s)?;
        validate_json(&request.ctx.context)?;
        Ok(request)
    }
}

/// The node's emission on the wire (the success case), mirroring
/// `wamn:node/types`'s `emission`: the output payload plus the output port
/// (absent = `main`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WireEmission {
    pub payload: WirePayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// Optional replacement durable context. This belongs only to the success
    /// envelope; [`WireNodeError`] has no corresponding field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<String>,
}

/// A machine-readable error detail (`wamn:node/types`'s `error-detail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WireErrorDetail {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Optional structured payload as a JSON string (mirrors the WIT `json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// A throttling signal (`wamn:node/types`'s `rate-limit-detail`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WireRateLimit {
    pub detail: WireErrorDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
}

/// The frozen `node-error` taxonomy on the wire, variant for variant. The
/// runner folds retry-vs-error-path-vs-fail mechanically from this — a swapped
/// arm silently changes run semantics, so it round-trips under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireNodeError {
    Retryable(WireErrorDetail),
    RateLimited(WireRateLimit),
    Terminal(WireErrorDetail),
    InvalidInput(WireErrorDetail),
    Cancelled,
}

/// The serve-node -> runner invocation response: the node's emission, or the
/// frozen `node-error`. Tagged `ok` / `err` so a transport-level body is
/// unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeInvokeResponse {
    Ok(WireEmission),
    Err(WireNodeError),
}

impl NodeInvokeResponse {
    /// Encode to the JSON body `serve-node` returns.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("NodeInvokeResponse serializes")
    }

    /// Decode a response body received by the runner.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let response: Self = serde_json::from_str(s)?;
        if let NodeInvokeResponse::Ok(WireEmission { ctx: Some(ctx), .. }) = &response {
            validate_json(ctx)?;
        }
        Ok(response)
    }
}

fn validate_json(json: &str) -> Result<(), serde_json::Error> {
    serde_json::from_str::<serde_json::Value>(json).map(|_| ())
}

// ---------------------------------------------------------------------------
// Runner -> node authn: a SIGNED invocation envelope (wamn-fqg.22)
// ---------------------------------------------------------------------------

/// The request header carrying the lower-hex HMAC-SHA256 signature of the
/// request body (runner→node authn). ASCII-safe so it rides an HTTP header
/// unencoded.
pub const SIGNATURE_HEADER: &str = "x-wamn-signature";

/// wamn-fqg.32: the request header carrying the invocation's freshness timestamp
/// (unix seconds, ASCII decimal). Its bytes are folded into the signed message
/// (see [`sign_envelope_with_timestamp`]), so the MAC binds it and a
/// stripped/edited timestamp fails verification. Absent = a legacy (fqg.22)
/// timestamp-less envelope.
pub const TIMESTAMP_HEADER: &str = "x-wamn-timestamp";

/// The reserved credential-vault name the per-project-env HMAC signing key is
/// banked under, distributed via the EXISTING runner-credentials Secret pattern
/// (`{project: {name: secret}}` — no new Secret, no new WIT). The colon marks it
/// non-colliding with a user-authored `wamn-flow` credential name (those are
/// bare logical names); the serve-node additionally REFUSES to install it into
/// any node grant, so a custom node can never read the signing key back through
/// `wamn:node/credentials`.
pub const SIGNING_KEY_CREDENTIAL: &str = "wamn:node-invoke-signing-key";

/// wamn-fqg.30: the reserved vault name for the PREVIOUS per-project-env signing
/// key during a rotation window. A SECOND name — not a delimited two-key value —
/// keeps the existing `{project: {name: secret}}` vault shape intact: one name,
/// one opaque secret, no in-band delimiter that could collide with key bytes.
/// The serve-node accepts a signature under EITHER the current
/// [`SIGNING_KEY_CREDENTIAL`] or this previous key, so an env's key rotates
/// (bank the new key as current, move the old to `-previous`) with no serve-node
/// restart and no flowrunner change — the flowrunner always signs with the
/// CURRENT key. Drop this entry once every runner has picked up the new current
/// key. Like the current key it is reserved: the serve-node never installs it
/// into a node grant.
pub const SIGNING_KEY_CREDENTIAL_PREVIOUS: &str = "wamn:node-invoke-signing-key-previous";

type HmacSha256 = Hmac<Sha256>;

/// Compute the canonical runner→node signature: HMAC-SHA256 over the EXACT
/// request body bytes (the serialized [`NodeInvokeRequest`] JSON that is
/// POSTed), lower-hex encoded. Signing the raw body — not a re-derived canonical
/// form — is deliberate: the verifier MACs the bytes it received off the wire
/// BEFORE parsing, so signer and verifier agree with zero normalization risk.
///
/// REPLAY (accepted risk for v0, per wamn-fqg.22): the MAC binds the body but
/// carries no timestamp/nonce, so a captured VALID envelope can be replayed
/// WITHIN its project-env — the per-project-env key scopes it, never across
/// project-envs, and never cross-project (the serve-node pins its OWN
/// `--project`, ignoring the request). This is accepted because the signature
/// closes the NAMED threat: a FORGED envelope with attacker-chosen input/grant,
/// which requires the key an in-cluster attacker does not hold. A replay only
/// re-invokes the node with the SAME bytes the legitimate runner already sent;
/// `ctx.run_id` rides the envelope but the serve-node is stateless and does NOT
/// dedupe on it, so a freshness check would require
/// serve-node nonce state or a synchronized absolute clock — neither is cheap
/// here, so none is added (no speculative machinery).
pub fn sign_envelope(key: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Why a runner→node signature was refused. A distinct, MAC-free taxonomy the
/// serve-node maps to a 401-class refusal and the gate asserts on; it NEVER
/// carries the expected MAC (a refusal must not become a verification oracle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// No `x-wamn-signature` header on a request to a key-configured host.
    Missing,
    /// The header was present but is not valid lower-hex.
    Malformed,
    /// A well-formed signature that does not match the body under the key.
    Mismatch,
    /// wamn-fqg.31: the host is fail-closed (a signing key is REQUIRED) but none
    /// is configured, so it refuses ALL invocations rather than silently
    /// reverting to network trust. A misconfiguration signal, not a caller fault.
    Unconfigured,
    /// wamn-fqg.32: freshness is enforced but the request carried no
    /// `x-wamn-timestamp` — an envelope that cannot prove freshness is refused.
    MissingTimestamp,
    /// wamn-fqg.32: the `x-wamn-timestamp` header was present but not a decimal
    /// unix-seconds integer.
    MalformedTimestamp,
    /// wamn-fqg.32: a well-signed envelope whose timestamp is outside the
    /// configured max-age window (a stale / replayed request).
    Stale,
}

impl SignatureError {
    /// A stable, MAC-free reason code for the refusal body / gate asserts.
    pub fn reason(self) -> &'static str {
        match self {
            SignatureError::Missing => "missing-signature",
            SignatureError::Malformed => "malformed-signature",
            SignatureError::Mismatch => "bad-signature",
            SignatureError::Unconfigured => "signing-key-required",
            SignatureError::MissingTimestamp => "missing-timestamp",
            SignatureError::MalformedTimestamp => "malformed-timestamp",
            SignatureError::Stale => "stale-timestamp",
        }
    }
}

impl std::fmt::Display for SignatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "runner→node signature refused: {}", self.reason())
    }
}

impl std::error::Error for SignatureError {}

/// Verify a hex signature over `body` under `key` in CONSTANT TIME: [`hmac`]'s
/// `verify_slice` compares via `subtle`, so a wrong signature never leaks how
/// many leading bytes matched. The signer is [`sign_envelope`]; the two live in
/// one crate so the bytes cannot drift. A `Missing` header is the caller's to
/// distinguish (there is nothing to verify) — this reports `Malformed` for
/// non-hex and `Mismatch` for a valid-but-wrong tag.
pub fn verify_envelope(key: &[u8], body: &[u8], signature_hex: &str) -> Result<(), SignatureError> {
    let provided = hex::decode(signature_hex).map_err(|_| SignatureError::Malformed)?;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(body);
    mac.verify_slice(&provided)
        .map_err(|_| SignatureError::Mismatch)
}

/// The canonical bytes signed by [`sign_envelope_with_timestamp`] /
/// [`verify_envelope_with_timestamp`]: the EXACT body, OPTIONALLY extended with a
/// freshness timestamp (wamn-fqg.32). ADDITIVE and VERSION-SAFE — with no
/// timestamp the signed bytes ARE the body, byte-identical to the fqg.22
/// definition, so a legacy signer/verifier is unaffected and a keyed host with
/// freshness OFF still verifies a legacy (timestamp-less) envelope. A present
/// timestamp appends a domain-separated `\n<header>:<value>` suffix over the raw
/// header bytes; the MAC binds it, so stripping the timestamp (verifying
/// body-only) or editing it breaks the signature.
fn signed_message<'a>(body: &'a [u8], timestamp: Option<&str>) -> Cow<'a, [u8]> {
    match timestamp {
        None => Cow::Borrowed(body),
        Some(ts) => {
            let mut m = Vec::with_capacity(body.len() + TIMESTAMP_HEADER.len() + ts.len() + 2);
            m.extend_from_slice(body);
            m.push(b'\n');
            m.extend_from_slice(TIMESTAMP_HEADER.as_bytes());
            m.push(b':');
            m.extend_from_slice(ts.as_bytes());
            Cow::Owned(m)
        }
    }
}

/// Sign the body with an OPTIONAL freshness timestamp folded in (wamn-fqg.32).
/// `timestamp` is `None` for a legacy (fqg.22) envelope — then this is exactly
/// [`sign_envelope`] over the body — or `Some(unix-seconds-decimal)`, the same
/// string carried in the [`TIMESTAMP_HEADER`].
pub fn sign_envelope_with_timestamp(key: &[u8], body: &[u8], timestamp: Option<&str>) -> String {
    sign_envelope(key, &signed_message(body, timestamp))
}

/// Verify a signature over the body + optional timestamp (wamn-fqg.32). The
/// caller passes the [`TIMESTAMP_HEADER`] value exactly as received (or `None`
/// for a legacy envelope); the freshness (max-age) decision is the host's
/// ([`timestamp_fresh`]) — this only binds the timestamp to the MAC.
pub fn verify_envelope_with_timestamp(
    key: &[u8],
    body: &[u8],
    timestamp: Option<&str>,
    signature_hex: &str,
) -> Result<(), SignatureError> {
    verify_envelope(key, &signed_message(body, timestamp), signature_hex)
}

/// Whether `timestamp` (unix seconds) is within `max_age_secs` of `now` (unix
/// seconds) in EITHER direction — a future timestamp beyond the window (clock
/// skew / forgery) is as stale as an old one. Pure arithmetic (the host owns the
/// clock, this owns only the window test), so it unit-tests without a clock.
pub fn timestamp_fresh(timestamp: u64, now: u64, max_age_secs: u64) -> bool {
    timestamp.abs_diff(now) <= max_age_secs
}

// ---------------------------------------------------------------------------
// Grant derivation (cjv.3): exactly the node step's declared credentials
// ---------------------------------------------------------------------------

/// The credential names granted to one custom-node invocation: EXACTLY what the
/// flow's node step declared (`node.credential`, 0 or 1 name in v0), never the
/// project's whole set. The serve-node host installs this as the node's
/// per-execution grant, so an ungranted (sibling) credential is `not-granted`
/// at the real WIT boundary.
///
/// This being the *narrow* declared set — not a broad "all of the project" — is
/// the load-bearing property the credprobe negative gate proves; widening it
/// here is the cjv.3 hole.
pub fn granted_credentials(node_credential: Option<&str>) -> Vec<String> {
    node_credential.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ctx() -> WireRunContext {
        WireRunContext {
            run_id: "run-1".into(),
            flow_id: "flow-a".into(),
            flow_version: 3,
            node_id: "n0".into(),
            attempt: 1,
            deadline_ms: Some(30_000),
            traceparent: None,
            tracestate: None,
            config: r#"{"mode":"noop"}"#.into(),
            context: r#"{"hold":{"id":7}}"#.into(),
        }
    }

    #[test]
    fn request_round_trips_through_json() {
        let req = NodeInvokeRequest {
            ctx: sample_ctx(),
            input: WirePayload::Inline(r#"{"x":7}"#.into()),
            grant: vec!["notify-token".into()],
        };
        let wire = req.to_json();
        let back = NodeInvokeRequest::from_json(&wire).expect("decodes");
        assert_eq!(req, back);
        // The grant is on the wire verbatim (the serve-node host reads it).
        assert!(wire.contains("notify-token"));
        // No secret material — only the credential NAME.
        assert!(!wire.contains("s3cr3t"));
        assert!(wire.contains(r#""context":"{\"hold\":{\"id\":7}}""#));
        assert!(
            !wire.contains("idempotency-key"),
            "the internal invocation envelope must not carry a generated key"
        );
    }

    #[test]
    fn request_without_input_context_is_rejected() {
        let mut value = serde_json::to_value(sample_request()).unwrap();
        value["ctx"].as_object_mut().unwrap().remove("context");
        let wire = serde_json::to_string(&value).unwrap();
        assert!(NodeInvokeRequest::from_json(&wire).is_err());
    }

    #[test]
    fn response_ok_and_err_round_trip_variant_for_variant() {
        let ok = NodeInvokeResponse::Ok(WireEmission {
            payload: WirePayload::Inline(r#"{"echo":1}"#.into()),
            port: Some("true".into()),
            ctx: Some(r#"{"hold":{"id":7}}"#.into()),
        });
        assert_eq!(NodeInvokeResponse::from_json(&ok.to_json()).unwrap(), ok);

        // Every taxonomy variant survives the wire (the engine routes off it).
        let variants = [
            WireNodeError::Retryable(WireErrorDetail {
                message: "transient".into(),
                code: Some("ECONNRESET".into()),
                data: None,
            }),
            WireNodeError::RateLimited(WireRateLimit {
                detail: WireErrorDetail {
                    message: "429".into(),
                    code: Some("HTTP_429".into()),
                    data: None,
                },
                retry_after_ms: Some(1500),
                target_host: Some("api.example".into()),
            }),
            WireNodeError::Terminal(WireErrorDetail {
                message: "boom".into(),
                code: None,
                data: Some(r#"{"k":1}"#.into()),
            }),
            WireNodeError::InvalidInput(WireErrorDetail {
                message: "bad".into(),
                code: Some("SCHEMA_MISMATCH".into()),
                data: None,
            }),
            WireNodeError::Cancelled,
        ];
        for v in variants {
            let resp = NodeInvokeResponse::Err(v.clone());
            let back = NodeInvokeResponse::from_json(&resp.to_json()).unwrap();
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn a_default_main_port_travels_absent() {
        let ok = NodeInvokeResponse::Ok(WireEmission {
            payload: WirePayload::Inline("null".into()),
            port: None,
            ctx: None,
        });
        let wire = ok.to_json();
        assert!(
            !wire.contains("port"),
            "absent port must not serialize: {wire}"
        );
    }

    #[test]
    fn successful_emission_with_invalid_replacement_context_is_rejected() {
        let wire = NodeInvokeResponse::Ok(WireEmission {
            payload: WirePayload::Inline("null".into()),
            port: None,
            ctx: Some("{".into()),
        })
        .to_json();
        assert!(NodeInvokeResponse::from_json(&wire).is_err());
    }

    #[test]
    fn error_emission_cannot_carry_context() {
        let wire = r#"{"err":{"terminal":{"message":"boom","ctx":"{}"}}}"#;
        assert!(NodeInvokeResponse::from_json(wire).is_err());

        let error = NodeInvokeResponse::Err(WireNodeError::Terminal(WireErrorDetail {
            message: "boom".into(),
            code: None,
            data: None,
        }));
        assert!(!error.to_json().contains(r#""ctx""#));
    }

    // --- runner->node authn (wamn-fqg.22) -----------------------------------

    fn sample_request() -> NodeInvokeRequest {
        NodeInvokeRequest {
            ctx: sample_ctx(),
            input: WirePayload::Inline(r#"{"x":7}"#.into()),
            grant: vec!["notify-token".into()],
        }
    }

    /// The signed bytes ARE the request body, and a body signed with a key
    /// verifies under that key — the canonical roundtrip both ends share.
    #[test]
    fn signed_envelope_bytes_are_the_body_and_verify() {
        let key = b"per-project-env-hmac-key";
        let body = sample_request().to_json();
        let sig = sign_envelope(key, body.as_bytes());
        // Deterministic over the exact serialized envelope.
        assert_eq!(sig, sign_envelope(key, body.as_bytes()));
        assert!(verify_envelope(key, body.as_bytes(), &sig).is_ok());
    }

    /// A one-byte tamper of the body (an attacker editing the grant/input the
    /// legitimate runner never sent) is `Mismatch`. Kills the client-side
    /// "sign the wrong bytes" mutant: the verifier only accepts the exact bytes.
    #[test]
    fn a_tampered_body_is_mismatch() {
        let key = b"per-project-env-hmac-key";
        let body = sample_request().to_json();
        let sig = sign_envelope(key, body.as_bytes());

        let mut tampered = sample_request();
        tampered.grant = vec!["sibling-token".into()]; // forge a wider grant
        let tampered_body = tampered.to_json();
        assert_ne!(body, tampered_body);
        assert_eq!(
            verify_envelope(key, tampered_body.as_bytes(), &sig),
            Err(SignatureError::Mismatch)
        );
    }

    /// A signature made under a DIFFERENT key never verifies (per-project-env
    /// scoping): an attacker without the env's key cannot forge.
    #[test]
    fn a_wrong_key_signature_is_mismatch() {
        let body = sample_request().to_json();
        let sig = sign_envelope(b"key-project-a", body.as_bytes());
        assert_eq!(
            verify_envelope(b"key-project-b", body.as_bytes(), &sig),
            Err(SignatureError::Mismatch)
        );
    }

    /// A non-hex header is `Malformed` (distinct from a well-formed wrong tag),
    /// and the reason codes are the stable, MAC-free strings the gate asserts.
    #[test]
    fn malformed_signature_and_reason_codes() {
        let key = b"k";
        assert_eq!(
            verify_envelope(key, b"body", "not-hex!!"),
            Err(SignatureError::Malformed)
        );
        assert_eq!(SignatureError::Missing.reason(), "missing-signature");
        assert_eq!(SignatureError::Malformed.reason(), "malformed-signature");
        assert_eq!(SignatureError::Mismatch.reason(), "bad-signature");
        // wamn-fqg.31: the fail-closed refusal reason.
        assert_eq!(
            SignatureError::Unconfigured.reason(),
            "signing-key-required"
        );
        // wamn-fqg.32: the freshness refusal reasons.
        assert_eq!(
            SignatureError::MissingTimestamp.reason(),
            "missing-timestamp"
        );
        assert_eq!(
            SignatureError::MalformedTimestamp.reason(),
            "malformed-timestamp"
        );
        assert_eq!(SignatureError::Stale.reason(), "stale-timestamp");
    }

    /// wamn-fqg.32: the timestamp extension is ADDITIVE and VERSION-SAFE. With no
    /// timestamp the signed bytes are byte-identical to fqg.22 (body-only). A
    /// present timestamp is bound by the MAC — stripping it (verifying body-only)
    /// or editing it breaks the signature.
    #[test]
    fn timestamp_signing_is_additive_and_version_safe() {
        let key = b"per-project-env-hmac-key";
        let body = sample_request().to_json();

        // No timestamp == the fqg.22 body-only signature, byte-identical.
        assert_eq!(
            sign_envelope_with_timestamp(key, body.as_bytes(), None),
            sign_envelope(key, body.as_bytes())
        );

        let ts = "1721470000";
        let sig = sign_envelope_with_timestamp(key, body.as_bytes(), Some(ts));
        // Verifies WITH that exact timestamp.
        assert!(verify_envelope_with_timestamp(key, body.as_bytes(), Some(ts), &sig).is_ok());
        // Stripping the timestamp (body-only) fails — the MAC bound it.
        assert_eq!(
            verify_envelope_with_timestamp(key, body.as_bytes(), None, &sig),
            Err(SignatureError::Mismatch)
        );
        // Editing the timestamp fails.
        assert_eq!(
            verify_envelope_with_timestamp(key, body.as_bytes(), Some("1721470001"), &sig),
            Err(SignatureError::Mismatch)
        );
    }

    /// wamn-fqg.32: the freshness window is symmetric and inclusive.
    #[test]
    fn timestamp_freshness_window() {
        assert!(timestamp_fresh(1000, 1000, 30));
        assert!(timestamp_fresh(1000, 1030, 30)); // exactly max age (old)
        assert!(timestamp_fresh(1030, 1000, 30)); // exactly max age (future skew)
        assert!(!timestamp_fresh(1000, 1031, 30)); // too old
        assert!(!timestamp_fresh(1031, 1000, 30)); // too far in the future
    }

    /// wamn-fqg.30: the reserved names are distinct stable strings — a body
    /// signed under the "previous" key never verifies under the "current" key
    /// (they are independent secrets; the serve-node accepts EITHER, but the
    /// pure verify never conflates them). Pins both reserved names.
    #[test]
    fn previous_signing_key_name_is_distinct_and_independent() {
        assert_eq!(SIGNING_KEY_CREDENTIAL, "wamn:node-invoke-signing-key");
        assert_eq!(
            SIGNING_KEY_CREDENTIAL_PREVIOUS,
            "wamn:node-invoke-signing-key-previous"
        );
        assert_ne!(SIGNING_KEY_CREDENTIAL, SIGNING_KEY_CREDENTIAL_PREVIOUS);
        let body = sample_request().to_json();
        let sig_prev = sign_envelope(b"previous-key", body.as_bytes());
        // Signed under "previous": verifies under that key, NOT under "current".
        assert!(verify_envelope(b"previous-key", body.as_bytes(), &sig_prev).is_ok());
        assert_eq!(
            verify_envelope(b"current-key", body.as_bytes(), &sig_prev),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn grant_is_exactly_the_declared_credential() {
        // A node that declared one credential grants exactly that one.
        assert_eq!(
            granted_credentials(Some("notify-token")),
            vec!["notify-token"]
        );
        // A node that declared none grants NOTHING — never a broad default.
        assert!(granted_credentials(None).is_empty());
    }
}
