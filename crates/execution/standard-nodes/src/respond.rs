//! `respond` — the webhook-response terminal: the payload passes through
//! unchanged and becomes the run's result; the DRIVER answers the held request
//! with it (D15 sync). The HTTP status is config, read by the driver through
//! the pure [`status_for`] rule — a Node cannot (and must not) touch the
//! transport. No capabilities.

use serde_json::Value;
use wamn_node_sdk::{Emission, Node, NodeCtx, NodeError, RunContext};

/// The configured response status (`{"status": 201}`), when present and a
/// valid HTTP status. The driver's default applies otherwise (200).
pub fn status_for(config: &Value) -> Option<u16> {
    let n = config.get("status")?.as_u64()?;
    u16::try_from(n).ok().filter(|s| (100..=599).contains(s))
}

pub(crate) struct Respond;

impl Node for Respond {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        _run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        Ok(Emission::main(input.clone()))
    }
}
