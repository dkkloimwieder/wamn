//! `jsonata` — evaluate one JSONata expression on the node input.
//!
//! The expression is the wiring parameter `expression`: configuration, not
//! input. It runs here, in the guest sandbox, and never in the host
//! (docs/plan/routes-router.md rule 6). The node imports nothing, so it holds
//! no authority, and its effect projection is empty.
//!
//! The node emits the expression's result as it is. A result that JSONata
//! calls undefined, for example a path that matches nothing, emits `null`.
//!
//! The result leaves on port `main`, whose schema admits any JSON. With the
//! optional parameter `port` set to `items`, it leaves on port `items`, whose
//! schema is the item array that palette nodes such as `label-render` take:
//! a wiring edge joins two ports only when their schemas are the same, and
//! the node refuses a result that is not an array there.
//!
//! An input that is not JSON is `invalid-input`. An expression that does not
//! parse is a configuration fault, and an evaluation failure repeats on a
//! retry, so both are `terminal`.

#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:jsonata/jsonata@0.1.0",
        path: ["../../../../crates/execution/workflow/router/wit", "wit"],
        generate_all,
    });
}

use bindings::exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use bindings::wamn::node::types::ErrorDetail;
use jsonata_core::evaluator::Evaluator;
use jsonata_core::parser;
use jsonata_core::value::JValue;

/// Why an evaluation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationErrorKind {
    /// The expression does not parse.
    Expression,
    /// The expression parsed and failed on this input.
    Evaluation,
}

/// A failed evaluation, with the engine's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationError {
    kind: EvaluationErrorKind,
    message: String,
}

impl EvaluationError {
    pub fn kind(&self) -> EvaluationErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Evaluate `expression` on `input` and return the result as JSON text.
pub fn evaluate(expression: &str, input: &serde_json::Value) -> Result<String, EvaluationError> {
    let ast = parser::parse(expression).map_err(|error| EvaluationError {
        kind: EvaluationErrorKind::Expression,
        message: error.to_string(),
    })?;
    let data = JValue::from(input.clone());
    let result = Evaluator::new()
        .evaluate(&ast, &data)
        .map_err(|error| EvaluationError {
            kind: EvaluationErrorKind::Evaluation,
            message: error.message().to_owned(),
        })?;
    result.to_json_string().map_err(|error| EvaluationError {
        kind: EvaluationErrorKind::Evaluation,
        message: format!("the result is not JSON: {error}"),
    })
}

struct Component;

impl Guest for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = serde_json::from_str::<serde_json::Value>(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: format!("input is not JSON: {error}"),
                code: Some("invalid_json".to_owned()),
            })
        })?;
        let config = serde_json::from_str::<serde_json::Value>(&context.config)
            .map_err(|error| terminal("invalid_config", format!("config is not JSON: {error}")))?;
        let expression = config
            .get("expression")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| terminal("invalid_config", "expression must be a string".to_owned()))?;
        let port = config
            .get("port")
            .map(|port| {
                port.as_str()
                    .filter(|port| [MAIN_PORT, ITEMS_PORT].contains(port))
                    .ok_or_else(|| {
                        terminal("invalid_config", "port must be main or items".to_owned())
                    })
            })
            .transpose()?;
        let payload = evaluate(expression, &input).map_err(|error| match error.kind() {
            EvaluationErrorKind::Expression => terminal(
                "invalid_expression",
                format!("the expression does not parse: {}", error.message()),
            ),
            EvaluationErrorKind::Evaluation => {
                terminal("evaluation_failed", error.message().to_owned())
            }
        })?;
        if port == Some(ITEMS_PORT) && !is_item_array(&payload) {
            return Err(terminal(
                "not_an_item_array",
                "the result on port items must be an array".to_owned(),
            ));
        }
        Ok(Emission {
            payload,
            port: port.map(str::to_owned),
        })
    }
}

/// The output port whose schema admits any JSON.
const MAIN_PORT: &str = "main";
/// The output port whose schema is an array of items.
const ITEMS_PORT: &str = "items";

/// Whether an emitted payload fits the `items` port schema.
fn is_item_array(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload).is_ok_and(|value| value.is_array())
}

fn terminal(code: &str, message: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message,
        code: Some(code.to_owned()),
    })
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{EvaluationErrorKind, evaluate, is_item_array};

    /// The WMS label workflow's shape expression (docs/plan/workflow-feature.md
    /// 4.4): a move row becomes one label-render item keyed by its command,
    /// and any other row none.
    const SHAPE: &str = r#"event = "insert" and new.kind = "move"
        ? [{"request_id": new.id,
            "value": {"label_key": new.idempotency_key, "movement_id": new.id,
                      "pallet_id": new.pallet_id, "location_id": new.to_location_id}}]
        : []"#;

    fn result(expression: &str, input: &Value) -> Value {
        serde_json::from_str(&evaluate(expression, input).expect("the expression evaluates"))
            .expect("the result is JSON")
    }

    #[test]
    fn the_shape_expression_turns_a_move_row_into_one_label_item() {
        let row = json!({"event": "insert", "new": {
            "id": "m-1", "idempotency_key": "k-1", "kind": "move", "pallet_id": "p-1",
            "from_location_id": "l-0", "to_location_id": "l-2", "quantity": "4"}});
        assert_eq!(
            result(SHAPE, &row),
            json!([{"request_id": "m-1", "value": {"label_key": "k-1",
                "movement_id": "m-1", "pallet_id": "p-1", "location_id": "l-2"}}])
        );
    }

    #[test]
    fn the_shape_expression_gives_no_item_for_another_movement() {
        let row = json!({"event": "insert", "new": {"id": "m-2", "kind": "receive"}});
        assert_eq!(result(SHAPE, &row), json!([]));
    }

    #[test]
    fn only_an_array_fits_the_items_port() {
        let row = json!({"event": "insert", "new": {"id": "m-2", "kind": "receive"}});
        assert!(is_item_array(&evaluate(SHAPE, &row).unwrap()));
        assert!(!is_item_array(&evaluate("new", &row).unwrap()));
        assert!(!is_item_array(&evaluate("absent", &row).unwrap()));
    }

    #[test]
    fn a_path_that_matches_nothing_emits_null() {
        assert_eq!(result("absent.field", &json!({"new": {}})), Value::Null);
    }

    #[test]
    fn an_expression_that_does_not_parse_is_an_expression_fault() {
        let error = evaluate("new.[", &json!({})).unwrap_err();
        assert_eq!(error.kind(), EvaluationErrorKind::Expression);
    }

    #[test]
    fn a_failure_on_the_input_is_an_evaluation_fault() {
        let error = evaluate("new.id + 1", &json!({"new": {"id": "m-1"}})).unwrap_err();
        assert_eq!(error.kind(), EvaluationErrorKind::Evaluation);
        assert!(!error.message().is_empty());
    }
}
