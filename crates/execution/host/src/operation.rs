//! One export call of one admitted component, shared by every entry.
//!
//! The router driver calls [`invoke_operation`] for each node of a wiring walk.
//! The route path calls it once for a route, with no walk. The released
//! application, its store and plugins, and the deadline belong here, so both
//! entries run a component the same way.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use futures_util::{StreamExt as _, stream};
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt as _;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wamn_catalog::{
    AdmittedComponent, ArtifactHash, ComponentOperationDependency, ComponentSqlField,
    ComponentSqlValueType, ServingComponent, ServingComponentOperation,
};
use wamn_event_wire::Causation;
use wamn_project_state::PlatformComponent;
use wamn_runtime::component_artifact_source::ComponentArtifactSource;
use wamn_runtime::engine::MAX_HOST_CALL_DURATION;
use wamn_runtime::plugins::EffectEvidence;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
    InvocationEntry,
};
use wamn_runtime::plugins::flow_http_routing::{AuthenticatedCaller, CredentialKind};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    PreparedStatementSet, ReleaseIdentity, SessionClaims, StatementField, StatementValueType,
    VerifiedStatement, VerifiedStatementSet, WamnPostgres,
};
use wamn_runtime::release_manifest::LoadedRelease;
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;

use crate::warm_reuse::WarmReuse;

mod native_call;
mod native_policy;
mod native_workload;

use native_call::{NativeInvocation, invoke_native, prepare_native};
use native_policy::{NATIVE_POLICY_ID, NativePolicyResources, new_native_policy};
pub(crate) use native_workload::{NativeApplication, NativeComponent};
use native_workload::{NativeWorkloadSpec, load_native_application};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../router/wit",
        world: "node",
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

pub(crate) use bindings::wamn::node::types as node_types;

/// Keep at most two verified artifact fetches in flight per release load.
/// Native workload loading owns compilation after these bounded fetches finish.
const COMPONENT_FETCH_CONCURRENCY: usize = 2;

/// Why an originating caller cannot invoke a registered operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationRefusalKind {
    PermissionDenied,
    FreshCredentialRequired,
}

/// Exact operation authority missing from the originating caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationRefusal {
    kind: OperationRefusalKind,
    operation: Box<str>,
}

impl OperationRefusal {
    pub(crate) fn new(kind: OperationRefusalKind, operation: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            operation: operation.into(),
        }
    }

    pub(crate) fn kind(&self) -> OperationRefusalKind {
        self.kind
    }

    pub(crate) fn operation(&self) -> &str {
        &self.operation
    }
}

impl fmt::Display for OperationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            OperationRefusalKind::PermissionDenied => "permission denied",
            OperationRefusalKind::FreshCredentialRequired => "fresh-credential-required",
        };
        write!(formatter, "{reason} for operation {}", self.operation)
    }
}

impl std::error::Error for OperationRefusal {}

pub(crate) fn authorize_registered_operation(
    caller: Option<&AuthenticatedCaller>,
    operation: Option<&str>,
    fresh_only: bool,
) -> Result<(), OperationRefusal> {
    let Some(operation) = operation else {
        return Ok(());
    };
    let caller = caller
        .filter(|caller| caller.permits(operation))
        .ok_or_else(|| OperationRefusal::new(OperationRefusalKind::PermissionDenied, operation))?;
    // Human sessions now receive a current authority check at request admission.
    // This does not widen the separately admitted queued-service contract.
    if fresh_only && caller.credential_kind() == CredentialKind::QueuedService {
        return Err(OperationRefusal::new(
            OperationRefusalKind::FreshCredentialRequired,
            operation,
        ));
    }
    Ok(())
}

struct TraceHeaders<'a> {
    traceparent: &'a str,
    tracestate: Option<&'a str>,
}

impl Extractor for TraceHeaders<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case("traceparent") {
            Some(self.traceparent)
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self.tracestate {
            Some(_) => vec!["traceparent", "tracestate"],
            None => vec!["traceparent"],
        }
    }
}

