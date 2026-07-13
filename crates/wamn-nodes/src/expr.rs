//! JMESPath evaluation shared by `transform`, `conditional`, and templating.
//!
//! JMESPath is deliberately the WHOLE expression surface (user decision,
//! wamn-3xa): a frozen public spec we do not maintain, JSON → JSON, side-effect
//! free, and with **no arithmetic operators** — it can select, reshape, compare
//! and construct, but it cannot manufacture floats out of the exact-decimal
//! STRINGS catalog numerics travel as (the no-float rule holds through a
//! transform by construction). Number cells pass through `serde_json::Number`
//! unchanged (probe-tested: 2^53+1 survives exactly).
//!
//! Expressions are compiled per dispatch; memoizing per (flow-version, node-id)
//! is the note-9b refinement if profiles ever demand it.

use serde_json::Value;
use wamn_node_sdk::{ErrorDetail, NodeError};

/// Compile and evaluate `expr` against `input`. A malformed expression is a
/// flow bug → `Terminal("invalid-expression")`; an evaluation failure (JMESPath
/// type errors) is `Terminal("expression-failed")`. A missing path is NOT an
/// error — JMESPath yields `null`.
pub(crate) fn eval(expr: &str, input: &Value) -> Result<jmespath::Rcvar, NodeError> {
    let compiled = jmespath::compile(expr).map_err(|e| {
        NodeError::Terminal(ErrorDetail::coded(
            "invalid-expression",
            format!("invalid JMESPath expression {expr:?}: {e}"),
        ))
    })?;
    compiled.search(input).map_err(|e| {
        NodeError::Terminal(ErrorDetail::coded(
            "expression-failed",
            format!("JMESPath expression {expr:?} failed: {e}"),
        ))
    })
}

/// Evaluate `expr` and convert the result back into a `serde_json::Value`.
pub(crate) fn eval_to_value(expr: &str, input: &Value) -> Result<Value, NodeError> {
    let var = eval(expr, input)?;
    serde_json::to_value(&var).map_err(|e| {
        NodeError::Terminal(ErrorDetail::coded(
            "expression-failed",
            format!("JMESPath result of {expr:?} not representable as JSON: {e}"),
        ))
    })
}

/// Evaluate `expr` for its JMESPath truthiness (`false`, `null`, empty string /
/// array / object are falsy; everything else — including `0` — is truthy).
pub(crate) fn eval_truthy(expr: &str, input: &Value) -> Result<bool, NodeError> {
    Ok(eval(expr, input)?.is_truthy())
}

/// A required string-typed config key, e.g. the expression itself. Absence or
/// a non-string is a flow-authoring bug → `Terminal("invalid-config")`.
pub(crate) fn config_str<'a>(config: &'a Value, key: &str) -> Result<&'a str, NodeError> {
    config.get(key).and_then(Value::as_str).ok_or_else(|| {
        NodeError::Terminal(ErrorDetail::coded(
            "invalid-config",
            format!("node config requires a string {key:?}"),
        ))
    })
}
