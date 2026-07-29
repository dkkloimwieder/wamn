//! Host plugin for `wamn:flow-invocation@0.1.0`.

use std::collections::{HashMap, HashSet};
use std::str::FromStr as _;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use deadpool_postgres::{Manager, Pool};
use sha2::{Digest as _, Sha256};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::flow_invocation::{
    InlineRunDriver, InvocationService, InvocationServiceConfig, PostgresInvocationBackend,
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
const PROJECT_CONFIG_KEY: &str = "wamn.project";
const SCHEMA_CONFIG_KEY: &str = "wamn.schema";

fn inline_executor_id(component_id: &str) -> String {
    let digest = hex::encode(Sha256::digest(component_id.as_bytes()));
    format!("flow-invocation-{}", &digest[..32])
}

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    invocation::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

pub struct WamnFlowInvocation {
    backend: Option<PostgresInvocationBackend>,
    database_url: Option<String>,
    driver: Arc<dyn InlineRunDriver>,
    services: RwLock<HashMap<String, InvocationService<PostgresInvocationBackend>>>,
}

impl WamnFlowInvocation {
    pub fn from_env(driver: Arc<dyn InlineRunDriver>) -> anyhow::Result<Self> {
        let database_url = std::env::var("WAMN_RUN_STORE_PG_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .or_else(|_| std::env::var("WAMN_PG_URL"))
            .ok();
        let backend = database_url
            .as_deref()
            .map(build_pool)
            .transpose()?
            .map(PostgresInvocationBackend::new);
        Ok(Self {
            backend,
            database_url,
            driver,
            services: RwLock::new(HashMap::new()),
        })
    }

    fn register(
        &self,
        component_id: &str,
        tenant: &str,
        catalog: &str,
        environment: &str,
        project: &str,
        schema: Option<&str>,
    ) -> anyhow::Result<()> {
        let backend = self
            .backend
            .clone()
            .ok_or_else(|| anyhow::anyhow!("flow invocation database is not configured"))?;
        let service = InvocationService::new(
            backend,
            self.database_url.clone(),
            InvocationServiceConfig {
                tenant_id: tenant.to_string(),
                catalog_id: catalog.to_string(),
                environment: environment.to_string(),
                project: project.to_string(),
                schema: schema.map(str::to_string),
                executor_id: inline_executor_id(component_id),
                platform_revision: env!("CARGO_PKG_VERSION").to_string(),
                lease_ttl: Duration::from_secs(30),
                admission_ttl: Duration::from_secs(86_400),
            },
            self.driver.clone(),
        );
        self.services
            .write()
            .expect("flow-invocation services lock poisoned")
            .insert(component_id.to_string(), service);
        Ok(())
    }

    fn service(&self, component_id: &str) -> Option<InvocationService<PostgresInvocationBackend>> {
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
        let project = config
            .get(PROJECT_CONFIG_KEY)
            .map_or("default", String::as_str);
        let schema = config.get(SCHEMA_CONFIG_KEY).map(String::as_str);
        self.register(item.id(), tenant, catalog, environment, project, schema)?;
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
    ) -> wash_runtime::wasmtime::Result<invocation::BeginResult> {
        let plugin = self.try_get_plugin::<WamnFlowInvocation>(WAMN_FLOW_INVOCATION_ID)?;
        let service = plugin.service(&self.component_id).ok_or_else(|| {
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
        Ok(map_begin(service.begin(request).await.map_err(
            |error| wash_runtime::wasmtime::Error::msg(error.to_string()),
        )?))
    }

    async fn wait(
        &mut self,
        run_id: String,
        timeout_ms: u32,
    ) -> wash_runtime::wasmtime::Result<Option<invocation::InvokeResult>> {
        let plugin = self.try_get_plugin::<WamnFlowInvocation>(WAMN_FLOW_INVOCATION_ID)?;
        let service = plugin.service(&self.component_id).ok_or_else(|| {
            wash_runtime::wasmtime::Error::msg("flow invocation component is not registered")
        })?;
        service
            .wait(run_id, timeout_ms)
            .await
            .map_err(|error| wash_runtime::wasmtime::Error::msg(error.to_string()))?
            .map(map_result)
            .transpose()
    }

    async fn cancel(
        &mut self,
        run_id: String,
    ) -> wash_runtime::wasmtime::Result<invocation::CancelAck> {
        let plugin = self.try_get_plugin::<WamnFlowInvocation>(WAMN_FLOW_INVOCATION_ID)?;
        let service = plugin.service(&self.component_id).ok_or_else(|| {
            wash_runtime::wasmtime::Error::msg("flow invocation component is not registered")
        })?;
        let ack = service
            .cancel(run_id)
            .await
            .map_err(|error| wash_runtime::wasmtime::Error::msg(error.to_string()))?;
        Ok(invocation::CancelAck { run_id: ack.run_id })
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

fn map_result(
    result: wamn_flow_invocation::InvokeResult,
) -> wash_runtime::wasmtime::Result<invocation::InvokeResult> {
    Ok(match result {
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
        wamn_flow_invocation::InvokeResult::Cancelled(failure) => {
            invocation::InvokeResult::Cancelled(map_failure(failure))
        }
    })
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

#[cfg(test)]
mod tests {
    use super::inline_executor_id;

    #[test]
    fn inline_executor_id_is_stable_and_runner_safe() {
        let id = inline_executor_id("workload/component:replica");
        assert_eq!(id, inline_executor_id("workload/component:replica"));
        assert_ne!(id, inline_executor_id("workload/component:other"));
        assert!(id.len() <= 128);
        assert!(
            id.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        );
    }
}
