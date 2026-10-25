//! Print the authoring gate request of one wiring, for docs/operations/gcp.md
//! section 5.
//!
//! The request uses the public authoring contract.
//!
//! cargo run -p wamn-test-infrastructure --example gate_request -- \
//!   <package id> <package version> <project> <environment> <wiring document>

use std::path::Path;

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
    let document = wamn_control::author_workflow::read_wiring_document(Path::new(&arguments[4]))?;
    let request = gate_document(
        &GateInput {
            command_id: format!(
                "gate-{}-{}-{}",
                package.package_id(),
                package.package_version(),
                document.wiring_id
            ),
            package,
            scope: AuthoringScope {
                project_id: arguments[2].clone(),
                environment: arguments[3].clone(),
            },
        },
        &serde_json::to_string(&document)?,
    )?;
    println!("{}", serde_json::to_string(&request)?);
    Ok(())
}
