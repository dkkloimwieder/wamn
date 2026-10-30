//! Apply WAMN invocation authority at native component binding and dispatch boundaries.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Context as _;
use tokio::time::Instant;
use wamn_catalog::{AdmittedComponent, ServingComponent, ServingComponentOperation};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_trace::{INVOCATION_TRACES_ID, InvocationTrace, InvocationTraces};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    OperationRefusal, OperationRefusalType, authorize_released_operation,
};
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::wamn_blobstore::plugin::{
    self as blobstore, WAMN_BLOBSTORE_ID, WamnBlobstore,
};
use wamn_runtime::plugins::wamn_logging::{WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{
    OperationTransaction, PreparedStatementMembers, PreparedStatementSet, UnprovisionedPrincipal,
    WAMN_POSTGRES_ID, WamnPostgres,
};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::invocation_policy::{InvocationPolicy, InvocationScope};
use super::native_call::{NativeCallFailure, NativeInvocation};
use super::native_workload::{
    NativeComponent, NativeEntry, native_component_name, native_entry_name,
};
use super::{
    NodeAcquisition, prepare_entry_members, prepare_statement_members, prepare_statement_sets,
};

pub(super) const NATIVE_POLICY_ID: &str = "wamn:native-operation-policy";

/// Process-owned capabilities; every authority registry remains keyed by invocation scope.
pub(super) struct NativePolicyResources {
    pub(super) files: Option<Arc<crate::file_route::FileRoutes>>,
    pub(super) connectors: Option<Arc<wamn_runtime::connectors::Connectors>>,
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
/// An application composes at build, so a call inside it reaches no host, and
/// the entry runs under its own grant. Each member reaches its own statements
/// through its named import.
#[derive(Debug)]
struct ComponentPolicy {
    fact: AdmittedComponent,
    operations: BTreeMap<String, ServingComponentOperation>,
    statements: BTreeMap<String, PreparedStatementSet>,
    /// Signing authority comes only from the release-pinned descriptor.
    signed_results: BTreeMap<String, wamn_catalog::SignedFileReads>,
    /// The set of each named `statements` import, one per member.
    members: PreparedStatementMembers,
    runtime_schema: Option<String>,
    /// The environment map name of the application that holds this component, which
    /// every call binds as `app.application`.
    application: String,
}

/// The facts of one native call that only this policy reads.
#[derive(Clone)]
pub struct NativeFacts {
    pub(super) acquisition: NodeAcquisition,
    pub(super) caller: Option<AuthenticatedCaller>,
    /// The host transaction of a logged request element, which activation binds.
    pub(super) transaction: Option<OperationTransaction>,
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
    pub fn entry(acquisition: NodeAcquisition, caller: Option<AuthenticatedCaller>) -> Self {
        Self {
            acquisition,
            caller,
            transaction: None,
        }
    }
}

/// The admitted components that one native component runs: its own fact, or
/// every member that the composed entry carries.
type ServedPolicies = Arc<[Arc<ComponentPolicy>]>;

/// The served component that `acquisition` names.
fn served_component<'a>(
    served: &'a [Arc<ComponentPolicy>],
    acquisition: &NodeAcquisition,
) -> Option<&'a Arc<ComponentPolicy>> {
    let invocation = &acquisition.invocation;
    served.iter().find(|component| {
        component.fact.scope.package_id == invocation.package_id
            && component.fact.component == invocation.component
            && component.fact.component_digest == invocation.component_digest
    })
}

/// Immutable component policy and the authority of calls that are still active.
#[derive(Debug, Clone)]
pub struct NativePolicy {
    resources: Arc<NativePolicyResources>,
    components: Arc<BTreeMap<String, ServedPolicies>>,
    bindings: Arc<RwLock<BTreeMap<String, ServedPolicies>>>,
    /// The scopes of calls that are still active.
    invocations: Arc<Mutex<BTreeSet<String>>>,
    traces: Arc<InvocationTraces>,
}

