//! Application workload service for walking workflows and driving their admissions.
#![cfg(target_env = "p3")]

#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen lifts canonical ABI lists with known length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:workflow-service/workflow-service@0.1.0",
        path: [
            "../../../wit/wamn-workflow",
            "../../../wit/wasi-cli",
            "../../../wit/wasi-clocks",
            "wit",
        ],
        generate_all,
    });
}

mod delivery;
mod graph;
mod retry;
mod service;
mod walk;

use bindings::exports::wamn::workflow::entry;
use bindings::exports::wasi::cli;
use bindings::wamn::workflow::run::Refusal;

struct Component;

impl cli::run::Guest for Component {
    async fn run() -> Result<(), ()> {
        service::serve().await;
        Ok(())
    }
}

impl entry::Guest for Component {
    async fn call(input: entry::Request) -> Result<entry::Response, Refusal> {
        delivery::request(input).await
    }

    async fn run(input: entry::LeasedRun) -> Result<(), Refusal> {
        delivery::run(input).await
    }
}

#[expect(
    unsafe_code,
    reason = "wit-bindgen generates the component ABI exports"
)]
const _: () = {
    bindings::export!(Component with_types_in bindings);
};
