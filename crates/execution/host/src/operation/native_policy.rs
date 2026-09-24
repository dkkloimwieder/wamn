//! Apply WAMN invocation authority at native component binding and dispatch boundaries.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use anyhow::Context as _;
use tokio::time::Instant;
use tracing::Instrument as _;
use wamn_catalog::{AdmittedComponent, ComponentOperationDependency};
use wamn_engine::invocation_trace::{
    INVOCATION_TRACES_ID, InvocationTrace, InvocationTraces, invocation_trace,
};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::flow_http_routing::{AuthenticatedCaller, CredentialKind};
use wamn_runtime::plugins::wamn_blobstore::plugin::{
    self as blobstore, WAMN_BLOBSTORE_ID, WamnBlobstore,
};
use wamn_runtime::plugins::wamn_logging::{WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{
    PreparedStatementSet, TransactionParticipation, UnprovisionedPrincipal, WAMN_POSTGRES_ID,
    WamnPostgres,
};
use wash_runtime::engine::ctx::extract_active_ctx;
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::invocation_policy::{InvocationPolicy, InvocationScope};
use super::native_call::{
    NativeCallFailure, NativeInput, NativeInvocation, NativeOutcome, invoke_owned, typed_context,
};
use super::native_workload::{NativeApplication, native_component_name};
use super::{
    NestedOperationRefusal, NestedOperationRefusalKind, NodeAcquisition, OperationRefusal,
    OperationRefusalKind, authorize_registered_operation, bounded_node_deadline_ms,
    nested_host_error, nested_operation_links, node_types, prepare_statement_sets,
    validate_component_in_release,
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

/// A caller-owned participant pinned to the same admitted component and release.
#[derive(Debug, Clone)]
pub(super) struct SelectedParticipant {
    dependency: ComponentOperationDependency,
    intent: String,
}

/// The facts of one native call that only this policy reads.
pub(crate) struct NativeFacts {
    pub(super) acquisition: NodeAcquisition,
    pub(super) caller: Option<AuthenticatedCaller>,
    /// Host-attested owner scope for one nested transaction participant.
    pub(super) transaction_participation: Option<TransactionParticipation>,
    pub(super) selected_participant: Option<SelectedParticipant>,
}

impl NativeFacts {
    /// The facts of an entry call, which joins no caller transaction.
    pub(crate) fn entry(acquisition: NodeAcquisition, caller: Option<AuthenticatedCaller>) -> Self {
        Self {
            acquisition,
            caller,
            transaction_participation: None,
            selected_participant: None,
        }
    }
}

#[derive(Debug, Clone)]
struct InvocationAuthority {
    acquisition: NodeAcquisition,
    caller: Option<AuthenticatedCaller>,
    operation: String,
    deadline: Instant,
    failure: NativeCallFailure,
    selected_participant: Option<SelectedParticipant>,
}

/// Immutable component policy and the authority of calls that are still active.
#[derive(Debug, Clone)]
pub(crate) struct NativePolicy {
    resources: Arc<NativePolicyResources>,
    components: Arc<BTreeMap<String, Arc<ComponentPolicy>>>,
    bindings: Arc<RwLock<BTreeMap<String, Arc<ComponentPolicy>>>>,
    invocations: Arc<Mutex<BTreeMap<String, InvocationAuthority>>>,
    traces: Arc<InvocationTraces>,
    application: Arc<OnceLock<Weak<NativeApplication<NativePolicy>>>>,
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

impl InvocationPolicy for NativePolicy {
    type Facts = NativeFacts;
    type Authority = NativeAuthorityGuard;

    /// Retain only a weak reference so native workload teardown has no ownership cycle.
    fn bind_application(&self, application: &Arc<NativeApplication<Self>>) -> anyhow::Result<()> {
        self.application
            .set(Arc::downgrade(application))
            .map_err(|_| anyhow::anyhow!("native-policy-workload-already-bound"))
    }

    /// Grant authority after native initialization, with rollback on every partial failure.
    async fn activate(
        &self,
        component_id: &str,
        invocation_scope: &InvocationScope<Self>,
        request: &NativeInvocation<Self>,
        failure: NativeCallFailure,
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
            let admitted_operation = fact
                .operation(operation)
                .context("native invocation operation is not admitted")?;
            authorize_registered_operation(
                caller,
                admitted_operation.registered_operation.as_deref(),
                admitted_operation.fresh_only,
            )?;
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
        resources.postgres.bind_transaction_scope(
            scope,
            deadline,
            request.facts.transaction_participation.as_ref(),
        )?;
        if let Some(selected) = &request.facts.selected_participant {
            resources.postgres.bind_selected_participant(
                scope,
                selected.dependency.operation.clone(),
                selected.intent.clone(),
            )?;
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
                    selected_participant: request.facts.selected_participant.clone(),
                },
            );
        anyhow::ensure!(previous.is_none(), "native-invocation-scope-already-bound");
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
            .keys()
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

impl NativePolicy {
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
            None => nested_host_error(&error),
        }
    }

    async fn invoke_pre_commit(
        &self,
        scope: &str,
        slot: &str,
        context: node_types::NodeContext,
        input: NativeInput,
    ) -> anyhow::Result<NativeOutcome> {
        let bound = self
            .invocations
            .lock()
            .expect("native invocation lock poisoned")
            .get(scope)
            .cloned()
            .context("native-pre-commit-invocation-unbound")?;
        let owner = self
            .components
            .values()
            .find(|entry| {
                entry.fact.scope.package_id == bound.acquisition.invocation.package_id
                    && entry.fact.component_digest == bound.acquisition.invocation.component_digest
                    && entry.fact.component == bound.acquisition.invocation.component
            })
            .context("native-pre-commit-owner-missing")?;
        anyhow::ensure!(
            owner.fact.operations[&bound.operation]
                .pre_commit
                .as_deref()
                == Some(slot),
            "native-pre-commit-not-owned-by-operation"
        );
        let selected = bound
            .selected_participant
            .context("native-pre-commit-participant-unbound")?;
        anyhow::ensure!(
            self.resources
                .postgres
                .prepare_transaction_participation(scope, &selected.dependency.operation,)?
                .is_some(),
            "native-pre-commit-transaction-unselected"
        );
        self.invoke_nested(
            scope,
            &BTreeSet::from([bound.operation]),
            &selected.dependency,
            context,
            input,
        )
        .await
    }

    async fn invoke_nested(
        &self,
        scope: &str,
        owners: &BTreeSet<String>,
        dependency: &ComponentOperationDependency,
        mut context: node_types::NodeContext,
        input: NativeInput,
    ) -> anyhow::Result<NativeOutcome> {
        self.resources
            .postgres
            .permit_transaction_nested_call(scope)
            .map_err(|_| {
                OperationRefusal::new(
                    OperationRefusalKind::PermissionDenied,
                    dependency.operation.as_str(),
                )
            })?;
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
        // Admission enforces unique providers for ordinary imports. A selected
        // participant is an exact export of the admitted caller component.
        let mut providers = self.components.values().filter(|entry| {
            entry.fact.operations.contains_key(&dependency.operation)
                && entry.fact.scope.package_id == dependency.package
                && entry.fact.scope.package_version == dependency.version
                && entry.fact.component_digest == dependency.digest
        });
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
        let owner = self
            .components
            .values()
            .find(|entry| {
                entry.fact.scope.package_id == bound.acquisition.invocation.package_id
                    && entry.fact.component_digest == bound.acquisition.invocation.component_digest
                    && entry.fact.component == bound.acquisition.invocation.component
            })
            .context("native-participant-owner-missing")?;
        let declared_participant = owner.fact.operations[&bound.operation]
            .dependencies
            .iter()
            .find(|declared| declared.operation == dependency.operation)
            .and_then(|declared| declared.participant.as_ref());
        let selected_participant = if let Some(participant) = declared_participant {
            anyhow::ensure!(
                operation.pre_commit.is_some(),
                "native-base-has-no-pre-commit"
            );
            let participant_operation = owner
                .fact
                .operation(participant)
                .context("native-participant-export-missing")?;
            anyhow::ensure!(
                participant_operation.registered_operation.as_deref() == Some(participant),
                "native-participant-permission-unbound"
            );
            authorize_registered_operation(
                bound.caller.as_ref(),
                participant_operation.registered_operation.as_deref(),
                participant_operation.fresh_only,
            )?;
            let participant_dependency = ComponentOperationDependency {
                participant: None,
                package: owner.fact.scope.package_id.clone(),
                version: owner.fact.scope.package_version.clone(),
                digest: owner.fact.component_digest.clone(),
                operation: participant.clone(),
            };
            let intent = serde_json::to_string(&serde_json::json!({
                "participant": participant_dependency,
                "release": self.resources.release.manifest().release.effective_release_id,
                "manifest": self.resources.release.release().manifest_digest.as_str(),
            }))?;
            Some(SelectedParticipant {
                dependency: participant_dependency,
                intent,
            })
        } else {
            None
        };
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
        let transaction_participation = self
            .resources
            .postgres
            .prepare_transaction_participation(scope, &dependency.operation)?;
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
        let position = bound.acquisition.invocation.entry.wiring();
        let span = tracing::info_span!(
            "wamn.component.invoke",
            wamn.tenant = %target.scope.tenant_id,
            wamn.project = %self.resources.project,
            wamn.environment = %self.resources.release.manifest().release.environment,
            wamn.wiring_id = position.map_or("", |position| position.wiring_id.as_str()),
            wamn.wiring_version = position.map_or(0, |position| position.wiring_version),
            wamn.component_digest = %target.component_digest,
            wamn.node_id = position.map_or("", |position| position.node_id.as_str()),
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
                    CredentialKind::QueuedService => "queued-service",
                },
            );
        }
        invoke_owned(
            &dispatch,
            NativeInvocation {
                operation: dependency.operation.clone(),
                context,
                input,
                deadline,
                facts: NativeFacts {
                    acquisition: bound.acquisition.retarget(target, &dependency.operation),
                    caller: bound.caller,
                    transaction_participation,
                    selected_participant,
                },
                application,
            },
        )
        .instrument(span)
        .await
    }
}

