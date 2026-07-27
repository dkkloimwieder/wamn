//! JMESPath evaluation shared by every standard-node expression surface.
//!
//! JMESPath is deliberately the WHOLE expression surface (user decision,
//! wamn-3xa): a frozen public spec we do not maintain, JSON → JSON, side-effect
//! free, and with **no arithmetic operators** — it can select, reshape, compare
//! and construct, but it cannot manufacture floats out of the exact-decimal
//! STRINGS catalog numerics travel as (the no-float rule holds through a
//! transform by construction). The sole platform extension is the zero-argument
//! `context()` reader for durable run context; merge remains the standard
//! JMESPath `merge()` function. Number cells pass through `serde_json::Number`
//! unchanged (probe-tested: 2^53+1 survives exactly).
//!
//! Expressions are compiled per dispatch; memoizing per (flow-version, node-id)
//! is the note-9b refinement if profiles ever demand it.

use jmespath::functions::{CustomFunction, Signature};
use jmespath::{Context, Rcvar, Runtime, Variable};
use serde_json::Value;
use wamn_node_sdk::{ErrorDetail, NodeError};

fn runtime(context: &Value) -> Runtime {
    let mut runtime = Runtime::new();
    runtime.register_builtin_functions();
    let context = context.clone();
    runtime.register_function(
        "context",
        Box::new(CustomFunction::new(
            Signature::new(Vec::new(), None),
            Box::new(move |_: &[Rcvar], _: &mut Context<'_>| {
                Variable::from_serializable(&context).map(Rcvar::new)
            }),
        )),
    );
    runtime
}

/// Compile and evaluate `expr` against `input`. A malformed expression is a
/// flow bug → `Terminal("invalid-expression")`; an evaluation failure (JMESPath
/// type errors) is `Terminal("expression-failed")`. A missing path is NOT an
/// error — JMESPath yields `null`.
pub(crate) fn eval(expr: &str, input: &Value, context: &Value) -> Result<Rcvar, NodeError> {
    let runtime = runtime(context);
    let compiled = runtime.compile(expr).map_err(|e| {
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
pub(crate) fn eval_to_value(
    expr: &str,
    input: &Value,
    context: &Value,
) -> Result<Value, NodeError> {
    let var = eval(expr, input, context)?;
    serde_json::to_value(&var).map_err(|e| {
        NodeError::Terminal(ErrorDetail::coded(
            "expression-failed",
            format!("JMESPath result of {expr:?} not representable as JSON: {e}"),
        ))
    })
}

/// Evaluate `expr` for its JMESPath truthiness (`false`, `null`, empty string /
/// array / object are falsy; everything else — including `0` — is truthy).
pub(crate) fn eval_truthy(expr: &str, input: &Value, context: &Value) -> Result<bool, NodeError> {
    Ok(eval(expr, input, context)?.is_truthy())
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
