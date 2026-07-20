//! # wamn-node-invoke — the v0 custom-node invocation protocol (5.6 / wamn-bd5)
//!
//! §5.6's v0 dispatch of a *dynamically-loaded custom node* is a **boring,
//! debuggable in-cluster HTTP hop**: the trusted flow-runner POSTs an
//! invocation envelope to a `serve-node` host that owns the node's warm
//! instance, and the node runs under the REAL frozen `wamn:node` world
//! (`docs/wamn-node.wit`). This crate is the pure heart of that path so it
//! cannot drift between the two ends:
//!
//! - the **wire envelope** ([`NodeInvokeRequest`] / [`NodeInvokeResponse`]) —
//!   ctx + input + the per-invocation credential grant on the way in, the
//!   node's emission or the frozen `node-error` taxonomy on the way out;
//! - the **grant derivation** ([`granted_credentials`]) — the runner declares
//!   EXACTLY the credentials the flow's node step declared, never the project's
//!   whole set (the cjv.3 grant the serve-node host installs before dispatch);
//! - the **config-parse memoization** ([`ConfigCache`], design-note 9b) — the
//!   `json` config crosses the WIT boundary only for dynamic custom nodes, so
//!   the warm serve-node instance parses/validates a given config ONCE per
//!   `(node, flow-version, config-identity)` and reuses it across invocations.
//!
//! PURE — serde + the HMAC signing primitives, no DB / clock / wasm / network —
//! so BOTH the flowrunner GUEST (wasm32-wasip2) and the serve-node HOST link the
//! identical bytes. Runner↔node authn (wamn-fqg.22) lives HERE too — the
//! canonical signed bytes ([`sign_envelope`] / [`verify_envelope`]) are shared
//! so signer and verifier cannot drift; mTLS remains the later infra upgrade.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

// ---------------------------------------------------------------------------
// Wire envelope: runner -> serve-node (request) and back (response)
// ---------------------------------------------------------------------------

/// The `run-context` the runner hands a node, mirroring `wamn:node/types`'s
/// `run-context` (docs/wamn-node.wit) field-for-field. Deliberately carries NO
/// secrets — the node pulls its granted credential lazily through the
/// `wamn:node/credentials` import the serve-node host links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WireRunContext {
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: u32,
    pub node_id: String,
    pub attempt: u32,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
    /// The node's JSON config document (template-expanded by the runner). A
    /// `json` string, exactly as the frozen contract types it.
    pub config: String,
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
        serde_json::from_str(s)
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
        serde_json::from_str(s)
    }
}

// ---------------------------------------------------------------------------
// Runner -> node authn: a SIGNED invocation envelope (wamn-fqg.22)
// ---------------------------------------------------------------------------

/// The request header carrying the lower-hex HMAC-SHA256 signature of the
/// request body (runner→node authn). ASCII-safe so it rides an HTTP header
/// unencoded.
pub const SIGNATURE_HEADER: &str = "x-wamn-signature";

/// The reserved credential-vault name the per-project-env HMAC signing key is
/// banked under, distributed via the EXISTING runner-credentials Secret pattern
/// (`{project: {name: secret}}` — no new Secret, no new WIT). The colon marks it
/// non-colliding with a user-authored `wamn-flow` credential name (those are
/// bare logical names); the serve-node additionally REFUSES to install it into
/// any node grant, so a custom node can never read the signing key back through
/// `wamn:node/credentials`.
pub const SIGNING_KEY_CREDENTIAL: &str = "wamn:node-invoke-signing-key";

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
/// `ctx.run_id` / `ctx.idempotency_key` ride the envelope but the serve-node is
/// stateless and does NOT dedupe on them, so a freshness check would require
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
}

impl SignatureError {
    /// A stable, MAC-free reason code for the refusal body / gate asserts.
    pub fn reason(self) -> &'static str {
        match self {
            SignatureError::Missing => "missing-signature",
            SignatureError::Malformed => "malformed-signature",
            SignatureError::Mismatch => "bad-signature",
            SignatureError::Unconfigured => "signing-key-required",
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

// ---------------------------------------------------------------------------
// Config-parse memoization (design-note 9b)
// ---------------------------------------------------------------------------

/// The identity a parsed config is memoized under. Config is immutable per
/// `(flow-version, node-id)` (both already on `run-context`), so those pin it;
/// the content hash makes the cache robust to any drift within a version and
/// gives a version flip / edit a distinct key (never a stale hit).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConfigKey {
    node_id: String,
    flow_version: u32,
    config_hash: u64,
}

/// A rejected config: only reason in v0 is malformed JSON (schema validation is
/// a follow-up). Kept out of the hot path — validated once, then cached.
#[derive(Debug)]
pub struct ConfigError {
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "config is not valid JSON: {}", self.message)
    }
}

impl std::error::Error for ConfigError {}

fn hash_config(config: &str) -> u64 {
    let mut h = DefaultHasher::new();
    config.hash(&mut h);
    h.finish()
}

