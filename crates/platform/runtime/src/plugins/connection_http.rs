//! Trusted one-frame HTTP connection effect for the digest-pinned flowrunner.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use wamn_node_manifest::{
    HttpBodyDigest, HttpOperation, HttpSemanticHeader, fingerprint_http_operation,
    normalize_portable_http_target,
};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::connection_authority::{
    AuthorityError, NetworkPolicy, TlsPolicy, TokioDnsResolver, TransportDecision,
    parse_http_connection_authority, resolve_http_request,
};

use super::runner_egress::RunnerEgressPolicy;
use super::wamn_credentials::WamnCredentials;
use super::wamn_postgres::{ConnectionEffectLookup, ConnectionEffectSnapshot, WamnPostgres};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "runner-http-effect-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::runner::http_effect::{
    self, EffectError, Header, InvocationContext, RelativeRequest, Response,
};

pub const CONNECTION_HTTP_ID: &str = "wamn-connection-http";
const HTTP_CONTRACT: &str = "wamn:connection/http@0.1.0";

/// The authorized floor interpretation: Kubernetes/the network enforces the
/// cluster ceiling on the actual pinned connect.
#[derive(Debug, Clone, Copy)]
struct ExternallyEnforcedNetworkPolicy;

impl NetworkPolicy for ExternallyEnforcedNetworkPolicy {
    fn allows(&self, _address: SocketAddr) -> bool {
        true
    }
}

/// Host-owned services and claims for the trusted HTTP effect.
pub struct ConnectionHttp {
    postgres: Arc<WamnPostgres>,
    vault: Arc<WamnCredentials>,
    egress: Arc<RunnerEgressPolicy>,
    tenant: Box<str>,
    project: Box<str>,
    allowed_hosts: Arc<[AllowedHost]>,
}

impl ConnectionHttp {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        egress: Arc<RunnerEgressPolicy>,
        tenant: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        allowed_hosts: Arc<[AllowedHost]>,
    ) -> Self {
        Self {
            postgres,
            vault,
            egress,
            tenant: tenant.into(),
            project: project.into(),
            allowed_hosts,
        }
    }

    async fn send(
        &self,
        component_id: &str,
        context: &InvocationContext,
        requirement_name: &str,
        request: &RelativeRequest,
    ) -> Result<Response, EffectError> {
        validate_claims(&self.tenant, context, requirement_name, request)?;
        let target = normalize_portable_http_target(&request.path_and_query).map_err(|error| {
            tracing::warn!(
                phase = "target-normalization",
                error = %error,
                "trusted HTTP connection authority denied"
            );
            EffectError::AuthorityDenied
        })?;
        let operation_fingerprint = operation_fingerprint(request, target.clone())
            .map_err(|error| log_effect_authority_denied("operation-fingerprint", error))?;
        let stable_key = format!(
            "{}:{}:{}",
            context.run_id, context.node_id, context.occurrence
        );
        let occurrence =
            i32::try_from(context.occurrence).map_err(|_| EffectError::InvalidContext)?;
        let attempt = i32::try_from(context.attempt).map_err(|_| EffectError::InvalidContext)?;
        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &ConnectionEffectLookup {
                    run_id: &context.run_id,
                    node_id: &context.node_id,
                    occurrence,
                    attempt,
                    requirement_name,
                    flow_id: &context.flow_id,
                    flow_version: i32::try_from(context.flow_version)
                        .map_err(|_| EffectError::InvalidContext)?,
                    catalog_id: &context.catalog_id,
                    catalog_version: context.catalog_version,
                    environment: &context.environment,
                    artifact_digest: &context.artifact_digest,
                    operation_fingerprint: &operation_fingerprint,
                    stable_key: &stable_key,
                },
            )
            .await
            .map_err(|error| EffectError::Transport(error.to_string()))?
            .ok_or(EffectError::InvalidContext)?;
        authorize_snapshot(context, requirement_name, &snapshot)?;

        let definition = snapshot
            .definition
            .as_ref()
            .ok_or(EffectError::InactiveGeneration)?;
        let object = definition.as_object().ok_or(EffectError::Incompatible)?;
        let primary = object
            .get("primary-authority")
            .and_then(serde_json::Value::as_str)
            .ok_or(EffectError::Incompatible)?;
        let tls = match object
            .get("tls-verification")
            .and_then(serde_json::Value::as_str)
        {
            Some("disabled") => TlsPolicy::Disabled,
            Some("verify-authority") => TlsPolicy::VerifyAuthority,
            _ => return Err(EffectError::Incompatible),
        };
        require_direct_transport(object)?;
        let proxy = None;
        let authority = parse_http_connection_authority(primary, tls, proxy)
            .map_err(|error| authority_denied("definition", error))?;
        require_declared_egress(component_id, &self.egress, authority.canonical_base_url())?;
        let decision = resolve_http_request(
            &authority,
            &target,
            &self.allowed_hosts,
            &ExternallyEnforcedNetworkPolicy,
            &TokioDnsResolver,
        )
        .await
        .map_err(|error| authority_denied("request", error))?;

        let handle = snapshot
            .credential_handle
            .as_deref()
            .ok_or(EffectError::CredentialUnavailable)?;
        let secret = self
            .vault
            .lookup(&self.project, handle)
            .ok_or(EffectError::CredentialUnavailable)?;
        let credential_headers = credential_headers(&secret)?;
        execute(decision, request, credential_headers, &stable_key).await
    }
}

