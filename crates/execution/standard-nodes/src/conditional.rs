//! `conditional` — branch on a JMESPath predicate.
//!
//! Config: `{"expression": "<jmespath>"}` (comparisons and boolean logic are in
//! the JMESPath spec, e.g. `lines[?hold].length(@) > `0``). The input payload
//! passes through UNCHANGED on port `"true"` or `"false"` by the expression's
//! JMESPath truthiness (`false`/`null`/empty string/array/object are falsy;
//! numbers — including `0` — are truthy per the spec). No capabilities.

use serde_json::Value;
use wamn_node_sdk::{Emission, Node, NodeCtx, NodeError, RunContext};

use crate::expr::{config_str, eval_truthy};

/// The two branch ports a conditional emits on.
pub const TRUE_PORT: &str = "true";
pub const FALSE_PORT: &str = "false";

pub(crate) struct Conditional;

impl Node for Conditional {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let expr = config_str(run.config, "expression")?;
        let port = if eval_truthy(expr, input)? {
            TRUE_PORT
        } else {
            FALSE_PORT
        };
        Ok(Emission::on(input.clone(), port))
    }
}
