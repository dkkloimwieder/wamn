//! Invoke one admitted node through native dispatch and owned WIT values.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout_at};
use tracing::Instrument as _;
use wamn_engine::invocation_scope::InvocationExecution;
use wamn_engine::invocation_trace::InvocationTrace;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::wasmtime::component::{Accessor, Instance, Val};

use crate::policy::EdgeFacts;

use super::native_workload::NativeApp;
use super::node_types;

/// The request authority and absolute deadline carried into one native node call.
pub struct NativeInvocation {
    pub operation: wamn_package::canonical_name::CanonicalName,
    pub context: node_types::NodeContext,
    pub input: NativeInput,
    pub deadline: Instant,
    /// The facts of this call that only the policy reads.
    pub facts: EdgeFacts,
    pub app: Arc<NativeApp>,
}

/// JSON exists only at dynamic routing. Nested known calls retain WIT values.
#[derive(Debug)]
pub enum NativeInput {
    Encoded(String),
}

impl From<String> for NativeInput {
    fn from(value: String) -> Self {
        Self::Encoded(value)
    }
}

impl From<&str> for NativeInput {
    fn from(value: &str) -> Self {
        Self::Encoded(value.to_owned())
    }
}

pub use wamn_engine::operation::native_call::NativeOutcome;

impl fmt::Debug for NativeInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeInvocation")
            .field("operation", &self.operation)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

struct NativeCall {
    request: NativeInvocation,
    reply: oneshot::Sender<NativeOutcome>,
    trace: InvocationTrace,
    retired_after_reply: Arc<AtomicBool>,
    scope_id: Box<str>,
    execution: InvocationExecution,
    /// `wamn.operation.instantiate`: open from the dispatch until the call
    /// begins, so it holds the store creation, the instantiation and the
    /// wait for an instance.
    instantiate: tracing::Span,
}

/// Restore the native identity after the request's invocation scope is revoked.
struct ActiveScope<'a> {
    accessor: &'a Accessor<SharedCtx>,
    component_id: Arc<str>,
    execution: &'a InvocationExecution,
}

impl Drop for ActiveScope<'_> {
    fn drop(&mut self) {
        self.execution.cancel();
        self.accessor.with(|mut access| {
            access.get().active_ctx.component_id = Arc::clone(&self.component_id);
        });
    }
}

// End host work when the dispatching caller exits, including during cancellation.
struct CloseScope(InvocationExecution);

impl Drop for CloseScope {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl GuestCall for NativeCall {
    fn describe(&self) -> &str {
        self.request.operation.as_str()
    }