fn authority_denied(phase: &'static str, error: AuthorityError) -> EffectError {
    tracing::warn!(
        phase,
        kind = ?error.kind(),
        error = %error,
        "trusted HTTP connection authority denied"
    );
    EffectError::AuthorityDenied
}

fn log_effect_authority_denied(phase: &'static str, error: EffectError) -> EffectError {
    if matches!(error, EffectError::AuthorityDenied) {
        tracing::warn!(phase, "trusted HTTP connection authority denied");
    }
    error
}

fn operation_fingerprint(
    request: &RelativeRequest,
    target: wamn_node_manifest::CanonicalHttpTarget,
) -> Result<String, EffectError> {
    let semantic_headers = request
        .headers
        .iter()
        .filter(|header| wamn_node_manifest::is_http_operation_semantic_header(&header.name))
        .map(|header| {
            let value =
                std::str::from_utf8(&header.value).map_err(|_| EffectError::AuthorityDenied)?;
            Ok(HttpSemanticHeader {
                name: &header.name,
                value,
            })
        })
        .collect::<Result<Vec<_>, EffectError>>()?;
    let fingerprint = fingerprint_http_operation(&HttpOperation {
        method: &request.method,
        target,
        semantic_headers: &semantic_headers,
        body_digest: HttpBodyDigest::sha256(request.body.as_deref().unwrap_or_default()),
    })
    .map_err(|_| EffectError::AuthorityDenied)?;
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in fingerprint.digest() {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

fn require_direct_transport(
    definition: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), EffectError> {
    if matches!(
        definition.get("proxy-transport"),
        Some(serde_json::Value::Null)
    ) && matches!(
        definition.get("proxy-authority"),
        None | Some(serde_json::Value::Null)
    ) {
        Ok(())
    } else {
        Err(EffectError::Incompatible)
    }
}

fn validate_claims(
    tenant: &str,
    context: &InvocationContext,
    requirement_name: &str,
    request: &RelativeRequest,
) -> Result<(), EffectError> {
    if context.version != "0.1"
        || context.tenant_id != tenant
        || context.run_id.is_empty()
        || context.node_id.is_empty()
        || context.artifact_digest.is_empty()
        || context.requirement_name != requirement_name
        || request.method.is_empty()
    {
        return Err(EffectError::InvalidContext);
    }
    Ok(())
}

fn authorize_snapshot(
    context: &InvocationContext,
    requirement_name: &str,
    snapshot: &ConnectionEffectSnapshot,
) -> Result<(), EffectError> {
    if snapshot.run_status != "running"
        || snapshot.flow_id != context.flow_id
        || u32::try_from(snapshot.flow_version).ok() != Some(context.flow_version)
        || snapshot.catalog_id.as_deref() != Some(context.catalog_id.as_str())
        || snapshot.catalog_version != Some(i64::from(context.catalog_version))
        || snapshot.environment.as_deref() != Some(context.environment.as_str())
        || snapshot.admitted_artifact_digest.as_deref() != Some(context.artifact_digest.as_str())
        || !snapshot.source_admitted
        || !snapshot.attempt_matches
    {
        return Err(EffectError::InvalidContext);
    }
    let Some(requirement) = snapshot.requirement_json.as_ref() else {
        return Err(EffectError::UndeclaredRequirement);
    };
    if snapshot.node_connection.as_deref() != Some(requirement_name) || !snapshot.node_permitted {
        return Err(EffectError::NodeNotPermitted);
    }
    if !snapshot.binding_active || !snapshot.binding_valid || snapshot.instance_id.is_none() {
        return Err(EffectError::Unbound);
    }
    if !snapshot.instance_enabled
        || snapshot.active_generation.is_none()
        || snapshot.active_generation != snapshot.generation
    {
        return Err(EffectError::InactiveGeneration);
    }
    if !snapshot.draft_generation_granted {
        return Err(EffectError::AuthorityDenied);
    }
    if snapshot.requirement_type.as_deref() != Some("http")
        || snapshot.contract.as_deref() != Some(HTTP_CONTRACT)
        || requirement
            .pointer("/descriptor/requirement-type")
            .and_then(serde_json::Value::as_str)
            != Some("http")
        || requirement
            .pointer("/descriptor/contract")
            .and_then(serde_json::Value::as_str)
            != Some(HTTP_CONTRACT)
    {
        return Err(EffectError::Incompatible);
    }
    if !snapshot.attempt_recorded {
        return Err(EffectError::InvalidContext);
    }
    Ok(())
}

fn require_declared_egress(
    component_id: &str,
    policy: &RunnerEgressPolicy,
    logical_url: &str,
) -> Result<(), EffectError> {
    let uri: hyper::Uri = logical_url.parse().map_err(|error| {
        tracing::warn!(
            phase = "flow-egress-url",
            error = %error,
            "trusted HTTP connection authority denied"
        );
        EffectError::AuthorityDenied
    })?;
    if policy.allows_connection(component_id, &uri) {
        Ok(())
    } else {
        tracing::warn!(
            component_id,
            logical_url,
            "trusted HTTP connection denied by the flow egress narrowing"
        );
        Err(EffectError::AuthorityDenied)
    }
}

fn credential_headers(secret: &str) -> Result<HashMap<String, String>, EffectError> {
    let value: serde_json::Value =
        serde_json::from_str(secret).map_err(|_| EffectError::CredentialUnavailable)?;
    let object = value
        .as_object()
        .ok_or(EffectError::CredentialUnavailable)?;
    if object.len() != 1 || !object.contains_key("headers") {
        return Err(EffectError::CredentialUnavailable);
    }
    object["headers"]
        .as_object()
        .ok_or(EffectError::CredentialUnavailable)?
        .iter()
        .map(|(name, value)| {
            let value = value.as_str().ok_or(EffectError::CredentialUnavailable)?;
            let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| EffectError::CredentialUnavailable)?;
            if reserved_header(&header) {
                return Err(EffectError::CredentialUnavailable);
            }
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| EffectError::CredentialUnavailable)?;
            Ok((name.clone(), value.to_string()))
        })
        .collect()
}