pub(super) fn new_native_policy(
    components: &[NativeComponent],
    entry: Option<&NativeEntry>,
    resources: NativePolicyResources,
) -> anyhow::Result<Arc<NativePolicy>> {
    let manifest = resources.release.manifest();
    let served_of = |fact: &AdmittedComponent| {
        manifest
            .components
            .iter()
            .find(|served| {
                served.package_id == fact.scope.package_id
                    && served.descriptor.component == fact.component
                    && served.descriptor.component_digest.as_str() == fact.component_digest
            })
            .context("component-not-in-carried-release")
    };
    let policy =
        |fact: &AdmittedComponent, served: &ServingComponent, members: PreparedStatementMembers| {
            let mut signed_results = BTreeMap::new();
            for (name, operation) in &served.descriptor.operations {
                if let Some(reads) = wamn_catalog::signed_file_reads(operation)? {
                    signed_results.insert(name.clone(), reads);
                }
            }
            Ok::<_, anyhow::Error>(Arc::new(ComponentPolicy {
                signed_results,
                fact: fact.clone(),
                operations: served.operations.clone(),
                statements: prepare_statement_sets(served, &manifest.routes),
                members,
                runtime_schema: served.runtime_schema.clone(),
                application: wamn_catalog::application_for_package(
                    &resources.release.scope().applications,
                    &served.package_id,
                )
                .with_context(|| {
                    format!(
                        "package {} must belong to exactly one declared application",
                        served.package_id
                    )
                })?
                .to_owned(),
            }))
        };
    let mut facts = BTreeMap::new();
    for NativeComponent { fact, bytes } in components {
        let name = native_component_name(fact)?;
        if facts.contains_key(&name) {
            continue;
        }
        let served = served_of(fact)?;
        let instances = wamn_engine::statement_instances::statement_instances(bytes)
            .context("statement-instance-wiring")?;
        let members = prepare_statement_members(&instances, served)?;
        facts.insert(name, ServedPolicies::from([policy(fact, served, members)?]));
    }
    // Each member runs in the entry under its own facts, through the
    // statement instances that the entry carries.
    if let Some(entry) = entry {
        let instances = wamn_engine::statement_instances::statement_instances(&entry.bytes)
            .context("statement-instance-wiring")?;
        let composing = manifest
            .composing_member()
            .context("entry has no application members")?;
        let members = prepare_entry_members(&instances, composing)?;
        let served = entry
            .members
            .iter()
            .map(|fact| policy(fact, served_of(fact)?, members.clone()))
            .collect::<anyhow::Result<ServedPolicies>>()?;
        facts.insert(native_entry_name(&entry.digest), served);
    }
    Ok(Arc::new(NativePolicy {
        resources: Arc::new(resources),
        components: Arc::new(facts),
        bindings: Arc::default(),
        invocations: Arc::default(),
        traces: Arc::default(),
    }))
}

impl NativePolicy {
    /// Admit one call of `operation` on `component_id`: the component, the
    /// caller's identity and the caller's permissions. It returns the
    /// component's policy.
    fn admit(
        &self,
        component_id: &str,
        operation: &str,
        facts: &NativeFacts,
    ) -> anyhow::Result<Arc<ComponentPolicy>> {
        let acquisition = &facts.acquisition;
        let bindings = self
            .bindings
            .read()
            .expect("native component bindings lock poisoned");
        let served = bindings
            .get(component_id)
            .context("native invocation component is not admitted")?;
        let component = served_component(served, acquisition)
            .context("native-invocation-component-identity-mismatch")?;
        let fact = &component.fact;
        anyhow::ensure!(
            fact.scope.env_key == acquisition.claims.env_key
                && acquisition.invocation.operation == operation
                && acquisition.claims.project.as_deref() == Some(self.resources.project.as_str()),
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
        authorize_released_operation(facts.caller.as_ref(), released)?;
        Ok(Arc::clone(component))
    }

    /// Bind the claims of one call to `scope`.
    ///
    /// Every claims transaction binds its executing principal as
    /// `app.user_id` and the admitted operation token as `app.operation`,
    /// which the record-history triggers record, and the declared
    /// application of the component as `app.application`.
    ///
    /// The bind also reads the executing principal's `platform."user"` row in
    /// the tenant and refuses a principal that owns none. That refusal is an
    /// authorization fact, so it reaches the caller as the `permission-denied`
    /// the operation vocabulary already carries, not as a host failure.
    async fn bind_claims(
        &self,
        scope: &str,
        operation: &str,
        facts: &NativeFacts,
        component: &ComponentPolicy,
    ) -> anyhow::Result<()> {
        let mut claims = facts
            .acquisition
            .executing_claims(facts.caller.as_ref(), &component.application);
        if let Some(schema) = &component.runtime_schema {
            claims.schema = Some(schema.clone());
        }
        let postgres = &self.resources.postgres;
        // A request element's transaction began under a bind of these same facts, which
        // read the principal, so the call inside it does not read again.
        let bound = if facts.transaction.is_some() {
            postgres.bind_request_element_session_claims(scope, &claims)
        } else {
            postgres.bind_session_claims(scope, &claims).await
        };
        let Err(error) = bound else {
            return Ok(());
        };
        if error.downcast_ref::<UnprovisionedPrincipal>().is_some() {
            tracing::warn!(
                error = %format_args!("{error:#}"),
                "native invocation refused: the executing principal is not provisioned"
            );
            return Err(
                OperationRefusal::new(OperationRefusalType::PermissionDenied, operation).into(),
            );
        }
        Err(error)
    }
}

impl InvocationPolicy for NativePolicy {
    type Facts = NativeFacts;
    type Authority = NativeAuthorityGuard;
    type Transaction = OperationTransaction;

