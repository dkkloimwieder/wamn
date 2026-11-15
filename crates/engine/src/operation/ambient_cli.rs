//! Standard WASI CLI interfaces over an empty application context.

use std::collections::HashSet;

use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};
use wasmtime_wasi::cli::{WasiCli, WasiCliCtx, WasiCliCtxView, WasiCliView as _};
use wasmtime_wasi::p2::bindings::cli;

pub(crate) const ID: &str = "wamn-empty-cli";

#[derive(Debug)]
pub(crate) struct EmptyCli;

fn empty_context(ctx: &mut SharedCtx) -> WasiCliCtxView<'_> {
    *ctx.active_ctx.ctx.cli() = WasiCliCtx::default();
    ctx.cli()
}

#[async_trait::async_trait]
impl HostPlugin for EmptyCli {
    fn id(&self) -> &'static str {
        ID
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: wamn_component_policy::AMBIENT_CLI_INTERFACES
                .iter()
                .map(|interface| WitInterface::from(format!("{interface}@0.2.0").as_str()))
                .collect(),
            exports: HashSet::default(),
        }
    }
    fn wasi_overrides(&self) -> WitWorld {
        self.world()
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let linker = item.linker()?;
        cli::environment::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::exit::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::stdin::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::stdout::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::stderr::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::terminal_input::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::terminal_output::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::terminal_stdin::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::terminal_stdout::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        cli::terminal_stderr::add_to_linker::<SharedCtx, WasiCli>(linker, empty_context)?;
        Ok(())
    }
}
