//! The host half of one operation call.
//!
//! The released application, its plugins, and the policy that grants each
//! call's authority belong here. The call itself is
//! [`wamn_engine::operation::invoke_operation`], which the router driver and
//! the route path both call.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use futures_util::{StreamExt as _, stream};
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt as _;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wamn_catalog::{
    AdmittedComponent, ComponentSqlField, ComponentSqlValueType, ServingComponent, ServingRoute,
};
use wamn_control_registry::identifiers::valid_runner;
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::{LoadedRelease, validate_component_in_release};
use wamn_engine::router_delivery::bounded_node_deadline_ms;
use wamn_event_wire::Causation;
use wamn_project_state::PlatformComponent;
use wamn_runtime::plugins::EffectEvidence;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
    InvocationEntry,
};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    PreparedStatementMembers, PreparedStatementSet, ReleaseIdentity, SessionClaims, StatementField,
    StatementValueType, VerifiedStatement, VerifiedStatementSet, WamnPostgres, WriteLogStore,
};
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;

use wamn_engine::operation::{invocation_policy, native_call, native_workload, next_scope};
use wamn_engine::warm_reuse::WarmReuse;

mod native_policy;

/// What one cloud call needs to log its intents
/// (`docs/history/host-transaction.md` 5, issue 4).
#[derive(Debug)]
pub struct CallIntents {
    /// The two-step store of an operation without SQL. An operation with SQL
    /// claims in its item transaction instead.
    store: WriteLogStore,
    tenant: String,
    release: String,
    package: String,
    kind: wamn_catalog::OperationType,
    key_field: Option<String>,
    canonicalization: Option<wamn_catalog::RouteCanonicalization>,
}

impl CallIntents {
    /// The intent context of the call.
    pub fn context(&self) -> wamn_engine::operation::IntentContext<'_> {
        wamn_engine::operation::IntentContext {
            store: &self.store,
            tenant: &self.tenant,
            release: &self.release,
            package: &self.package,
            kind: self.kind,
            key_field: self.key_field.as_deref(),
            canonicalization: self.canonicalization.as_ref(),
        }
    }
}

use invocation_policy::ApplicationHost;
use native_call::prepare_native;
use native_policy::{NATIVE_POLICY_ID, NativePolicyResources, new_native_policy};
pub use native_policy::{NativeFacts, NativePolicy};
use native_workload::{NativeApplication, NativeComponent, NativeEntry};
use native_workload::{NativeWorkloadSpec, load_native_application};

