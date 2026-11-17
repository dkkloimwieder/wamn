//! Dispatch application operations through the workload's resolved component targets.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::oneshot;
use tokio::time::Instant;
use wamn_catalog::application_config::{APPLICATION_CONFIG, ApplicationConfig};
use wamn_catalog::canonical_name::CanonicalName;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, Instance, Val};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::invocation_scope::{InvocationExecution, InvocationScopes};
use crate::invocation_trace::InvocationTrace;

pub mod ambient_cli;
pub mod json;
pub mod native_call;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../wit/wamn-node",
        world: "node",
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

pub use bindings::wamn::node::types as node_types;

/// Host plugin that retains application targets; SQL owns the operation interface.
pub struct WamnOperation {
    host: Arc<dyn HostPlugin>,
    bindings: RwLock<HashMap<String, Arc<Binding>>>,
}

struct Binding {
    workload: String,
    application: ApplicationConfig,
    targets: OnceLock<BTreeMap<String, ComponentTarget>>,
}

struct ComponentTarget {
    dispatch: DispatchTarget,
    warm: bool,
}

impl std::fmt::Debug for WamnOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WamnOperation")
            .field("host", &self.host.id())
            .finish_non_exhaustive()
    }
}

impl WamnOperation {
    /// Delegate the operation WIT binding to its SQL owner, without a crate cycle.
    pub fn new(host: Arc<dyn HostPlugin>) -> Self {
        Self {
            host,
            bindings: RwLock::default(),
        }
    }

    /// Invoke an export under the authority already admitted by SQL.
    pub async fn invoke(
        &self,
        component: &str,
        scopes: &InvocationScopes,
        scope: Arc<str>,
        operation: CanonicalName,
        context: node_types::NodeContext,
        input: String,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let binding = self
            .bindings
            .read()
            .expect("operation bindings lock poisoned")
            .get(component)
            .map(Arc::clone)
            .context("operation binding missing")?;
        let serving = binding
            .application
            .components
            .iter()
            .find_map(|component| component.operations.get(&operation))
            .context("operation is not configured")?;
        let target = binding
            .targets
            .get()
            .and_then(|targets| targets.get(&serving.component))
            .context("operation dispatch target missing")?;
        let deadline = Instant::now()
            + Duration::from_millis(crate::native_routes::bounded_node_deadline_ms(
                context.deadline_ms,
            ));
        let execution = InvocationExecution::new(deadline);
        scopes.set_execution(&scope, execution)?;
        let execution = scopes
            .execution(&scope)
            .context("operation scope missing")?;
        let deadline = execution.deadline();
        let (reply, response) = oneshot::channel();
        let retired = Arc::new(AtomicBool::new(false));
        tokio::time::timeout_at(deadline, async {
            let result = target
                .dispatch
                .dispatch(OperationInvocation {
                    scope,
                    warm: target.warm,
                    operation,
                    context,
                    input,
                    execution,
                    trace: InvocationTrace::capture(),
                    reply,
                    retired: Arc::clone(&retired),
                })
                .await;
            if !retired.load(Ordering::SeqCst) {
                result.context("dispatch application operation")?;
            }
            response
                .await
                .context("operation dispatch returned no result")
        })
        .await
        .context("operation deadline elapsed")?
    }
}

#[async_trait::async_trait]
impl HostPlugin for WamnOperation {
    fn id(&self) -> &'static str {
        "wamn-operation"
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:operation/operation@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if let Some(config) = item.local_resources().config.get(APPLICATION_CONFIG) {
            let application = serde_json::from_str(config).context("decode wamn.application")?;
            let WorkloadItem::Component(component) = &*item else {
                anyhow::bail!("operation authority requires a route component");
            };
            self.bindings
                .write()
                .expect("operation bindings lock poisoned")
                .insert(
                    component.id().to_owned(),
                    Arc::new(Binding {
                        workload: component.workload_id().to_owned(),
                        application,
                        targets: OnceLock::new(),
                    }),
                );
        }
        item.add_plugin(self.host.id(), Arc::clone(&self.host) as _);
        self.host.on_workload_item_bind(item, interfaces).await
    }

    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        let binding = self
            .bindings
            .read()
            .expect("operation bindings lock poisoned")
            .get(component_id)
            .map(Arc::clone);
        let Some(binding) = binding else {
            return Ok(());
        };
        let components = workload.components();
        let components = components.read().await;
        let ids = binding
            .application
            .components
            .iter()
            .map(|component| {
                let id = components
                    .iter()
                    .find(|(_, sibling)| sibling.name() == component.name)
                    .map(|(id, _)| Arc::clone(id))
                    .context("configured application component missing from workload")?;
                Ok((component.name.clone(), id))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        drop(components);
        let mut targets = BTreeMap::new();
        for (name, id) in ids {
            targets.insert(
                name,
                ComponentTarget {
                    dispatch: workload.dispatch_target(&id, self.id()).await?,
                    warm: workload
                        .warm_instance_policy(&id)
                        .await
                        .keeps_instances_warm(),
                },
            );
        }
        binding
            .targets
            .set(targets)
            .map_err(|_| anyhow::anyhow!("operation targets already resolved"))
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.bindings
            .write()
            .expect("operation bindings lock poisoned")
            .retain(|_, binding| binding.workload != workload_id);
        self.host.on_workload_unbind(workload_id, interfaces).await
    }
}

