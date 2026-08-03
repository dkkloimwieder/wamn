//! `request` emits the admitted request payload unchanged.

use serde_json::Value;
use wamn_node_sdk::{Emission, Node, NodeCtx, NodeError, RunContext};

pub(crate) struct Request;

impl Node for Request {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        Ok(Emission::main(input.clone()))
    }
}
