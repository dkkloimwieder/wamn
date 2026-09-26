//! Invoke one admitted node through native dispatch and owned WIT values.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::invocation_trace::InvocationTrace;
use anyhow::Context as _;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, timeout_at};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::wasmtime::StoreContextMut;
use wash_runtime::wasmtime::component::{
    Accessor, ComponentExportIndex, FutureConsumer, FutureReader, Instance, Source, StreamConsumer,
    StreamReader, StreamResult, TypedFunc, Val,
};

use super::invocation_policy::{InvocationPolicy, InvocationScope};
use super::native_workload::NativeApplication;
use super::node_types;

#[cfg(test)]
mod tests;

/// Preserve a host failure's Rust type across the native guest trap boundary.
pub type NativeCallFailure = Arc<std::sync::Mutex<Option<anyhow::Error>>>;

/// The request authority and absolute deadline carried into one native node call.
pub struct NativeInvocation<P: InvocationPolicy> {
    pub operation: String,
    pub context: node_types::NodeContext,
    pub input: NativeInput,
    pub deadline: Instant,
    /// The facts of this call that only the policy reads.
    pub facts: P::Facts,
    pub application: Arc<NativeApplication<P>>,
}

/// Each batch of rows a streamed query writes, as one JSON object per row.
pub type RowBatches = mpsc::Sender<Vec<String>>;

/// JSON exists only at dynamic routing. Nested known calls retain WIT values.
#[derive(Debug)]
pub enum NativeInput {
    Json(String),
    Typed(Val),
    /// A query's JSON request. The call hands each batch of rows to `rows` as
    /// the component writes it, and stays open until the component writes the
    /// outcome, so the request's authority covers every row it reads.
    Stream {
        input: String,
        rows: RowBatches,
    },
}

impl From<String> for NativeInput {
    fn from(value: String) -> Self {
        Self::Json(value)
    }
}

impl From<&str> for NativeInput {
    fn from(value: &str) -> Self {
        Self::Json(value.to_owned())
    }
}

#[derive(Debug)]
pub enum NativeOutcome {
    Json(Result<node_types::Emission, node_types::NodeError>),
    Typed(Val),
    /// A streamed query's outcome JSON, written after its last row.
    Stream(Result<String, node_types::NodeError>),
}

impl NativeOutcome {
    pub fn into_json(self) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        match self {
            Self::Json(value) => Ok(value),
            Self::Typed(_) => anyhow::bail!("typed operation returned to a JSON-only caller"),
            Self::Stream(_) => anyhow::bail!("streamed operation returned to a JSON-only caller"),
        }
    }
}

impl<P: InvocationPolicy> fmt::Debug for NativeInvocation<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeInvocation")
            .field("operation", &self.operation)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

