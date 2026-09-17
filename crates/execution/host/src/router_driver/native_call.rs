//! Invoke one admitted node through native dispatch and owned WIT values.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout_at};
use wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller;
use wamn_runtime::plugins::invocation_trace::InvocationTrace;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::wasmtime::component::{Accessor, Instance, TypedFunc};

use super::native_policy::InvocationScope;
use super::native_workload::NativeApplication;
use super::{NodeAcquisition, node_types};

#[cfg(test)]
mod tests;

/// Preserve a host failure's Rust type across the native guest trap boundary.
pub(super) type NativeCallFailure = Arc<std::sync::Mutex<Option<anyhow::Error>>>;

/// The request authority and absolute deadline carried into one native node call.
pub(super) struct NativeInvocation {
    pub(super) operation: String,
    pub(super) context: node_types::NodeContext,
    pub(super) input: String,
    pub(super) deadline: Instant,
    pub(super) acquisition: NodeAcquisition,
    pub(super) caller: Option<AuthenticatedCaller>,
    pub(super) application: Arc<NativeApplication>,
}

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
    reply: oneshot::Sender<Result<node_types::Emission, node_types::NodeError>>,
    failure: NativeCallFailure,
    trace: InvocationTrace,
    retired_after_reply: Arc<AtomicBool>,
    scope: Arc<InvocationScope>,
}

/// Restore the native identity after the request's capability scope is revoked.
struct ActiveScope<'a> {
    accessor: &'a Accessor<SharedCtx>,
    component_id: Arc<str>,
}

impl Drop for ActiveScope<'_> {
    fn drop(&mut self) {
        self.accessor.with(|mut access| {
            access.get().active_ctx.component_id = Arc::clone(&self.component_id);
        });
    }
}

// This guard belongs to the dispatching caller, not the native guest task.
// Cancellation revokes authority before native's abandoned-store grace ends.
struct CloseScope(Arc<InvocationScope>);
impl Drop for CloseScope {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl GuestCall for NativeCall {
    fn describe(&self) -> &str {
        &self.request.operation
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
                failure,
                retired_after_reply,
                scope,
                ..
            } = *self;
            anyhow::ensure!(
                Instant::now() < request.deadline,
                "native-node-deadline-exceeded"
            );
            let native_id =
                accessor.with(|mut access| access.get().active_ctx.component_id.clone());
            let warm = request
                .application
                .workload
                .resolved
                .warm_instance_policy(&native_id)
                .await
                .keeps_instances_warm();
            let component_id = accessor.with(|mut access| {
                let active = &mut access.get().active_ctx;
                std::mem::replace(&mut active.component_id, Arc::from(scope.id.as_ref()))
            });
            let active_scope = ActiveScope {
                accessor,
                component_id,
            };
            // Declared after active_scope, so authority is revoked before the
            // native component identity is restored, including cancellation.
            let _authority = request
                .application
                .policy
                .activate(
                    &active_scope.component_id,
                    &scope,
                    &request,
                    Arc::clone(&failure),
                )
                .await?;
            let run: TypedFunc<
                (node_types::NodeContext, String),
                (Result<node_types::Emission, node_types::NodeError>,),
            > = accessor.with(|mut access| {
                let handler = instance
                    .get_export_index(&mut access, None, &request.operation)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "component has no exported operation {:?}",
                            request.operation
                        )
                    })?;
                let run = instance
                    .get_export_index(&mut access, Some(&handler), "run")
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "operation {:?} has no handler.run export",
                            request.operation
                        )
                    })?;
                instance.get_typed_func(&mut access, run).map_err(|error| {
                    anyhow::anyhow!(
                        "operation {:?} handler.run has wrong type: {error}",
                        request.operation
                    )
                })
            })?;
            anyhow::ensure!(
                Instant::now() < request.deadline,
                "native-node-deadline-exceeded"
            );
            // call_concurrent owns its parameters and performs post-return.
            // The unmoved application field stays owned by this call future
            // while native abandonment stops a cancelled caller's guest.
            let result = tokio::select! {
                biased;
                () = scope.cancelled() => anyhow::bail!("native invocation was cancelled"),
                result = run.call_concurrent(accessor, (request.context, request.input)) => result,
            };
            let (outcome,) = result.map_err(anyhow::Error::from).with_context(|| {
                format!("operation {:?} handler.run trapped", request.operation)
            })?;
            let refused = outcome.is_err().then_some("handler");
            reply
                .send(outcome)
                .map_err(|_| anyhow::anyhow!("native-node-response-abandoned"))?;
            if warm && accessor.with(|mut access| !access.get().table.is_empty()) {
                // Deleting table entries and then reusing this store can alias a
                // retained guest handle to a later invocation's resource. Native
                // retirement discards the whole instance before accepting more work.
                let resources = accessor.with(|mut access| std::mem::take(&mut access.get().table));
                drop(resources);
                retired_after_reply.store(true, Ordering::SeqCst);
                anyhow::bail!(
                    "native invocation completed with retained resources; retire instance"
                );
            }
            Ok(refused)
        }))
    }
}

/// Native initialization for readiness, without running an application handler.
struct ReadinessCall {
    deadline: Instant,
}

impl GuestCall for ReadinessCall {
    fn describe(&self) -> &'static str {
        "readiness"
    }

    fn deadline(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    fn call(
        self: Box<Self>,
        _accessor: &Accessor<SharedCtx>,
        _instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

/// Instantiate through native dispatch under a bound, without invocation authority.
pub(super) async fn prepare_native(
    target: &DispatchTarget,
    deadline: Instant,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        Instant::now() < deadline,
        "native-readiness-deadline-exceeded"
    );
    timeout_at(deadline, target.dispatch(ReadinessCall { deadline }))
        .await
        .context("native readiness enclosing deadline elapsed")?
        .context("initialize native component for readiness")
}

/// Dispatch and receive a typed node result under the same enclosing deadline.
pub(super) async fn invoke_native(
    target: &DispatchTarget,
    request: NativeInvocation,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    let deadline = request.deadline;
    anyhow::ensure!(Instant::now() < deadline, "native-node-deadline-exceeded");
    let (reply, response) = oneshot::channel();
    let failure = NativeCallFailure::default();
    let retired_after_reply = Arc::new(AtomicBool::new(false));
    let scope = Arc::new(InvocationScope::new(Arc::clone(
        &request.application.policy,
    )));
    let _close = CloseScope(Arc::clone(&scope));
    timeout_at(deadline, async move {
        // Await native completion first: an initialization or guest failure
        // must not be replaced by the resulting closed response channel.
        if let Err(native_error) = target
            .dispatch(NativeCall {
                request,
                reply,
                failure: Arc::clone(&failure),
                // Native starts a separate task. Carry the existing invocation
                // span and subscriber into its GuestCall instead of creating
                // another invocation span or relying on executor-local state.
                trace: InvocationTrace::capture(),
                retired_after_reply: Arc::clone(&retired_after_reply),
                scope,
            })
            .await
        {
            if retired_after_reply.load(Ordering::SeqCst) {
                return response
                    .await
                    .context("retired native invocation lost its completed response");
            }
            // Only host policy writes this request-owned error, before it
            // traps. Guest text cannot supply an error's Rust classification.
            let host_error = failure
                .lock()
                .expect("native call failure lock poisoned")
                .take();
            return Err(match host_error {
                Some(error) => {
                    error.context(format!("dispatch native node operation: {native_error:#}"))
                }
                None => native_error.context("dispatch native node operation"),
            });
        }
        response
            .await
            .context("native node dispatch completed without a response")
    })
    .await
    .context("native node enclosing deadline elapsed")?
}
