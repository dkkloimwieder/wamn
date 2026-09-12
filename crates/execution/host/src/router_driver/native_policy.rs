//! Apply WAMN invocation authority at native component binding and dispatch boundaries.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use anyhow::Context as _;
use tokio::time::Instant;
use tracing::Instrument as _;
use wamn_catalog::{AdmittedComponent, ComponentOperationDependency};
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::flow_http_routing::{AuthenticatedCaller, CredentialKind};
use wamn_runtime::plugins::invocation_trace::{
    INVOCATION_TRACES_ID, InvocationTrace, InvocationTraces, invocation_trace,
};
use wamn_runtime::plugins::wamn_blobstore::plugin::{
    self as blobstore, WAMN_BLOBSTORE_ID, WamnBlobstore,
};
use wamn_runtime::plugins::wamn_logging::{WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{PreparedStatementSet, WAMN_POSTGRES_ID, WamnPostgres};
use wamn_runtime::release_manifest::LoadedRelease;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::native_call::{NativeCallFailure, NativeInvocation, invoke_native};
use super::native_workload::{NativeApplication, native_component_name};
use super::{
    NestedOperationRefusal, NestedOperationRefusalKind, NodeAcquisition,
    authorize_registered_operation, bounded_node_deadline_ms, nested_host_error,
    nested_operation_links, node_types, prepare_statement_sets, validate_component_in_release,
};

pub(super) const NATIVE_POLICY_ID: &str = "wamn:native-operation-policy";

/// Process-owned capabilities; every authority registry remains keyed by invocation scope.
pub(super) struct NativePolicyResources {
    pub(super) postgres: Arc<WamnPostgres>,
    pub(super) logging: Arc<WamnLogging>,
    pub(super) connection_http: Arc<ConnectionHttp>,
    pub(super) blobstore: Arc<WamnBlobstore>,
    pub(super) release: Arc<LoadedRelease>,
    pub(super) project: String,
}

impl std::fmt::Debug for NativePolicyResources {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativePolicyResources")
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ComponentPolicy {
    fact: AdmittedComponent,
    statements: BTreeMap<String, PreparedStatementSet>,
}

#[derive(Debug, Clone)]
struct InvocationAuthority {
    acquisition: NodeAcquisition,
    caller: Option<AuthenticatedCaller>,
    operation: String,
    deadline: Instant,
    failure: NativeCallFailure,
}

/// Immutable component policy and the authority of calls that are still active.
#[derive(Debug, Clone)]
pub(super) struct NativePolicy {
    resources: Arc<NativePolicyResources>,
    components: Arc<BTreeMap<String, Arc<ComponentPolicy>>>,
    bindings: Arc<RwLock<BTreeMap<String, Arc<ComponentPolicy>>>>,
    invocations: Arc<Mutex<BTreeMap<String, InvocationAuthority>>>,
    traces: Arc<InvocationTraces>,
    application: Arc<OnceLock<Weak<NativeApplication>>>,
}

pub(super) fn new_native_policy(
    components: &[AdmittedComponent],
    resources: NativePolicyResources,
) -> anyhow::Result<Arc<NativePolicy>> {
    let mut facts = BTreeMap::new();
    for fact in components {
        let name = native_component_name(fact)?;
        if facts.contains_key(&name) {
            continue;
        }
        let policy = ComponentPolicy {
            fact: fact.clone(),
            statements: prepare_statement_sets(fact)?,
        };
        facts.insert(name, Arc::new(policy));
    }
    Ok(Arc::new(NativePolicy {
        resources: Arc::new(resources),
        components: Arc::new(facts),
        bindings: Arc::default(),
        invocations: Arc::default(),
        traces: Arc::default(),
        application: Arc::new(OnceLock::new()),
    }))
}

impl NativePolicy {
    /// Retain only a weak reference so native workload teardown has no ownership cycle.
    pub(super) fn bind_application(
        &self,
        application: &Arc<NativeApplication>,
    ) -> anyhow::Result<()> {
        self.application
            .set(Arc::downgrade(application))
            .map_err(|_| anyhow::anyhow!("native-policy-workload-already-bound"))
    }

    /// Grant authority after native initialization, with rollback on every partial failure.
    pub(super) fn activate(
        &self,
        component_id: &str,
        scope: &str,
        request: &NativeInvocation,
        failure: NativeCallFailure,
    ) -> anyhow::Result<NativeAuthorityGuard> {
        let operation = request.operation.as_str();
        let acquisition = &request.acquisition;
        let caller = request.caller.as_ref();
        let deadline = request.deadline;
        anyhow::ensure!(Instant::now() < deadline, "native-node-deadline-exceeded");
        // Keep admission locked until every registry is installed. Shutdown
        // obtains the write lock before revoking, so no late insert survives.
        let bindings = self
            .bindings
            .read()
            .expect("native component bindings lock poisoned");
        let component = bindings
            .get(component_id)
            .context("native invocation component is not admitted")?;
        let fact = &component.fact;
        anyhow::ensure!(
            fact.scope.tenant_id == acquisition.claims.tenant
                && fact.scope.package_id == acquisition.invocation.package_id
                && fact.component == acquisition.invocation.component
                && fact.component_digest == acquisition.invocation.component_digest
                && acquisition.invocation.operation == operation
                && acquisition.claims.project.as_deref() == Some(self.resources.project.as_str()),
            "native-invocation-component-identity-mismatch"
        );
        let admitted_operation = fact
            .operation(operation)
            .context("native invocation operation is not admitted")?;
        authorize_registered_operation(
            caller,
            admitted_operation.registered_operation.as_deref(),
            admitted_operation.fresh_only,
        )?;
        let statement_set = component
            .statements
            .get(operation)
            .context("native invocation statement set is missing")?;
        let guard = NativeAuthorityGuard {
            policy: self.clone(),
            scope: scope.to_owned(),
        };
        let resources = &self.resources;
        resources
            .postgres
            .bind_session_claims(scope, &acquisition.claims)?;
        resources
            .postgres
            .set_current_run(scope, acquisition.causation.clone());
        resources
            .postgres
            .bind_prepared_statement_operation(scope, operation, statement_set)?;
        resources
            .postgres
            .activate_statement_operation(scope, operation)?;
        resources
            .logging
            .set_claim(scope, &acquisition.claims.tenant, &resources.project);
        resources
            .connection_http
            .bind_invocation(scope, acquisition.invocation.clone())?;
        resources
            .blobstore
            .bind_invocation(scope, acquisition.invocation.clone())?;
        let previous = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .insert(
                scope.to_owned(),
                InvocationAuthority {
                    acquisition: acquisition.clone(),
                    caller: caller.cloned(),
                    operation: operation.to_owned(),
                    deadline,
                    failure,
                },
            );
        anyhow::ensure!(previous.is_none(), "native-invocation-scope-already-bound");
        self.traces.bind(scope, InvocationTrace::capture());
        Ok(guard)
    }

    /// Close component admission and revoke all synchronous WAMN authority.
    pub(super) fn shutdown(&self) {
        let mut bindings = self
            .bindings
            .write()
            .expect("native component bindings lock poisoned");
        bindings.clear();
        let scopes: Vec<_> = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .keys()
            .cloned()
            .collect();
        for scope in scopes {
            self.revoke(&scope);
        }
    }

    fn revoke(&self, scope: &str) {
        self.traces.revoke(scope);
        self.invocations
            .lock()
            .expect("native invocation lock poisoned")
            .remove(scope);
        self.resources.blobstore.revoke_invocation(scope);
        self.resources.connection_http.revoke_invocation(scope);
        self.resources.logging.clear_claim(scope);
        self.resources.postgres.revoke_session_claims(scope);
        self.resources.postgres.clear_statement_scope(scope);
    }

    fn host_failure(&self, scope: &str, error: anyhow::Error) -> wash_runtime::wasmtime::Error {
        let failure = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .get(scope)
            .map(|bound| Arc::clone(&bound.failure));
        match failure {
            Some(failure) => {
                // Native store faults flatten host error types. Keep the
                // original in this invocation before the guest trap crosses it.
                let trap = wash_runtime::wasmtime::Error::msg(format!("{error:#}"));
                failure
                    .lock()
                    .expect("native call failure lock poisoned")
                    .get_or_insert(error);
                trap
            }
            None => nested_host_error(error),
        }
    }

    async fn invoke_nested(
        &self,
        scope: &str,
        owners: &BTreeSet<String>,
        dependency: &ComponentOperationDependency,
        mut context: node_types::NodeContext,
        input: String,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let bound = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .get(scope)
            .cloned()
            .ok_or_else(|| {
                NestedOperationRefusal::new(
                    NestedOperationRefusalKind::IdentityUnbound,
                    &dependency.operation,
                )
            })?;
        anyhow::ensure!(
            Instant::now() < bound.deadline,
            "native-node-deadline-exceeded"
        );
        if !owners.contains(&bound.operation) {
            return Err(NestedOperationRefusal::new(
                NestedOperationRefusalKind::UndeclaredForExport,
                &dependency.operation,
            )
            .into());
        }
        if bound.acquisition.claims.release.is_none() {
            return Err(NestedOperationRefusal::new(
                NestedOperationRefusalKind::ReleaseClosureUnavailable,
                &dependency.operation,
            )
            .into());
        }
        // The interface selects its single admitted provider. The dependency
        // digest checks provenance; it cannot choose among implementations.
        let mut providers = self
            .components
            .values()
            .filter(|entry| entry.fact.operations.contains_key(&dependency.operation));
        let provider = providers
            .next()
            .context("native-operation-provider-missing")?;
        anyhow::ensure!(
            providers.next().is_none(),
            "native-operation-provider-ambiguous"
        );
        let target = &provider.fact;
        anyhow::ensure!(
            target.scope.tenant_id == bound.acquisition.claims.tenant
                && target.scope.package_id == dependency.package
                && target.scope.package_version == dependency.version
                && target.component_digest == dependency.digest,
            "native-operation-provider-provenance-mismatch"
        );
        let operation = target
            .operation(&dependency.operation)
            .expect("provider lookup confirmed the operation exists");
        authorize_registered_operation(
            bound.caller.as_ref(),
            operation.registered_operation.as_deref(),
            operation.fresh_only,
        )?;
        validate_component_in_release(&self.resources.release, target)?;
        let application = self
            .application
            .get()
            .and_then(Weak::upgrade)
            .context("native-operation-workload-unavailable")?;
        let component_id = application
            .workload
            .facts_by_component_id
            .iter()
            .find_map(|(id, fact)| (fact == target).then_some(id))
            .context("native-operation-component-unavailable")?;
        let dispatch = application
            .workload
            .resolved
            .dispatch_target(component_id, NATIVE_POLICY_ID)
            .await?;
        let deadline = bound.deadline.min(
            Instant::now()
                + std::time::Duration::from_millis(bounded_node_deadline_ms(context.deadline_ms)),
        );
        context.deadline_ms = Some(
            u64::try_from(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
        );
        let span = tracing::info_span!(
            "wamn.component.invoke",
            wamn.tenant = %target.scope.tenant_id,
            wamn.project = %self.resources.project,
            wamn.environment = %self.resources.release.manifest().release.environment,
            wamn.wiring_id = %bound.acquisition.invocation.wiring_id,
            wamn.wiring_version = bound.acquisition.invocation.wiring_version,
            wamn.component_digest = %target.component_digest,
            wamn.node_id = %bound.acquisition.invocation.node_id,
            wamn.operation = %dependency.operation,
            wamn.caller_principal_id = tracing::field::Empty,
            wamn.caller_credential_kind = tracing::field::Empty,
        );
        if let Some(caller) = bound.caller.as_ref() {
            span.record("wamn.caller_principal_id", caller.principal_id());
            span.record(
                "wamn.caller_credential_kind",
                match caller.credential_kind() {
                    CredentialKind::Pat => "pat",
                    CredentialKind::Session => "session",
                },
            );
        }
        invoke_native(
            &dispatch,
            NativeInvocation {
                operation: dependency.operation.clone(),
                context,
                input,
                deadline,
                acquisition: bound.acquisition.retarget(target, &dependency.operation),
                caller: bound.caller,
                application,
            },
        )
        .instrument(span)
        .await
    }
}

/// Revoke host registries when a call returns, traps, fails to bind, or is cancelled.
#[derive(Debug)]
pub(super) struct NativeAuthorityGuard {
    policy: NativePolicy,
    scope: String,
}

impl Drop for NativeAuthorityGuard {
    fn drop(&mut self) {
        self.policy.revoke(&self.scope);
    }
}

#[async_trait::async_trait]
impl HostPlugin for NativePolicy {
    fn id(&self) -> &'static str {
        NATIVE_POLICY_ID
    }

    fn world(&self) -> WitWorld {
        let mut imports = self.resources.postgres.world().imports;
        // Shared node ABI types carry no callable capability. Native resolution
        // still requires a plugin to account for this admitted structural import.
        imports.insert(WitInterface::from("wamn:node/types@0.1.0"));
        imports.extend(self.resources.logging.world().imports);
        imports.extend(self.resources.connection_http.world().imports);
        imports.extend(self.resources.blobstore.world().imports);
        let mut exports = HashSet::new();
        for entry in self.components.values() {
            for (name, operation) in &entry.fact.operations {
                exports.insert(WitInterface::from(name.as_str()));
                imports.extend(
                    operation
                        .dependencies
                        .iter()
                        .map(|dependency| WitInterface::from(dependency.operation.as_str())),
                );
            }
        }
        WitWorld { imports, exports }
    }

    fn supports_named_instances(&self) -> bool {
        true
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let name = match &*item {
            WorkloadItem::Component(component) => component.name(),
            WorkloadItem::Service(_) => anyhow::bail!("native-operation-service-not-admitted"),
        };
        let policy = self
            .components
            .get(name)
            .context("native binding does not name an admitted component")?;
        let links = nested_operation_links(&policy.fact)?;
        let imports: HashSet<_> = item
            .world()
            .imports
            .into_iter()
            .map(|mut interface| {
                if interface.namespace == "wamn"
                    && interface.package == "postgres"
                    && interface.name.is_some()
                {
                    interface
                        .config
                        .insert("project".to_owned(), self.resources.project.clone());
                }
                interface
            })
            .collect();
        let interfaces = WitInterfaces::new(&imports);
        let component = item.component().clone();
        let linker = item.linker();
        for (dependency, owners) in links.values() {
            let dependency = dependency.clone();
            let owners = Arc::new(owners.clone());
            linker.instance(&dependency.operation)?.func_wrap_async(
                "run",
                move |mut store: wash_runtime::wasmtime::StoreContextMut<'_, SharedCtx>,
                      (context, input): (node_types::NodeContext, String)| {
                    let active = extract_active_ctx(store.data_mut());
                    let trace = invocation_trace(&active);
                    let scope = Arc::clone(&active.ctx.component_id);
                    let policy = active.ctx.get_plugin::<NativePolicy>(NATIVE_POLICY_ID);
                    let owners = Arc::clone(&owners);
                    let dependency = dependency.clone();
                    Box::new(trace.run(async move {
                        let result = policy
                            .invoke_nested(&scope, &owners, &dependency, context, input)
                            .await
                            .map_err(|error| policy.host_failure(&scope, error))?;
                        Ok((result,))
                    }))
                },
            )?;
        }
        self.resources
            .postgres
            .add_linker_entries(linker, &component, &interfaces)?;
        self.resources
            .logging
            .add_linker_entries(linker, &interfaces)?;
        if interfaces.contains("wamn", "connection", &["http"]) {
            connection_http::add_to_linker(linker)?;
        }
        if interfaces.contains(
            "wasmcloud",
            "blobstore",
            &["types", "container", "blobstore"],
        ) {
            blobstore::add_to_linker(linker)?;
        }
        item.add_plugin(INVOCATION_TRACES_ID, Arc::clone(&self.traces) as _);
        item.add_plugin(WAMN_POSTGRES_ID, Arc::clone(&self.resources.postgres) as _);
        item.add_plugin(WAMN_LOGGING_ID, Arc::clone(&self.resources.logging) as _);
        item.add_plugin(
            CONNECTION_HTTP_ID,
            Arc::clone(&self.resources.connection_http) as _,
        );
        item.add_plugin(
            WAMN_BLOBSTORE_ID,
            Arc::clone(&self.resources.blobstore) as _,
        );
        self.bindings
            .write()
            .expect("native component bindings lock poisoned")
            .insert(item.id().to_owned(), Arc::clone(policy));
        // Native installs this policy plugin itself after this hook succeeds.
        // No invocation registries are populated during binding or initialization.
        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        _workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.shutdown();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
