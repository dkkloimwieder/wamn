//! Writes the generated TypeScript clients of the application host routes,
//! from the control contract at `crates/catalog/model/src/host_route`, into
//! `target/wamn/wamn_control` at the repository root.
//!
//! Every web package that reads the clients runs this first.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use wamn_schema_generator::materialize_host_route_client;

fn main() -> Result<()> {
    let output_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/wamn");
    materialize_host_route_client(&output_root).context("materialize the host route client")
}
