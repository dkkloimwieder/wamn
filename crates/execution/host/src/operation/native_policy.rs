//! Apply WAMN invocation authority at native component binding and dispatch boundaries.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use tokio::time::Instant;
use wamn_catalog::{AdmittedComponent, ServingComponent};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_scope::INVOCATION_SCOPES_ID;
use wamn_engine::native_routes::{OperationRefusal, OperationRefusalType};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_runtime::invocation::{AuthorizedInvocation, InvocationRequest, PrincipalVerification};
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::wamn_blobstore::plugin::{
    self as blobstore, WAMN_BLOBSTORE_ID, WamnBlobstore,
};
use wamn_runtime::plugins::wamn_postgres::{
    PreparedStatementMembers, PreparedStatementSet, UnprovisionedPrincipal, WAMN_POSTGRES_ID,
    WamnPostgres,
};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::wasi_logging::TracingLogger;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::invocation_policy::{InvocationPolicy, InvocationScope};
use super::native_call::{NativeCallFailure, NativeInvocation};
use super::native_workload::{NativeComponent, native_component_name};
use super::{NodeAcquisition, prepare_statement_members, prepare_statement_sets};

mod logging;

pub(super) const NATIVE_POLICY_ID: &str = "wamn:native-operation-policy";

/// Capabilities bound by the host invocation policy.
pub(super) struct NativePolicyResources {
    pub(super) files: Option<Arc<crate::file_route::FileRoutes>>,
    pub(super) integrations: Option<Arc<wamn_runtime::integrations::Integrations>>,
    pub(super) postgres: Arc<WamnPostgres>,
    pub(super) logging: Arc<TracingLogger>,
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
/// An application composes at build. Each member reaches its own statements
/// through its named import. The route lends the application its authority.
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
    application: Option<String>,
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
    pub(super) fn authorized(
        acquisition: NodeAcquisition,
        authority: Arc<AuthorizedInvocation>,
    ) -> Self {
        Self {
            acquisition,
            caller: None,
            authority: Some(authority),
        }
    }

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
    resources: NativePolicyResources,
) -> anyhow::Result<Arc<NativePolicy>> {
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
                application: if served.package_id == wamn_catalog::PALETTE_PACKAGE_ID {
                    None
                } else {
                    Some(
                        wamn_catalog::application_for_package(
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
                    )
                },
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
        let application = match component.application.as_deref() {
            Some(application) => application,
            None => connection_http::released_invocation_application(
                self.resources.release.manifest(),
                self.resources.release.target(),
                &facts.acquisition.invocation,
            )
            .context("palette invocation has no declared workflow application")?,
        };
        let mut identity = facts
            .acquisition
            .executing_identity(facts.caller.as_ref(), application);
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

    async fn invoke(
        &self,
        component_id: &str,
        target: &wash_runtime::engine::dispatch::DispatchTarget,
        request: NativeInvocation<Self>,
    ) -> anyhow::Result<super::native_call::NativeOutcome> {
        let component = self.admit(component_id, &request.operation, &request.facts)?;
        let route = self.resources.release.manifest().route(
            &component.fact.target.package_id,
            &component.fact.component,
            &request.operation,
        );
        // HTTP already owns the claim. Palette and event-handler exports have no write envelope.
        if !matches!(
            request.facts.acquisition.invocation.entry,
            wamn_runtime::plugins::connection_http::InvocationEntry::Wiring(_)
        ) || route.is_none_or(|route| {
            route.type_.is_read() || route.type_ == wamn_catalog::OperationType::EventHandler
        }) {
            return super::native_call::invoke_raw(target, request).await;
        }
        let route = route.expect("a workflow write has a released route");
        let super::native_call::NativeInput::Encoded(input) = &request.input else {
            anyhow::bail!("workflow input is not JSON");
        };
        let input: serde_json::Value = serde_json::from_str(input)?;
        let authority = request
            .facts
            .authority
            .as_ref()
            .context("invocation is not prepared")?;
        let outcome = self
            .resources
            .postgres
            .invoke_node(
                authority,
                &request.operation,
                route,
                component.fact.operations[&request.operation].host_transaction,
                &input,
                |input| {
                    super::native_call::invoke_raw(
                        target,
                        NativeInvocation {
                            operation: request.operation.clone(),
                            context: request.context.clone(),
                            input: super::native_call::NativeInput::Encoded(input),
                            deadline: request.deadline,
                            facts: request.facts.clone(),
                            application: Arc::clone(&request.application),
                        },
                    )
                },
            )
            .await?;
        Ok(super::native_call::NativeOutcome::Json(outcome))
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
        // A route lends its authority to the native call and owns its cleanup.
        if scope
            .strip_prefix(self.owner.as_ref())
            .is_some_and(|suffix| suffix.starts_with('#'))
        {
            self.resources.postgres.scopes.revoke(scope);
        }
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
        if interfaces.contains("wasi", "logging", &[]) {
            self.resources
                .logging
                .on_workload_item_bind(item, interfaces)
                .await?;
            logging::bind(item)?;
            item.add_plugin(
                self.resources.logging.id(),
                Arc::clone(&self.resources.logging) as _,
            );
        }
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
        workload_id: &str,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.shutdown();
        self.resources
            .logging
            .on_workload_unbind(workload_id, interfaces)
            .await
    }
}

#[cfg(test)]
mod tests;
