//! `event` emits the externally admitted event payload unchanged.

use serde_json::Value;
use wamn_node_sdk::{Emission, Node, NodeCtx, NodeError, RunContext};

pub(crate) struct Event;

impl Node for Event {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        Ok(Emission::main(input.clone()))
    }
}
