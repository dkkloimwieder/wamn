//! Trusted one-frame custom-node invocation for the digest-pinned flowrunner.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use url::Url;
use wamn_node_invoke::{
    NodeInvokeRequest, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL, TIMESTAMP_HEADER,
    granted_credentials, sign_envelope_with_timestamp,
};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::wamn_credentials::WamnCredentials;
use super::wamn_postgres::WamnPostgres;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "runner-node-invocation-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::runner::node_invocation::{self, EffectError, InvocationContext};

pub const NODE_INVOCATION_ID: &str = "wamn-node-invocation";
const INVOCATION_CONTEXT_VERSION: u32 = 1;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Immutable environment-owned placement from admitted implementation digest
/// to the node-host authority serving those exact bytes.
#[derive(Debug, Clone, Default)]
pub struct NodePlacementMap {
    endpoints: Arc<HashMap<Box<str>, Url>>,
}

impl NodePlacementMap {
    pub fn from_json(json: &str) -> Result<Self, NodePlacementError> {
        let NodePlacementEntries(placements) = serde_json::from_str(json).map_err(|error| {
            NodePlacementError::new(format!(
                "node placements must be a JSON object of digest-to-endpoint strings: {error}"
            ))
        })?;
        Self::new(placements)
    }

    pub fn singleton(
        implementation_digest: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Result<Self, NodePlacementError> {
        Self::new([(implementation_digest.into(), endpoint.into())])
    }

    pub fn new(
        placements: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, NodePlacementError> {
        let mut endpoints = HashMap::new();
        for (digest, endpoint) in placements {
            if !is_sha256_digest(&digest) {
                return Err(NodePlacementError::new(format!(
                    "invalid node implementation digest {digest:?}"
                )));
            }
            let endpoint = parse_endpoint(&endpoint)?;
            if endpoints
                .insert(digest.clone().into_boxed_str(), endpoint)
                .is_some()
            {
                return Err(NodePlacementError::new(format!(
                    "duplicate node implementation digest {digest:?}"
                )));
            }
        }
        Ok(Self {
            endpoints: Arc::new(endpoints),
        })
    }

    fn endpoint(&self, implementation_digest: &str) -> Option<&Url> {
        self.endpoints.get(implementation_digest)
    }
}

struct NodePlacementEntries(Vec<(String, String)>);

impl<'de> serde::Deserialize<'de> for NodePlacementEntries {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PlacementVisitor;

        impl<'de> serde::de::Visitor<'de> for PlacementVisitor {
            type Value = NodePlacementEntries;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object of unique digest-to-endpoint strings")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut entries = Vec::new();
                let mut seen = HashSet::new();
                while let Some((digest, endpoint)) = map.next_entry::<String, String>()? {
                    if !seen.insert(digest.clone()) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate node implementation digest {digest:?}"
                        )));
                    }
                    entries.push((digest, endpoint));
                }
                Ok(NodePlacementEntries(entries))
            }
        }

        deserializer.deserialize_map(PlacementVisitor)
    }
}

/// One invalid environment-owned node placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePlacementError {
    message: Box<str>,
}

