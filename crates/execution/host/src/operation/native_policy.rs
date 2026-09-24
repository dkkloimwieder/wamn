//! Apply WAMN invocation authority at native component binding and dispatch boundaries.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Context as _;
use tokio::time::Instant;
use wamn_catalog::{AdmittedComponent, ComponentOperationDependency, ServingComponentOperation};
use wamn_engine::invocation_trace::{INVOCATION_TRACES_ID, InvocationTrace, InvocationTraces};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller;
use wamn_runtime::plugins::wamn_blobstore::plugin::{
    self as blobstore, WAMN_BLOBSTORE_ID, WamnBlobstore,
};
use wamn_runtime::plugins::wamn_logging::{WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{
    PreparedStatementSet, UnprovisionedPrincipal, WAMN_POSTGRES_ID, WamnPostgres,
};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::invocation_policy::{InvocationPolicy, InvocationScope};
use super::native_call::{NativeCallFailure, NativeInvocation};
use super::native_workload::native_component_name;
use super::{
    NodeAcquisition, OperationRefusal, OperationRefusalKind, authorize_registered_operation,
    prepare_statement_sets,
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

/// One admitted component and the operations its release publishes for it.
///
/// Publish folds each export's call graph into its released operation: the
/// permission union, `fresh_only` and the statement union. An application
/// composes at build, so a call inside it reaches no host, and the entry
/// carries the authority of the whole graph.
#[derive(Debug)]
struct ComponentPolicy {
    fact: AdmittedComponent,
    operations: BTreeMap<String, ServingComponentOperation>,
    statements: BTreeMap<String, PreparedStatementSet>,
}

/// The facts of one native call that only this policy reads.
pub struct NativeFacts {
    pub(super) acquisition: NodeAcquisition,
    pub(super) caller: Option<AuthenticatedCaller>,
}

impl std::fmt::Debug for NativeFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeFacts")
            .field("caller_attached", &self.caller.is_some())
            .finish_non_exhaustive()
    }
}

impl NativeFacts {
    /// The facts of an entry call.
    pub(crate) fn entry(acquisition: NodeAcquisition, caller: Option<AuthenticatedCaller>) -> Self {
        Self {
            acquisition,
            caller,
        }
    }
}

/// Immutable component policy and the authority of calls that are still active.
#[derive(Debug, Clone)]
pub struct NativePolicy {
    resources: Arc<NativePolicyResources>,
    components: Arc<BTreeMap<String, Arc<ComponentPolicy>>>,
    bindings: Arc<RwLock<BTreeMap<String, Arc<ComponentPolicy>>>>,
    /// The scopes of calls that are still active.
    invocations: Arc<Mutex<BTreeSet<String>>>,
    traces: Arc<InvocationTraces>,
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
        let served = resources
            .release
            .manifest()
            .components
            .iter()
            .find(|served| {
                served.package_id == fact.scope.package_id
                    && served.component == fact.component
                    && served.digest.as_str() == fact.component_digest
            })
            .context("component-not-in-carried-release")?;
        let policy = ComponentPolicy {
            fact: fact.clone(),
            operations: served.operations.clone(),
            statements: prepare_statement_sets(&served.operations)?,
        };
        facts.insert(name, Arc::new(policy));
    }
    Ok(Arc::new(NativePolicy {
        resources: Arc::new(resources),
        components: Arc::new(facts),
        bindings: Arc::default(),
        invocations: Arc::default(),
        traces: Arc::default(),
    }))
}

impl InvocationPolicy for NativePolicy {
    type Facts = NativeFacts;
    type Authority = NativeAuthorityGuard;

