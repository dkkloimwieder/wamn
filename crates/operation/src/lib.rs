//! Real `wamn:postgres` host plugin (S2).
//!
//! Contract source of truth: `wit/wamn-postgres-0.4/package.wit`.
//! Guest bindgen paths reference that canonical package directly.
//! SQLx owns connection pooling and the prepared statement cache. The host
//! binds each caller's identity inside its transaction. Both SQL interfaces
//! stream results under the database login permissions.

use std::collections::HashSet;
use std::sync::Arc;

use wamn_authentication::authorization::AuthorizationStore as _;
use wamn_authority::identity::SessionIdentity;
use wamn_postgres::DEFAULT_PROJECT;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Component, Linker};
use wash_runtime::wit::{WitInterface, WitWorld};

mod executor;
mod host;
pub mod invocation;
pub mod number_counter;
mod operation;
mod operation_transaction;
mod query;
mod resources;
pub mod result_signing;
pub mod scope_registry;
mod transactions;
pub mod write_log;

pub use executor::OperationDatabase;
pub use host::OperationHost;
pub use operation::WorkflowNode;
pub use operation_transaction::{OperationTransaction, PgCommitFailure};
pub use resources::{PgStatementTransaction, PgTransaction};
pub use write_log::WriteLogStore;

#[cfg(not(feature = "wasm_component_model_implements"))]
mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: [
            "../../wit/wamn-connection",
            "../../wit/wamn-ingress",
            "../../wit/wamn-postgres-0.4",
            "../../wit/wamn-node",
        "../../wit/wamn-operation-0.1",
            "../../wit/wasmcloud-blobstore",
            "../authority/wit",
        ],
        world: "wamn:host/postgres-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:postgres/statements.transaction": super::PgStatementTransaction,
            "wamn:ingress/routing.authenticated-caller": wamn_engine::ingress::AuthenticatedCaller,
            // The engine owns the operation binding and the types its refusal names.
            "wamn:postgres/types": wamn_engine::entry_operation::postgres_types,
            "wamn:operation/operation": wamn_engine::entry_operation::operation,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

