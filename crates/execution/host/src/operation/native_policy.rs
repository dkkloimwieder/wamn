//! Apply WAMN invocation authority at native component binding and dispatch boundaries.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use tokio::time::Instant;
use wamn_catalog::{AdmittedComponent, ServingComponent};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_scope::INVOCATION_SCOPES_ID;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{OperationRefusal, OperationRefusalType};
use wamn_runtime::invocation::{AuthorizedInvocation, InvocationRequest, PrincipalVerification};
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

/// Process-owned capabilities that read the shared invocation registry.
pub(super) struct NativePolicyResources {
    pub(super) files: Option<Arc<crate::file_route::FileRoutes>>,
    pub(super) integrations: Option<Arc<wamn_runtime::integrations::Integrations>>,
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
    authority: Option<Arc<AuthorizedInvocation>>,
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
            authority: None,
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
        component.fact.target.package_id == invocation.package_id
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
    owner: Arc<str>,
}

pub(super) fn new_native_policy(
    components: &[NativeComponent],
    entry: Option<&NativeEntry>,
    resources: NativePolicyResources,
) -> anyhow::Result<Arc<NativePolicy>> {
    resources
        .logging
        .set_scope_registry(Arc::clone(&resources.postgres.scopes))?;
    let manifest = resources.release.manifest();
    let served_of = |fact: &AdmittedComponent| {
        manifest
            .components
            .iter()
            .find(|served| {
                served.package_id == fact.target.package_id
                    && served.descriptor.component == fact.component
                    && served.descriptor.component_digest.as_str() == fact.component_digest
                    && served.descriptor.operations == fact.operations
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
                statements: prepare_statement_sets(served, &manifest.routes),
                members,
                runtime_schema: served.runtime_schema.clone(),
                application: wamn_catalog::application_for_package(
                    &resources.release.target().applications,
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
        owner: wamn_engine::operation::next_scope("native-policy").into(),
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
            fact.target.project_env_key == acquisition.identity.project_env_key
                && acquisition.invocation.operation == operation
                && acquisition.identity.project.as_deref() == Some(self.resources.project.as_str()),
            "native-invocation-component-identity-mismatch"
        );
        fact.operations
            .get(operation)
            .context("native invocation operation is not admitted")?;
        Ok(Arc::clone(component))
    }
}

impl InvocationPolicy for NativePolicy {
    type Facts = NativeFacts;
    type Authority = Arc<AuthorizedInvocation>;
    type Transaction = OperationTransaction;

    async fn prepare(
        &self,
        component_id: &str,
        operation: &str,
        facts: &NativeFacts,
    ) -> anyhow::Result<NativeFacts> {
        let component = self.admit(component_id, operation, facts)?;
        if let Some(authority) = &facts.authority {
            let identity = self
                .resources
                .postgres
                .session_identity(authority.id())
                .context("invocation scope was revoked")?;
            anyhow::ensure!(
                identity.operation.as_deref() == Some(operation)
                    && self.resources.postgres.invocation(authority.id()).as_ref()
                        == Some(&facts.acquisition.invocation),
                "prepared invocation authority differs from the call"
            );
            return Ok(facts.clone());
        }
        let mut identity = facts
            .acquisition
            .executing_identity(facts.caller.as_ref(), &component.application);
        if let Some(schema) = &component.runtime_schema {
            identity.schema = Some(schema.clone());
        }
        let authority = AuthorizedInvocation::enter(
            &self.resources.postgres,
            InvocationRequest {
                scope: wamn_engine::operation::next_scope(&self.owner).into_string(),
                identity,
                caller: facts.caller.clone(),
                registered_operation: component.fact.operations[operation]
                    .registered_operation
                    .as_deref(),
                causation: facts.acquisition.causation.clone(),
                invocation: Some(facts.acquisition.invocation.clone()),
                statements: component
                    .statements
                    .get(operation)
                    .context("native invocation statement set is missing")?
                    .clone(),
                members: component.members.clone(),
                principal: PrincipalVerification::Required,
            },
        )
        .await
        .map_err(|error| {
            if error.downcast_ref::<UnprovisionedPrincipal>().is_some() {
                OperationRefusal::new(OperationRefusalType::PermissionDenied, operation).into()
            } else {
                error
            }
        })?;
        // Shutdown closes admission before revoking this policy's records.
        let bindings = self
            .bindings
            .read()
            .expect("native component bindings lock poisoned");
        anyhow::ensure!(
            bindings.contains_key(component_id),
            "native component is no longer admitted"
        );
        Ok(NativeFacts {
            authority: Some(authority),
            ..facts.clone()
        })
    }

    fn scope_id<'a>(&self, facts: &'a NativeFacts) -> Option<&'a str> {
        facts.authority.as_ref().map(|authority| authority.id())
    }

    async fn admit_and_open_transaction(
        &self,
        component_id: &str,
        operation: &str,
        facts: &NativeFacts,
    ) -> anyhow::Result<Option<OperationTransaction>> {
        let component = self.admit(component_id, operation, facts)?;
        if !component.fact.operations[operation].host_transaction {
            return Ok(None);
        }
        let scope = facts
            .authority
            .as_ref()
            .context("invocation is not prepared")?
            .id();
        let postgres = &self.resources.postgres;
        postgres.revoke_operation_transaction(scope);
        let transaction = postgres
            .begin_operation_transaction(scope)
            .await
            .map_err(|error| anyhow::anyhow!("begin operation transaction: {error:?}"))?;
        let transaction = if self
            .resources
            .release
            .manifest()
            .routes
            .iter()
            .any(|route| {
                route.operation == operation && route.type_ == wamn_catalog::OperationType::Delete
            }) {
            transaction.with_known_result(
                wamn_runtime::plugins::wamn_postgres::write_log::DELETED_RESULT.to_owned(),
            )
        } else {
            transaction
        };
        postgres.bind_operation_transaction(scope, &transaction)?;
        Ok(Some(transaction))
    }

    fn transaction_facts(
        &self,
        facts: &NativeFacts,
        _transaction: &OperationTransaction,
    ) -> NativeFacts {
        facts.clone()
    }

    /// Borrow the prepared authority after native initialization.
    fn activate(
        &self,
        component_id: &str,
        invocation_scope: &InvocationScope<Self>,
        request: &NativeInvocation<Self>,
        _failure: NativeCallFailure,
    ) -> impl Future<Output = anyhow::Result<Arc<AuthorizedInvocation>>> + Send {
        let result = (|| {
            anyhow::ensure!(
                Instant::now() < request.deadline,
                "native-node-deadline-exceeded"
            );
            let closed = invocation_scope
                .closed
                .lock()
                .expect("invocation scope lock poisoned");
            anyhow::ensure!(
                !*closed,
                "native invocation was cancelled during activation"
            );
            let authority = request
                .facts
                .authority
                .as_ref()
                .context("invocation is not prepared")?;
            anyhow::ensure!(
                authority.id() == invocation_scope.id.as_ref(),
                "invocation scope differs"
            );
            let bindings = self
                .bindings
                .read()
                .expect("native component bindings lock poisoned");
            anyhow::ensure!(
                bindings.contains_key(component_id),
                "native component is no longer admitted"
            );
            anyhow::ensure!(
                self.resources
                    .postgres
                    .session_identity(authority.id())
                    .is_some(),
                "invocation scope was revoked"
            );
            self.resources.postgres.scopes.set_trace(
                authority.id(),
                wamn_engine::invocation_trace::InvocationTrace::capture(),
            )?;
            Ok(Arc::clone(authority))
        })();
        std::future::ready(result)
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
        let authority = facts
            .authority
            .as_ref()
            .context("signing invocation is not prepared")?;
        let caller = authority
            .caller
            .as_ref()
            .context("signed file results require an authenticated operation caller")?;
        let scope = authority.id();
        super::signed_result::sign_result(outcome, reads, |id, lifetime| async move {
            files
                .signed_url(&self.resources.blobstore, scope, caller, id, lifetime)
                .await
                .map_err(anyhow::Error::from)
        })
        .await
        .map_err(anyhow::Error::from)
    }

    /// Close component admission and revoke all synchronous WAMN authority.
    fn shutdown(&self) {
        self.bindings
            .write()
            .expect("native component bindings lock poisoned")
            .clear();
        self.resources
            .postgres
            .scopes
            .revoke_workload(&format!("{}#", self.owner));
    }

    fn revoke(&self, scope: &str) {
        self.resources.postgres.scopes.revoke(scope);
    }
}

