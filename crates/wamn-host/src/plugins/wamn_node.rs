//! S1 stub of the `wamn:node/control` host import (cooperative cancellation).
//!
//! Always answers "not cancelled". The real implementation is wired to run
//! state by the flow-runner work (Epic 5). Contract: docs/wamn-node.wit.
//! `payloads` and `credentials` are deliberately not registered in S1.

use std::collections::HashSet;

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "node-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::node::control::{self, CancelReason};

pub const WAMN_NODE_ID: &str = "wamn-node";

#[derive(Default)]
pub struct WamnNodeControl;

#[async_trait::async_trait]
impl HostPlugin for WamnNodeControl {
    fn id(&self) -> &'static str {
        WAMN_NODE_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:node/control@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "node", &["control"]) {
            return Ok(());
        }
        control::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        tracing::debug!(component = item.id(), "bound wamn:node/control stub");
        Ok(())
    }
}

impl control::Host for ActiveCtx<'_> {
    async fn cancelled(&mut self) -> wash_runtime::wasmtime::Result<Option<CancelReason>> {
        Ok(None)
    }
}
