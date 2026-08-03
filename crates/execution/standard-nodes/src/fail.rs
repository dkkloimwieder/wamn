//! `fail` returns the authored terminal failure detail.

use serde_json::Value;
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RunContext};

pub(crate) struct Fail;

impl Node for Fail {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        _input: &Value,
    ) -> Result<Emission, NodeError> {
        let code = run
            .config
            .get("code")
            .and_then(Value::as_str)
            .expect("validated fail config has code");
        let message = run
            .config
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(code);
        Err(NodeError::Terminal(ErrorDetail::coded(code, message)))
    }
}
