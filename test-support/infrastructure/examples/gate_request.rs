//! Print the authoring gate request of one wiring, for docs/operations/gcp.md
//! section 5.
//!
//! The request comes from `declarations::gate_document`, the one the WMS
//! cluster case posts to the scenario worker's `/authoring` route, with the
//! same command id and scope.
//!
//! cargo run -p wamn-test-infrastructure --example gate_request -- \
//!   <package id> <package version> <project> <environment> <wiring document>

use std::fs;

use anyhow::ensure;
use wamn_authoring_model::AuthoringScope;
use wamn_catalog::PackageCoordinate;
use wamn_test_infrastructure::declarations::{GateInput, gate_document};

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 5,
        "usage: <package id> <package version> <project> <environment> <wiring document>"
    );
    let package = PackageCoordinate::new(&arguments[0], &arguments[1])?;
    let wiring = fs::read_to_string(&arguments[4])?;
    let wiring_id = serde_json::from_str::<serde_json::Value>(&wiring)?["wiring-id"]
        .as_str()
        .map(str::to_owned);
    let wiring_id = match wiring_id {
        Some(id) => id,
        None => anyhow::bail!("the wiring document names no wiring-id"),
    };
    let request = gate_document(
        &GateInput {
            command_id: format!("gate-{}-{wiring_id}", package.package_id()),
            package,
            scope: AuthoringScope {
                project_id: arguments[2].clone(),
                environment: arguments[3].clone(),
            },
        },
        &wiring,
    )?;
    println!("{}", serde_json::to_string(&request)?);
    Ok(())
}
