//! Print the authoring gate request of one wiring, for docs/operations/gcp.md
//! section 5.
//!
//! The request uses the same library builder as the wired release Job.
//!
//! cargo run -p wamn-test-infrastructure --example gate_request -- \
//!   <package id> <package version> <project> <environment> <wiring document>

use std::path::Path;

use anyhow::ensure;
use wamn_catalog::PackageCoordinate;
use wamn_control::platform::wired_release::gate_request;

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 5,
        "usage: <package id> <package version> <project> <environment> <wiring document>"
    );
    let package = PackageCoordinate::new(&arguments[0], &arguments[1])?;
    let request = gate_request(
        &package,
        &arguments[2],
        &arguments[3],
        Path::new(&arguments[4]),
    )?;
    println!("{}", serde_json::to_string(&request)?);
    Ok(())
}