struct OperationInvocation {
    scope: Arc<str>,
    warm: bool,
    operation: CanonicalName,
    context: node_types::NodeContext,
    input: String,
    execution: InvocationExecution,
    trace: InvocationTrace,
    reply: oneshot::Sender<Result<node_types::Emission, node_types::NodeError>>,
    retired: Arc<AtomicBool>,
}

/// Restore the native identity before an instance can serve another invocation.
struct ActiveScope<'a> {
    accessor: &'a Accessor<SharedCtx>,
    component: Arc<str>,
}

impl Drop for ActiveScope<'_> {
    fn drop(&mut self) {
        self.accessor.with(|mut access| {
            access.get().active_ctx.component_id = Arc::clone(&self.component);
        });
    }
}

impl GuestCall for OperationInvocation {
    fn describe(&self) -> &str {
        self.operation.as_str()
    }

    fn deadline(&self) -> Duration {
        self.execution
            .deadline()
            .saturating_duration_since(Instant::now())
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        let trace = self.trace.clone();
        Box::pin(trace.run(async move {
            let Self {
                scope,
                warm,
                operation,
                context,
                input,
                execution,
                reply,
                retired,
                ..
            } = *self;
            let component = accessor.with(|mut access| {
                std::mem::replace(&mut access.get().active_ctx.component_id, scope)
            });
            let _active = ActiveScope {
                accessor,
                component,
            };
            let run = accessor.with(|mut access| {
                let handler = instance
                    .get_export_index(&mut access, None, operation.as_str())
                    .context("component has no operation export")?;
                let export = instance
                    .get_export_index(&mut access, Some(&handler), "run")
                    .context("operation has no run entrypoint")?;
                instance
                    .get_func(&mut access, export)
                    .context("operation run is not a function")
            })?;
            let ty = accessor
                .with(|mut access| run.ty(&mut access).params().nth(1).map(|(_, ty)| ty))
                .context("operation has no input")?;
            let input = if ty == wash_runtime::wasmtime::component::Type::String {
                Ok(Val::String(input))
            } else {
                serde_json::from_str(&input)
                    .map_err(anyhow::Error::from)
                    .and_then(|value| json::input(&ty, &value))
            };
            let outcome = match input {
                Ok(input) => {
                    let arguments = [native_call::context_value(context), input];
                    let mut results = [Val::Bool(false)];
                    execution
                        .run(run.call_concurrent(accessor, &arguments, &mut results))
                        .await
                        .context("operation invocation ended")??;
                    json::emission(&results[0])?
                }
                Err(error) => Err(node_types::NodeError::InvalidInput(
                    node_types::ErrorDetail {
                        code: Some("invalid_input".into()),
                        message: error.to_string(),
                    },
                )),
            };
            let refused = outcome.is_err().then_some("handler");
            reply
                .send(outcome)
                .map_err(|_| anyhow::anyhow!("operation response abandoned"))?;
            if warm && accessor.with(|mut access| !access.get().table.is_empty()) {
                let resources = accessor.with(|mut access| std::mem::take(&mut access.get().table));
                drop(resources);
                retired.store(true, Ordering::SeqCst);
                anyhow::bail!("operation completed with retained resources; retire instance");
            }
            Ok(refused)
        }))
    }
}

static NEXT_TARGET: AtomicU64 = AtomicU64::new(0);

/// Allocate a process-local invocation scope under its component's identity.
pub fn next_scope(component: &str) -> Box<str> {
    format!(
        "{component}#{}",
        NEXT_TARGET.fetch_add(1, Ordering::Relaxed)
    )
    .into()
}