/// The pair of W3C fields the router hands one guest, written by the global
/// propagator.
///
/// The mirror image of [`TraceHeaders`]: that one reads the caller's headers in,
/// this one writes the host's own span out.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NodeTraceContext {
    pub(crate) traceparent: Option<String>,
    pub(crate) tracestate: Option<String>,
}

impl opentelemetry::propagation::Injector for NodeTraceContext {
    fn set(&mut self, key: &str, value: String) {
        // The W3C propagator writes `tracestate` even when the context carries
        // none, and an empty field is not a field.
        let value = (!value.is_empty()).then_some(value);
        if key.eq_ignore_ascii_case("traceparent") {
            self.traceparent = value;
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate = value;
        }
    }
}

/// The context the guest — and every hop below it — must parent to: the
/// `wamn.component.invoke` span this node is running under, NOT the raw ingress
/// header that opened the delivery.
///
/// Forwarding the ingress header verbatim made every downstream service a
/// sibling of the host rather than its child, skipping the component span
/// entirely, and left the queue path (which carries no ingress header at all,
/// by the ratified host-scoped re-root) with no context to send.
///
/// Read through [`tracing_opentelemetry::OpenTelemetrySpanExt`], never
/// `opentelemetry::global::tracer`: the runtime's `initialize_observability`
/// installs the layer but no global tracer provider, so the global tracer is a
/// silent no-op.
pub(crate) fn node_trace_context(
    traceparent: Option<&str>,
    tracestate: Option<&str>,
) -> NodeTraceContext {
    let mut carrier = NodeTraceContext::default();
    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut carrier);
    });
    if carrier.traceparent.is_none() {
        // A tracing subscriber without the OTel layer is the supported
        // no-export mode: the span has no exportable context to inject, so pass
        // the caller's own header through rather than dropping propagation.
        carrier.traceparent = traceparent.map(str::to_owned);
        carrier.tracestate = tracestate.map(str::to_owned);
    }
    carrier
}

/// The caller's W3C parent, or `None` when the header is absent or malformed.
pub(crate) fn remote_trace_context(
    traceparent: Option<&str>,
    tracestate: Option<&str>,
) -> Option<opentelemetry::Context> {
    let headers = TraceHeaders {
        traceparent: traceparent?,
        tracestate,
    };
    let context =
        opentelemetry::global::get_text_map_propagator(|propagator| propagator.extract(&headers));
    if context.span().span_context().is_valid() {
        Some(context)
    } else {
        None
    }
}

/// The coordinates of one `wamn.component.invoke` span.
///
/// A route has no wiring position, so it names the empty wiring, version 0
/// and the empty node, the same as its effect spans.
pub(crate) struct InvocationSite<'a> {
    pub(crate) tenant_id: &'a str,
    pub(crate) project: &'a str,
    pub(crate) environment: &'a str,
    pub(crate) wiring_id: &'a str,
    pub(crate) wiring_version: u32,
    pub(crate) node_id: &'a str,
    pub(crate) input_port: Option<&'a str>,
    pub(crate) operation: &'a str,
    pub(crate) component_digest: &'a str,
    pub(crate) caller: Option<&'a AuthenticatedCaller>,
}

pub(crate) fn invocation_span(
    site: &InvocationSite<'_>,
    remote_parent: Option<&opentelemetry::Context>,
) -> tracing::Span {
    let span = tracing::info_span!(
        target: "wamn::router",
        "wamn.component.invoke",
        wamn.tenant = %site.tenant_id,
        wamn.project = %site.project,
        wamn.environment = %site.environment,
        wamn.wiring_id = %site.wiring_id,
        wamn.wiring_version = site.wiring_version,
        wamn.component_digest = %site.component_digest,
        wamn.node_id = %site.node_id,
        wamn.operation = %site.operation,
        wamn.caller_principal_id = tracing::field::Empty,
        wamn.caller_credential_kind = tracing::field::Empty,
        wamn.input_port = tracing::field::Empty,
    );
    if let Some(caller) = site.caller {
        span.record("wamn.caller_principal_id", caller.principal_id());
        span.record(
            "wamn.caller_credential_kind",
            match caller.credential_kind() {
                CredentialKind::Pat => "pat",
                CredentialKind::Session => "session",
                CredentialKind::QueuedService => "queued-service",
            },
        );
    }
    if let Some(input_port) = site.input_port {
        span.record("wamn.input_port", input_port);
    }
    if let Some(parent) = remote_parent {
        // A tracing subscriber without the OTel layer is the supported
        // no-export mode. `set_parent` may then refuse; the span still records
        // locally and no invocation is affected.
        let _ = span.set_parent(parent.clone());
    }
    span
}

