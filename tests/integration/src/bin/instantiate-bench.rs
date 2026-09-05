//! What does a fresh component instance actually cost, and does it scale with
//! artifact size?
//!
//! `wamn.component.instantiate` measures 1.3 ms in the request path. The
//! fresh-store rule (native-alignment ledger row 4) says every invocation gets a
//! new instance, so that millisecond is either the price of the rule or the
//! price of a 20 MB debug artifact -- and those have opposite remedies. This
//! walks the built guests from 1.2 MB to 20.5 MB against the PRODUCTION engine
//! and times `instantiate_async` alone, with compilation and pre-instantiation
//! already done, which is exactly the state the router is in when it calls it.
//!
//! Run: cargo run -p wamn-proof-integration --bin instantiate-bench -- <wasm>...

use std::time::Instant;

use anyhow::Context as _;
use wamn_runtime::engine::build_engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Linker};

const WARMUP: usize = 3;
const SAMPLES: usize = 20;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!paths.is_empty(), "usage: instantiate-bench <wasm>...");

    let engine = build_engine(&[]).context("build the production engine")?;
    let mut base: Linker<SharedCtx> = Linker::new(engine.inner());
    wasmtime_wasi::p2::add_to_linker_async(&mut base)
        .map_err(|error| anyhow::anyhow!("add the WASI p2 surface: {error}"))?;

    println!(
        "{:<34} {:>9} {:>10} {:>9} {:>11} {:>11}",
        "artifact", "MiB", "compile_ms", "pre_ms", "inst_mean_us", "inst_min_us"
    );
    for path in &paths {
        let bytes = std::fs::read(path).with_context(|| format!("read {path}"))?;
        let mib = bytes.len() as f64 / (1024.0 * 1024.0);

        let started = Instant::now();
        let component = Component::new(engine.inner(), &bytes)
            .map_err(|error| anyhow::anyhow!("compile {path}: {error}"))?;
        let compile_ms = started.elapsed().as_secs_f64() * 1000.0;

        // The wamn:* imports are stubbed as traps rather than bound to real
        // plugins. INSTANTIATION DOES NOT CALL THEM -- it links, initializes
        // memory and tables, and runs the component's own start -- so their
        // bodies cannot affect this number, and stubbing is what lets the real
        // 19 MB artifact be measured beside a 3 MB one instead of skipped.
        let mut linker = base.clone();
        // Guests carry different wasi minor versions than the base linker added;
        // without shadowing the stub pass dies on "defined twice".
        linker.allow_shadowing(true);
        linker
            .define_unknown_imports_as_traps(&component)
            .map_err(|error| anyhow::anyhow!("stub the non-WASI imports of {path}: {error}"))?;
        let started = Instant::now();
        let pre = match linker.instantiate_pre(&component) {
            Ok(pre) => pre,
            Err(error) => {
                println!("{:<34} {mib:>9.2}   (skipped: {error})", short(path));
                continue;
            }
        };
        let pre_ms = started.elapsed().as_secs_f64() * 1000.0;

        let mut samples = Vec::with_capacity(SAMPLES);
        for round in 0..(WARMUP + SAMPLES) {
            let ctx = Ctx::builder("bench".to_owned(), "bench".to_owned()).build();
            let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));
            // The production engine enables epoch interruption; without a
            // deadline every instantiation traps on interrupt immediately.
            store.set_epoch_deadline(1_000_000);
            let started = Instant::now();
            let instance = pre.instantiate_async(&mut store).await;
            let elapsed = started.elapsed();
            instance.map_err(|error| anyhow::anyhow!("instantiate {path}: {error}"))?;
            if round >= WARMUP {
                samples.push(elapsed.as_secs_f64() * 1_000_000.0);
            }
        }
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
        println!(
            "{:<34} {mib:>9.2} {compile_ms:>10.0} {pre_ms:>9.2} {mean:>11.1} {min:>11.1}",
            short(path)
        );
    }
    Ok(())
}

fn short(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}