fn reserved_header(name: &reqwest::header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-connection"
            | "proxy-authorization"
            | "idempotency-key"
    )
}

fn outbound_headers(
    request: &RelativeRequest,
    credentials: HashMap<String, String>,
    idempotency_key: &str,
) -> Result<reqwest::header::HeaderMap, EffectError> {
    let mut headers = reqwest::header::HeaderMap::new();
    for header in &request.headers {
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| EffectError::AuthorityDenied)?;
        if reserved_header(&name) {
            return Err(EffectError::AuthorityDenied);
        }
        let value = reqwest::header::HeaderValue::from_bytes(&header.value)
            .map_err(|_| EffectError::AuthorityDenied)?;
        headers.append(name, value);
    }
    for (name, value) in credentials {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| EffectError::CredentialUnavailable)?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| EffectError::CredentialUnavailable)?;
        headers.insert(name, value);
    }
    let idempotency_key = reqwest::header::HeaderValue::from_str(idempotency_key)
        .map_err(|_| EffectError::InvalidContext)?;
    headers.insert(
        reqwest::header::HeaderName::from_static("idempotency-key"),
        idempotency_key,
    );
    Ok(headers)
}

async fn execute(
    decision: crate::connection_authority::AuthorityDecision,
    request: &RelativeRequest,
    credentials: HashMap<String, String>,
    idempotency_key: &str,
) -> Result<Response, EffectError> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| EffectError::Transport("invalid HTTP method".into()))?;
    let headers = outbound_headers(request, credentials, idempotency_key)
        .map_err(|error| log_effect_authority_denied("outbound-headers", error))?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(30));
    let host = decision.logical_authority.host();
    match &decision.transport {
        TransportDecision::Direct { origin } => {
            builder = builder.resolve(host, origin.address);
        }
        TransportDecision::Proxy { .. } => return Err(EffectError::Incompatible),
    }
    let client = builder
        .build()
        .map_err(|error| EffectError::Transport(error.to_string()))?;
    let mut outbound = client
        .request(method, decision.logical_url.as_ref())
        .headers(headers);
    if let Some(body) = &request.body {
        outbound = outbound.body(body.clone());
    }
    let response = outbound.send().await.map_err(|error| {
        if error.is_timeout() {
            EffectError::Timeout
        } else {
            EffectError::Transport(error.to_string())
        }
    })?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| Header {
            name: name.as_str().to_string(),
            value: value.as_bytes().to_vec(),
        })
        .collect();
    let body = response
        .bytes()
        .await
        .map_err(|error| EffectError::Transport(error.to_string()))?
        .to_vec();
    Ok(Response {
        status,
        headers,
        body,
    })
}

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    http_effect::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

