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
//! schema is the request element array that palette nodes such as `label-render` take:
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
use jsonata_core::evaluator::{Context, Evaluator};
use jsonata_core::parser;
use jsonata_core::value::JValue;

/// Why an evaluation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationErrorType {
    /// The expression does not parse.
    Expression,
    /// The expression parsed and failed on this input.
    Evaluation,
}

/// A failed evaluation, with the engine's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationError {
    type_: EvaluationErrorType,
    message: String,
}

impl EvaluationError {
    pub fn error_type(&self) -> EvaluationErrorType {
        self.type_
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Evaluate `expression` on `input` and return the result as JSON text.
pub fn evaluate(expression: &str, input: &serde_json::Value) -> Result<String, EvaluationError> {
    evaluate_with_parameters(expression, input, &serde_json::Value::Null)
}

/// Evaluate with the node's optional parameters bound to `$parameters`.
pub fn evaluate_with_parameters(
    expression: &str,
    input: &serde_json::Value,
    parameters: &serde_json::Value,
) -> Result<String, EvaluationError> {
    let ast = parser::parse(expression).map_err(|error| EvaluationError {
        type_: EvaluationErrorType::Expression,
        message: error.to_string(),
    })?;
    let data = JValue::from(input.clone());
    let mut context = Context::new();
    if !parameters.is_null() {
        context.bind("parameters".to_owned(), JValue::from(parameters.clone()));
    }
    let result = Evaluator::with_context(context)
        .evaluate(&ast, &data)
        .map_err(|error| EvaluationError {
            type_: EvaluationErrorType::Evaluation,
            message: error.message().to_owned(),
        })?;
    result.to_json_string().map_err(|error| EvaluationError {
        type_: EvaluationErrorType::Evaluation,
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
                    .filter(|port| [MAIN_PORT, REQUEST_ELEMENTS_PORT].contains(port))
                    .ok_or_else(|| {
                        terminal("invalid_config", "port must be main or items".to_owned())
                    })
            })
            .transpose()?;
        if config
            .get("parameters")
            .is_some_and(|value| !value.is_object())
        {
            return Err(terminal(
                "invalid_config",
                "parameters must be an object".into(),
            ));
        }
        let payload = evaluate_with_parameters(expression, &input, &config["parameters"]).map_err(
            |error| match error.error_type() {
                EvaluationErrorType::Expression => terminal(
                    "invalid_expression",
                    format!("the expression does not parse: {}", error.message()),
                ),
                EvaluationErrorType::Evaluation => {
                    terminal("evaluation_failed", error.message().to_owned())
                }
            },
        )?;
        let selected_port = select_port(&config, &payload)?;
        let port = selected_port.as_deref().or(port);
        if port == Some(REQUEST_ELEMENTS_PORT) && !is_request_element_array(&payload) {
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

/// Select an existing output port from the expression result.
fn select_port(config: &serde_json::Value, payload: &str) -> Result<Option<String>, NodeError> {
    let Some(pointer) = config.get("port-input-pointer") else {
        return Ok(None);
    };
    let pointer = pointer.as_str().ok_or_else(|| {
        terminal(
            "invalid_config",
            "port-input-pointer must be a string".into(),
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| terminal("invalid_result", error.to_string()))?;
    let port = value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .filter(|port| [MAIN_PORT, REQUEST_ELEMENTS_PORT].contains(port))
        .ok_or_else(|| {
            terminal(
                "invalid_port",
                "port-input-pointer must select main or items".into(),
            )
        })?;
    Ok(Some(port.to_owned()))
}

/// The output port whose schema admits any JSON.
const MAIN_PORT: &str = "main";
/// The output port whose schema is an array of request elements.
const REQUEST_ELEMENTS_PORT: &str = "items";

/// Whether an emitted payload fits the `items` port schema.
fn is_request_element_array(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload).is_ok_and(|value| value.is_array())
}

fn terminal(code: &str, message: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message,
        code: Some(code.to_owned()),
    })
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{EvaluationErrorType, evaluate, is_request_element_array};

    #[test]
    fn parameters_are_bound_without_changing_the_input() {
        let output = super::evaluate_with_parameters(
            "{\"key\": key, \"type\": $parameters.record_type}",
            &json!({"key":"1"}),
            &json!({"record_type":"customer"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"key":"1", "type":"customer"})
        );
    }

    #[test]
    fn port_pointer_selects_only_existing_ports() {
        let config = json!({"port-input-pointer":"/0/port"});
        for port in ["main", "items"] {
            assert_eq!(
                super::select_port(&config, &json!([{"port":port}]).to_string())
                    .unwrap()
                    .as_deref(),
                Some(port)
            );
        }
        for input in [json!([]), json!([{"port":42}]), json!([{"port":"unknown"}])] {
            assert!(super::select_port(&config, &input.to_string()).is_err());
        }
        assert!(super::select_port(&json!({"port-input-pointer":7}), "[]").is_err());
        assert_eq!(super::select_port(&json!({}), "[]").unwrap(), None);
    }

    /// The WMS label workflow's shape expression (docs/plan/workflow-feature.md
    /// 4.4): a move row becomes one label-render request element keyed by its command,
    /// and any other row none.
    const SHAPE: &str = r#"event = "insert" and new.kind = "move"
        ? [{"request_id": new.id,
            "value": {"label_key": new.idempotency_key, "movement_id": new.id,
                      "packaging_id": new.packaging_id, "location_id": new.to_location_id}}]
        : []"#;

    fn result(expression: &str, input: &Value) -> Value {
        serde_json::from_str(&evaluate(expression, input).expect("the expression evaluates"))
            .expect("the result is JSON")
    }

    #[test]
    fn the_shape_expression_turns_a_move_row_into_one_label_request_element() {
        let row = json!({"event": "insert", "new": {
            "id": "m-1", "idempotency_key": "k-1", "kind": "move", "packaging_id": "p-1",
            "from_location_id": "l-0", "to_location_id": "l-2", "quantity": "4"}});
        assert_eq!(
            result(SHAPE, &row),
            json!([{"request_id": "m-1", "value": {"label_key": "k-1",
                "movement_id": "m-1", "packaging_id": "p-1", "location_id": "l-2"}}])
        );
    }

    #[test]
    fn the_shape_expression_gives_no_request_element_for_another_movement() {
        let row = json!({"event": "insert", "new": {"id": "m-2", "kind": "receive"}});
        assert_eq!(result(SHAPE, &row), json!([]));
    }

    #[test]
    fn only_an_array_fits_the_items_port() {
        let row = json!({"event": "insert", "new": {"id": "m-2", "kind": "receive"}});
        assert!(is_request_element_array(&evaluate(SHAPE, &row).unwrap()));
        assert!(!is_request_element_array(&evaluate("new", &row).unwrap()));
        assert!(!is_request_element_array(
            &evaluate("absent", &row).unwrap()
        ));
    }

    #[test]
    fn a_path_that_matches_nothing_emits_null() {
        assert_eq!(result("absent.field", &json!({"new": {}})), Value::Null);
    }

    #[test]
    fn an_expression_that_does_not_parse_is_an_expression_fault() {
        let error = evaluate("new.[", &json!({})).unwrap_err();
        assert_eq!(error.error_type(), EvaluationErrorType::Expression);
    }

    #[test]
    fn a_failure_on_the_input_is_an_evaluation_fault() {
        let error = evaluate("new.id + 1", &json!({"new": {"id": "m-1"}})).unwrap_err();
        assert_eq!(error.error_type(), EvaluationErrorType::Evaluation);
        assert!(!error.message().is_empty());
    }
}