/// The warm serve-node instance's config-parse cache (design-note 9b): the
/// `json` config crosses the WIT boundary only for dynamic custom nodes, so a
/// given `(node, flow-version, config-identity)` is parsed + validated ONCE and
/// reused across every invocation of that step. `parse_count` is the observable
/// witness — N invocations of one config parse it once.
///
/// Not thread-safe by itself (a `&mut self` cache); the serve-node host holds it
/// behind the same mutex as its single warm node instance (requests are served
/// sequentially, one instance).
#[derive(Debug, Default)]
pub struct ConfigCache {
    entries: HashMap<ConfigKey, Arc<Value>>,
    parses: u64,
}

impl ConfigCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The prepared (parsed + validated) config for this step, parsing on the
    /// first sight of a `(node, flow-version, config-identity)` and returning a
    /// cached clone thereafter. A malformed config is [`ConfigError`] and is NOT
    /// cached (so a fixed redeploy re-validates).
    pub fn prepared(
        &mut self,
        node_id: &str,
        flow_version: u32,
        config: &str,
    ) -> Result<Arc<Value>, ConfigError> {
        let key = ConfigKey {
            node_id: node_id.to_string(),
            flow_version,
            config_hash: hash_config(config),
        };
        if let Some(v) = self.entries.get(&key) {
            return Ok(v.clone());
        }
        // Miss: pay the parse exactly once for this identity.
        let value: Value = serde_json::from_str(config).map_err(|e| ConfigError {
            message: e.to_string(),
        })?;
        self.parses += 1;
        let arc = Arc::new(value);
        self.entries.insert(key, arc.clone());
        Ok(arc)
    }

    /// How many real `serde_json` parses this cache has performed — one per
    /// distinct config identity, regardless of invocation count.
    pub fn parse_count(&self) -> u64 {
        self.parses
    }

    /// How many distinct config identities are memoized.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
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
            idempotency_key: "run-1:n0".into(),
            deadline_ms: Some(30_000),
            traceparent: None,
            tracestate: None,
            config: r#"{"mode":"noop"}"#.into(),
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
    }

    #[test]
    fn response_ok_and_err_round_trip_variant_for_variant() {
        let ok = NodeInvokeResponse::Ok(WireEmission {
            payload: WirePayload::Inline(r#"{"echo":1}"#.into()),
            port: Some("true".into()),
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
        });
        let wire = ok.to_json();
        assert!(
            !wire.contains("port"),
            "absent port must not serialize: {wire}"
        );
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
        assert_eq!(SignatureError::Unconfigured.reason(), "signing-key-required");
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

    #[test]
    fn config_cache_parses_once_per_identity() {
        let mut cache = ConfigCache::new();
        let cfg = r#"{"mode":"io","wait_ns":25}"#;
        // First sighting parses; the next four are pure cache hits.
        let first = cache.prepared("n0", 1, cfg).unwrap();
        for _ in 0..4 {
            let hit = cache.prepared("n0", 1, cfg).unwrap();
            assert_eq!(*hit, *first);
        }
        assert_eq!(cache.parse_count(), 1, "5 invocations, one parse (9b)");
        assert_eq!(cache.len(), 1);
        assert_eq!(*first, serde_json::json!({"mode":"io","wait_ns":25}));
    }

    /// Mutation (b) killer: a changed config for the SAME (node, version) must
    /// re-parse to the NEW value and never return the stale one. A cache that
    /// drops `config_hash` from the key (keys on node+version only) returns the
    /// stale value here and fails.
    #[test]
    fn config_cache_does_not_return_a_stale_value_when_config_changes() {
        let mut cache = ConfigCache::new();
        let a = cache.prepared("n0", 1, r#"{"v":1}"#).unwrap();
        assert_eq!(*a, serde_json::json!({"v":1}));
        let b = cache.prepared("n0", 1, r#"{"v":2}"#).unwrap();
        assert_eq!(
            *b,
            serde_json::json!({"v":2}),
            "changed config must not be stale"
        );
        assert_eq!(cache.parse_count(), 2, "two distinct configs = two parses");
    }

    /// Distinct nodes / versions never share a memoized config (a cache that
    /// drops `node_id` or `flow_version` from the key collides here).
    #[test]
    fn config_cache_keys_on_node_and_version() {
        let mut cache = ConfigCache::new();
        let cfg = r#"{"same":true}"#;
        cache.prepared("n0", 1, cfg).unwrap();
        cache.prepared("n1", 1, cfg).unwrap(); // different node
        cache.prepared("n0", 2, cfg).unwrap(); // different version
        assert_eq!(cache.parse_count(), 3);
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn a_malformed_config_is_rejected_and_not_cached() {
        let mut cache = ConfigCache::new();
        assert!(cache.prepared("n0", 1, "{not json").is_err());
        assert_eq!(cache.parse_count(), 0);
        assert!(cache.is_empty());
        // A fixed redeploy of the same identity re-validates and succeeds.
        assert!(cache.prepared("n0", 1, "{}").is_ok());
        assert_eq!(cache.parse_count(), 1);
    }
}