impl NativePolicy {
    async fn bind_component(&self, item: &mut WorkloadItem<'_>) -> anyhow::Result<()> {
        let name = match &*item {
            WorkloadItem::Component(component) => component.name(),
            WorkloadItem::Service(_) => anyhow::bail!("native-operation-service-not-admitted"),
        };
        let policy = self
            .components
            .get(name)
            .context("native binding does not name an admitted component")?;
        let imports = item.world().imports;
        let imports: HashSet<_> = imports
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
        let linker = item.linker()?;
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
        if let Some(integrations) = &self.resources.integrations {
            let owners = policy
                .iter()
                .map(|member| {
                    (
                        member.fact.component_digest.as_str(),
                        member.fact.target.package_id.as_str(),
                    )
                })
                .collect::<Vec<_>>();
            integrations.add_to_linker(item, &owners).await?;
        }
        item.add_plugin(
            INVOCATION_SCOPES_ID,
            Arc::clone(&self.resources.postgres.scopes) as _,
        );
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
        // Native resolution installs this policy plugin after binding succeeds.
        // No invocation registries are populated during binding or initialization.
        Ok(())
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
        if let Some(integrations) = &self.resources.integrations {
            imports.extend(integrations.interfaces());
        }
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
        self.bind_component(item).await
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
