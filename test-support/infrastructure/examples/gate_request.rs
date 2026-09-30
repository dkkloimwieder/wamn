//! Print the authoring gate request of one wiring, for docs/operations/gcp.md
//! section 5.
//!
//! The request comes from `declarations::gate_document`, the one the WMS
//! cluster case posts to the scenario worker's `/authoring` route, with the
//! same command id and scope.
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
    // The gate judges sealed ids, so the authored references resolve against
    // the wamn.json of the package that holds the document.
    let document = wamn_schema_generator::operation_reference::read_authored_document(
        Path::new(&arguments[4]),
        wamn_schema_generator::operation_reference::AuthoredDocument::Wiring,
    )?;
    let Some(wiring_id) = document["wiring-id"].as_str().map(str::to_owned) else {
        anyhow::bail!("the wiring document names no wiring-id")
    };
    let wiring = serde_json::to_string(&document)?;
    let request = gate_document(
        &GateInput {
            // The version keeps a gate of a new package version apart from the
            // recorded gate of the old one (docs/plan/kind-to-type.md §3.1 A11).
            command_id: format!(
                "gate-{}-{}-{wiring_id}",
                package.package_id(),
                package.package_version()
            ),
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
