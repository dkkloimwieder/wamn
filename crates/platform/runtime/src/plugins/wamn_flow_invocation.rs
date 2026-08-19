//! Host plugin for `wamn:flow-invocation@0.1.0`.

use std::collections::{HashMap, HashSet};
use std::str::FromStr as _;
use std::sync::RwLock;

use deadpool_postgres::{Manager, Pool};
use tracing::Instrument as _;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::flow_invocation::{
    InvocationFailure, InvocationService, InvocationServiceConfig, PostgresInvocationBackend,
    SharedOutcomeListener,
};
use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectIdentity, INVOCATION_DURATION_MS, effect_span, record_effect_ms,
};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "flow-invocation-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::flow_invocation::invocation;

pub const WAMN_FLOW_INVOCATION_ID: &str = "wamn-flow-invocation";
const TENANT_CONFIG_KEY: &str = "wamn.tenant";
const CATALOG_CONFIG_KEY: &str = "wamn.catalog";
const ENVIRONMENT_CONFIG_KEY: &str = "wamn.environment";

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    invocation::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

pub struct WamnFlowInvocation {
    backend: Option<PostgresInvocationBackend>,
    outcome_listener: Option<SharedOutcomeListener>,
    services: RwLock<HashMap<String, Registration>>,
}

/// One registered component's invocation service and the bind-time tenant claim
/// its effect spans are enriched with.
///
/// The tenant is held HERE rather than read back out of the service because
/// [`InvocationService`]'s config is private and gets no getter for this: the
/// span needs the same trusted `wamn.tenant` workload config the registration
/// already consumed, not the service's view of it.
///
/// There is deliberately NO project. This plugin's identity is tenant +
/// catalog + environment; `wamn.project` is not its vocabulary. Reading the
/// project or schema workload config back into this plugin is what the deleted
/// inline-execution driver did, and `tests/flow_invocation_wit_coherence.rs`
/// forbids both key constants here by name — so the span leaves the field
/// empty, which truthfully says "this surface holds no such claim". Borrowing
/// the catalog id to fill it would not be true.
#[derive(Clone)]
struct Registration {
    service: InvocationService<PostgresInvocationBackend>,
    tenant: Box<str>,
}

/// The span one flow-invocation effect opens, enriched from the component's
/// bind-time tenant claim.
fn invocation_span(
    registration: &Registration,
    component_id: &str,
    operation: &'static str,
) -> tracing::Span {
    effect_span!(
        "wamn.invocation",
        EffectIdentity {
            tenant: &registration.tenant,
            project: "",
            component: component_id,
        },
        None,
        effect.operation = operation,
    )
}

impl WamnFlowInvocation {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("WAMN_RUN_STORE_PG_URL")
            .or_else(|_| std::env::var("WAMN_PG_URL"))
            .ok();
        let backend = database_url
            .as_deref()
            .map(build_pool)
            .transpose()?
            .map(PostgresInvocationBackend::new);
        let outcome_listener = database_url.map(SharedOutcomeListener::postgres);
        Ok(Self {
            backend,
            outcome_listener,
            services: RwLock::new(HashMap::new()),
        })
    }

    fn register(
        &self,
        component_id: &str,
        tenant: &str,
        catalog: &str,
        environment: &str,
    ) -> anyhow::Result<()> {
        let backend = self
            .backend
            .clone()
            .ok_or_else(|| anyhow::anyhow!("flow invocation database is not configured"))?;
        let outcome_listener = self
            .outcome_listener
            .clone()
            .ok_or_else(|| anyhow::anyhow!("flow invocation listener is not configured"))?;
        let service = InvocationService::with_listener(
            backend,
            outcome_listener,
            InvocationServiceConfig {
                tenant_id: tenant.to_string(),
                catalog_id: catalog.to_string(),
                environment: environment.to_string(),
                platform_revision: env!("CARGO_PKG_VERSION").to_string(),
            },
        );
        self.services
            .write()
            .expect("flow-invocation services lock poisoned")
            .insert(
                component_id.to_string(),
                Registration {
                    service,
                    tenant: tenant.into(),
                },
            );
        Ok(())
    }

    fn registration(&self, component_id: &str) -> Option<Registration> {
        self.services
            .read()
            .expect("flow-invocation services lock poisoned")
            .get(component_id)
            .cloned()
    }

    fn clear_workload(&self, workload_id: &str) {
        self.services
            .write()
            .expect("flow-invocation services lock poisoned")
            .retain(|component, _| !component.starts_with(workload_id));
    }
}

fn build_pool(database_url: &str) -> anyhow::Result<Pool> {
    let config = tokio_postgres::Config::from_str(database_url)?;
    let manager = Manager::new(config, tokio_postgres::NoTls);
    Ok(Pool::builder(manager).max_size(16).build()?)
}

#[async_trait::async_trait]
impl HostPlugin for WamnFlowInvocation {
    fn id(&self) -> &'static str {
        WAMN_FLOW_INVOCATION_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:flow-invocation/invocation@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "flow-invocation", &["invocation"]) {
            return Ok(());
        }
        let config = &item.local_resources().config;
        let tenant = config
            .get(TENANT_CONFIG_KEY)
            .ok_or_else(|| anyhow::anyhow!("missing {TENANT_CONFIG_KEY}"))?;
        let catalog = config
            .get(CATALOG_CONFIG_KEY)
            .ok_or_else(|| anyhow::anyhow!("missing {CATALOG_CONFIG_KEY}"))?;
        let environment = config
            .get(ENVIRONMENT_CONFIG_KEY)
            .ok_or_else(|| anyhow::anyhow!("missing {ENVIRONMENT_CONFIG_KEY}"))?;
        self.register(item.id(), tenant, catalog, environment)?;
        invocation::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.clear_workload(workload_id);
        Ok(())
    }
}

