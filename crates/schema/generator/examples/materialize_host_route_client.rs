//! Writes or checks the generated TypeScript client of the application host
//! routes, from the control contract at `crates/catalog/model/src/host_route`.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use wamn_schema_generator::{MaterializeMode, materialize_host_route_client};

fn main() -> Result<()> {
    let mut values = std::env::args().skip(1);
    let mode = match values.next().as_deref() {
        Some("write") => MaterializeMode::Write,
        Some("check") => MaterializeMode::Check,
        _ => bail!("usage: materialize_host_route_client <write|check>"),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog/model/src/host_route");
    materialize_host_route_client(mode, &root).context("materialize the host route client")
}
