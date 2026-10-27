//! Writes the generated TypeScript clients of the application host routes,
//! from the control contract at `crates/catalog/model/src/host_route`, into
//! `apps/target/wamn/wamn_control`, the build output root of the applications.
//! It then copies each client into the workspace packages that name it in
//! `package.json` `"wamn": { "clients": [...] }`: `wamn_control` for the
//! application routes and `wamn_control_org` for the control routes.
//!
//! Every web package that reads the clients runs this first.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use wamn_schema_generator::{
    host_route_client_output_root, materialize_client_consumers, materialize_host_route_client,
};

fn main() -> Result<()> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output_root = host_route_client_output_root(&repository);
    materialize_host_route_client(&output_root).context("materialize the host route client")?;
    let output = output_root.join(wamn_catalog::host_route_package());
    materialize_client_consumers(&repository, "wamn_control", &output.join("client-ts"))?;
    materialize_client_consumers(
        &repository,
        "wamn_control_org",
        &output.join("control/client-ts"),
    )
}
