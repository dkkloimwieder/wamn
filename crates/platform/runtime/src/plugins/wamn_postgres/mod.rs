//! Real `wamn:postgres` host plugin (S2).
//!
//! Contract source of truth: docs/wamn-postgres.wit. Host-enforced invariants:
//!
//! - The guest never holds a socket. Connections live in a deadpool pool
//!   owned by the plugin; guests get resource handles only.
//! - Claims are derived from the executing component's identity
//!   (`Ctx::component_id` → tenant, registered at workload bind time from
//!   `localResources.config["wamn.tenant"]` or via [`WamnPostgres::set_tenant`])
//!   and injected by one fully-bound `set_config(…, is_local => true)` statement
//!   — the `SET LOCAL` equivalent — inside the plugin-managed transaction; every
//!   claim value travels as a bind parameter, so no interpolation path exists
//!   (R2/R16). Guest SQL that tries to set or reset a session variable or
//!   role in-band (`SET` / `RESET` / `set_config`, e.g. a later
//!   `SET app.tenant = 'other'` that would override the BEGIN-time claim) is
//!   rejected on the query/execute/cursor surface (see
//!   [`reject_claim_mutation`], wamn-cjv.2), closing the reachable
//!   transaction-API override. This is a defense-in-depth blocklist, **not** a
//!   structural close: raw dynamic SQL (`DO` / `EXECUTE`) can still construct a
//!   claim mutation, so re-keying RLS onto a non-settable identity (a per-tenant
//!   DB role + `current_user`) is a prerequisite for enabling the raw-SQL node
//!   (wamn-1nd) — until then the claim is trusted only on the parameterized
//!   standard-node path.
//! - `statement_timeout` and a row limit are applied host-side per call.
//! - Abnormal instance death (store dropped mid-transaction, e.g. an epoch
//!   kill) destroys the underlying connection via [`Drop`] on
//!   [`PgTransaction`] — the connection is closed, which makes the server
//!   abort the open transaction, and it is never returned to the pool.
//! - No LISTEN/NOTIFY surface.
//!
//! All parameters travel through the extended-query protocol as bound values
//! (`$1..$n`); there is no interpolation path. Params are sent in the *text*
//! wire format so `numeric`/`timestamptz`/`json`/`uuid` strings are parsed
//! exactly by the server; results arrive in the binary format and are decoded
//! per-type (including a manual binary-NUMERIC → canonical-string decoder to
//! honor the exact-decimal rule).

use std::collections::HashSet;

use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

mod claims;
mod pool;
mod resources;
mod types;

pub use claims::WamnPostgres;
pub use pool::{
    CheckoutProbe, CredentialProvider, K8sSecretProvider, ProjectConfig, StaticCredentialProvider,
    WamnPostgresConfig,
};
pub use resources::{PgCursor, PgTransaction};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "postgres-plugin",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:postgres/client.transaction": super::PgTransaction,
            "wamn:postgres/client.cursor": super::PgCursor,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{Column, PgError, RowSet, SqlValue};

pub const WAMN_POSTGRES_ID: &str = "wamn-postgres";

/// Wire the `wamn:postgres/client` host functions into a linker directly.
/// The host path calls this from [`HostPlugin::on_workload_item_bind`]; the
/// `pgbench` harness calls it to link the capability into a hand-built store.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    client::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

mod causation_bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "causation-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use causation_bindings::wamn::runner::causation;