/// Revoke host registries when a call returns, traps, fails to bind, or is cancelled.
#[derive(Debug)]
pub(crate) struct NativeAuthorityGuard {
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
        let slots: BTreeSet<_> = policy
            .fact
            .operations
            .values()
            .filter_map(|operation| operation.pre_commit.as_ref())
            .collect();
        for slot in slots {
            let slot = slot.clone();
            linker.instance(&slot)?.func_new_concurrent(
                "run",
                move |accessor, _ty, params, results| {
                    let (trace, scope, policy) = accessor.with(|mut access| {
                        let active = extract_active_ctx(access.get());
                        (
                            invocation_trace(&active),
                            Arc::clone(&active.ctx.component_id),
                            active.ctx.get_plugin::<NativePolicy>(NATIVE_POLICY_ID),
                        )
                    });
                    let slot = slot.clone();
                    Box::pin(trace.run(async move {
                        let [context, input] = params else {
                            wash_runtime::wasmtime::bail!(
                                "pre-commit requires context and typed input"
                            );
                        };
                        let context = typed_context(context)
                            .map_err(|error| policy.host_failure(&scope, error))?;
                        let outcome = policy
                            .invoke_pre_commit(
                                &scope,
                                &slot,
                                context,
                                NativeInput::Typed(input.clone()),
                            )
                            .await
                            .map_err(|error| policy.host_failure(&scope, error))?;
                        let NativeOutcome::Typed(value) = outcome else {
                            wash_runtime::wasmtime::bail!("pre-commit returned JSON");
                        };
                        let [result] = results else {
                            wash_runtime::wasmtime::bail!("pre-commit requires one result");
                        };
                        *result = value;
                        Ok(())
                    }))
                },
            )?;
        }
        for (dependency, owners) in links.values() {
            let dependency = dependency.clone();
            let owners = Arc::new(owners.clone());
            let component_type = component.component_type();
            let import = component_type
                .get_import(component.engine(), &dependency.operation)
                .context("admitted dependency import is absent")?;
            let wash_runtime::wasmtime::component::types::ComponentItem::ComponentInstance(
                interface,
            ) = import.ty
            else {
                anyhow::bail!("admitted dependency is not an interface");
            };
            let run = interface
                .get_export(component.engine(), "run")
                .context("admitted dependency run is absent")?;
            let wash_runtime::wasmtime::component::types::ComponentItem::ComponentFunc(run) =
                run.ty
            else {
                anyhow::bail!("admitted dependency run is not a function");
            };
            let typed = run.params().nth(1).is_some_and(|(_, ty)| {
                matches!(
                    ty,
                    wash_runtime::wasmtime::component::Type::List(_)
                        | wash_runtime::wasmtime::component::Type::Record(_)
                )
            });
            if typed {
                let mut dependency_linker = linker.instance(&dependency.operation)?;
                dependency_linker.func_new_concurrent(
                    "run",
                    move |accessor, _ty, params, results| {
                        let (trace, scope, policy) = accessor.with(|mut access| {
                            let active = extract_active_ctx(access.get());
                            (
                                invocation_trace(&active),
                                Arc::clone(&active.ctx.component_id),
                                active.ctx.get_plugin::<NativePolicy>(NATIVE_POLICY_ID),
                            )
                        });
                        let owners = Arc::clone(&owners);
                        let dependency = dependency.clone();
                        Box::pin(trace.run(async move {
                            let [context, input] = params else {
                                wash_runtime::wasmtime::bail!(
                                    "typed operation requires context and input"
                                );
                            };
                            let context = typed_context(context)
                                .map_err(|error| policy.host_failure(&scope, error))?;
                            let outcome = policy
                                .invoke_nested(
                                    &scope,
                                    &owners,
                                    &dependency,
                                    context,
                                    NativeInput::Typed(input.clone()),
                                )
                                .await
                                .map_err(|error| policy.host_failure(&scope, error))?;
                            let NativeOutcome::Typed(value) = outcome else {
                                wash_runtime::wasmtime::bail!("typed operation returned JSON");
                            };
                            let [result] = results else {
                                wash_runtime::wasmtime::bail!(
                                    "typed operation requires one result"
                                );
                            };
                            *result = value;
                            Ok(())
                        }))
                    },
                )?;
                // This adapter belongs to dynamic entry, never to a known nested call.
                dependency_linker.func_new_concurrent(
                    "run-json",
                    |_accessor, _ty, _params, _results| {
                        Box::pin(async {
                            wash_runtime::wasmtime::bail!(
                                "nested application calls must use the typed operation"
                            )
                        })
                    },
                )?;
                continue;
            }
            linker
                .instance(&dependency.operation)?
                .func_wrap_concurrent(
                    "run",
                    move |accessor, (context, input): (node_types::NodeContext, String)| {
                        let (trace, scope, policy) = accessor.with(|mut access| {
                            let active = extract_active_ctx(access.get());
                            (
                                invocation_trace(&active),
                                Arc::clone(&active.ctx.component_id),
                                active.ctx.get_plugin::<NativePolicy>(NATIVE_POLICY_ID),
                            )
                        });
                        let owners = Arc::clone(&owners);
                        let dependency = dependency.clone();
                        Box::pin(trace.run(async move {
                            let result = policy
                                .invoke_nested(&scope, &owners, &dependency, context, input.into())
                                .await
                                .and_then(NativeOutcome::into_json)
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
