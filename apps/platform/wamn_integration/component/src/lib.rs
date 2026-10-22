//! Standard integration settings and retained delivery outcomes.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "wamn-integration:component/wamn-integration@0.1.0",
    path: [
        "../../../../crates/execution/workflow/router/wit",
        "../../../../crates/platform/runtime/wit/deps/wamn-postgres-0.4",
        "../../../target/wamn/wamn_integration/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;
export!(Component);

use wamn::node::types::{ErrorDetail, NodeError};

fn invalid(error: impl ToString) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: error.to_string(),
        code: Some("invalid_input".into()),
    })
}

fn statement_error(error: wamn_postgres_statements::StatementError) -> NodeError {
    use wamn_postgres_statements::StatementErrorType;
    let code = match error.type_() {
        StatementErrorType::SerializationFailure | StatementErrorType::ConnectionUnavailable => {
            "retry"
        }
        StatementErrorType::StatementTimeout => "timeout",
        _ => "internal_error",
    };
    let detail = ErrorDetail {
        message: error.to_string(),
        code: Some(code.into()),
    };
    if code == "internal_error" {
        NodeError::Terminal(detail)
    } else {
        NodeError::Retryable(detail)
    }
}

// Keep codec validation and typed outcomes, but return SQL failures through
// the existing NodeError boundary, whose message can carry the cause.
macro_rules! export_operation {
    ($contract:ident, $codec:ident, $item:ident, $outcome:ident, $error:ident, $state:expr, $handler:path) => {
        impl $contract::Guest for super::Component {
            async fn run(
                _context: super::wamn::node::types::NodeContext,
                input: Vec<$contract::$item>,
            ) -> Result<Vec<$contract::$outcome>, super::NodeError> {
                $codec::validate(&input).map_err(|error| super::invalid(error.context()))?;
                let mut state = ($state, None);
                let rows = $codec::run(input, &mut state, async |state, request| {
                    if state.1.is_some() {
                        return Err($contract::$error::InternalError);
                    }
                    $handler(&mut state.0, request).await.map_err(|error| {
                        state.1 = Some(error);
                        $contract::$error::InternalError
                    })
                })
                .await;
                match state.1 {
                    Some(error) => Err(error),
                    None => Ok(rows),
                }
            }

            async fn run_json(
                context: super::wamn::node::types::NodeContext,
                input: String,
            ) -> Result<super::wamn::node::types::Emission, super::NodeError> {
                let input =
                    $codec::decode(&input).map_err(|error| super::invalid(error.context()))?;
                let rows = <Self as $contract::Guest>::run(context, input).await?;
                Ok(super::wamn::node::types::Emission {
                    payload: $codec::encode(&rows),
                    port: None,
                })
            }
        }
    };
}

mod log;
mod setting;