impl NodePlacementError {
    fn new(message: impl Into<Box<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NodePlacementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NodePlacementError {}

fn parse_endpoint(endpoint: &str) -> Result<Url, NodePlacementError> {
    let mut url = Url::parse(endpoint).map_err(|error| {
        NodePlacementError::new(format!("invalid node placement endpoint: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(NodePlacementError::new(
            "node placement endpoint must be an HTTP(S) authority without credentials, path, query, or fragment",
        ));
    }
    url.set_path("/run");
    Ok(url)
}

/// Host-only lookup key for one exact custom-node invocation frame.
pub struct NodeInvocationLookup<'a> {
    pub run_id: &'a str,
    pub node_id: &'a str,
    pub occurrence: i32,
    pub attempt: i32,
}

/// One transactionally consistent set of admitted custom-node facts.
#[derive(Debug, Clone)]
pub struct NodeInvocationSnapshot {
    pub run_status: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub catalog_id: Option<String>,
    pub catalog_version: Option<i64>,
    pub environment: Option<String>,
    pub admitted_artifact_digest: Option<String>,
    pub admitted_artifact: bool,
    pub attempt_matches: bool,
    pub node_type: Option<String>,
    pub executable_kind: Option<String>,
    pub admitted_implementation_digest: Option<String>,
    pub admitted_config: Option<serde_json::Value>,
    pub admitted_connection: Option<String>,
    pub admitted_credential: Option<String>,
    pub attempt_input_ref: Option<String>,
    pub attempt_key: Option<String>,
}

/// Host-owned authorization, placement, signing, and transport for custom nodes.
pub struct NodeInvocation {
    postgres: Arc<WamnPostgres>,
    vault: Arc<WamnCredentials>,
    placements: NodePlacementMap,
    tenant: Box<str>,
    project: Box<str>,
}

impl NodeInvocation {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        tenant: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        placements: NodePlacementMap,
    ) -> Self {
        Self {
            postgres,
            vault,
            placements,
            tenant: tenant.into(),
            project: project.into(),
        }
    }

    async fn invoke(
        &self,
        component_id: &str,
        context: &InvocationContext,
        request: &[u8],
    ) -> Result<Vec<u8>, EffectError> {
        validate_context(&self.tenant, context, request)?;
        let decoded_request = std::str::from_utf8(request)
            .map_err(|_| EffectError::InvalidContext)
            .and_then(|request| {
                NodeInvokeRequest::from_json(request).map_err(|_| EffectError::InvalidContext)
            })?;
        validate_request_context(context, &decoded_request)?;
        let occurrence =
            i32::try_from(context.occurrence).map_err(|_| EffectError::InvalidContext)?;
        let attempt = i32::try_from(context.attempt).map_err(|_| EffectError::InvalidContext)?;
        let snapshot = self
            .postgres
            .node_invocation_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &NodeInvocationLookup {
                    run_id: &context.run_id,
                    node_id: &context.node_id,
                    occurrence,
                    attempt,
                },
            )
            .await
            .map_err(|error| EffectError::Transport(error.to_string()))?
            .ok_or(EffectError::InvalidContext)?;
        authorize_snapshot(context, &snapshot)?;
        authorize_request(context, &decoded_request, &snapshot)?;

        // Every refusal above is resolved before endpoint selection, signing, or
        // construction of a client that could perform network I/O.
        let endpoint = self
            .placements
            .endpoint(&context.implementation_digest)
            .ok_or(EffectError::PlacementUnavailable)?;
        let signing_key = self
            .vault
            .lookup(&self.project, SIGNING_KEY_CREDENTIAL)
            .filter(|key| !key.is_empty())
            .ok_or(EffectError::SigningUnavailable)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| EffectError::SigningUnavailable)?
            .as_secs()
            .to_string();
        let signature =
            sign_envelope_with_timestamp(signing_key.as_bytes(), request, Some(&timestamp));

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| EffectError::Transport(error.to_string()))?;
        let response = client
            .post(endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(SIGNATURE_HEADER, signature)
            .header(TIMESTAMP_HEADER, &timestamp)
            .body(request.to_vec())
            .send()
            .await
            .map_err(map_transport_error)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(EffectError::SigningRefused);
        }
        if status != reqwest::StatusCode::OK {
            return Err(EffectError::Transport(format!(
                "node host returned HTTP {}",
                status.as_u16()
            )));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(map_transport_error)
    }
}

fn validate_context(
    tenant: &str,
    context: &InvocationContext,
    request: &[u8],
) -> Result<(), EffectError> {
    if context.version != INVOCATION_CONTEXT_VERSION
        || context.tenant_id != tenant
        || context.environment.is_empty()
        || context.catalog_id.is_empty()
        || context.catalog_version <= 0
        || context.run_id.is_empty()
        || context.flow_id.is_empty()
        || context.flow_version == 0
        || !is_sha256_digest(&context.artifact_digest)
        || context.node_id.is_empty()
        || !is_sha256_digest(&context.implementation_digest)
        || request.is_empty()
    {
        return Err(EffectError::InvalidContext);
    }
    Ok(())
}

fn authorize_snapshot(
    context: &InvocationContext,
    snapshot: &NodeInvocationSnapshot,
) -> Result<(), EffectError> {
    if snapshot.run_status != "running"
        || snapshot.flow_id != context.flow_id
        || u32::try_from(snapshot.flow_version).ok() != Some(context.flow_version)
        || snapshot.catalog_id.as_deref() != Some(context.catalog_id.as_str())
        || snapshot.catalog_version != Some(i64::from(context.catalog_version))
        || snapshot.environment.as_deref() != Some(context.environment.as_str())
        || snapshot.admitted_artifact_digest.as_deref() != Some(context.artifact_digest.as_str())
        || !snapshot.admitted_artifact
        || !snapshot.attempt_matches
    {
        return Err(EffectError::InvalidContext);
    }
    if snapshot.node_type.is_none()
        || snapshot.executable_kind.as_deref() != Some("component")
        || snapshot.admitted_implementation_digest.is_none()
    {
        return Err(EffectError::NodeNotPermitted);
    }
    if snapshot.admitted_implementation_digest.as_deref()
        != Some(context.implementation_digest.as_str())
    {
        return Err(EffectError::ImplementationMismatch);
    }
    Ok(())
}

