//! One export call of one admitted component, shared by every entry.
//!
//! The router driver calls [`invoke_operation`] for each node of a wiring walk.
//! The route path calls it once for a route, with no walk. The deadline and the
//! native call belong here, so both entries run a component the same way. The
//! host gives the loaded application through [`ApplicationHost`] and grants the
//! authority of each call through [`InvocationPolicy`].

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use wamn_catalog::AdmittedComponent;
use wash_runtime::plugin::HostPlugin as _;

pub mod intent;
pub mod invocation_policy;
pub mod native_call;
pub mod native_workload;

pub use intent::{IntentContext, logs_intent};
pub use invocation_policy::{ApplicationHost, InvocationPolicy};
use native_call::{NativeInput, NativeInvocation, NativeOutcome, RowBatches, invoke_owned};
pub use native_workload::NativeApplication;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../execution/workflow/router/wit",
        world: "node",
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

pub use bindings::wamn::node::types as node_types;

/// The application that one operation call runs in.
pub enum OperationClosure<'a, P: InvocationPolicy> {
    /// The carried release, loaded once from its complete component list.
    Released(&'a [AdmittedComponent]),
    /// A candidate application that the caller loaded and unbinds.
    Candidate(&'a Arc<NativeApplication<P>>),
}

impl<P: InvocationPolicy> Clone for OperationClosure<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: InvocationPolicy> Copy for OperationClosure<'_, P> {}

impl<P: InvocationPolicy> fmt::Debug for OperationClosure<'_, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Released(components) => formatter
                .debug_tuple("Released")
                .field(&components.len())
                .finish(),
            Self::Candidate(_) => formatter.write_str("Candidate"),
        }
    }
}

/// One export call of one admitted component.
pub struct OperationCall<'a, P: InvocationPolicy> {
    pub closure: OperationClosure<'a, P>,
    pub component: &'a AdmittedComponent,
    pub operation: &'a str,
    pub context: node_types::NodeContext,
    pub input: &'a serde_json::Value,
    /// The bounded deadline, the same value as `context.deadline_ms`.
    pub deadline_ms: u64,
    /// The facts of this call that only the policy reads.
    pub facts: P::Facts,
}

impl<P: InvocationPolicy> fmt::Debug for OperationCall<'_, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationCall")
            .field("closure", &self.closure)
            .field("operation", &self.operation)
            .field("deadline_ms", &self.deadline_ms)
            .finish_non_exhaustive()
    }
}

/// Call one export once, under the call's deadline, and return what the
/// component returned. The caller lowers the result for its own entry.
///
/// With an [`IntentContext`] of a kind that [`logs_intent`], the call logs one
/// intent for each input item and runs only the new items ([`intent`]).
pub async fn invoke_operation<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
    intent: Option<IntentContext<'_>>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    match intent.filter(|intent| logs_intent(intent.kind)) {
        Some(intent) => intent::invoke_logged(host, call, intent).await,
        None => run_export(host, call).await,
    }
}

/// Call one query export once, under the call's deadline, and hand its rows
/// to `rows` as the component writes them. The result is the outcome JSON the
/// component wrote after its last row. A query logs no intent.
pub async fn invoke_operation_stream<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
    rows: RowBatches,
) -> anyhow::Result<Result<String, node_types::NodeError>> {
    match call_export(host, call, |input| NativeInput::Stream { input, rows }).await? {
        NativeOutcome::Stream(outcome) => Ok(outcome),
        _ => anyhow::bail!("a streamed call returned a whole outcome"),
    }
}

/// Run one export under the call's deadline.
async fn run_export<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    call_export(host, call, NativeInput::Json)
        .await?
        .into_json()
}

/// Run one export under the call's deadline with the input `input` builds from
/// the call's JSON.
async fn call_export<H: ApplicationHost>(
    host: &H,
    call: OperationCall<'_, H::Policy>,
    input: impl FnOnce(String) -> NativeInput,
) -> anyhow::Result<NativeOutcome> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(call.deadline_ms);
    tokio::time::timeout_at(deadline, async {
        let application = match call.closure {
            OperationClosure::Released(components) => host.released_application(components).await?,
            OperationClosure::Candidate(application) => Arc::clone(application),
        };
        let id = application
            .workload
            .facts_by_component_id
            .iter()
            .find_map(|(id, fact)| (fact == call.component).then_some(id))
            .context("native-node-component-fact-missing")?;
        let target = application
            .workload
            .dispatch_target(id, application.policy.id())
            .await?;
        let json = serde_json::to_string(call.input).context("encode node input")?;
        invoke_owned(
            &target,
            NativeInvocation {
                operation: call.operation.to_owned(),
                context: call.context,
                input: input(json),
                deadline,
                facts: call.facts,
                application,
            },
        )
        .await
    })
    .await
    .context("native node enclosing deadline elapsed")?
}

/// Unique process-local application and invocation scope identifiers.
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

pub fn next_scope(component: &str) -> Box<str> {
    format!("{component}#{}", NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)).into()
}