    /// Begin the request element's transaction under the claims of the call, on a scope
    /// of its own that ends after the transaction captures its claims, when the
    /// admitted operation declares a host transaction. BEGIN waits for the claim.
    async fn admit_and_open_transaction(
        &self,
        component_id: &str,
        operation: &str,
        facts: &NativeFacts,
    ) -> anyhow::Result<Option<OperationTransaction>> {
        let component = self.admit(component_id, operation, facts)?;
        // The admitted operation's own declaration decides, so a command
        // without SQL that calls members with SQL still begins one.
        if !component
            .fact
            .operation(operation)
            .is_some_and(|admitted| admitted.host_transaction)
        {
            return Ok(None);
        }
        let postgres = &self.resources.postgres;
        let scope = wamn_engine::operation::next_scope("native-request-element");
        let begun = async {
            self.bind_claims(&scope, operation, facts, &component)
                .await?;
            postgres.set_current_run(&scope, facts.acquisition.causation.clone());
            postgres
                .begin_operation_transaction(&scope)
                .await
                .map(|transaction| {
                    if self
                        .resources
                        .release
                        .manifest()
                        .routes
                        .iter()
                        .any(|route| {
                            route.operation == operation
                                && route.type_ == wamn_catalog::OperationType::Delete
                        })
                    {
                        transaction.with_known_result(
                            wamn_runtime::plugins::wamn_postgres::write_log::DELETED_RESULT
                                .to_owned(),
                        )
                    } else {
                        transaction
                    }
                })
                .map_err(|error| {
                    anyhow::anyhow!("begin the request element transaction: {error:?}")
                })
        }
        .await;
        postgres.revoke_session_claims(&scope);
        begun.map(Some)
    }

    fn transaction_facts(
        &self,
        facts: &NativeFacts,
        transaction: &OperationTransaction,
    ) -> NativeFacts {
        NativeFacts {
            transaction: Some(transaction.clone()),
            ..facts.clone()
        }
    }

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
        let deadline = request.deadline;
        anyhow::ensure!(Instant::now() < deadline, "native-node-deadline-exceeded");
        // Admission is read TWICE, around the claim bind, because that bind now
        // awaits a database read (`wamn-0h0g.9.19`) and this is a std lock. The
        // second read is the one the invariant below needs: it covers every
        // registry install, and a shutdown that lands in between clears the map
        // so the second lookup refuses instead of installing late.
        let component = self.admit(component_id, operation, &request.facts)?;
        let guard = NativeAuthorityGuard {
            policy: self.clone(),
            scope: scope.to_owned(),
        };
        let resources = &self.resources;
        self.bind_claims(scope, operation, &request.facts, &component)
            .await?;
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
            .and_then(|served| served_component(served, acquisition))
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
            .bind_prepared_statement_members(scope, &component.members)?;
        resources
            .postgres
            .bind_prepared_statement_operation(scope, operation, statement_set)?;
        resources
            .postgres
            .activate_statement_operation(scope, operation)?;
        if let Some(transaction) = &request.facts.transaction {
            resources
                .postgres
                .bind_operation_transaction(scope, transaction)?;
        }
        resources
            .logging
            .set_claim(scope, &acquisition.claims.env_key, &resources.project);
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

