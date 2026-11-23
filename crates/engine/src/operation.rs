//! Dispatch application operations through the workload's resolved component targets.

use crate::application::bind_application_config;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::oneshot;
use tokio::time::Instant;
use wamn_application::application_config::ApplicationConfig;
use wamn_application::native_routes::bounded_node_deadline_ms;
use wamn_application::result_signing::ResultSigningError;
use wamn_package::canonical_name::CanonicalName;
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

impl From<ResultSigningError> for node_types::NodeError {
    fn from(ResultSigningError { code, message }: ResultSigningError) -> Self {
        Self::Terminal(node_types::ErrorDetail { message, code })
    }
}

/// Host plugin that retains application targets; SQL owns the operation interface.
pub struct WamnOperation {
    host: Arc<dyn HostPlugin>,
    scopes: Arc<InvocationScopes>,
    bindings: RwLock<HashMap<String, Arc<Binding>>>,
}

struct Binding {
    workload: String,
    ingress: bool,
    application: Arc<ApplicationConfig>,
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
    pub fn new(host: Arc<dyn HostPlugin>, scopes: Arc<InvocationScopes>) -> Self {
        Self {
            host,
            scopes,
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
        let binding = self.binding(component)?;
        let serving = binding
            .application
            .components
            .iter()
            .find_map(|component| component.operations.get(&operation))
            .context("operation is not configured")?;
        binding
            .invoke(&serving.component, scopes, scope, operation, context, input)
            .await
    }

    /// Invoke the operation on the exact configured sibling.
    #[expect(
        clippy::too_many_arguments,
        reason = "Dispatch needs both the route and sibling identities"
    )]
    pub async fn invoke_sibling(
        &self,
        route: &str,
        sibling: &str,
        scopes: &InvocationScopes,
        scope: Arc<str>,
        operation: CanonicalName,
        context: node_types::NodeContext,
        input: String,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let binding = self.binding(route)?;
        binding
            .invoke(sibling, scopes, scope, operation, context, input)
            .await
    }

    /// Verify the active component is the resolved ingress of this workload.
    pub fn is_ingress_caller(&self, active: &str, workload: &str) -> bool {
        self.binding(active).is_ok_and(|binding| {
            binding.ingress
                && binding
                    .targets
                    .get()
                    .and_then(BTreeMap::first_key_value)
                    .is_some_and(|(_, target)| target.dispatch.workload().id() == workload)
        })
    }

    /// Deliver one event to the exact sibling under the admitted invocation scope.
    pub async fn dispatch_data_change(
        &self,
        ingress: &str,
        sibling: &str,
        scopes: &InvocationScopes,
        scope: Arc<str>,
        event: String,
    ) -> anyhow::Result<Result<(), String>> {
        let binding = self.binding(ingress)?;
        let deadline = Instant::now()
            + Duration::from_millis(bounded_node_deadline_ms(None));
        binding
            .target(sibling)?
            .call(scopes, scope, deadline, move |call| DataChangeInvocation {
                call,
                event,
            })
            .await
    }

    fn binding(&self, route: &str) -> anyhow::Result<Arc<Binding>> {
        self.bindings
            .read()
            .expect("operation bindings lock poisoned")
            .get(route)
            .map(Arc::clone)
            .context("operation binding missing")
    }
}

impl Binding {
    async fn invoke(
        &self,
        sibling: &str,
        scopes: &InvocationScopes,
        scope: Arc<str>,
        operation: CanonicalName,
        context: node_types::NodeContext,
        input: String,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let deadline = Instant::now()
            + Duration::from_millis(bounded_node_deadline_ms(
                context.deadline_ms,
            ));
        self.target(sibling)?
            .call(scopes, scope, deadline, move |call| OperationInvocation {
                call,
                operation,
                context,
                input,
            })
            .await
    }

    fn target(&self, sibling: &str) -> anyhow::Result<&ComponentTarget> {
        self.targets
            .get()
            .and_then(|targets| targets.get(sibling))
            .context("operation dispatch target missing")
    }
}