struct NativeCall<P: InvocationPolicy> {
    request: NativeInvocation<P>,
    reply: oneshot::Sender<NativeOutcome>,
    failure: NativeCallFailure,
    trace: InvocationTrace,
    retired_after_reply: Arc<AtomicBool>,
    scope: Arc<InvocationScope<P>>,
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
struct CloseScope<P: InvocationPolicy>(Arc<InvocationScope<P>>);
impl<P: InvocationPolicy> Drop for CloseScope<P> {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl<P: InvocationPolicy> GuestCall for NativeCall<P> {
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
                .warm_instance_policy(&native_id)
                .await?
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
            let outcome =
                match request.input {
                    NativeInput::Json(input) => {
                        let (entry, whole) = accessor.with(|mut access| {
                            let handler = instance
                                .get_export_index(&mut access, None, &request.operation)
                                .context("component has no admitted operation export")?;
                            // Generated adapters own JSON at the dynamic routing boundary.
                            let entry = instance
                                .get_export_index(&mut access, Some(&handler), "run-json")
                                .or_else(|| {
                                    instance.get_export_index(&mut access, Some(&handler), "run")
                                })
                                .context("operation has no dynamic entrypoint")?;
                            let whole: Result<WholeEntry, _> =
                                instance.get_typed_func(&mut access, entry);
                            Ok::<_, anyhow::Error>((entry, whole))
                        })?;
                        if let Ok(run) = whole {
                            let (outcome,) = tokio::select! {
                            biased;
                            () = scope.cancelled() => anyhow::bail!("native invocation was cancelled"),
                            result = run.call_concurrent(accessor, (request.context, input)) => result,
                        }.map_err(anyhow::Error::from).context("dynamic operation trapped")?;
                            NativeOutcome::Json(outcome)
                        } else {
                            // A query streams its rows. A caller that reads one
                            // outcome gets them gathered into one page.
                            let (rows, mut batches) = mpsc::channel(PAGE_BATCHES_IN_FLIGHT);
                            let gathered = async {
                                let mut item = Vec::new();
                                while let Some(batch) = batches.recv().await {
                                    item.extend(batch);
                                }
                                item
                            };
                            let (outcome, item) = tokio::join!(
                                run_streamed(
                                    accessor,
                                    instance,
                                    entry,
                                    &scope,
                                    request.context,
                                    input,
                                    rows
                                ),
                                gathered
                            );
                            NativeOutcome::Json(outcome?.map(|outcome| node_types::Emission {
                                payload: page_outcome(&outcome, &item),
                                port: None,
                            }))
                        }
                    }
                    NativeInput::Typed(input) => {
                        let run = accessor.with(|mut access| {
                            let handler = instance
                                .get_export_index(&mut access, None, &request.operation)
                                .context("component has no admitted operation export")?;
                            let run = instance
                                .get_export_index(&mut access, Some(&handler), "run")
                                .context("operation has no typed entrypoint")?;
                            instance
                                .get_func(&mut access, run)
                                .context("typed entrypoint is not a function")
                        })?;
                        let arguments = [context_value(request.context), input];
                        let mut results = [Val::Bool(false)];
                        tokio::select! {
                        biased;
                        () = scope.cancelled() => anyhow::bail!("native invocation was cancelled"),
                        result = run.call_concurrent(accessor, &arguments, &mut results) => result,
                    }.map_err(anyhow::Error::from).context("typed operation trapped")?;
                        NativeOutcome::Typed(
                            results
                                .into_iter()
                                .next()
                                .expect("one typed operation result"),
                        )
                    }
                    NativeInput::Stream { input, rows } => {
                        let entry = accessor.with(|mut access| {
                            let handler = instance
                                .get_export_index(&mut access, None, &request.operation)
                                .context("component has no admitted operation export")?;
                            instance
                                .get_export_index(&mut access, Some(&handler), "run-json")
                                .context("streamed operation has no JSON entrypoint")
                        })?;
                        NativeOutcome::Stream(
                            run_streamed(
                                accessor,
                                instance,
                                entry,
                                &scope,
                                request.context,
                                input,
                                rows,
                            )
                            .await?,
                        )
                    }
                };
            let refused = match &outcome {
                NativeOutcome::Json(value) => value.is_err(),
                NativeOutcome::Typed(value) => matches!(value, Val::Result(Err(_))),
                NativeOutcome::Stream(value) => value.is_err(),
            }
            .then_some("handler");
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

/// The JSON adapter of an operation that returns one whole outcome.
type WholeEntry = TypedFunc<
    (node_types::NodeContext, String),
    (Result<node_types::Emission, node_types::NodeError>,),
>;

/// The JSON adapter of a query: its row stream and its outcome future.
type StreamedEntry = TypedFunc<
    (node_types::NodeContext, String),
    (Result<(StreamReader<String>, FutureReader<String>), node_types::NodeError>,),
>;

/// Batches a gathered page holds between the component and its outcome.
const PAGE_BATCHES_IN_FLIGHT: usize = 4;

/// Call a query's JSON adapter, hand each batch of its rows to `rows`, and
/// return the outcome JSON it writes after the last row.
async fn run_streamed<P: InvocationPolicy>(
    accessor: &Accessor<SharedCtx>,
    instance: Instance,
    entry: ComponentExportIndex,
    scope: &InvocationScope<P>,
    context: node_types::NodeContext,
    input: String,
    rows: RowBatches,
) -> anyhow::Result<Result<String, node_types::NodeError>> {
    let run: StreamedEntry = accessor.with(|mut access| {
        instance
            .get_typed_func(&mut access, entry)
            .map_err(anyhow::Error::from)
    })?;
    let (returned,) = tokio::select! {
        biased;
        () = scope.cancelled() => anyhow::bail!("native invocation was cancelled"),
        result = run.call_concurrent(accessor, (context, input)) => result,
    }
    .map_err(anyhow::Error::from)
    .context("streamed operation trapped")?;
    let (stream, end) = match returned {
        Ok(returned) => returned,
        Err(error) => return Ok(Err(error)),
    };
    let (ended, outcome) = oneshot::channel();
    accessor.with(|mut access| {
        stream.pipe(&mut access, RowForwarder::new(rows))?;
        end.pipe(&mut access, OutcomeForwarder(Some(ended)))
    })?;
    let outcome = tokio::select! {
        biased;
        () = scope.cancelled() => anyhow::bail!("native invocation was cancelled"),
        outcome = outcome => outcome,
    };
    Ok(Ok(outcome.context("streamed operation wrote no outcome")?))
}

/// The page outcome list of a query: its rows and the cursor after them when
/// the read ended, or the error when it did not.
pub fn page_outcome(outcome: &str, item: &[String]) -> String {
    let Ok(serde_json::Value::Object(outcome)) = serde_json::from_str(outcome) else {
        return serde_json::json!([{ "error": { "code": "internal_error", "detail": {} } }])
            .to_string();
    };
    match outcome.get("value") {
        Some(value) => format!(
            "[{{\"value\":{{\"item\":[{}],\"next_cursor\":{}}}}}]",
            item.join(","),
            value.get("next_cursor").unwrap_or(&serde_json::Value::Null)
        ),
        None => serde_json::Value::Array(vec![serde_json::Value::Object(outcome)]).to_string(),
    }
}

/// The host's read end of a query's rows. Each write of the component becomes
/// one batch. A batch waits for room in the channel, so a slow reader slows
/// the component, and a reader that left drops the stream.
struct RowForwarder {
    rows: Option<RowBatches>,
    reserve: Option<ReserveFuture>,
}

type ReserveFuture = Pin<
    Box<
        dyn Future<Output = Result<mpsc::OwnedPermit<Vec<String>>, mpsc::error::SendError<()>>>
            + Send,
    >,
>;

impl RowForwarder {
    fn new(rows: RowBatches) -> Self {
        Self {
            rows: Some(rows),
            reserve: None,
        }
    }
}

impl<D> StreamConsumer<D> for RowForwarder {
    type Item = String;

