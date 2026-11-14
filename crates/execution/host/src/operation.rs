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
use wamn_blobstore::plugin::WamnBlobstore;
use wamn_capability_authority::connection::{
    ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin, InvocationEntry,
};
use wamn_capability_authority::effect_span::EffectEvidence;
use wamn_capability_authority::identity::{ReleaseIdentity, SessionIdentity};
use wamn_catalog::{AdmittedComponent, ComponentSqlField, ServingComponent, ServingRoute};
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::native_routes::bounded_node_deadline_ms;
use wamn_engine::release_manifest::{LoadedRelease, validate_component_in_release};
use wamn_event_wire::Causation;
use wamn_http_capability::ConnectionHttp;
use wamn_http_capability::transport::HttpTransport;
use wamn_identity_protocol::PlatformComponent;
use wamn_postgres_capability::valid_runner;
use wamn_postgres_capability::{
    PreparedStatementMembers, PreparedStatementSet, StatementField, VerifiedStatement,
    VerifiedStatementSet, WamnPostgres,
};
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::plugin::wasi_logging::TracingLogger;

use wamn_engine::operation::{
    invocation_policy, native_call, native_workload, next_scope, node_types,
};
use wamn_engine::warm_reuse::WarmReuse;

mod native_policy;
mod signed_result;