#[cfg(feature = "wasm_component_model_implements")]
mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: [
            "../../wit/wamn-connection",
            "../../wit/wamn-ingress",
            "../../wit/wamn-postgres-0.4",
            "../../wit/wamn-node",
        "../../wit/wamn-operation-0.1",
            "../../wit/wasmcloud-blobstore",
            "../authority/wit",
        ],
        world: "wamn:host/postgres-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        named_imports: {
            "wamn:postgres/statements": super::NamedStatements,
        },
        with: {
            "wamn:postgres/statements.transaction": super::PgStatementTransaction,
            "wamn:ingress/routing.authenticated-caller": wamn_engine::ingress::AuthenticatedCaller,
            // The engine owns the operation binding and the types its refusal names.
            "wamn:postgres/types": wamn_engine::entry_operation::postgres_types,
            "wamn:operation/operation": wamn_engine::entry_operation::operation,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

/// Each named SQL import borrows the same operation transaction.
#[cfg(feature = "wasm_component_model_implements")]
#[derive(Clone, Debug)]
pub struct NamedStatements;

use bindings::wamn::postgres::client;
use bindings::wamn::postgres::statements as statement_wit;
use bindings::wamn::postgres::types::{PgError, SqlValue};

pub const WAMN_POSTGRES_ID: &str = "wamn-postgres";

/// Link dynamic reads and SQL in the host operation transaction.
/// The host path calls this from [`HostPlugin::on_workload_item_bind`]; the
/// `pgbench` harness calls it to link the host service into a hand-built store.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    client::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
    statement_wit::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// Per-workload config key carrying the project identity (plumbed end-to-end
/// from the WorkloadDeployment CRD's `localResources.config`, i.e. set by the
/// platform, not the guest).
pub const PROJECT_ENV_KEY_CONFIG_KEY: &str = "wamn.project_env_key";

/// Per-workload config key carrying the `search_path` schema. Optional: absent
/// leaves the server's default search_path in place. Set by the platform (not
/// the guest), like the project identity.
pub const SCHEMA_CONFIG_KEY: &str = "wamn.schema";

/// Per-workload config key naming the project whose database this component
/// uses. Optional: absent ⇒ the default project (single-DB deployments and the
/// S2 bench). Set by the platform, not the guest.
pub const PROJECT_CONFIG_KEY: &str = "wamn.project";

/// Per-workload config key carrying the runner's durable-queue LEASE OWNER
/// identity (fqg.4). Optional: absent leaves `app.runner` unset (the S2..S6 and
/// gateway paths never identity from the queue). When set, the host-only
/// production composer injects it alongside the project identity as the stable,
/// non-spoofable lease owner used for identity/reclaim and owner-guarded renewal.
/// It is set by the platform (the workload instance id), never by the guest.
pub const RUNNER_CONFIG_KEY: &str = "wamn.runner";

// ---------------------------------------------------------------------------
// Plugin configuration
// ---------------------------------------------------------------------------

/// How many times each half of the un-fused workload bind has run.
///
/// Observability for the count rule on `wamn-0h0g.17.15` -- registration once
/// per request, linker entries once per digest -- not state a request reads.
#[derive(Default)]
pub(crate) struct BindCounters {
    linker_entries: std::sync::atomic::AtomicU64,
    scope_registrations: std::sync::atomic::AtomicU64,
}

impl OperationHost {
    /// The per-REQUEST half of the workload bind: every identity keyed by `scope`.
    ///
    /// Un-fused from [`HostPlugin::on_workload_item_bind`] so the router driver
    /// can run this once per request while [`Self::add_linker_entries`] runs
    /// once per digest and backs a shared `InstancePre`. Registers nothing when
    /// the workload imports no wamn:postgres interface, exactly as the fused
    /// hook did.
    pub fn register_workload_scope(
        &self,
        scope: &str,
        config: &std::collections::HashMap<String, String>,
        interfaces: &WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let has_client = interfaces.contains("wamn", "postgres", &["client"]);
        let has_statements = interfaces.contains("wamn", "postgres", &["statements"]);
        if !has_client && !has_statements {
            return Ok(());
        }
        self.bind_counters
            .scope_registrations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(project_env_key) = config.get(PROJECT_ENV_KEY_CONFIG_KEY) {
            let project_env_key: wamn_registry::ProjectEnvKey = project_env_key.parse()?;
            scope_registry::set_project_env_key(&self.scopes, scope, &project_env_key);
            tracing::debug!(
                component = scope,
                %project_env_key,
                "wamn:postgres project registered"
            );
        } else {
            tracing::warn!(
                component = scope,
                "component imports wamn:postgres but sets no {PROJECT_ENV_KEY_CONFIG_KEY}; calls will be refused"
            );
        }
        if let Some(project) = config.get(PROJECT_CONFIG_KEY) {
            let project: wamn_registry::ProjectId = project.parse()?;
            scope_registry::set_project(&self.scopes, scope, &project)?;
            tracing::debug!(
                component = scope,
                %project,
                "wamn:postgres project registered"
            );
        }
        if let Some(schema) = config.get(SCHEMA_CONFIG_KEY) {
            let schema = schema.clone();
            scope_registry::set_schema(&self.scopes, scope, &schema)?;
            tracing::debug!(
                component = scope,
                schema,
                "wamn:postgres search_path schema registered"
            );
        }
        if let Some(runner) = config.get(RUNNER_CONFIG_KEY) {
            let runner = runner.clone();
            scope_registry::set_runner(&self.scopes, scope, &runner)?;
            tracing::debug!(
                component = scope,
                runner,
                "wamn:postgres runner lease-owner registered"
            );
        }
        // Release identity is deliberately NOT read here. Under ruling
        // `wamn-0h0g.15.102` the mounted manifest is the sole carrier of the
        // (release version, manifest digest) pair, so the serving process injects
        // it from its loaded release at instantiation — see
        // `ExecutionHost::instantiate`. A bind-time config read would be a second,
        // *asserted* carrier that cannot correct the loaded one, and the pair it
        // asserted could disagree with the manifest the same pod resolves plans
        // against.
        Ok(())
    }

    /// The per-DIGEST half: this plugin's host definitions on `linker`.
    ///
    /// CAPTURES NOTHING PER-REQUEST. Every definition reaches its state through
    /// the store's active ctx, so a linker carrying these entries is shareable
    /// by digest -- which is what lets the driver seal it into an `InstancePre`
    /// once and instantiate from it on every request.
    pub fn add_linker_entries(
        &self,
        linker: &mut Linker<SharedCtx>,
        component: &Component,
        interfaces: &WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let has_types = interfaces.contains("wamn", "postgres", &["types"]);
        let has_client = interfaces.contains("wamn", "postgres", &["client"]);
        let has_statements = interfaces.contains("wamn", "postgres", &["statements"]);
        if interfaces.contains("wamn", "operation", &["operation"]) {
            wamn_engine::entry_operation::operation::add_to_linker::<
                _,
                operation::PostgresOperation,
            >(linker, extract_active_ctx)?;
        }
        if has_types {
            bindings::wamn::postgres::types::add_to_linker::<_, SharedCtx>(
                linker,
                extract_active_ctx,
            )?;
        }
        if !has_client && !has_statements {
            return Ok(());
        }
        self.bind_counters
            .linker_entries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(feature = "wasm_component_model_implements"))]
        let _ = component;
        #[cfg(not(feature = "wasm_component_model_implements"))]
        {
            if has_client {
                client::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
            }
            if has_statements {
                statement_wit::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
            }
        }

        #[cfg(feature = "wasm_component_model_implements")]
        {
            if has_client {
                client::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
            }
            if has_statements {
                statement_wit::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
                // One instance per `(implements ..)` import the component itself
                // declares. The workload's host interface entry is unnamed, so
                // only the component knows the member import names.
                bindings::named_imports::wamn::postgres::statements::add_to_linker::<_, SharedCtx>(
                    linker,
                    component,
                    |_name| Ok(NamedStatements),
                    extract_active_ctx,
                )?;
            }
        }
        Ok(())
    }

    /// Linker-entry binds since startup: one per digest under the driver.
    pub fn linker_entry_binds(&self) -> u64 {
        self.bind_counters
            .linker_entries
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Scope registrations since startup: one per request under the driver.
    pub fn scope_registrations(&self) -> u64 {
        self.bind_counters
            .scope_registrations
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl HostPlugin for OperationHost {
    fn id(&self) -> &'static str {
        WAMN_POSTGRES_ID
    }

    fn binding_schema(&self) -> wash_runtime::plugin::BindingSchema {
        wash_runtime::plugin::BindingSchema::with_host_owned_keys(
            self.pools
                .host_projects()
                .map(wamn_registry::ProjectId::as_str),
        )
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wamn:postgres/types@0.4.0"),
                WitInterface::from("wamn:postgres/client@0.4.0"),
                WitInterface::from("wamn:postgres/statements@0.4.0"),
                WitInterface::from("wamn:operation/operation@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    #[cfg(feature = "wasm_component_model_implements")]
    fn supports_named_instances(&self) -> bool {
        true
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let application = wamn_engine::application::bind_application_config(&self.scopes, item)?;
        if let Some(application) = &application {
            let key = wamn_registry::name::project_env_key(
                &application.org,
                &application.project,
                &application.environment,
            );
            self.pools
                .bind_host_project(&application.project, &key)
                .await?;
            if let Some(store) = &self.authorization {
                store.configure_application(application)?;
            }
        }
        if interfaces.contains("wamn", "operation", &["operation"]) {
            let WorkloadItem::Component(component) = item else {
                anyhow::bail!("operation authority requires an entry component");
            };
            if let Some(application) = application {
                self.bind_entry_instance(component.workload_id(), component.id(), application)?;
            }
            // A control root links the interface without operation authority.
            // Calls on an unbound instance refuse before accessing SQL or identity.
        }
        // The wash host path: one workload, one bind, both halves in order.
        item.add_plugin(
            wamn_engine::invocation_scope::INVOCATION_SCOPES_ID,
            Arc::clone(&self.scopes) as _,
        );
        self.register_workload_scope(item.id(), &item.local_resources().config, &interfaces)?;
        let component = item.component().clone();
        self.add_linker_entries(item.linker()?, &component, &interfaces)
    }

    /// R31: on workload teardown, reap the per-component identity registries
    /// (`OperationHost::clear_component_identity`) so a stale project / project /
    /// schema / runner / workload authority / release-identity / causation
    /// identity cannot survive unbind
    /// or be inherited by
    /// a rebound component id. The project pools stay — they are project-keyed
    /// (shared, memoized), not per component.
    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.clear_component_identity(workload_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Session identity
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "session_identity/tests.rs"]
mod session_identity_tests;