fn validate_request_context(
    context: &InvocationContext,
    request: &NodeInvokeRequest,
) -> Result<(), EffectError> {
    if request.ctx.run_id != context.run_id
        || request.ctx.flow_id != context.flow_id
        || request.ctx.flow_version != context.flow_version
        || request.ctx.node_id != context.node_id
        || request.ctx.attempt != context.attempt
    {
        return Err(EffectError::InvalidContext);
    }
    Ok(())
}

fn authorize_request(
    context: &InvocationContext,
    request: &NodeInvokeRequest,
    snapshot: &NodeInvocationSnapshot,
) -> Result<(), EffectError> {
    let config: serde_json::Value =
        serde_json::from_str(&request.ctx.config).map_err(|_| EffectError::InvalidContext)?;
    let admitted_deadline_ms = config
        .get("deadline-ms")
        .and_then(serde_json::Value::as_u64);
    if request.ctx.deadline_ms != admitted_deadline_ms
        || request.ctx.traceparent.is_some()
        || request.ctx.tracestate.is_some()
    {
        return Err(EffectError::InvalidContext);
    }
    let durable_context: serde_json::Value =
        serde_json::from_str(&request.ctx.context).map_err(|_| EffectError::InvalidContext)?;
    let input: serde_json::Value =
        serde_json::from_str(request.input.inline().ok_or(EffectError::InvalidContext)?)
            .map_err(|_| EffectError::InvalidContext)?;
    let attempt_input = serde_json::json!({
        "connection": snapshot.admitted_connection.as_deref(),
        "config": config.clone(),
        "context": durable_context,
        "input": input,
    });
    let attempt_input_ref = wamn_flow::canonical_json_sha256(&attempt_input);
    let admitted_attempt_key = snapshot.attempt_key.clone().unwrap_or_else(|| {
        format!(
            "{}:{}:{}",
            context.run_id, context.node_id, context.occurrence
        )
    });
    if snapshot.admitted_config.as_ref() != Some(&config)
        || snapshot.attempt_input_ref.as_deref() != Some(attempt_input_ref.as_str())
        || request.ctx.idempotency_key != admitted_attempt_key
    {
        return Err(EffectError::InvalidContext);
    }
    if request.grant != granted_credentials(snapshot.admitted_credential.as_deref()) {
        return Err(EffectError::NodeNotPermitted);
    }
    Ok(())
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn map_transport_error(error: reqwest::Error) -> EffectError {
    if error.is_timeout() {
        EffectError::Timeout
    } else {
        EffectError::Transport(error.to_string())
    }
}

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    node_invocation::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

impl HostPlugin for NodeInvocation {
    fn id(&self) -> &'static str {
        NODE_INVOCATION_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:runner/node-invocation@0.1.0")]),
            exports: HashSet::new(),
        }
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<NodeInvocation>> {
    ctx.try_get_plugin::<NodeInvocation>(NODE_INVOCATION_ID)
}

impl node_invocation::Host for ActiveCtx<'_> {
    async fn invoke(
        &mut self,
        context: InvocationContext,
        request: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<Vec<u8>, EffectError>> {
        let plugin = plugin_of(self)?;
        let result = plugin
            .invoke(self.component_id.as_ref(), &context, &request)
            .await;
        if let Err(error) = &result {
            tracing::warn!(
                error = ?error,
                run_id = context.run_id,
                node_id = context.node_id,
                occurrence = context.occurrence,
                attempt = context.attempt,
                implementation_digest = context.implementation_digest,
                "trusted node invocation refused"
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use wamn_node_invoke::{WirePayload, WireRunContext};

    use super::*;

    fn invocation_context() -> InvocationContext {
        InvocationContext {
            version: 1,
            tenant_id: "tenant-a".into(),
            environment: "test".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 1,
            run_id: "run-a".into(),
            flow_id: "flow-a".into(),
            flow_version: 1,
            artifact_digest: format!("sha256:{}", "a".repeat(64)),
            node_id: "node-a".into(),
            occurrence: 0,
            attempt: 1,
            implementation_digest: format!("sha256:{}", "b".repeat(64)),
        }
    }

    fn invocation_snapshot(context: &InvocationContext) -> NodeInvocationSnapshot {
        NodeInvocationSnapshot {
            run_status: "running".into(),
            flow_id: context.flow_id.clone(),
            flow_version: i32::try_from(context.flow_version).expect("test flow version fits"),
            catalog_id: Some(context.catalog_id.clone()),
            catalog_version: Some(i64::from(context.catalog_version)),
            environment: Some(context.environment.clone()),
            admitted_artifact_digest: Some(context.artifact_digest.clone()),
            admitted_artifact: true,
            attempt_matches: true,
            node_type: Some("custom".into()),
            executable_kind: Some("component".into()),
            admitted_implementation_digest: Some(context.implementation_digest.clone()),
            admitted_config: Some(serde_json::json!({"mode": "noop"})),
            admitted_connection: None,
            admitted_credential: Some("api-key".into()),
            attempt_input_ref: Some(wamn_flow::canonical_json_sha256(&serde_json::json!({
                "connection": null,
                "config": {"mode": "noop"},
                "context": {},
                "input": {},
            }))),
            attempt_key: Some("durable-attempt-key".into()),
        }
    }

    fn invocation_request(idempotency_key: &str) -> NodeInvokeRequest {
        NodeInvokeRequest {
            ctx: WireRunContext {
                run_id: "run-a".into(),
                flow_id: "flow-a".into(),
                flow_version: 1,
                node_id: "node-a".into(),
                attempt: 1,
                idempotency_key: idempotency_key.into(),
                deadline_ms: None,
                traceparent: None,
                tracestate: None,
                config: r#"{"mode":"noop"}"#.into(),
                context: "{}".into(),
            },
            input: WirePayload::Inline("{}".into()),
            grant: vec!["api-key".into()],
        }
    }

    #[test]
    fn placement_accepts_only_digest_to_authority() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let placements = NodePlacementMap::new([(digest.clone(), "http://node:8080".into())])
            .expect("valid placement");
        assert_eq!(
            placements.endpoint(&digest).map(Url::as_str),
            Some("http://node:8080/run")
        );
        assert!(NodePlacementMap::new([(digest, "http://node:8080/other".into())]).is_err());

        let duplicate = format!(
            r#"{{"{digest}":"http://node-a:8080","{digest}":"http://node-b:8080"}}"#,
            digest = format!("sha256:{}", "b".repeat(64)),
        );
        assert!(NodePlacementMap::from_json(&duplicate).is_err());
    }

    #[test]
    fn authorization_rejects_an_unadmitted_implementation_digest() {
        let context = invocation_context();
        let mut snapshot = invocation_snapshot(&context);
        snapshot.admitted_implementation_digest = Some(format!("sha256:{}", "c".repeat(64)));

        assert!(matches!(
            authorize_snapshot(&context, &snapshot),
            Err(EffectError::ImplementationMismatch)
        ));
    }

    #[test]
    fn authorization_rejects_request_identity_config_and_grant_mismatches() {
        let context = invocation_context();
        let snapshot = invocation_snapshot(&context);

        let mut identity_mismatch = invocation_request("durable-attempt-key");
        identity_mismatch.ctx.run_id = "other-run".into();
        assert!(matches!(
            validate_request_context(&context, &identity_mismatch),
            Err(EffectError::InvalidContext)
        ));

        let mut config_mismatch = invocation_request("durable-attempt-key");
        config_mismatch.ctx.config = r#"{"mode":"other"}"#.into();
        assert!(matches!(
            authorize_request(&context, &config_mismatch, &snapshot),
            Err(EffectError::InvalidContext)
        ));

        let mut grant_mismatch = invocation_request("durable-attempt-key");
        grant_mismatch.grant = vec!["sibling-secret".into()];
        assert!(matches!(
            authorize_request(&context, &grant_mismatch, &snapshot),
            Err(EffectError::NodeNotPermitted)
        ));

        let mut deadline_mismatch = invocation_request("durable-attempt-key");
        deadline_mismatch.ctx.deadline_ms = Some(1);
        assert!(matches!(
            authorize_request(&context, &deadline_mismatch, &snapshot),
            Err(EffectError::InvalidContext)
        ));

        let mut trace_mismatch = invocation_request("durable-attempt-key");
        trace_mismatch.ctx.traceparent = Some("00-forged".into());
        assert!(matches!(
            authorize_request(&context, &trace_mismatch, &snapshot),
            Err(EffectError::InvalidContext)
        ));

        let mut context_mismatch = invocation_request("durable-attempt-key");
        context_mismatch.ctx.context = r#"{"forged":true}"#.into();
        let mut input_mismatch = invocation_request("durable-attempt-key");
        input_mismatch.input = WirePayload::Inline(r#"{"forged":true}"#.into());
        for input_mismatch in [context_mismatch, input_mismatch] {
            assert!(matches!(
                authorize_request(&context, &input_mismatch, &snapshot),
                Err(EffectError::InvalidContext)
            ));
        }
    }

    #[test]
    fn never_replay_uses_the_exact_deterministic_wire_key() {
        let context = invocation_context();
        let mut snapshot = invocation_snapshot(&context);
        snapshot.attempt_key = None;
        let expected_key = format!(
            "{}:{}:{}",
            context.run_id, context.node_id, context.occurrence
        );
        let request = invocation_request(&expected_key);
        assert!(authorize_request(&context, &request, &snapshot).is_ok());

        let wrong_request = invocation_request("wrong-key");
        assert!(matches!(
            authorize_request(&context, &wrong_request, &snapshot),
            Err(EffectError::InvalidContext)
        ));
    }
}