    /// Grant authority after native initialization, with rollback on every partial failure.
    async fn activate(
        &self,
        component_id: &str,
        invocation_scope: &InvocationScope<Self>,
        request: &NativeInvocation<Self>,
        _failure: NativeCallFailure,
    ) -> anyhow::Result<NativeAuthorityGuard> {
        let scope = invocation_scope.id.as_ref();
        let operation = request.operation.as_str();
        let acquisition = &request.facts.acquisition;
        let caller = request.facts.caller.as_ref();
        let deadline = request.deadline;
        anyhow::ensure!(Instant::now() < deadline, "native-node-deadline-exceeded");
        // Admission is read TWICE, around the claim bind, because that bind now
        // awaits a database read (`wamn-0h0g.9.19`) and this is a std lock. The
        // second read is the one the invariant below needs: it covers every
        // registry install, and a shutdown that lands in between clears the map
        // so the second lookup refuses instead of installing late.
        {
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
                    && acquisition.claims.project.as_deref()
                        == Some(self.resources.project.as_str()),
                "native-invocation-component-identity-mismatch"
            );
            anyhow::ensure!(
                fact.operation(operation).is_some(),
                "native invocation operation is not admitted"
            );
            let released = component
                .operations
                .get(operation)
                .context("native invocation operation is not released")?;
            // The entry's own grant first, so a refusal names it, then every
            // other operation its call graph reaches.
            let own = released.registered_operation.as_deref();
            authorize_registered_operation(caller, own, released.fresh_only)?;
            for permission in &released.permissions {
                if Some(permission.as_str()) != own {
                    authorize_registered_operation(caller, Some(permission), released.fresh_only)?;
                }
            }
        }
        let guard = NativeAuthorityGuard {
            policy: self.clone(),
            scope: scope.to_owned(),
        };
        let resources = &self.resources;
        // Every claims transaction binds its executing principal as
        // `app.user_id` and the admitted operation token as `app.operation`,
        // which the record-history triggers record.
        //
        // The bind also reads the executing principal's `app_system.users` row
        // in the tenant and refuses a principal that owns none. That refusal is
        // an authorization fact, so it reaches the caller as the
        // `permission-denied` the operation vocabulary already carries, not as
        // a host failure.
        if let Err(error) = resources
            .postgres
            .bind_session_claims(scope, &acquisition.executing_claims(caller))
            .await
        {
            if error.downcast_ref::<UnprovisionedPrincipal>().is_some() {
                tracing::warn!(
                    error = %format_args!("{error:#}"),
                    "native invocation refused: the executing principal is not provisioned"
                );
                return Err(OperationRefusal::new(
                    OperationRefusalKind::PermissionDenied,
                    operation,
                )
                .into());
            }
            return Err(error);
        }
        // Serialize late activation with caller cancellation. No await follows
        // this lock, so cancellation cannot leave a newly installed authority.
        let closed = invocation_scope
            .closed
            .lock()
            .expect("invocation scope lock poisoned");
        anyhow::ensure!(
            !*closed,
            "native invocation was cancelled during activation"
        );
        // Keep admission locked until every registry is installed. Shutdown
        // obtains the write lock before revoking, so no late insert survives.
        let bindings = self
            .bindings
            .read()
            .expect("native component bindings lock poisoned");
        let component = bindings
            .get(component_id)
            .context("native invocation component is not admitted")?;
        let statement_set = component
            .statements
            .get(operation)
            .context("native invocation statement set is missing")?;
        resources
            .postgres
            .set_current_run(scope, acquisition.causation.clone());
        // The run half of a postgres effect's coordinates is `set_current_run`
        // above; this is the node and wiring half (`wamn-0h0g.7.9`). Both are
        // host-attested, and the surface that reads them is the plugin's.
        resources
            .postgres
            .bind_invocation(scope, acquisition.invocation.clone())?;
        resources
            .postgres
            .bind_prepared_statement_operation(scope, operation, statement_set)?;
        resources
            .postgres
            .activate_statement_operation(scope, operation)?;
        resources.postgres.bind_transaction_scope(scope, deadline)?;
        if let Some(participant) = component
            .operations
            .get(operation)
            .and_then(|released| released.participant.as_ref())
        {
            // The composed component embeds the participant, so the entry
            // component names it in the intent.
            let fact = &component.fact;
            let participant_dependency = ComponentOperationDependency {
                participant: None,
                package: fact.scope.package_id.clone(),
                version: fact.scope.package_version.clone(),
                digest: fact.component_digest.clone(),
                operation: participant.clone(),
            };
            let intent = serde_json::to_string(&serde_json::json!({
                "participant": participant_dependency,
                "release": resources.release.manifest().release.effective_release_id,
                "manifest": resources.release.release().manifest_digest.as_str(),
            }))?;
            resources
                .postgres
                .bind_selected_participant(scope, participant.clone(), intent)?;
        }
        resources
            .logging
            .set_claim(scope, &acquisition.claims.tenant, &resources.project);
        resources
            .connection_http
            .bind_invocation(scope, acquisition.invocation.clone())?;
        resources
            .blobstore
            .bind_invocation(scope, acquisition.invocation.clone())?;
        let inserted = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .insert(scope.to_owned());
        anyhow::ensure!(inserted, "native-invocation-scope-already-bound");
        self.traces.bind(scope, InvocationTrace::capture());
        Ok(guard)
    }

    /// Close component admission and revoke all synchronous WAMN authority.
    fn shutdown(&self) {
        let mut bindings = self
            .bindings
            .write()
            .expect("native component bindings lock poisoned");
        bindings.clear();
        let scopes: Vec<_> = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .iter()
            .cloned()
            .collect();
        for scope in scopes {
            self.revoke(&scope);
        }
    }

    fn revoke(&self, scope: &str) {
        self.resources.postgres.revoke_transaction_scope(scope);
        self.traces.revoke(scope);
        self.invocations
            .lock()
            .expect("native invocation lock poisoned")
            .remove(scope);
        self.resources.blobstore.revoke_invocation(scope);
        self.resources.connection_http.revoke_invocation(scope);
        self.resources.logging.clear_claim(scope);
        self.resources.postgres.revoke_invocation(scope);
        self.resources.postgres.revoke_session_claims(scope);
        self.resources.postgres.clear_statement_scope(scope);
    }
}

/// Revoke host registries when a call returns, traps, fails to bind, or is cancelled.
#[derive(Debug)]
pub struct NativeAuthorityGuard {
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
                if let Some(slot) = &operation.pre_commit {
                    imports.insert(WitInterface::from(slot.as_str()));
                }
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
        let slots: BTreeSet<_> = policy
            .fact
            .operations
            .values()
            .filter_map(|operation| operation.pre_commit.as_ref())
            .collect();
        // A base released on its own leaves its pre-commit slot unplugged. An
        // application plugs the slot with its participant at build, so a call
        // that reaches this stub has no participant and traps.
        for slot in slots {
            linker.instance(slot)?.func_new_concurrent(
                "run",
                |_accessor, _ty, _params, _results| {
                    Box::pin(async {
                        wash_runtime::wasmtime::bail!("pre-commit slot has no participant")
                    })
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
