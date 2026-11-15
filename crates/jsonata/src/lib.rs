//! JSONata evaluation shared by publication and the palette guest.

use jsonata_core::evaluator::{Context, Evaluator, EvaluatorOptions};
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

impl std::fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvaluationError {}

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
    evaluate_with_options(expression, input, parameters, EvaluatorOptions::default())
}

/// Evaluate one publication probe with bounded engine work.
pub fn evaluate_sample(
    expression: &str,
    input: &serde_json::Value,
    parameters: &serde_json::Value,
) -> Result<String, EvaluationError> {
    evaluate_with_options(
        expression,
        input,
        parameters,
        EvaluatorOptions {
            timeout_ms: Some(100),
            max_stack_depth: None,
            max_sequence_length: Some(10_000),
        },
    )
}

fn evaluate_with_options(
    expression: &str,
    input: &serde_json::Value,
    parameters: &serde_json::Value,
    options: EvaluatorOptions,
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
    let result = Evaluator::with_options(context, options)
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