/// The process capabilities that one released application runs on.
///
/// The router driver and the route path share one host, so both call into the
/// same loaded application.
pub(crate) struct OperationHost {
    engine: Arc<Engine>,
    pub(crate) postgres: Arc<WamnPostgres>,
    /// Shared by every driver in this process, independently of fresh stores.
    http_transport: Arc<HttpTransport>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    pub(crate) release: Arc<LoadedRelease>,
    pub(crate) source: ComponentArtifactSource,
    pub(crate) project: String,
    schema: Option<String>,
    owner_prefix: String,
    warm_reuse: WarmReuse,
    native: tokio::sync::OnceCell<Arc<NativeApplication>>,
    /// The complete release component list a route loads, read once.
    components: tokio::sync::OnceCell<Arc<[AdmittedComponent]>>,
}

/// The process-owned facts of an [`OperationHost`], outside its capabilities.
pub(crate) struct OperationScope {
    pub(crate) project: String,
    pub(crate) schema: Option<String>,
    pub(crate) owner_prefix: String,
    pub(crate) warm_reuse: WarmReuse,
}

impl OperationHost {
    /// Bind one release to the process-owned capabilities.
    #[expect(
        clippy::too_many_arguments,
        reason = "each host-owned capability is an independent production dependency"
    )]
    pub(crate) fn new(
        engine: Arc<Engine>,
        postgres: Arc<WamnPostgres>,
        http_transport: Arc<HttpTransport>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<LoadedRelease>,
        source: ComponentArtifactSource,
        scope: OperationScope,
    ) -> Self {
        Self {
            engine,
            postgres,
            http_transport,
            credentials,
            logging,
            allowed_hosts,
            release,
            source,
            project: scope.project,
            schema: scope.schema,
            owner_prefix: scope.owner_prefix,
            warm_reuse: scope.warm_reuse,
            native: tokio::sync::OnceCell::new(),
            components: tokio::sync::OnceCell::new(),
        }
    }

    /// The identity of the carried release, bound on every released call.
    pub(crate) fn release_identity(&self) -> ReleaseIdentity {
        ReleaseIdentity {
            effective_release_id: self.release.release().effective_release_id,
            manifest_digest: self.release.release().manifest_digest.clone(),
        }
    }

    /// The session claims of one call, before activation binds its principal.
    pub(crate) fn claims(&self, tenant: &str, release: Option<ReleaseIdentity>) -> SessionClaims {
        SessionClaims {
            tenant: tenant.to_owned(),
            project: Some(self.project.clone()),
            schema: self.schema.clone(),
            runner: Some(self.owner_prefix.clone()),
            role: None,
            // Activation binds the executing principal from the caller or
            // `platform`, so a nested call derives the same one.
            user_id: None,
            // Activation binds the operation token of `invocation`, so a
            // nested call binds its own operation.
            operation: None,
            release,
        }
    }

    /// The complete component list of the carried release, read once.
    ///
    /// A wiring reads the same list beside its graph, so a route and a wiring
    /// load one application.
    pub(crate) async fn release_components(&self) -> anyhow::Result<Arc<[AdmittedComponent]>> {
        self.components
            .get_or_try_init(|| async {
                let manifest = self.release.manifest();
                self.postgres
                    .resolve_release_components(
                        &self.project,
                        &manifest.release.tenant_id,
                        &manifest.release.environment,
                        manifest.release.effective_release_id.get(),
                        self.release.release().manifest_digest.as_str(),
                    )
                    .await
                    .map(Arc::from)
            })
            .await
            .map(Arc::clone)
    }

    /// Load the released application and initialize each component without
    /// invoking its handler, so the first request finds it resident.
    pub(crate) async fn prepare_released(
        &self,
        components: &[AdmittedComponent],
    ) -> anyhow::Result<()> {
        let application = self.released_application(components).await?;
        for id in application.workload.facts_by_component_id.keys() {
            let target = application
                .workload
                .resolved
                .dispatch_target(id, NATIVE_POLICY_ID)
                .await?;
            prepare_native(
                &target,
                tokio::time::Instant::now() + Duration::from_millis(bounded_node_deadline_ms(None)),
            )
            .await
            .with_context(|| format!("initialize native release component {id:?}"))?;
        }
        Ok(())
    }

    pub(crate) async fn released_application(
        &self,
        components: &[AdmittedComponent],
    ) -> anyhow::Result<Arc<NativeApplication>> {
        for component in components {
            validate_component_in_release(&self.release, component)?;
        }
        let application = self
            .native
            .get_or_try_init(|| async {
                let pulls = components.iter().cloned().map(|fact| {
                    let source = self.source.clone();
                    async move {
                        let bytes = source
                            .pull_verified(&fact)
                            .instrument(tracing::info_span!(
                                "wamn.component.pull",
                                wamn.component_digest = %fact.component_digest,
                            ))
                            .await?;
                        anyhow::Ok(NativeComponent { fact, bytes })
                    }
                });
                let mut pulls = stream::iter(pulls).buffered(COMPONENT_FETCH_CONCURRENCY);
                let mut native = Vec::with_capacity(components.len());
                while let Some(component) = pulls.next().await {
                    native.push(component?);
                }
                self.load_application(native).await
            })
            .await?;
        let loaded = &application.workload.facts_by_component_id;
        anyhow::ensure!(
            components.len() == loaded.len()
                && components
                    .iter()
                    .all(|fact| loaded.values().any(|loaded| loaded == fact)),
            "native-release-component-closure-mismatch"
        );
        Ok(Arc::clone(application))
    }

    pub(crate) async fn load_application(
        &self,
        components: Vec<NativeComponent>,
    ) -> anyhow::Result<Arc<NativeApplication>> {
        let facts: Vec<_> = components
            .iter()
            .map(|component| component.fact.clone())
            .collect();
        let policy = new_native_policy(
            &facts,
            NativePolicyResources {
                postgres: Arc::clone(&self.postgres),
                logging: Arc::clone(&self.logging),
                connection_http: Arc::new(ConnectionHttp::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.http_transport),
                    Arc::clone(&self.credentials),
                    self.release.manifest().release.tenant_id.as_str(),
                    self.project.as_str(),
                    Arc::clone(&self.allowed_hosts),
                    Some(Arc::clone(&self.release)),
                )),
                blobstore: Arc::new(WamnBlobstore::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.credentials),
                    self.release.manifest().release.tenant_id.as_str(),
                    self.project.as_str(),
                    Some(Arc::clone(&self.release)),
                )),
                release: Arc::clone(&self.release),
                project: self.project.clone(),
            },
        )?;
        let world = policy.world();
        // This list requests plugin binding. Native initialization already
        // links WASI, so copying every admitted import here would require a
        // second provider for clocks and polling that the engine supplies.
        let host_interfaces = world
            .imports
            .into_iter()
            .chain(world.exports)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
            HashMap::from([(NATIVE_POLICY_ID, Arc::clone(&policy) as Arc<dyn HostPlugin>)]);
        load_native_application(
            Arc::clone(&self.engine),
            NativeWorkloadSpec {
                id: next_scope("wamn-application").into(),
                namespace: self.project.clone(),
                name: self.owner_prefix.clone(),
                components,
                warm_reuse: self.warm_reuse.clone(),
                local_resources: wash_runtime::types::LocalResources::default(),
                host_interfaces,
            },
            policy,
            &plugins,
            &wash_runtime::plugin::PluginBindings::new(),
            &wash_runtime::observability::Meters::new(
                wash_runtime::observability::MeterKind::Duration,
            ),
        )
        .await
    }
}