impl HostPlugin for ConnectionHttp {
    fn id(&self) -> &'static str {
        CONNECTION_HTTP_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:runner/http-effect@0.1.0")]),
            exports: HashSet::new(),
        }
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<ConnectionHttp>> {
    ctx.try_get_plugin::<ConnectionHttp>(CONNECTION_HTTP_ID)
}

impl http_effect::Host for ActiveCtx<'_> {
    async fn send(
        &mut self,
        context: InvocationContext,
        requirement_name: String,
        request: RelativeRequest,
    ) -> wash_runtime::wasmtime::Result<Result<Response, EffectError>> {
        let plugin = plugin_of(self)?;
        let result = plugin
            .send(
                self.component_id.as_ref(),
                &context,
                &requirement_name,
                &request,
            )
            .await;
        if let Err(error) = &result {
            tracing::warn!(
                error = ?error,
                run_id = context.run_id,
                node_id = context.node_id,
                occurrence = context.occurrence,
                attempt = context.attempt,
                requirement_name,
                "trusted HTTP effect refused"
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> InvocationContext {
        InvocationContext {
            version: "0.1".to_string(),
            tenant_id: "tenant-a".into(),
            environment: "prod".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 3,
            run_id: "run-a".into(),
            flow_id: "flow-a".into(),
            flow_version: 2,
            artifact_digest: "sha256:artifact".into(),
            node_id: "notify".into(),
            occurrence: 0,
            attempt: 1,
            requirement_name: "manager-notifications".into(),
        }
    }

    fn snapshot() -> ConnectionEffectSnapshot {
        ConnectionEffectSnapshot {
            run_status: "running".into(),
            flow_id: "flow-a".into(),
            flow_version: 2,
            catalog_id: Some("catalog-a".into()),
            catalog_version: Some(3),
            environment: Some("prod".into()),
            admitted_artifact_digest: Some("sha256:artifact".into()),
            source_admitted: true,
            attempt_matches: true,
            requirement_json: Some(serde_json::json!({
                "descriptor": {
                    "requirement-type": "http",
                    "contract": HTTP_CONTRACT
                }
            })),
            node_connection: Some("manager-notifications".into()),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some("notifications".into()),
            requirement_type: Some("http".into()),
            contract: Some(HTTP_CONTRACT.into()),
            instance_enabled: true,
            active_generation: Some(7),
            generation: Some(7),
            definition: Some(serde_json::json!({})),
            definition_hash: Some("sha256:def".into()),
            credential_handle: Some("notify-auth".into()),
            draft_generation_granted: true,
            attempt_recorded: true,
        }
    }

    #[test]
    fn refusal_precedence_is_explicit_and_typed() {
        let context = context();
        let mut facts = snapshot();
        facts.requirement_json = None;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &facts),
            Err(EffectError::UndeclaredRequirement)
        ));

        let mut facts = snapshot();
        facts.node_permitted = false;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &facts),
            Err(EffectError::NodeNotPermitted)
        ));

        let mut facts = snapshot();
        facts.instance_enabled = false;
        facts.active_generation = None;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &facts),
            Err(EffectError::InactiveGeneration)
        ));
    }

    #[test]
    fn callback_binding_mutant_is_denied_before_transport_resolution() {
        let context = context();
        let mut inactive = snapshot();
        inactive.binding_active = false;
        let mut invalid = snapshot();
        invalid.binding_valid = false;
        let mut missing = snapshot();
        missing.instance_id = None;
        for facts in [&inactive, &invalid, &missing] {
            assert!(matches!(
                authorize_snapshot(&context, "manager-notifications", facts),
                Err(EffectError::Unbound)
            ));
        }
    }

    #[test]
    fn wrong_attempt_and_wrong_run_identity_fail_before_authorization() {
        let context = context();
        let mut wrong_attempt = snapshot();
        wrong_attempt.attempt_matches = false;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &wrong_attempt),
            Err(EffectError::InvalidContext)
        ));

        let mut wrong_run = snapshot();
        wrong_run.admitted_artifact_digest = Some("sha256:other-run-artifact".into());
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &wrong_run),
            Err(EffectError::InvalidContext)
        ));
    }

    #[test]
    fn durable_intent_is_required_before_the_wire_path() {
        let context = context();
        let mut missing = snapshot();
        missing.attempt_recorded = false;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &missing),
            Err(EffectError::InvalidContext)
        ));
        assert!(authorize_snapshot(&context, "manager-notifications", &snapshot()).is_ok());
    }

    #[test]
    fn mismatched_source_or_revoked_draft_generation_is_denied_before_network_data() {
        let context = context();
        let mut mismatched_source = snapshot();
        mismatched_source.source_admitted = false;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &mismatched_source),
            Err(EffectError::InvalidContext)
        ));

        let mut revoked = snapshot();
        revoked.draft_generation_granted = false;
        assert!(matches!(
            authorize_snapshot(&context, "manager-notifications", &revoked),
            Err(EffectError::AuthorityDenied)
        ));
    }

    /// Until .4.9 installs the immutable attempt reader/writer together, the
    /// production `send` path refuses before a socket reaches the listener.
    #[tokio::test]
    async fn live_missing_immutable_attempt_authority_is_denied_before_wire() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::io::AsyncWriteExt as _;

        use crate::plugins::wamn_postgres::{DEFAULT_PROJECT, WamnPostgresConfig};

        const OPERATION_FINGERPRINT: &str =
            "sha256:a7f547c9a327cb96a331dde7f8760ef8c8b16b63174aac4864019001ebf25e79";
        const PROBE_AUTHORITY: &str = "127.0.0.1:18081";

        let Some(url) = std::env::var("WAMN_CONNECTION_EFFECT_PG_URL").ok() else {
            return;
        };
        let listener = tokio::net::TcpListener::bind(PROBE_AUTHORITY)
            .await
            .expect("bind revoked-draft zero-wire probe");
        let wire_count = Arc::new(AtomicUsize::new(0));
        let server_wire_count = Arc::clone(&wire_count);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept probe request");
            server_wire_count.fetch_add(1, Ordering::SeqCst);
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write probe response");
        });

        let postgres = Arc::new(
            WamnPostgres::new(WamnPostgresConfig {
                database_url: Some(url),
                pool_max_size: 1,
                wait_timeout_ms: 2_000,
                statement_timeout_ms: 5_000,
                row_limit: 1_000,
            })
            .expect("build live Postgres adapter"),
        );
        postgres
            .set_schema("draft-http-runner", "wamn_run")
            .expect("set live run schema");

        let request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/probe".into(),
            headers: Vec::new(),
            body: None,
        };
        let target = normalize_portable_http_target(&request.path_and_query)
            .expect("canonical probe target");
        assert_eq!(
            operation_fingerprint(&request, target).expect("probe operation fingerprint"),
            OPERATION_FINGERPRINT,
            "the live fixture's durable intent must pin this exact request"
        );
        let lookup = ConnectionEffectLookup {
            run_id: "draft-http-granted",
            node_id: "notify",
            occurrence: 0,
            attempt: 0,
            requirement_name: "manager",
            flow_id: "http-flow",
            flow_version: 2,
            catalog_id: "c-http",
            catalog_version: 1,
            environment: "dev",
            artifact_digest: "artifact-http-draft",
            operation_fingerprint: OPERATION_FINGERPRINT,
            stable_key: "draft-http-granted:notify:0",
        };
        let snapshot = postgres
            .connection_effect_snapshot("draft-http-runner", DEFAULT_PROJECT, "t1", &lookup)
            .await
            .expect("load live draft authorization snapshot")
            .expect("live exact draft run exists");
        assert!(snapshot.source_admitted);
        assert!(!snapshot.attempt_matches);
        assert!(!snapshot.attempt_recorded);
        assert_eq!(snapshot.active_generation, Some(1));
        assert_eq!(snapshot.generation, Some(1));
        assert!(!snapshot.draft_generation_granted);

        let vault = Arc::new(WamnCredentials::from_projects(HashMap::from([(
            DEFAULT_PROJECT.to_string(),
            HashMap::from([(
                "manager-credential-v1".to_string(),
                r#"{"headers":{}}"#.to_string(),
            )]),
        )])));
        let allowed_hosts: Arc<[AllowedHost]> =
            vec![PROBE_AUTHORITY.parse().expect("parse probe host policy")].into();
        let connection = ConnectionHttp::new(
            postgres,
            vault,
            Arc::new(RunnerEgressPolicy::default()),
            "t1",
            DEFAULT_PROJECT,
            allowed_hosts,
        );
        let context = InvocationContext {
            version: "0.1".to_string(),
            tenant_id: "t1".into(),
            environment: "dev".into(),
            catalog_id: "c-http".into(),
            catalog_version: 1,
            run_id: "draft-http-granted".into(),
            flow_id: "http-flow".into(),
            flow_version: 2,
            artifact_digest: "artifact-http-draft".into(),
            node_id: "notify".into(),
            occurrence: 0,
            attempt: 0,
            requirement_name: "manager".into(),
        };

        let result = connection
            .send("draft-http-runner", &context, "manager", &request)
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let observed_wires = wire_count.load(Ordering::SeqCst);
        server.abort();

        assert!(matches!(result, Err(EffectError::InvalidContext)));
        assert_eq!(
            observed_wires, 0,
            "missing immutable attempt authority must deny all network access"
        );
    }

    #[test]
    fn credential_set_is_strict_structured_json() {
        let headers = credential_headers(
            r#"{"headers":{"authorization":"Bearer secret","x-api-key":"key"}}"#,
        )
        .expect("credential set");
        assert_eq!(headers["authorization"], "Bearer secret");
        assert_eq!(headers["x-api-key"], "key");
        for invalid in [
            "secret",
            r#"{"headers":[],"extra":{}}"#,
            r#"{"headers":{"bad header":"secret"}}"#,
            r#"{"headers":{"host":"other.example"}}"#,
        ] {
            assert!(matches!(
                credential_headers(invalid),
                Err(EffectError::CredentialUnavailable)
            ));
        }
    }

    #[test]
    fn authority_headers_are_host_owned_and_credentials_override_author_headers() {
        let mut request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/holds".into(),
            headers: vec![Header {
                name: "authorization".into(),
                value: b"caller-value".to_vec(),
            }],
            body: None,
        };
        let headers = outbound_headers(
            &request,
            HashMap::from([("authorization".into(), "Bearer host-value".into())]),
            "run-a:notify:0",
        )
        .expect("headers");
        assert_eq!(headers["authorization"], "Bearer host-value");
        assert_eq!(headers["idempotency-key"], "run-a:notify:0");

        request.headers = vec![Header {
            name: "host".into(),
            value: b"other.example".to_vec(),
        }];
        assert!(matches!(
            outbound_headers(&request, HashMap::new(), "run-a:notify:0"),
            Err(EffectError::AuthorityDenied)
        ));
    }

    #[test]
    fn idempotency_key_is_system_owned_and_injected_exactly_once() {
        let request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/holds".into(),
            headers: Vec::new(),
            body: None,
        };
        let key = "run-a:notify:0";
        let headers = outbound_headers(&request, HashMap::new(), key).expect("headers");
        assert_eq!(headers.get_all("idempotency-key").iter().count(), 1);
        assert_eq!(headers["idempotency-key"], key);

        let authored = RelativeRequest {
            headers: vec![Header {
                name: "Idempotency-Key".into(),
                value: b"author-controlled".to_vec(),
            }],
            ..request
        };
        assert!(matches!(
            outbound_headers(&authored, HashMap::new(), key),
            Err(EffectError::AuthorityDenied)
        ));
        assert!(matches!(
            credential_headers(r#"{"headers":{"idempotency-key":"credential-controlled"}}"#),
            Err(EffectError::CredentialUnavailable)
        ));
    }

    #[test]
    fn trace_propagation_does_not_change_the_operation_fingerprint() {
        let target = normalize_portable_http_target("/holds").unwrap();
        let mut request = RelativeRequest {
            method: "POST".into(),
            path_and_query: "/holds".into(),
            headers: vec![Header {
                name: "content-type".into(),
                value: b"application/json".to_vec(),
            }],
            body: Some(br#"{"hold":7}"#.to_vec()),
        };
        let before = operation_fingerprint(&request, target.clone()).unwrap();
        request.headers.push(Header {
            name: "traceparent".into(),
            value: b"00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_vec(),
        });
        assert_eq!(operation_fingerprint(&request, target).unwrap(), before);
    }

    #[test]
    fn proxy_generation_is_incompatible_without_direct_fallback() {
        let direct = serde_json::json!({"proxy-transport": null});
        assert!(require_direct_transport(direct.as_object().unwrap()).is_ok());

        let proxied = serde_json::json!({
            "proxy-transport": "connect",
            "proxy-authority": "http://proxy.internal:8080/"
        });
        assert!(matches!(
            require_direct_transport(proxied.as_object().unwrap()),
            Err(EffectError::Incompatible)
        ));
    }
}
