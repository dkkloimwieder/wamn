//! Writes the generated TypeScript clients of the application host routes,
//! from the control contract at `crates/catalog/model/src/host_route`, into
//! `apps/target/wamn/wamn_control`, the build output root of the applications.
//!
//! Every web package that reads the clients runs this first.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use wamn_schema_generator::{host_route_client_output_root, materialize_host_route_client};

fn main() -> Result<()> {
    let output_root =
        host_route_client_output_root(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    materialize_host_route_client(&output_root).context("materialize the host route client")
}
