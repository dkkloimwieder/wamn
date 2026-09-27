//! Write the Google Cloud `flow-http` and materializer workloads of
//! docs/plan/gcp-deployment.md step 3.
//!
//! The documents come from `rendering::render_http_workload` and
//! `rendering::render_materializer`, the derivations of the Receiving cluster
//! tests, over `deploy/platform/http-route-workload.example.yaml` and
//! `deploy/platform/materializer.example.yaml`. The program writes
//! `flow-http.yaml` and `materializer.yaml` into the output directory.
//!
//! cargo run -p wamn-test-infrastructure --example workload_files -- \
//!   <output directory> <flow-http image> <materializer image>

use std::fs;
use std::path::PathBuf;

use anyhow::ensure;
use wamn_test_infrastructure::rendering::{
    EventIdentity, HttpClaims, HttpWorkloadInput, MaterializerInput, render_http_workload,
    render_materializer,
};

const ORG: &str = "dkk";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
const TENANT: &str = "dev";
const NAMESPACE: &str = "hosts";
const ROUTE_HOST: &str = "receiving.wamn.dev";
const EVENT_STREAM: &str = "EVT_3_dkk_9_receiving_3_dev";
/// Deployment values, not the test-speed values of the kind cases.
const FETCH_MS: u64 = 1000;
const SWEEP_MS: u64 = 5000;

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 3,
        "usage: <output directory> <flow-http image> <materializer image>"
    );
    let output = PathBuf::from(&arguments[0]);
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let http = render_http_workload(
        &fs::read_to_string(repository.join("deploy/platform/http-route-workload.example.yaml"))?,
        &HttpWorkloadInput {
            namespace: NAMESPACE.to_owned(),
            image: arguments[1].clone(),
            route_host: ROUTE_HOST.to_owned(),
            claims: HttpClaims {
                tenant: TENANT.to_owned(),
                catalog: "default".to_owned(),
                environment: NAMESPACE.to_owned(),
                project: PROJECT.to_owned(),
                schema: "receiving".to_owned(),
            },
        },
    )?;
    let materializer = render_materializer(
        &fs::read_to_string(repository.join("deploy/platform/materializer.example.yaml"))?,
        &MaterializerInput {
            workload: "receiving-materializer".to_owned(),
            namespace: NAMESPACE.to_owned(),
            image: arguments[2].clone(),
            tenant: TENANT.to_owned(),
            event: EventIdentity {
                org: ORG.to_owned(),
                project: PROJECT.to_owned(),
                environment: ENVIRONMENT.to_owned(),
            },
            event_stream: EVENT_STREAM.to_owned(),
            fetch_ms: FETCH_MS,
            sweep_ms: SWEEP_MS,
        },
    )?;
    fs::write(output.join("flow-http.yaml"), http)?;
    fs::write(output.join("materializer.yaml"), materializer)?;
    println!("{}", output.display());
    Ok(())
}