impl invocation::Host for ActiveCtx<'_> {
    async fn begin(
        &mut self,
        request: invocation::InvokeRequest,
    ) -> wash_runtime::wasmtime::Result<Result<invocation::BeginResult, invocation::InvocationError>>
    {
        let plugin = self.try_get_plugin::<WamnFlowInvocation>(WAMN_FLOW_INVOCATION_ID)?;
        let registration = plugin.registration(&self.component_id).ok_or_else(|| {
            wash_runtime::wasmtime::Error::msg("flow invocation component is not registered")
        })?;
        let request = wamn_flow_invocation::InvokeRequest {
            attachment_id: request.attachment_id,
            expected_catalog_version: request.expected_catalog_version,
            expected_definition_hash: request.expected_definition_hash,
            client_request_fingerprint: request.client_request_fingerprint,
            payload: request.payload,
            idempotency_key: request.idempotency_key,
            principal: request.principal,
            deadline_override: request.deadline_override,
            trace: request
                .trace
                .map(|trace| wamn_flow_invocation::TraceContext {
                    traceparent: trace.traceparent,
                    tracestate: trace.tracestate,
                }),
        };
        let span = invocation_span(&registration, self.component_id.as_ref(), "begin");
        let started = std::time::Instant::now();
        let outcome = registration.service.begin(request).instrument(span).await;
        record_effect_ms(
            &INVOCATION_DURATION_MS,
            EFFECT_OPERATION,
            "begin",
            "",
            started.elapsed(),
        );
        Ok(outcome.map(map_begin).map_err(map_invocation_error))
    }

    async fn wait(
        &mut self,
        run_id: String,
        timeout_ms: u32,
    ) -> wash_runtime::wasmtime::Result<
        Result<Option<invocation::InvokeResult>, invocation::InvocationError>,
    > {
        let plugin = self.try_get_plugin::<WamnFlowInvocation>(WAMN_FLOW_INVOCATION_ID)?;
        let registration = plugin.registration(&self.component_id).ok_or_else(|| {
            wash_runtime::wasmtime::Error::msg("flow invocation component is not registered")
        })?;
        let span = invocation_span(&registration, self.component_id.as_ref(), "wait");
        let started = std::time::Instant::now();
        let outcome = registration
            .service
            .wait(run_id, timeout_ms)
            .instrument(span)
            .await;
        record_effect_ms(
            &INVOCATION_DURATION_MS,
            EFFECT_OPERATION,
            "wait",
            "",
            started.elapsed(),
        );
        Ok(outcome
            .map(|outcome| outcome.map(map_result))
            .map_err(map_invocation_error))
    }
}

/// The one translation from the host's contextual failure to the frozen
/// contract's error arm (wamn-0h0g.15.40). The operation, field, run and source
/// chain are logged here and stop here: a guest learns the category only, never
/// run-store internals.
fn map_invocation_error(failure: InvocationFailure) -> invocation::InvocationError {
    tracing::warn!(error = %failure, "flow invocation could not answer");
    match failure.kind() {
        wamn_flow_invocation::InvocationError::StoreUnavailable => {
            invocation::InvocationError::StoreUnavailable
        }
        wamn_flow_invocation::InvocationError::StoreCorrupt => {
            invocation::InvocationError::StoreCorrupt
        }
        wamn_flow_invocation::InvocationError::UnknownRun => {
            invocation::InvocationError::UnknownRun
        }
        wamn_flow_invocation::InvocationError::InvalidRequest => {
            invocation::InvocationError::InvalidRequest
        }
    }
}

fn map_begin(result: wamn_flow_invocation::BeginResult) -> invocation::BeginResult {
    match result {
        wamn_flow_invocation::BeginResult::Admitted(admitted) => {
            invocation::BeginResult::Admitted(invocation::Admitted {
                run_id: admitted.run_id,
            })
        }
        wamn_flow_invocation::BeginResult::Rejected(rejection) => {
            invocation::BeginResult::Rejected(invocation::Rejection {
                status: rejection.status,
                code: rejection.code,
            })
        }
    }
}

fn map_result(result: wamn_flow_invocation::InvokeResult) -> invocation::InvokeResult {
    match result {
        wamn_flow_invocation::InvokeResult::Responded(response) => {
            invocation::InvokeResult::Responded(invocation::Response {
                run_id: response.run_id,
                body: response.body,
                status_hint: response.status_hint,
            })
        }
        wamn_flow_invocation::InvokeResult::Failed(failure) => {
            invocation::InvokeResult::Failed(map_failure(failure))
        }
    }
}

fn map_failure(failure: wamn_flow_invocation::Failure) -> invocation::Failure {
    invocation::Failure {
        status: failure.status,
        error: invocation::FlowError {
            code: failure.error.code,
            message: failure.error.message,
            run_id: failure.error.run_id,
            flow_id: failure.error.flow_id,
            flow_version: failure.error.flow_version,
        },
    }
}