use invocation_policy::ApplicationHost;
use native_call::prepare_native;
use native_policy::{NATIVE_POLICY_ID, NativePolicyResources, new_native_policy};
pub use native_policy::{NativeFacts, NativePolicy};
use native_workload::{NativeApplication, NativeComponent};
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
/// by the ratified host-targeted re-root) with no context to send.
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
    integrations: std::sync::OnceLock<Arc<wamn_integrations::Integrations>>,
    engine: Arc<Engine>,
    pub postgres: Arc<WamnPostgres>,
    /// Shared by every driver in this process, independently of fresh stores.
    http_transport: Arc<HttpTransport>,
    pub context: Arc<wamn_postgres_capability::invocation::ReleaseContext>,
    logging: Arc<TracingLogger>,
    allowed_hosts: Arc<[AllowedHost]>,
    pub release: Arc<LoadedRelease>,
    pub source: Arc<dyn ArtifactSource>,
    pub project: String,
    schema: Option<String>,
    owner_prefix: String,
    warm_reuse: WarmReuse,
    native: tokio::sync::OnceCell<ReleasedApplication>,
    files: std::sync::OnceLock<Arc<crate::file_route::FileRoutes>>,
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
pub struct OperationHostConfig {
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
        facts: Arc<ConnectionFacts>,
        logging: Arc<TracingLogger>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<LoadedRelease>,
        source: Arc<dyn ArtifactSource>,
        target: OperationHostConfig,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_runner(&target.owner_prefix),
            "invalid router owner {:?}: 1-128 chars of [A-Za-z0-9_-] required",
            target.owner_prefix
        );
        anyhow::ensure!(
            target.project == facts.project(),
            "operation project authority mismatch"
        );
        Ok(Self {
            engine,
            postgres,
            http_transport,
            integrations: std::sync::OnceLock::new(),
            context: Arc::new(wamn_postgres_capability::invocation::ReleaseContext::new(
                Arc::clone(&release),
                facts,
            )?),
            logging,
            allowed_hosts,
            release,
            source,
            project: target.project,
            schema: target.schema,
            owner_prefix: target.owner_prefix,
            warm_reuse: target.warm_reuse,
            native: tokio::sync::OnceCell::new(),
            files: std::sync::OnceLock::new(),
        })
    }

    /// Attach retained integration readers before application stores load.
    pub fn set_integrations(
        &self,
        integrations: Arc<wamn_integrations::Integrations>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.native.get().is_none(),
            "integrations must precede application loading"
        );
        self.integrations
            .set(integrations)
            .map_err(|_| anyhow::anyhow!("integrations already attached"))
    }

    /// Attach native file signing before the released application loads.
    pub fn set_file_routes(&self, files: Arc<crate::file_route::FileRoutes>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.native.get().is_none(),
            "file routes must precede application loading"
        );
        self.files
            .set(files)
            .map_err(|_| anyhow::anyhow!("file routes already attached"))
    }

    /// The identity of the carried release, bound on every released call.
    pub fn release_identity(&self) -> ReleaseIdentity {
        ReleaseIdentity::from_loaded(&self.release)
    }

    /// The session identity of one call, before activation binds its principal.
    pub fn identity(
        &self,
        project_env_key: &str,
        release: Option<ReleaseIdentity>,
    ) -> SessionIdentity {
        SessionIdentity {
            project_env_key: project_env_key.to_owned(),
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
            // Activation binds the declared application of the component
            // that runs.
            application: None,
            release,
        }
    }

    pub async fn release_components(&self) -> anyhow::Result<Arc<[AdmittedComponent]>> {
        let loaded = self
            .native
            .get_or_try_init(|| async {
                self.load_released_application(Arc::clone(self.release.components()))
                    .await
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
        let own = components.iter().cloned().collect::<Vec<_>>();
        let pulls = own.into_iter().map(|fact| {
            let source = Arc::clone(&self.source);
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
        let application = self.load_application(native).await?;
        anyhow::ensure!(
            components.len() == application.workload.facts_by_component_id.len(),
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
    ) -> anyhow::Result<Arc<NativeApplication<NativePolicy>>> {
        let policy = new_native_policy(
            &components,
            NativePolicyResources {
                files: self.files.get().cloned(),
                integrations: self.integrations.get().map(Arc::clone),
                postgres: Arc::clone(&self.postgres),
                logging: Arc::clone(&self.logging),
                connection_http: Arc::new(ConnectionHttp::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.http_transport),
                    Arc::clone(&self.allowed_hosts),
                )),
                blobstore: Arc::new(WamnBlobstore::new(Arc::clone(&self.postgres))),
                context: Arc::clone(&self.context),
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
            package_id: component.target.package_id.clone(),
            component_digest: component.component_digest.clone(),
            component: component.component.clone(),
            interface_version: component.interface_version.clone(),
            operation: operation.to_owned(),
        },
        entry,
        package_id: component.target.package_id.clone(),
        component_digest: component.component_digest.clone(),
        // The admitted component name, read off the catalog fact this
        // call resolved to. The per-request pooled target is an instance
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
    pub identity: SessionIdentity,
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

    /// The identity that activation binds: [`Self::identity`] with the executing
    /// principal as `app.user_id`, the operation token that this acquisition
    /// executes as `app.operation`, and `application`, the declared
    /// application of the component that runs it, as `app.application`.
    fn executing_identity(
        &self,
        caller: Option<&AuthenticatedCaller>,
        application: &str,
    ) -> SessionIdentity {
        SessionIdentity {
            user_id: self.executing_principal(caller),
            operation: Some(self.invocation.operation.clone()),
            application: Some(application.to_owned()),
            ..self.identity.clone()
        }
    }
}

fn lower_statement_field(field: &ComponentSqlField) -> StatementField {
    StatementField {
        value_type: field.value_type,
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
        .descriptor
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

#[async_trait::async_trait]
impl wamn_postgres_capability::OperationInvoker for OperationHost {
    async fn invoke(
        &self,
        authority: Arc<wamn_postgres_capability::invocation::AuthorizedInvocation>,
        operation: String,
        context: node_types::NodeContext,
        input: String,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let components = self.release_components().await?;
        let application = self.released_application(&components).await?;
        let fact = components
            .iter()
            .find(|fact| fact.operations.contains_key(&operation))
            .context("operation is outside the release")?;
        let id = application
            .workload
            .component_id(fact)
            .context("operation component is not loaded")?;
        let target = application.dispatch_target(id)?;
        let acquisition = NodeAcquisition {
            identity: self
                .postgres
                .session_identity(authority.id())
                .context("invocation was revoked")?,
            invocation: self
                .postgres
                .invocation(authority.id())
                .context("invocation has no application")?,
            causation: None,
            platform: None,
        };
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(bounded_node_deadline_ms(context.deadline_ms));
        native_call::invoke_raw(
            target,
            native_call::NativeInvocation {
                operation,
                context,
                input: native_call::NativeInput::Encoded(input),
                deadline,
                facts: native_policy::NativeFacts::authorized(acquisition, authority),
                application: Arc::clone(&application),
            },
        )
        .await?
        .into_json()
    }

    async fn sign_result(
        &self,
        authority: Arc<wamn_postgres_capability::invocation::AuthorizedInvocation>,
        operation: String,
        payload: String,
    ) -> anyhow::Result<String> {
        use wamn_engine::operation::InvocationPolicy as _;
        let components = self.release_components().await?;
        let application = self.released_application(&components).await?;
        let fact = components
            .iter()
            .find(|fact| fact.operations.contains_key(&operation))
            .context("operation is outside the release")?;
        let id = application
            .workload
            .component_id(fact)
            .context("operation component is not loaded")?;
        let acquisition = NodeAcquisition {
            identity: self.identity(
                &self.release.target().project_env_key,
                Some(self.release_identity()),
            ),
            invocation: self
                .postgres
                .invocation(authority.id())
                .context("signing authority was revoked")?,
            causation: None,
            platform: None,
        };
        let facts = NativeFacts::authorized(acquisition, authority);
        let outcome = native_call::NativeOutcome::Json(Ok(node_types::Emission {
            payload,
            port: None,
        }));
        let result = application
            .policy
            .finish_result(id, &operation, &facts, outcome)
            .await?
            .into_json()?;
        result
            .map(|emission| emission.payload)
            .map_err(|error| anyhow::anyhow!("result signing refused: {error:?}"))
    }
}