    fn deadline(&self) -> Duration {
        self.request
            .deadline
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
                request,
                reply,
                retired_after_reply,
                scope_id,
                execution,
                instantiate,
                ..
            } = *self;
            drop(instantiate);
            // The guest's host calls parent to this span, because the policy
            // binds the current span when it activates the scope.
            async move {
                anyhow::ensure!(
                    Instant::now() < request.deadline,
                    "native-node-deadline-exceeded"
                );
                let native_id =
                    accessor.with(|mut access| Arc::clone(&access.get().active_ctx.component_id));
                let warm = request
                    .app
                    .workload
                    .warm_instance_policy(&native_id)
                    .await?
                    .keeps_instances_warm();
                let component_id = accessor.with(|mut access| {
                    let active = &mut access.get().active_ctx;
                    std::mem::replace(&mut active.component_id, Arc::from(scope_id.as_ref()))
                });
                let active_scope = ActiveScope {
                    accessor,
                    component_id,
                    execution: &execution,
                };
                request
                    .app
                    .policy
                    .activate(&active_scope.component_id, &request)?;
                accessor.with(|mut access| {
                    if let Ok(scopes) = access
                        .get()
                        .active_ctx
                        .try_get_plugin::<wamn_engine::invocation_scope::InvocationScopes>(
                        wamn_engine::invocation_scope::INVOCATION_SCOPES_ID,
                    ) {
                        scopes.set_execution(&scope_id, execution.clone())?;
                    }
                    Ok::<_, anyhow::Error>(())
                })?;
                let run = accessor.with(|mut access| {
                    let handler = instance
                        .get_export_index(&mut access, None, request.operation.as_str())
                        .context("component has no admitted operation export")?;
                    let export = instance
                        .get_export_index(&mut access, Some(&handler), "run")
                        .context("operation has no typed entrypoint")?;
                    instance
                        .get_func(&mut access, export)
                        .context("typed entrypoint is not a function")
                })?;
                let input = match request.input {
                    NativeInput::Encoded(input) => {
                        let ty = accessor
                            .with(|mut access| {
                                run.ty(&mut access).params().nth(1).map(|(_, ty)| ty)
                            })
                            .context("operation has no input")?;
                        if ty == wash_runtime::wasmtime::component::Type::String {
                            Ok(Val::String(input))
                        } else {
                            serde_json::from_str(&input)
                                .map_err(anyhow::Error::from)
                                .and_then(|value| super::json::input(&ty, &value))
                        }
                    }
                };
                let outcome = match input {
                    Ok(input) => {
                        let arguments = [context_value(request.context), input];
                        let mut results = [Val::Bool(false)];
                        execution
                            .run(run.call_concurrent(accessor, &arguments, &mut results))
                            .await
                            .context("native invocation ended")?
                            .map_err(anyhow::Error::from)
                            .context("typed operation trapped")?;
                        NativeOutcome::Json(super::json::emission(&results[0])?)
                    }
                    Err(error) => NativeOutcome::Json(Err(node_types::NodeError::InvalidInput(
                        node_types::ErrorDetail {
                            code: Some("invalid_input".into()),
                            message: error.to_string(),
                        },
                    ))),
                };
                let refused = match &outcome {
                    NativeOutcome::Json(value) => value.is_err(),
                }
                .then_some("handler");
                reply
                    .send(outcome)
                    .map_err(|_| anyhow::anyhow!("native-node-response-abandoned"))?;
                if warm && accessor.with(|mut access| !access.get().table.is_empty()) {
                    // Deleting table entries and then reusing this store can alias a
                    // retained guest handle to a later invocation's resource. Native
                    // retirement discards the whole instance before accepting more work.
                    let resources =
                        accessor.with(|mut access| std::mem::take(&mut access.get().table));
                    drop(resources);
                    retired_after_reply.store(true, Ordering::SeqCst);
                    anyhow::bail!(
                        "native invocation completed with retained resources; retire instance"
                    );
                }
                Ok(refused)
            }
            .instrument(tracing::info_span!("wamn.operation.call"))
            .await
        }))
    }
}

/// Dispatch and receive a typed node result under the same enclosing deadline.
pub async fn invoke_owned(
    target: &DispatchTarget,
    request: NativeInvocation,
) -> anyhow::Result<NativeOutcome> {
    invoke_raw(target, request).await
}

/// The operation owner persists the raw result before it signs the response.
pub async fn invoke_raw(
    target: &DispatchTarget,
    mut request: NativeInvocation,
) -> anyhow::Result<NativeOutcome> {
    let deadline = request
        .deadline
        .min(Instant::now() + wamn_app::MAX_HOST_CALL_DURATION);
    request.deadline = deadline;
    anyhow::ensure!(Instant::now() < deadline, "native-node-deadline-exceeded");
    request
        .app
        .workload
        .target_component(target)
        .context("dispatch target is outside the application")?;
    let (reply, response) = oneshot::channel();
    let retired_after_reply = Arc::new(AtomicBool::new(false));
    let execution = InvocationExecution::new(deadline);
    let _close = CloseScope(execution.clone());
    timeout_at(deadline, async move {
        // Await native completion first: an initialization or guest failure
        // must not be replaced by the resulting closed response channel.
        if let Err(native_error) = target
            .dispatch(NativeCall {
                request,
                reply,
                // Native starts a separate task. Carry the existing invocation
                // span and subscriber into its GuestCall instead of creating
                // another invocation span or relying on executor-local state.
                trace: InvocationTrace::capture(),
                retired_after_reply: Arc::clone(&retired_after_reply),
                scope_id: super::next_scope("native-invocation"),
                execution,
                instantiate: tracing::info_span!("wamn.operation.instantiate"),
            })
            .await
        {
            if retired_after_reply.load(Ordering::SeqCst) {
                return response
                    .await
                    .context("retired native invocation lost its completed response");
            }
            return Err(native_error.context("dispatch native node operation"));
        }
        response
            .await
            .context("native node dispatch completed without a response")
    })
    .await
    .context("native node enclosing deadline elapsed")?
}

/// Invoke the JSON adapter at an HTTP or dynamic routing boundary.
pub async fn invoke_native(
    target: &DispatchTarget,
    request: NativeInvocation,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    invoke_owned(target, request).await?.into_json()
}

use wamn_engine::operation::native_call::context_value;