/// The connection invocation of one admitted component at one entry.
pub(crate) fn component_invocation(
    component: &AdmittedComponent,
    operation: &str,
    entry: InvocationEntry,
    closure: ConnectionExecutionClosure,
    effects: Option<EffectEvidence>,
) -> ConnectionInvocation {
    ConnectionInvocation {
        origin: ConnectionOrigin {
            package_id: component.scope.package_id.clone(),
            component_digest: component.component_digest.clone(),
            component: component.component.clone(),
            interface_version: component.interface_version.clone(),
            operation: operation.to_owned(),
        },
        entry,
        package_id: component.scope.package_id.clone(),
        component_digest: component.component_digest.clone(),
        // The admitted component name, read off the catalog fact this
        // call resolved to. The per-request pooled scope is an instance
        // id and names no component a reader can look up, so an effect
        // span takes its component identity from here (`wamn-b2m6.7`).
        component: component.component.clone(),
        operation: operation.to_owned(),
        closure,
        effects,
    }
}

/// The application that one operation call runs in.
#[derive(Clone, Copy)]
pub(crate) enum OperationClosure<'a> {
    /// The carried release, loaded once from its complete component list.
    Released(&'a [AdmittedComponent]),
    /// A candidate application that the caller loaded and unbinds.
    Candidate(&'a Arc<NativeApplication>),
}

/// One export call of one admitted component.
pub(crate) struct OperationCall<'a> {
    pub(crate) closure: OperationClosure<'a>,
    pub(crate) component: &'a AdmittedComponent,
    pub(crate) operation: &'a str,
    pub(crate) context: node_types::NodeContext,
    pub(crate) input: &'a serde_json::Value,
    /// The bounded deadline, the same value as `context.deadline_ms`.
    pub(crate) deadline_ms: u64,
    pub(crate) acquisition: NodeAcquisition,
    pub(crate) caller: Option<AuthenticatedCaller>,
}