    async fn finish_result(
        &self,
        component_id: &str,
        operation: &str,
        facts: &NativeFacts,
        outcome: super::native_call::NativeOutcome,
    ) -> anyhow::Result<super::native_call::NativeOutcome> {
        let component = self.admit(component_id, operation, facts)?;
        let Some(reads) = component.signed_results.get(operation) else {
            return Ok(outcome);
        };
        let files = self
            .resources
            .files
            .as_ref()
            .context("signed file results require native file routes")?;
        let caller = facts
            .caller
            .as_ref()
            .context("signed file results require an authenticated operation caller")?;
        let scope = wamn_engine::operation::next_scope("signed-file-result");
        // The same guard revokes every plugin scope on failure or cancellation.
        let _authority = NativeAuthorityGuard {
            policy: self.clone(),
            scope: scope.to_string(),
        };
        {
            // Serialize registration with shutdown, as activation does.
            let bindings = self
                .bindings
                .read()
                .expect("native component bindings lock poisoned");
            bindings
                .get(component_id)
                .and_then(|served| served_component(served, &facts.acquisition))
                .context("signed result component is no longer admitted")?;
            self.resources
                .blobstore
                .bind_invocation(&scope, facts.acquisition.invocation.clone())?;
            let inserted = self
                .invocations
                .lock()
                .expect("native invocation lock poisoned")
                .insert(scope.to_string());
            anyhow::ensure!(inserted, "signed-result-scope-already-bound");
        }
        super::signed_result::sign_result(outcome, reads, |id, lifetime| {
            let scope = &scope;
            async move {
                files
                    .signed_url(&self.resources.blobstore, scope, caller, id, lifetime)
                    .await
                    .map_err(anyhow::Error::from)
            }
        })
        .await
        .map_err(anyhow::Error::from)
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
        self.resources.postgres.revoke_operation_transaction(scope);
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
        imports.insert(WitInterface::from(wamn_engine::connector::READER));
        // The route layer of a composed entry imports these. A call enters an
        // operation export, never the HTTP handler, so none reaches them.
        imports.insert(WitInterface::from("wamn:flow-http-routing/routing@0.1.0"));
        imports.insert(WitInterface::from("wamn:router-delivery/delivery@0.4.0"));
        let mut exports = HashSet::new();
        for entry in self.components.values().flat_map(|served| served.iter()) {
            for name in entry.fact.operations.keys() {
                exports.insert(WitInterface::from(name.as_str()));
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
        // No native call binds an operation entry, so each `wamn:operation`
        // call of the entry's route layer refuses as on an unbound entry.
        self.resources
            .postgres
            .add_linker_entries(linker, &component, &interfaces)?;
        if interfaces.contains("wamn", "flow-http-routing", &["routing"]) {
            wamn_engine::flow_http_routing::add_to_linker(linker)?;
        }
        if interfaces.contains("wamn", "router-delivery", &["delivery"]) {
            wamn_engine::router_delivery::add_to_linker(linker)?;
        }
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
        if interfaces.contains("wamn", "connector", &["reader"]) {
            let connectors = self
                .resources
                .connectors
                .as_ref()
                .context("connector reader unavailable")?;
            let application = &policy
                .first()
                .context("connector component policy missing")?
                .application;
            anyhow::ensure!(
                policy
                    .iter()
                    .all(|member| &member.application == application),
                "connector entry has ambiguous application"
            );
            let mut names = std::collections::HashSet::new();
            for member in policy.iter() {
                names.extend(
                    self.resources
                        .postgres
                        .declared_connection_names(
                            &self.resources.project,
                            &member.fact.scope.env_key,
                            &member.fact.scope.package_id,
                            &member.fact.component_digest,
                        )
                        .await?,
                );
            }
            connectors.reader(application)?.add_to_linker(item, names)?;
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
