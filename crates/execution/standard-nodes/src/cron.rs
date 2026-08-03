//! `cron` emits the scheduler-admitted payload unchanged.

use serde_json::Value;
use wamn_node_sdk::{Emission, Node, NodeCtx, NodeError, RunContext};

pub(crate) struct Cron;

impl Node for Cron {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        Ok(Emission::main(input.clone()))
    }
}