/// Call one export once, under the call's deadline, and return what the
/// component returned. The caller lowers the result for its own entry.
pub(crate) async fn invoke_operation(
    host: &OperationHost,
    call: OperationCall<'_>,
    #[expect(
        unused_variables,
        reason = "route intent logging is a later epic; every caller passes None"
    )]
    intent: Option<&dyn wamn_run_state::IntentStore>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(call.deadline_ms);
    tokio::time::timeout_at(deadline, async {
        let application = match call.closure {
            OperationClosure::Released(components) => host.released_application(components).await?,
            OperationClosure::Candidate(application) => Arc::clone(application),
        };
        let id = application
            .workload
            .facts_by_component_id
            .iter()
            .find_map(|(id, fact)| (fact == call.component).then_some(id))
            .context("native-node-component-fact-missing")?;
        let target = application
            .workload
            .resolved
            .dispatch_target(id, NATIVE_POLICY_ID)
            .await?;
        let input = serde_json::to_string(call.input).context("encode node input")?;
        invoke_native(
            &target,
            NativeInvocation {
                operation: call.operation.to_owned(),
                context: call.context,
                input: input.into(),
                deadline,
                transaction_participation: None,
                selected_participant: None,
                acquisition: call.acquisition,
                caller: call.caller,
                application,
            },
        )
        .await
    })
    .await
    .context("native node enclosing deadline elapsed")?
}