/// Wire the TRUSTED `wamn:runner/causation` `set-run-context` channel into a
/// linker (l5i9.12.2). Call this ONLY for the trusted, compiled-in flow-runner
/// — the sole component allowed to declare the run it is driving. A custom node
/// must NOT get this: it never imports `wamn:runner`, and the frozen
/// `wamn:postgres` surface rejects a raw-SQL `wamn.*` emit, so guest causation
/// is unforgeable. The handler feeds the [`WamnPostgres`] plugin resolved from
/// the invoking context.
pub fn add_runner_causation_to_linker(
    linker: &mut Linker<SharedCtx>,
) -> wash_runtime::wasmtime::Result<()> {
    causation::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// Per-workload config key carrying the tenant identity (plumbed end-to-end
/// from the WorkloadDeployment CRD's `localResources.config`, i.e. set by the
/// platform, not the guest).
pub const TENANT_CONFIG_KEY: &str = "wamn.tenant";

/// Per-workload config key carrying the `search_path` schema. Optional: absent
/// leaves the server's default search_path in place. Set by the platform (not
/// the guest), like the tenant claim.
pub const SCHEMA_CONFIG_KEY: &str = "wamn.schema";

/// Per-workload config key naming the project whose database this component
/// uses. Optional: absent ⇒ the default project (single-DB deployments and the
/// S2 bench). Set by the platform, not the guest.
pub const PROJECT_CONFIG_KEY: &str = "wamn.project";

/// Per-workload config key carrying the runner's durable-queue LEASE OWNER
/// identity (fqg.4). Optional: absent leaves `app.runner` unset (the S2..S6 and
/// gateway paths never claim from the queue). When set, the plugin injects
/// `SET LOCAL app.runner` alongside the tenant claim, so a flowrunner replica
/// that claims its own work reads a stable, non-spoofable owner
/// (`current_setting('app.runner', true)`) to lease/renew queue rows under —
/// the per-replica identity the reclaim + owner-guarded heartbeat need. Set by
/// the platform (the workload instance id), not the guest.
pub const RUNNER_CONFIG_KEY: &str = "wamn.runner";

/// The project id used when a component names none — the single database a
/// [`WamnPostgresConfig`] URL points at.
pub const DEFAULT_PROJECT: &str = "default";

// ---------------------------------------------------------------------------
// Plugin configuration
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl HostPlugin for WamnPostgres {
    fn id(&self) -> &'static str {
        WAMN_POSTGRES_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wamn:postgres/types@0.1.0"),
                WitInterface::from("wamn:postgres/client@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "postgres", &["client"]) {
            return Ok(());
        }
        if let Some(tenant) = item.local_resources().config.get(TENANT_CONFIG_KEY) {
            let tenant = tenant.clone();
            self.set_tenant(item.id(), &tenant)?;
            tracing::debug!(
                component = item.id(),
                tenant,
                "wamn:postgres tenant registered"
            );
        } else {
            tracing::warn!(
                component = item.id(),
                "component imports wamn:postgres but sets no {TENANT_CONFIG_KEY}; calls will be refused"
            );
        }
        if let Some(project) = item.local_resources().config.get(PROJECT_CONFIG_KEY) {
            let project = project.clone();
            self.set_project(item.id(), &project)?;
            tracing::debug!(
                component = item.id(),
                project,
                "wamn:postgres project registered"
            );
        }
        if let Some(schema) = item.local_resources().config.get(SCHEMA_CONFIG_KEY) {
            let schema = schema.clone();
            self.set_schema(item.id(), &schema)?;
            tracing::debug!(
                component = item.id(),
                schema,
                "wamn:postgres search_path schema registered"
            );
        }
        if let Some(runner) = item.local_resources().config.get(RUNNER_CONFIG_KEY) {
            let runner = runner.clone();
            self.set_runner(item.id(), &runner)?;
            tracing::debug!(
                component = item.id(),
                runner,
                "wamn:postgres runner lease-owner registered"
            );
        }
        client::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }

    /// R31: on workload teardown, reap the per-component claim registries
    /// ([`WamnPostgres::clear_component_claims`]) so a stale tenant / project /
    /// schema / runner / causation claim cannot survive unbind or be inherited by
    /// a rebound component id. The project pools stay — they are project-keyed
    /// (shared, memoized), not per component.
    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.clear_component_claims(workload_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Transaction / cursor resources
// ---------------------------------------------------------------------------
