//! Link only the WASI interfaces in an application's admitted declaration.

use wamn_catalog::AdmittedComponent;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::component::{HasData, Linker, ResourceTable};
use wasmtime_wasi::WasiView as _;
use wasmtime_wasi::p2::bindings::{clocks, io, random};

struct Io;

impl HasData for Io {
    type Data<'a> = &'a mut ResourceTable;
}

/// Build the WASI portion of one application's linker from its declared imports.
pub fn linker(
    engine: &wash_runtime::wasmtime::Engine,
    fact: &AdmittedComponent,
) -> anyhow::Result<Linker<SharedCtx>> {
    let mut linker = Linker::new(engine);
    let mut registered = std::collections::BTreeSet::new();
    for import in &fact.imports {
        // The policy owns family matching and the build-derived WASI ceiling.
        if wamn_component_policy::capability_row(import).is_none() {
            continue;
        }
        let interface = import.split('@').next().expect("an import has a name");
        if !registered.insert(interface) {
            continue;
        }
        match interface {
            "wasi:io/error" => {
                io::error::add_to_linker::<_, Io>(&mut linker, |ctx: &mut SharedCtx| {
                    &mut ctx.table
                })?;
            }
            "wasi:io/poll" => {
                io::poll::add_to_linker::<_, Io>(&mut linker, |ctx: &mut SharedCtx| {
                    &mut ctx.table
                })?;
            }
            "wasi:io/streams" => {
                io::streams::add_to_linker::<_, Io>(&mut linker, |ctx: &mut SharedCtx| {
                    &mut ctx.table
                })?;
            }
            "wasi:clocks/wall-clock" => {
                clocks::wall_clock::add_to_linker::<_, wasmtime_wasi::clocks::WasiClocks>(
                    &mut linker,
                    <SharedCtx as wasmtime_wasi::clocks::WasiClocksView>::clocks,
                )?;
            }
            "wasi:clocks/monotonic-clock" => {
                clocks::monotonic_clock::add_to_linker::<_, wasmtime_wasi::clocks::WasiClocks>(
                    &mut linker,
                    <SharedCtx as wasmtime_wasi::clocks::WasiClocksView>::clocks,
                )?;
            }
            "wasi:random/random" => {
                random::random::add_to_linker::<_, wasmtime_wasi::random::WasiRandom>(
                    &mut linker,
                    |ctx: &mut SharedCtx| ctx.ctx().ctx.random(),
                )?;
            }
            "wasi:random/insecure" => {
                random::insecure::add_to_linker::<_, wasmtime_wasi::random::WasiRandom>(
                    &mut linker,
                    |ctx: &mut SharedCtx| ctx.ctx().ctx.random(),
                )?;
            }
            "wasi:random/insecure-seed" => {
                random::insecure_seed::add_to_linker::<_, wasmtime_wasi::random::WasiRandom>(
                    &mut linker,
                    |ctx: &mut SharedCtx| ctx.ctx().ctx.random(),
                )?;
            }
            _ => {}
        }
    }
    Ok(linker)
}
