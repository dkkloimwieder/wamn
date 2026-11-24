//! Host authority and dispatch for the workflow service in an application workload.

mod admission;
mod broker;
mod entry;
pub(super) mod lifecycle;
mod node;
mod request;

pub(super) use entry::{RequestCall, RunAs, RunNow};

use wamn_package::canonical_name::CanonicalName;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use tokio::time::Instant;
use wamn_application::application_config::ApplicationConfig;
use wamn_engine::operation::WamnOperation;
use wamn_event::Causation;
use wamn_identity_protocol::PrincipalId;
use wamn_plugin::OperationHost;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::dispatch::DispatchTarget;
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Accessor;
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../wit/wamn-workflow",
        world: "service",
        imports: { default: async | trappable | tracing },
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::workflow::{node as wit_node, run as wit_run};
pub(super) use wit_run::Refusal;

const ID: &str = "wamn-workflow";
const LEASE_MS: i64 = 30_000;
const LEASE_TTL: Duration = Duration::from_secs(30);

/// Shared owner of the service dispatch target and guest import authority.
pub(super) struct Workflow {
    postgres: Arc<OperationHost>,
    operations: Arc<WamnOperation>,
    workloads: RwLock<HashMap<Box<str>, Arc<Workload>>>,
    broker: Arc<broker::Broker>,
}

struct Workload {
    service: Box<str>,
    route: Box<str>,
    application: Arc<ApplicationConfig>,
    project_key: wamn_registry::ProjectEnvKey,
    entry: DispatchTarget,
    runner: String,
    authorities: Mutex<HashMap<String, AuthorityEntry>>,
}

struct AuthorityEntry {
    authority: Arc<Authority>,
    expires: Instant,
}

struct Authority {
    delivery: String,
    workflow_id: CanonicalName,
    version: u32,
    hash: wamn_digest::Digest,
    principal: PrincipalId,
    role: String,
    generation: Option<i64>,
    request_deadline: Option<Instant>,
    responded: AtomicBool,
    causation: Option<Causation>,
}

impl std::fmt::Debug for Workflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Workflow").finish_non_exhaustive()
    }
}

impl Workflow {
    pub(super) fn new(
        postgres: Arc<OperationHost>,
        operations: Arc<WamnOperation>,
        nats_server: String,
        tls_first: bool,
    ) -> Self {
        Self {
            postgres,
            operations,
            workloads: RwLock::default(),
            broker: Arc::new(broker::Broker::new(nats_server, tls_first)),
        }
    }

    fn workload(&self, id: &str) -> Result<Arc<Workload>, Refusal> {
        self.workloads
            .read()
            .expect("workflow workloads lock poisoned")
            .get(id)
            .map(Arc::clone)
            .ok_or(Refusal::Unavailable)
    }
}

impl Workload {
    fn create_authority(&self, authority: Arc<Authority>) -> String {
        let handle = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        let expires = if authority.generation.is_some() {
            now + LEASE_TTL
        } else {
            authority.request_deadline.unwrap_or(now)
        };
        let mut entries = self
            .authorities
            .lock()
            .expect("workflow authority lock poisoned");
        entries.retain(|_, entry| entry.expires > now);
        entries.insert(handle.clone(), AuthorityEntry { authority, expires });
        handle
    }

    fn authority(&self, handle: &str) -> Result<Arc<Authority>, Refusal> {
        self.authorities
            .lock()
            .expect("workflow authority lock poisoned")
            .get(handle)
            .filter(|entry| entry.expires > Instant::now())
            .map(|entry| Arc::clone(&entry.authority))
            .ok_or(Refusal::InvalidAuthority)
    }

    fn remove_authority(&self, handle: &str) {
        self.authorities
            .lock()
            .expect("workflow authority lock poisoned")
            .remove(handle);
    }
}

#[async_trait::async_trait]
impl HostPlugin for Workflow {
    fn id(&self) -> &'static str {
        ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wamn:workflow/run@0.1.0"),
                WitInterface::from("wamn:workflow/node@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            item.is_service(),
            "workflow imports require the workload service"
        );
        let run = interfaces.contains("wamn", "workflow", &["run"]);
        let node = interfaces.contains("wamn", "workflow", &["node"]);
        let linker = item.linker()?;
        if run || node {
            wit_run::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
        }
        if node {
            wit_node::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
        }
        Ok(())
    }

    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        item: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            workload.service_id() == Some(item),
            "workflow service identity mismatch"
        );
        let route: Box<str> = {
            let components = workload.components();
            let components = components.read().await;
            components
                .iter()
                .find(|(_, component)| component.name() == "ingress")
                .map(|(route, _)| Box::from(route.as_ref()))
                .context("workflow workload has no ingress sibling")?
        };
        let application = self
            .postgres
            .workflow_application(&route)
            .context("ingress has no bound wamn.application")?;
        let project_key = wamn_registry::name::project_env_key(
            &application.org,
            &application.project,
            &application.environment,
        );
        let entry = workload.dispatch_target(item, ID).await?;
        self.workloads
            .write()
            .expect("workflow workloads lock poisoned")
            .insert(
                workload.id().into(),
                Arc::new(Workload {
                    service: item.into(),
                    route,
                    application,
                    project_key,
                    entry,
                    runner: uuid::Uuid::new_v4().to_string(),
                    authorities: Mutex::default(),
                }),
            );
        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.workloads
            .write()
            .expect("workflow workloads lock poisoned")
            .remove(workload_id);
        Ok(())
    }
}

type BoundService = Result<(Arc<Workflow>, Arc<Workload>), Refusal>;
type WasmResult<T> = wash_runtime::wasmtime::Result<T>;

fn service<T: 'static + Send>(accessor: &Accessor<T, SharedCtx>) -> WasmResult<BoundService> {
    accessor.with(|mut access| {
        let context = access.get();
        let plugin = context.try_get_plugin::<Workflow>(ID)?;
        let workload = plugin.workload(&context.workload_id).and_then(|workload| {
            if *workload.service == *context.component_id {
                Ok(workload)
            } else {
                Err(Refusal::InvalidAuthority)
            }
        });
        Ok(workload.map(|workload| (plugin, workload)))
    })
}

fn storage_error(error: &wamn_run_state::run_store::ProductionLeaseError) -> Refusal {
    tracing::warn!(%error, "workflow persistence failed");
    Refusal::Unavailable
}
