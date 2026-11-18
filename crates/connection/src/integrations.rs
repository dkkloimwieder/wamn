//! The bound guard of integration sibling components; wasmCloud owns their code.

use crate::connection_facts::{ConnectionCache, ConnectionFacts};
use anyhow::Context as _;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Resource;
use wash_runtime::wit::{WitInterface, WitWorld};

const ID: &str = "wamn-integration-bound";
const BOUND: &str = "wamn:integration/bound@0.1.0";
pub const INTEGRATION_CONFIG: &str = "wamn.integration";

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../wit/wamn-integration",
        inline: "package wamn:integration-host; world host { import wamn:integration/bound@0.1.0; }",
        world: "wamn:integration-host/host",
        imports: { default: async | trappable },
        with: { "wamn:integration/bound.call": super::Call },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}
use bindings::wamn::integration::bound;

/// Parameters and credential selected for one integration call.
pub struct Call {
    parameters: String,
    credential: Option<Arc<crate::connection_credentials::Secret>>,
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call").finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct State {
    workload: String,
    id: uuid::Uuid,
    bound: OnceLock<Arc<ConnectionFacts>>,
}

#[async_trait::async_trait]
impl HostPlugin for State {
    fn id(&self) -> &'static str {
        ID
    }
    fn world(&self) -> WitWorld {
        world()
    }
}
fn world() -> WitWorld {
    WitWorld {
        imports: HashSet::from([WitInterface::from(BOUND)]),
        exports: HashSet::new(),
    }
}

/// Fixed-interface host plugin; each sibling supplies its connection id.
#[derive(Debug)]
pub struct IntegrationBound {
    cache: Arc<dyn ConnectionCache>,
    states: RwLock<HashMap<String, Arc<State>>>,
}
impl IntegrationBound {
    pub fn new(cache: Arc<dyn ConnectionCache>) -> Self {
        Self {
            cache,
            states: RwLock::default(),
        }
    }
}
#[async_trait::async_trait]
impl HostPlugin for IntegrationBound {
    fn id(&self) -> &'static str {
        ID
    }
    fn world(&self) -> WitWorld {
        world()
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let id: uuid::Uuid = item
            .local_resources()
            .config
            .get(INTEGRATION_CONFIG)
            .context("integration sibling has no wamn.integration")?
            .parse()?;
        let WorkloadItem::Component(component) = &*item else {
            anyhow::bail!("integration must be a sibling component");
        };
        let state = Arc::new(State {
            workload: component.workload_id().to_owned(),
            id,
            bound: OnceLock::new(),
        });
        self.states
            .write()
            .expect("integration states lock poisoned")
            .insert(component.id().to_owned(), Arc::clone(&state));
        item.add_plugin(ID, state);
        bound::add_to_linker::<_, SharedCtx>(
            item.linker()?,
            wash_runtime::engine::ctx::extract_active_ctx,
        )?;
        Ok(())
    }
    async fn on_workload_resolved(
        &self,
        _workload: &ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        let state = self
            .states
            .read()
            .expect("integration states lock poisoned")
            .get(component_id)
            .map(Arc::clone)
            .context("integration state missing")?;
        let facts = Arc::clone(&self.cache).integration(&state.workload, &state.id)?;
        state
            .bound
            .set(facts)
            .map_err(|_| anyhow::anyhow!("integration already resolved"))?;
        Ok(())
    }
    async fn on_workload_unbind(&self, workload: &str, _: WitInterfaces<'_>) -> anyhow::Result<()> {
        self.states
            .write()
            .expect("integration states lock poisoned")
            .retain(|_, state| state.workload != workload);
        Ok(())
    }
}
impl bound::Host for ActiveCtx<'_> {
    async fn authority(&mut self) -> wash_runtime::wasmtime::Result<String> {
        let state = self.try_get_plugin::<State>(ID)?;
        let bound = state
            .bound
            .get()
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("integration unresolved"))?;
        let answer = bound
            .answer(&state.id)
            .await
            .map_err(|error| wash_runtime::wasmtime::Error::msg(error.to_string()))?
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("connection unavailable"))?;
        answer
            .fact
            .authority
            .clone()
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("integration authority absent"))
    }
    async fn begin(&mut self) -> wash_runtime::wasmtime::Result<Result<Resource<Call>, String>> {
        let state = self.try_get_plugin::<State>(ID)?;
        let result = async {
            let bound = state.bound.get().context("integration unresolved")?;
            let answer = bound
                .answer(&state.id)
                .await?
                .context("connection unavailable")?;
            anyhow::ensure!(
                answer.fact.status == crate::connection_memory::ConnectionStatus::Active,
                "connection disabled"
            );
            let mut parameters = answer.generation.parameters.clone();
            if let Some(parameters) = parameters.as_object_mut() {
                parameters.remove("auth");
            }
            let credential = match answer.fact.credential_id {
                Some(id) => Some(bound.secret(id, None).await?),
                None => None,
            };
            Ok::<_, anyhow::Error>(Call {
                parameters: parameters.to_string(),
                credential: credential.filter(|_| answer.generation.auth_type == "custom"),
            })
        }
        .await;
        match result {
            Ok(call) => Ok(Ok(self.table.push(call)?)),
            Err(error) => {
                tracing::warn!(connection = %state.id, error = %error, "integration begin refused");
                Ok(Err("connection unavailable".into()))
            }
        }
    }
}
impl bound::HostCall for ActiveCtx<'_> {
    async fn parameters(&mut self, call: Resource<Call>) -> wash_runtime::wasmtime::Result<String> {
        Ok(self.table.get(&call)?.parameters.clone())
    }
    async fn credential(
        &mut self,
        call: Resource<Call>,
    ) -> wash_runtime::wasmtime::Result<Option<String>> {
        Ok(self
            .table
            .get(&call)?
            .credential
            .as_deref()
            .map(|secret| secret.expose().to_owned()))
    }
    async fn drop(&mut self, call: Resource<Call>) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(call)?;
        Ok(())
    }
}