pub(crate) fn validate_component_in_release(
    release: &LoadedRelease,
    component: &AdmittedComponent,
) -> anyhow::Result<()> {
    let manifest = release.manifest();
    let package_version = manifest
        .release
        .packages
        .iter()
        .find(|package| package.package_id() == component.scope.package_id)
        .map(wamn_catalog::PackageCoordinate::package_version);
    anyhow::ensure!(
        component.scope.tenant_id == manifest.release.tenant_id
            && package_version == Some(component.scope.package_version.as_str()),
        "release-component-scope-mismatch"
    );
    let expected = ServingComponent {
        package_id: component.scope.package_id.clone(),
        component: component.component.clone(),
        interface_version: component.interface_version.clone(),
        digest: ArtifactHash::parse(component.component_digest.clone())
            .context("component fact carries a non-canonical artifact hash")?,
        operations: component
            .operations
            .iter()
            .map(|(name, operation)| {
                (
                    name.clone(),
                    ServingComponentOperation {
                        pre_commit: operation.pre_commit.clone(),
                        registered_operation: operation.registered_operation.clone(),
                        fresh_only: operation.fresh_only,
                        committed_result_schema: operation.committed_result_schema.as_ref().map(
                            |schema| {
                                String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                                    &schema.schema,
                                ))
                                .expect("canonical JSON is UTF-8")
                            },
                        ),
                        dependencies: operation.dependencies.clone(),
                        statements: operation.statements.clone(),
                    },
                )
            })
            .collect(),
    };
    anyhow::ensure!(
        manifest.components.contains(&expected),
        "component-not-in-carried-release"
    );
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct NodeAcquisition {
    pub(crate) claims: SessionClaims,
    pub(crate) invocation: ConnectionInvocation,
    /// Event provenance for every transaction opened during this acquisition.
    /// This is intentionally independent of `caller`: post-commit delivery has
    /// causation but no caller identity.
    pub(crate) causation: Option<Causation>,
    /// The platform component that executes a callerless delivery. Activation
    /// binds the caller principal as `app.user_id` when a caller exists, and
    /// this component's principal otherwise. A nested call keeps both.
    pub(crate) platform: Option<PlatformComponent>,
}

impl NodeAcquisition {
    /// The `app.user_id` of this acquisition: the caller principal, or the
    /// platform principal of a callerless delivery. An anonymous attachment
    /// has neither.
    fn executing_principal(&self, caller: Option<&AuthenticatedCaller>) -> Option<String> {
        match caller {
            Some(caller) => Some(caller.principal_id().to_owned()),
            None => self
                .platform
                .map(|component| component.principal_id().to_string()),
        }
    }

    /// The claims that activation binds: [`Self::claims`] with the executing
    /// principal as `app.user_id` and the operation token that this
    /// acquisition executes as `app.operation`. A retargeted acquisition
    /// carries the nested operation, so a nested call binds its own token.
    fn executing_claims(&self, caller: Option<&AuthenticatedCaller>) -> SessionClaims {
        SessionClaims {
            user_id: self.executing_principal(caller),
            operation: Some(self.invocation.operation.clone()),
            ..self.claims.clone()
        }
    }

    /// Point one acquisition at the nested target it is about to enter.
    ///
    /// The target arrives as the catalog fact rather than as its parts, because
    /// three of the four retargeted values are read off one fact and four bare
    /// strings let a caller swap two of them with no type error.
    ///
    /// The component name and the operation move with the package and the
    /// digest. A child that kept the parent's pair would raise its effects under
    /// the caller's identity while naming its own package (`wamn-b2m6.7`).
    /// The original wiring owner and root component remain in `origin`.
    fn retarget(mut self, target: &AdmittedComponent, operation: &str) -> Self {
        self.invocation
            .package_id
            .clone_from(&target.scope.package_id);
        self.invocation
            .component_digest
            .clone_from(&target.component_digest);
        self.invocation.component.clone_from(&target.component);
        operation.clone_into(&mut self.invocation.operation);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedOperationRefusalKind {
    IdentityUnbound,
    UndeclaredForExport,
    ReleaseClosureUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NestedOperationRefusal {
    kind: NestedOperationRefusalKind,
    operation: Box<str>,
}

impl NestedOperationRefusal {
    fn new(kind: NestedOperationRefusalKind, operation: &str) -> Self {
        Self {
            kind,
            operation: operation.into(),
        }
    }

    fn literal(&self) -> &'static str {
        match self.kind {
            NestedOperationRefusalKind::IdentityUnbound => "nested-operation-identity-unbound",
            NestedOperationRefusalKind::UndeclaredForExport => {
                "nested-operation-not-declared-for-export"
            }
            NestedOperationRefusalKind::ReleaseClosureUnavailable => {
                "nested-operation-release-closure-unavailable"
            }
        }
    }
}

impl fmt::Display for NestedOperationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.literal(), self.operation)
    }
}

impl std::error::Error for NestedOperationRefusal {}

