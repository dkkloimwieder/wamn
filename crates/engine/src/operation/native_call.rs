//! Shared node results and context values for native dispatch.

use wash_runtime::wasmtime::component::Val;

use super::node_types;

#[derive(Debug)]
pub enum NativeOutcome {
    Json(Result<node_types::Emission, node_types::NodeError>),
}

impl NativeOutcome {
    pub fn into_json(self) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        match self {
            Self::Json(value) => Ok(value),
        }
    }
}

/// The node context supplied with a typed operation argument.
pub fn context_value(context: node_types::NodeContext) -> Val {
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