    fn poll_consume(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<D>,
        mut source: Source<'_, Self::Item>,
        finish: bool,
    ) -> Poll<wash_runtime::wasmtime::Result<StreamResult>> {
        let this = self.get_mut();
        let Some(rows) = this.rows.as_ref() else {
            return Poll::Ready(Ok(StreamResult::Dropped));
        };
        let reserve = this
            .reserve
            .get_or_insert_with(|| Box::pin(rows.clone().reserve_owned()));
        let permit = match reserve.as_mut().poll(cx) {
            Poll::Ready(Ok(permit)) => permit,
            Poll::Ready(Err(_)) => {
                this.reserve = None;
                this.rows = None;
                return Poll::Ready(Ok(StreamResult::Dropped));
            }
            Poll::Pending if finish => {
                this.reserve = None;
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            Poll::Pending => return Poll::Pending,
        };
        this.reserve = None;
        let mut batch = Vec::with_capacity(source.remaining(&mut store));
        source.read(&mut store, &mut batch)?;
        if !batch.is_empty() {
            permit.send(batch);
        }
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

/// The host's read end of a query's outcome.
struct OutcomeForwarder(Option<oneshot::Sender<String>>);

impl<D> FutureConsumer<D> for OutcomeForwarder {
    type Item = String;

    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        store: StoreContextMut<D>,
        mut source: Source<'_, Self::Item>,
        _finish: bool,
    ) -> Poll<wash_runtime::wasmtime::Result<()>> {
        let mut outcome = None;
        source.read(store, &mut outcome)?;
        if let (Some(outcome), Some(sender)) = (outcome, self.get_mut().0.take()) {
            let _ = sender.send(outcome);
        }
        Poll::Ready(Ok(()))
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
pub async fn prepare_native(target: &DispatchTarget, deadline: Instant) -> anyhow::Result<()> {
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
pub async fn invoke_owned<P: InvocationPolicy>(
    target: &DispatchTarget,
    request: NativeInvocation<P>,
) -> anyhow::Result<NativeOutcome> {
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

/// Invoke a query's JSON adapter and hand its rows to `rows` as they arrive.
/// The result is the outcome JSON the component wrote after its last row.
pub async fn invoke_native_stream<P: InvocationPolicy>(
    target: &DispatchTarget,
    request: NativeInvocation<P>,
) -> anyhow::Result<Result<String, node_types::NodeError>> {
    match invoke_owned(target, request).await? {
        NativeOutcome::Stream(outcome) => Ok(outcome),
        _ => anyhow::bail!("a streamed call returned a whole outcome"),
    }
}

/// Invoke the JSON adapter at an HTTP or dynamic routing boundary.
pub async fn invoke_native<P: InvocationPolicy>(
    target: &DispatchTarget,
    request: NativeInvocation<P>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    invoke_owned(target, request).await?.into_json()
}

fn context_value(context: node_types::NodeContext) -> Val {
    fn optional_string(value: Option<String>) -> Val {
        Val::Option(value.map(|value| Box::new(Val::String(value))))
    }
    Val::Record(vec![
        ("wiring-id".into(), Val::String(context.wiring_id)),
        ("wiring-version".into(), Val::U32(context.wiring_version)),
        ("node-id".into(), Val::String(context.node_id)),
        ("delivery-id".into(), Val::String(context.delivery_id)),
        ("input-port".into(), optional_string(context.input_port)),
        ("occurrence".into(), Val::U32(context.occurrence)),
        ("traceparent".into(), optional_string(context.traceparent)),
        ("tracestate".into(), optional_string(context.tracestate)),
        (
            "deadline-ms".into(),
            Val::Option(context.deadline_ms.map(|value| Box::new(Val::U64(value)))),
        ),
        ("config".into(), Val::String(context.config)),
    ])
}

/// Read the same context record that admission compares with the node contract.
pub fn typed_context(value: &Val) -> anyhow::Result<node_types::NodeContext> {
    let Val::Record(fields) = value else {
        anyhow::bail!("typed operation context is not a record");
    };
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
            .with_context(|| format!("typed operation context lacks {name}"))
    };
    let string = |name: &str| match field(name)? {
        Val::String(value) => Ok(value.clone()),
        _ => anyhow::bail!("typed context {name} is not a string"),
    };
    let u32_field = |name: &str| match field(name)? {
        Val::U32(value) => Ok(*value),
        _ => anyhow::bail!("typed context {name} is not a u32"),
    };
    let optional_string = |name: &str| match field(name)? {
        Val::Option(None) => Ok(None),
        Val::Option(Some(value)) => match value.as_ref() {
            Val::String(value) => Ok(Some(value.clone())),
            _ => anyhow::bail!("typed context {name} is not an optional string"),
        },
        _ => anyhow::bail!("typed context {name} is not optional"),
    };
    let deadline_ms = match field("deadline-ms")? {
        Val::Option(None) => None,
        Val::Option(Some(value)) => match value.as_ref() {
            Val::U64(value) => Some(*value),
            _ => anyhow::bail!("typed context deadline is not a u64"),
        },
        _ => anyhow::bail!("typed context deadline is not optional"),
    };
    Ok(node_types::NodeContext {
        wiring_id: string("wiring-id")?,
        wiring_version: u32_field("wiring-version")?,
        node_id: string("node-id")?,
        delivery_id: string("delivery-id")?,
        input_port: optional_string("input-port")?,
        occurrence: u32_field("occurrence")?,
        traceparent: optional_string("traceparent")?,
        tracestate: optional_string("tracestate")?,
        deadline_ms,
        config: string("config")?,
    })
}