fn nested_host_error(error: &anyhow::Error) -> wash_runtime::wasmtime::Error {
    if let Some(denial) = error.downcast_ref::<OperationRefusal>() {
        return wash_runtime::wasmtime::Error::new(denial.clone());
    }
    if let Some(refusal) = error.downcast_ref::<NestedOperationRefusal>() {
        return wash_runtime::wasmtime::Error::new(refusal.clone());
    }
    wash_runtime::wasmtime::Error::msg(format!("{error:#}"))
}

/// Dependency operation -> (its exact pin, the owner operations that import it).
type NestedOperationLinks = BTreeMap<String, (ComponentOperationDependency, BTreeSet<String>)>;

fn nested_operation_links(component: &AdmittedComponent) -> anyhow::Result<NestedOperationLinks> {
    let mut links = NestedOperationLinks::new();
    for (owner_operation, operation) in &component.operations {
        for dependency in &operation.dependencies {
            if links.get(&dependency.operation).is_some_and(|(pinned, _)| {
                pinned.package != dependency.package
                    || pinned.version != dependency.version
                    || pinned.digest != dependency.digest
            }) {
                anyhow::bail!("component-operation-dependency-pin-mismatch");
            }
            let (_, owners) = links
                .entry(dependency.operation.clone())
                .or_insert_with(|| (dependency.clone(), BTreeSet::new()));
            owners.insert(owner_operation.clone());
        }
    }
    Ok(links)
}

fn lower_statement_value_type(value_type: ComponentSqlValueType) -> StatementValueType {
    match value_type {
        ComponentSqlValueType::Boolean => StatementValueType::Boolean,
        ComponentSqlValueType::Int32 => StatementValueType::Int32,
        ComponentSqlValueType::Int64 => StatementValueType::Int64,
        ComponentSqlValueType::Float64 => StatementValueType::Float64,
        ComponentSqlValueType::Text => StatementValueType::Text,
        ComponentSqlValueType::Bytes => StatementValueType::Bytes,
        ComponentSqlValueType::Numeric => StatementValueType::Numeric,
        ComponentSqlValueType::Timestamptz => StatementValueType::Timestamptz,
        ComponentSqlValueType::Json => StatementValueType::Json,
        ComponentSqlValueType::Uuid => StatementValueType::Uuid,
    }
}

fn lower_statement_field(field: &ComponentSqlField) -> StatementField {
    StatementField {
        value_type: lower_statement_value_type(field.value_type),
        nullable: field.nullable,
    }
}

/// Lower and verify every operation's statement set out of the admitted facts.
/// The complete admitted fact owns these immutable statements.
fn prepare_statement_sets(
    component: &AdmittedComponent,
) -> anyhow::Result<BTreeMap<String, PreparedStatementSet>> {
    component
        .operations
        .iter()
        .map(|(operation, fact)| {
            WamnPostgres::prepare_statement_set(lower_statement_set(&fact.statements))
                .with_context(|| format!("prepare verified statements for operation {operation:?}"))
                .map(|prepared| (operation.clone(), prepared))
        })
        .collect()
}

fn lower_statement_set(
    statements: &BTreeMap<String, wamn_catalog::ComponentSqlStatement>,
) -> VerifiedStatementSet {
    statements
        .iter()
        .map(|(digest, statement)| {
            (
                digest.clone(),
                VerifiedStatement {
                    exact_sql: statement.sql.clone().into_boxed_str(),
                    binds: statement.binds.iter().map(lower_statement_field).collect(),
                    columns: statement
                        .columns
                        .iter()
                        .map(lower_statement_field)
                        .collect(),
                    transactional: statement.transactional,
                },
            )
        })
        .collect()
}

/// Unique process-local application and invocation scope identifiers.
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

fn next_scope(component: &str) -> Box<str> {
    format!("{component}#{}", NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)).into()
}

/// The host call ceiling in milliseconds. The constant is well under an hour,
/// so it fits every width this bound converts to.
fn max_host_call_ms() -> u64 {
    u64::try_from(MAX_HOST_CALL_DURATION.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn bounded_node_deadline_ms(deadline_ms: Option<u64>) -> u64 {
    deadline_ms
        .unwrap_or(max_host_call_ms())
        .clamp(1, max_host_call_ms())
}

#[cfg(test)]
mod tests;
