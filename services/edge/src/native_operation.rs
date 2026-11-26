//! One export call of one admitted component, shared by every entry.
//!
//! The router driver calls [`invoke_operation`] for each node of a graph walk.
//! The route path calls it once for a route, with no walk. The deadline and the
//! native call belong here, so both entries run a component the same way.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use wamn_component_policy::AdmittedComponent;

use crate::app::EdgeApp;
use crate::policy::EdgeFacts;

pub mod native_call;
pub mod native_workload;

use native_call::{NativeInput, NativeInvocation, NativeOutcome, invoke_raw};
pub use native_workload::NativeApp;

pub use wamn_engine::operation::{ambient_cli, json, next_scope, node_types};

/// The application that one operation call runs in.
#[derive(Clone, Copy)]
pub enum OperationClosure<'a> {
    /// The carried release, loaded once from its complete component list.
    Released(&'a [AdmittedComponent]),
    /// A candidate application that the caller loaded and unbinds.
    Candidate(&'a Arc<NativeApp>),
}

impl fmt::Debug for OperationClosure<'_> {
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
pub struct OperationCall<'a> {
    pub closure: OperationClosure<'a>,
    pub component: &'a AdmittedComponent,
    pub operation: &'a wamn_package::canonical_name::CanonicalName,
    pub context: node_types::NodeContext,
    pub input: &'a serde_json::Value,
    /// The bounded deadline, the same value as `context.deadline_ms`.
    pub deadline_ms: u64,
    /// The facts of this call that only the policy reads.
    pub facts: EdgeFacts,
}

impl fmt::Debug for OperationCall<'_> {
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
pub async fn invoke_operation(
    host: &EdgeApp,
    call: OperationCall<'_>,
) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(call.deadline_ms).min(wamn_app::MAX_HOST_CALL_DURATION);
    let app = match call.closure {
        OperationClosure::Released(components) => host.released_app(components)?,
        OperationClosure::Candidate(app) => app,
    };
    tokio::time::timeout_at(deadline, call_export(app, call))
        .await
        .context("native operation deadline elapsed")??
        .into_json()
}

/// Run one export under the call's deadline with the call's JSON as input.
async fn call_export(
    app: &Arc<NativeApp>,
    call: OperationCall<'_>,
) -> anyhow::Result<NativeOutcome> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(call.deadline_ms);
    tokio::time::timeout_at(deadline, async {
        let id = app
            .workload
            .component_id(call.component)
            .context("native-node-component-fact-missing")?;
        let target = app.dispatch_target(id)?;
        let json = serde_json::to_string(call.input).context("encode node input")?;
        invoke_raw(
            target,
            NativeInvocation {
                operation: call.operation.to_owned(),
                context: call.context,
                input: NativeInput::Encoded(json),
                deadline,
                facts: call.facts,
                app: Arc::clone(app),
            },
        )
        .await
    })
    .await
    .context("native node enclosing deadline elapsed")?
}
