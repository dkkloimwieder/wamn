//! `transform` — reshape the payload with a JMESPath expression.
//!
//! Config: `{"expression": "<jmespath>"}`. The output payload is the
//! expression's result over the input (a multiselect hash constructs new
//! objects, e.g. `{id: receipt.id, lines: lines[].material}`). No
//! capabilities: a transform is physically incapable of I/O.

use serde_json::Value;
use wamn_node_sdk::{Emission, Node, NodeCtx, NodeError, RunContext};

use crate::expr::{config_str, eval_to_value};

pub(crate) struct Transform;

impl Node for Transform {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let expr = config_str(run.config, "expression")?;
        Ok(Emission::main(eval_to_value(expr, input)?))
    }
}