impl ComponentTarget {
    async fn call<T: Send + 'static, C: GuestCall>(
        &self,
        scopes: &InvocationScopes,
        scope: Arc<str>,
        deadline: Instant,
        guest_call: impl FnOnce(ScopedCall<T>) -> C,
    ) -> anyhow::Result<T> {
        scopes.set_execution(&scope, InvocationExecution::new(deadline))?;
        let execution = scopes
            .execution(&scope)
            .context("operation scope missing")?;
        let deadline = execution.deadline();
        let (reply, response) = oneshot::channel();
        let retired = Arc::new(AtomicBool::new(false));
        let call = guest_call(ScopedCall {
            scope,
            execution,
            trace: InvocationTrace::capture(),
            result: CallResult {
                warm: self.warm,
                reply,
                retired: Arc::clone(&retired),
            },
        });
        tokio::time::timeout_at(deadline, async {
            let result = self.dispatch.dispatch(call).await;
            if !retired.load(Ordering::SeqCst) {
                result.context("dispatch application component")?;
            }
            response
                .await
                .context("component dispatch returned no result")
        })
        .await
        .context("component dispatch deadline elapsed")?
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
        if let Some(application) =
            bind_application_config(&self.scopes, item).context("decode wamn.application")?
        {
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
                        ingress: component.name() == "ingress",
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

struct ScopedCall<T> {
    scope: Arc<str>,
    execution: InvocationExecution,
    trace: InvocationTrace,
    result: CallResult<T>,
}

struct CallResult<T> {
    warm: bool,
    reply: oneshot::Sender<T>,
    retired: Arc<AtomicBool>,
}

impl<T> CallResult<T> {
    fn send(self, accessor: &Accessor<SharedCtx>, answer: T) -> anyhow::Result<()> {
        self.reply
            .send(answer)
            .map_err(|_| anyhow::anyhow!("guest response abandoned"))?;
        if self.warm && accessor.with(|mut access| !access.get().table.is_empty()) {
            let resources = accessor.with(|mut access| std::mem::take(&mut access.get().table));
            drop(resources);
            self.retired.store(true, Ordering::SeqCst);
            anyhow::bail!("guest completed with retained resources; retire instance");
        }
        Ok(())
    }
}

struct OperationInvocation {
    call: ScopedCall<Result<node_types::Emission, node_types::NodeError>>,
    operation: CanonicalName,
    context: node_types::NodeContext,
    input: String,
}

struct DataChangeInvocation {
    call: ScopedCall<Result<(), String>>,
    event: String,
}

/// Restore the native identity before an instance can serve another invocation.
struct ActiveScope<'a> {
    accessor: &'a Accessor<SharedCtx>,
    component: Arc<str>,
}

impl<'a> ActiveScope<'a> {
    fn enter(accessor: &'a Accessor<SharedCtx>, scope: Arc<str>) -> Self {
        let component = accessor
            .with(|mut access| std::mem::replace(&mut access.get().active_ctx.component_id, scope));
        Self {
            accessor,
            component,
        }
    }
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
        self.call
            .execution
            .deadline()
            .saturating_duration_since(Instant::now())
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        let Self {
            call,
            operation,
            context,
            input,
        } = *self;
        let ScopedCall {
            scope,
            execution,
            trace,
            result,
        } = call;
        Box::pin(trace.run(async move {
            let _active = ActiveScope::enter(accessor, scope);
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
            result.send(accessor, outcome)?;
            Ok(refused)
        }))
    }
}

impl GuestCall for DataChangeInvocation {
    fn describe(&self) -> &str {
        wamn_package::DATA_CHANGE_EXPORT
    }

    fn deadline(&self) -> Duration {
        self.call
            .execution
            .deadline()
            .saturating_duration_since(Instant::now())
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        let Self { call, event } = *self;
        let ScopedCall {
            scope,
            execution,
            trace,
            result,
        } = call;
        Box::pin(trace.run(async move {
            let _active = ActiveScope::enter(accessor, scope);
            let handle = accessor.with(|mut access| {
                let interface = instance
                    .get_export_index(&mut access, None, wamn_package::DATA_CHANGE_EXPORT)
                    .context("component has no data change export")?;
                let export = instance
                    .get_export_index(&mut access, Some(&interface), "handle")
                    .context("data change interface has no handle entrypoint")?;
                instance
                    .get_typed_func::<(String,), (Result<(), String>,)>(&mut access, export)
                    .map_err(anyhow::Error::from)
                    .context("data change handle has the wrong signature")
            })?;
            let (answer,) = execution
                .run(handle.call_concurrent(accessor, (event,)))
                .await
                .context("data change invocation ended")??;
            let refused = answer.is_err().then_some("handler");
            result.send(accessor, answer)?;
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
