//! Real `wamn:postgres` host plugin (S2).
//!
//! Contract source of truth: docs/archive/contracts/wamn-postgres.wit. Host-enforced invariants:
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
//!   `reject_claim_mutation`, wamn-cjv.2), closing the reachable
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
#[cfg(feature = "wasm_component_model_implements")]
use std::sync::Arc;

use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

mod claims;
mod pool;
mod production_claim;
mod resources;
mod types;

pub use claims::{
    ConnectionEffectLookup, ConnectionEffectSnapshot, ReleaseIdentity, SessionClaims, WamnPostgres,
};
pub use pool::{
    CheckoutProbe, CredentialProvider, K8sSecretProvider, ProjectConfig, StaticCredentialProvider,
    WamnPostgresConfig,
};
pub use production_claim::{
    ProductionClaimError, ProductionClaimErrorKind, ProductionClaimResult, ProductionReapResult,
};
pub use resources::{PgCursor, PgTransaction};

#[cfg(not(feature = "wasm_component_model_implements"))]
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

#[cfg(feature = "wasm_component_model_implements")]
mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "postgres-plugin",
        imports: { default: async | trappable | tracing },
        named_imports: {
            "wamn:postgres/client": super::NamedProject,
        },
        with: {
            "wamn:postgres/client.transaction": super::PgTransaction,
            "wamn:postgres/client.cursor": super::PgCursor,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

#[cfg(feature = "wasm_component_model_implements")]
#[derive(Clone, Debug)]
pub struct NamedProject(Arc<str>);

#[cfg(feature = "wasm_component_model_implements")]
impl NamedProject {
    fn project(&self) -> &str {
        &self.0
    }

    fn from_interface(interface: &WitInterface) -> anyhow::Result<Self> {
        let name = interface
            .name
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("named wamn:postgres import has no name"))?;
        let project = interface
            .config
            .get(NAMED_PROJECT_CONFIG_KEY)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "named wamn:postgres import {name:?} has no {NAMED_PROJECT_CONFIG_KEY} config"
                )
            })?;
        anyhow::ensure!(
            project.len() <= 64
                && !project.is_empty()
                && project
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'),
            "invalid project {project:?} for named wamn:postgres import {name:?}"
        );
        Ok(Self(Arc::from(project.as_str())))
    }
}

#[cfg(feature = "wasm_component_model_implements")]
const NAMED_PROJECT_CONFIG_KEY: &str = "project";

use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{Column, PgError, RowSet, SqlValue};

#[cfg(feature = "wasm_component_model_implements")]
impl bindings::wamn::postgres::types::Host for wash_runtime::engine::ctx::ActiveCtx<'_> {}

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
/// — the sole component allowed to declare the run it is driving. Other components
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
/// gateway paths never claim from the queue). When set, the host-only
/// production composer injects it alongside the tenant claim as the stable,
/// non-spoofable lease owner used for claim/reclaim and owner-guarded renewal.
/// It is set by the platform (the workload instance id), never by the guest.
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

    #[cfg(feature = "wasm_component_model_implements")]
    fn supports_named_instances(&self) -> bool {
        true
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
        // Release identity is deliberately NOT read here. Under ruling
        // `wamn-0h0g.15.102` the mounted manifest is the sole carrier of the
        // (release version, manifest digest) pair, so the serving process injects
        // it from its loaded weld at instantiation — see
        // `ExecutionHost::instantiate`. A bind-time config read would be a second,
        // *asserted* carrier that cannot correct the welded one, and the pair it
        // asserted could disagree with the manifest the same pod resolves plans
        // against.
        #[cfg(not(feature = "wasm_component_model_implements"))]
        client::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;

        #[cfg(feature = "wasm_component_model_implements")]
        {
            let mut named = std::collections::HashMap::new();
            let mut has_unnamed = false;
            for interface in interfaces.iter().filter(|interface| {
                interface.namespace == "wamn"
                    && interface.package == "postgres"
                    && interface.interfaces.contains("client")
            }) {
                if let Some(name) = interface.name.as_deref() {
                    named.insert(name.to_string(), NamedProject::from_interface(interface)?);
                } else {
                    has_unnamed = true;
                }
            }
            let component = item.component().clone();
            let linker = item.linker();
            bindings::wamn::postgres::types::add_to_linker::<_, SharedCtx>(
                linker,
                extract_active_ctx,
            )?;
            if has_unnamed {
                client::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
            }
            if !named.is_empty() {
                bindings::named_imports::wamn::postgres::client::add_to_linker::<_, SharedCtx>(
                    linker,
                    &component,
                    |name| {
                        named.get(name).cloned().ok_or_else(|| {
                            wash_runtime::wasmtime::Error::msg(format!(
                                "unknown named wamn:postgres import {name:?}"
                            ))
                        })
                    },
                    extract_active_ctx,
                )?;
            }
        }
        Ok(())
    }

    /// R31: on workload teardown, reap the per-component claim registries
    /// (`WamnPostgres::clear_component_claims`) so a stale tenant / project /
    /// schema / runner / release-identity / causation claim cannot survive unbind
    /// or be inherited by
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

#[cfg(all(test, feature = "wasm_component_model_implements"))]
mod tests {
    use super::*;

    fn named_interface(name: &str, project: &str) -> WitInterface {
        WitInterface {
            namespace: "wamn".to_string(),
            package: "postgres".to_string(),
            interfaces: HashSet::from(["client".to_string()]),
            version: None,
            config: std::collections::HashMap::from([(
                NAMED_PROJECT_CONFIG_KEY.to_string(),
                project.to_string(),
            )]),
            name: Some(name.to_string()),
        }
    }

    #[test]
    fn named_postgres_import_carries_its_own_project() {
        let alpha = NamedProject::from_interface(&named_interface("alpha", "project-a"))
            .expect("named project config");
        let beta = NamedProject::from_interface(&named_interface("beta", "project-b"))
            .expect("named project config");
        assert_eq!(alpha.project(), "project-a");
        assert_eq!(beta.project(), "project-b");
    }

    #[test]
    fn named_postgres_import_requires_a_valid_project() {
        let interface = named_interface("alpha", "project/a");
        let error = NamedProject::from_interface(&interface).expect_err("invalid project");
        assert!(error.to_string().contains("invalid project"));
    }
}

// ---------------------------------------------------------------------------
// Transaction / cursor resources
// ---------------------------------------------------------------------------