/// Keep at most two verified artifact fetches in flight per release load.
/// Native workload loading owns compilation after these bounded fetches finish.
const COMPONENT_FETCH_CONCURRENCY: usize = 2;

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
pub struct NodeTraceContext {
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
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
pub fn node_trace_context(traceparent: Option<&str>, tracestate: Option<&str>) -> NodeTraceContext {
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
pub fn remote_trace_context(
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

pub use wamn_engine::invocation_trace::{InvocationSite, invocation_span};

/// The process capabilities that one released application runs on.
///
/// The process builds one host. The route path calls it directly, and the
/// router driver runs each wiring node on the same host, so both call into the
/// same loaded application.
pub struct OperationHost {
    engine: Arc<Engine>,
    pub postgres: Arc<WamnPostgres>,
    /// Shared by every driver in this process, independently of fresh stores.
    http_transport: Arc<HttpTransport>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    pub release: Arc<LoadedRelease>,
    pub source: Arc<dyn ArtifactSource>,
    pub project: String,
    schema: Option<String>,
    owner_prefix: String,
    warm_reuse: WarmReuse,
    native: tokio::sync::OnceCell<ReleasedApplication>,
}

/// Keep the validated input alive so pointer identity proves immutable reuse.
struct ReleasedApplication {
    release: Arc<LoadedRelease>,
    components: Arc<[AdmittedComponent]>,
    application: Arc<NativeApplication<NativePolicy>>,
}

fn validate_released_components(
    release: &Arc<LoadedRelease>,
    components: &Arc<[AdmittedComponent]>,
    loaded: Option<(&Arc<LoadedRelease>, &Arc<[AdmittedComponent]>)>,
) -> anyhow::Result<()> {
    if loaded.is_some_and(|(loaded_release, loaded)| {
        Arc::ptr_eq(loaded_release, release) && Arc::ptr_eq(loaded, components)
    }) {
        return Ok(());
    }
    for component in components.iter() {
        validate_component_in_release(release, component)?;
    }
    if let Some((loaded_release, loaded)) = loaded {
        anyhow::ensure!(
            Arc::ptr_eq(loaded_release, release),
            "native-release-identity-mismatch"
        );
        anyhow::ensure!(
            components.len() == loaded.len()
                && components
                    .iter()
                    .all(|fact| loaded.iter().any(|loaded| loaded == fact)),
            "native-release-component-closure-mismatch"
        );
    }
    Ok(())
}

impl fmt::Debug for OperationHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationHost")
            .field("project", &self.project)
            .field("schema", &self.schema)
            .field("owner_prefix", &self.owner_prefix)
            .finish_non_exhaustive()
    }
}

/// The process-owned facts of an [`OperationHost`], outside its capabilities.
#[derive(Debug, Clone)]
pub struct OperationScope {
    pub project: String,
    pub schema: Option<String>,
    pub owner_prefix: String,
    pub warm_reuse: WarmReuse,
}

impl OperationHost {
    /// Bind one release to the process-owned capabilities.
    #[expect(
        clippy::too_many_arguments,
        reason = "each host-owned capability is an independent production dependency"
    )]
    /// Every host in the same process must receive the same HTTP transport.
    pub fn new(
        engine: Arc<Engine>,
        postgres: Arc<WamnPostgres>,
        http_transport: Arc<HttpTransport>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<LoadedRelease>,
        source: Arc<dyn ArtifactSource>,
        scope: OperationScope,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_runner(&scope.owner_prefix),
            "invalid router owner {:?}: 1-128 chars of [A-Za-z0-9_-] required",
            scope.owner_prefix
        );
        Ok(Self {
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
        })
    }

    /// The identity of the carried release, bound on every released call.
    pub fn release_identity(&self) -> ReleaseIdentity {
        ReleaseIdentity::from_loaded(&self.release)
    }

    /// The session claims of one call, before activation binds its principal.
    pub fn claims(&self, tenant: &str, release: Option<ReleaseIdentity>) -> SessionClaims {
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
            // Activation binds the composing application of the component
            // that runs.
            application: None,
            release,
        }
    }

    /// The complete component list of the carried release, read once.
    ///
    /// A wiring reads the same list beside its graph, so a route and a wiring
    /// load one application.
    /// The intents of a call of `operation` on `component`, from the route
    /// that the release publishes for it. An operation without a route logs
    /// none.
    pub fn call_intents(
        &self,
        component: &AdmittedComponent,
        operation: &str,
    ) -> anyhow::Result<Option<CallIntents>> {
        let manifest = self.release.manifest();
        let Some(route) =
            manifest.route(&component.scope.package_id, &component.component, operation)
        else {
            return Ok(None);
        };
        let tenant = self.release.scope().tenant_id.clone();
        Ok(Some(CallIntents {
            store: WriteLogStore::new(
                Arc::clone(&self.postgres),
                self.project.clone(),
                tenant.clone(),
            ),
            tenant,
            release: self.release.release().manifest_digest.to_string(),
            package: route.package_id.clone(),
            kind: route.type_,
            key_field: route.idempotency.clone(),
            canonicalization: route.canonicalization.clone(),
        }))
    }

    pub async fn release_components(&self) -> anyhow::Result<Arc<[AdmittedComponent]>> {
        let loaded = self
            .native
            .get_or_try_init(|| async {
                let components = self
                    .postgres
                    .resolve_release_components(
                        &self.project,
                        &self.release.scope().tenant_id,
                        &self.release.scope().environment,
                        self.release.release().manifest_digest.as_str(),
                    )
                    .await?;
                self.load_released_application(Arc::from(components)).await
            })
            .await?;
        anyhow::ensure!(
            Arc::ptr_eq(&loaded.release, &self.release),
            "native-release-identity-mismatch"
        );
        Ok(Arc::clone(&loaded.components))
    }

    /// Load the released application and initialize each component without
    /// invoking its handler, so the first request finds it resident.
    pub async fn prepare_released(
        &self,
        components: &Arc<[AdmittedComponent]>,
    ) -> anyhow::Result<()> {
        let application = self.released_application(components).await?;
        for id in application.workload.component_ids() {
            let target = application.dispatch_target(id)?;
            prepare_native(
                target,
                tokio::time::Instant::now() + Duration::from_millis(bounded_node_deadline_ms(None)),
            )
            .await
            .with_context(|| format!("initialize native release component {id:?}"))?;
        }
        Ok(())
    }

    pub async fn released_application(
        &self,
        components: &Arc<[AdmittedComponent]>,
    ) -> anyhow::Result<Arc<NativeApplication<NativePolicy>>> {
        let loaded = self
            .native
            .get_or_try_init(|| self.load_released_application(Arc::clone(components)))
            .await?;
        validate_released_components(
            &self.release,
            components,
            Some((&loaded.release, &loaded.components)),
        )?;
        Ok(Arc::clone(&loaded.application))
    }

    async fn load_released_application(
        &self,
        components: Arc<[AdmittedComponent]>,
    ) -> anyhow::Result<ReleasedApplication> {
        validate_released_components(&self.release, &components, None)?;
        // The composed entry runs its members, so the host loads the entry
        // and not their own components. One compile then serves the HTTP
        // entry and every call here.
        let recorded = self.release.manifest().composed_entry.as_ref();
        let (members, own): (Vec<_>, Vec<_>) = components.iter().cloned().partition(|fact| {
            recorded.is_some_and(|entry| {
                entry.members.iter().any(|member| {
                    member.package_id == fact.scope.package_id
                        && member.component == fact.component
                        && member.digest == fact.component_digest
                })
            })
        });
        let entry = match recorded {
            Some(recorded) if !members.is_empty() => {
                let entry = self
                    .source
                    .pull_entry(&recorded.reference)
                    .instrument(tracing::info_span!(
                        "wamn.component.pull",
                        wamn.composed_entry = true
                    ))
                    .await?;
                Some(NativeEntry {
                    members,
                    bytes: entry.bytes,
                    digest: entry.digest,
                })
            }
            _ => None,
        };
        let pulls = own.into_iter().map(|fact| {
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
        drop(pulls);
        let application = self.load_application(native, entry).await?;
        let entry_members = application
            .workload
            .entry
            .as_ref()
            .map_or(0, |(_, members)| members.len());
        anyhow::ensure!(
            components.len() == application.workload.facts_by_component_id.len() + entry_members,
            "native-release-component-closure-mismatch"
        );
        Ok(ReleasedApplication {
            release: Arc::clone(&self.release),
            components,
            application,
        })
    }

    /// Load `components`, and the composed entry that runs its members, as one
    /// application.
    pub async fn load_application(
        &self,
        components: Vec<NativeComponent>,
        entry: Option<NativeEntry>,
    ) -> anyhow::Result<Arc<NativeApplication<NativePolicy>>> {
        let policy = new_native_policy(
            &components,
            entry.as_ref(),
            NativePolicyResources {
                postgres: Arc::clone(&self.postgres),
                logging: Arc::clone(&self.logging),
                connection_http: Arc::new(ConnectionHttp::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.http_transport),
                    Arc::clone(&self.credentials),
                    self.release.scope().tenant_id.as_str(),
                    self.project.as_str(),
                    Arc::clone(&self.allowed_hosts),
                    Some(Arc::clone(&self.release)),
                )),
                blobstore: Arc::new(WamnBlobstore::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.credentials),
                    self.release.scope().tenant_id.as_str(),
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
                entry,
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

impl ApplicationHost for OperationHost {
    type Policy = NativePolicy;

    fn released_application(
        &self,
        components: &Arc<[AdmittedComponent]>,
    ) -> impl Future<Output = anyhow::Result<Arc<NativeApplication<NativePolicy>>>> + Send {
        OperationHost::released_application(self, components)
    }
}

/// The connection invocation of one admitted component at one entry.
pub fn component_invocation(
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

#[derive(Debug, Clone)]
pub struct NodeAcquisition {
    pub claims: SessionClaims,
    pub invocation: ConnectionInvocation,
    /// Event provenance for every transaction opened during this acquisition.
    /// This is intentionally independent of `caller`: post-commit delivery has
    /// causation but no caller identity.
    pub causation: Option<Causation>,
    /// The platform component that executes a callerless delivery. Activation
    /// binds the caller principal as `app.user_id` when a caller exists, and
    /// this component's principal otherwise. A nested call keeps both.
    pub platform: Option<PlatformComponent>,
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
    /// principal as `app.user_id`, the operation token that this acquisition
    /// executes as `app.operation`, and `application`, the composing
    /// application of the component that runs it, as `app.application`.
    fn executing_claims(
        &self,
        caller: Option<&AuthenticatedCaller>,
        application: &str,
    ) -> SessionClaims {
        SessionClaims {
            user_id: self.executing_principal(caller),
            operation: Some(self.invocation.operation.clone()),
            application: Some(application.to_owned()),
            ..self.claims.clone()
        }
    }
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

/// Lower and seal every operation's statement set out of the admitted facts.
/// The complete admitted fact owns these immutable statements. An operation
/// is a read when the release routes it as a get, query or projection.
fn prepare_statement_sets(
    served: &ServingComponent,
    routes: &BTreeSet<ServingRoute>,
) -> BTreeMap<String, PreparedStatementSet> {
    served
        .operations
        .iter()
        .map(|(operation, fact)| {
            let read = routes.iter().any(|route| {
                route.package_id == served.package_id
                    && route.component == served.descriptor.component
                    && &route.operation == operation
                    && route.type_.is_read()
            });
            (
                operation.clone(),
                WamnPostgres::prepare_statement_set(lower_statement_set(&fact.statements), read),
            )
        })
        .collect()
}

/// Lower and seal the statement set of each named `statements` import.
///
/// `instances` is the record verified in the component's bytes. Publish
/// wrote each member's own set into the served component, keyed by import
/// name, so the host binds from that field only.
///
/// # Errors
///
/// Refuses a composition whose served component does not carry a set for
/// exactly its named imports.
fn prepare_statement_members(
    instances: &BTreeMap<String, String>,
    served: &ServingComponent,
) -> anyhow::Result<PreparedStatementMembers> {
    anyhow::ensure!(
        instances.keys().eq(served.statement_instances.keys())
            && instances == &served.descriptor.statement_instances,
        "statement-instances-unreleased: the component imports {:?}, the release binds {:?}",
        instances.keys().collect::<Vec<_>>(),
        served.statement_instances.keys().collect::<Vec<_>>()
    );
    Ok(WamnPostgres::prepare_statement_members(
        served
            .statement_instances
            .iter()
            .map(|(instance, statements)| {
                (
                    instance.clone(),
                    WamnPostgres::prepare_statement_set(lower_statement_set(statements), false),
                )
            }),
    ))
}

/// Lower and seal the statement set of each named `statements` import of the
/// composed entry.
///
/// `instances` is the record verified in the entry's bytes. Publication keeps
/// a composed application's named imports in the entry, so their sets are the
/// composing member's. A plain application gets one named import there, whose
/// set is the statements of all its operations.
///
/// # Errors
///
/// Refuses an entry whose record names imports the release does not bind.
fn prepare_entry_members(
    instances: &BTreeMap<String, String>,
    composing: &ServingComponent,
) -> anyhow::Result<PreparedStatementMembers> {
    if !composing.statement_instances.is_empty() {
        return prepare_statement_members(instances, composing);
    }
    anyhow::ensure!(
        instances
            .values()
            .all(|digest| digest == composing.descriptor.component_digest.as_str()),
        "statement-instances-unreleased: the entry imports {:?} for another member",
        instances.keys().collect::<Vec<_>>()
    );
    let statements = composing
        .operations
        .values()
        .flat_map(|fact| fact.statements.clone())
        .collect();
    Ok(WamnPostgres::prepare_statement_members(
        instances.keys().map(|instance| {
            (
                instance.clone(),
                WamnPostgres::prepare_statement_set(lower_statement_set(&statements), false),
            )
        }),
    ))
}

fn lower_statement_set(
    statements: &BTreeMap<String, wamn_catalog::ComponentSqlStatement>,
) -> VerifiedStatementSet {
    statements
        .iter()
        .map(|(path, statement)| {
            (
                path.clone(),
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

#[cfg(test)]
mod tests;
