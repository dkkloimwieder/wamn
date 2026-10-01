//! Write the Google Cloud workloads of docs/operations/gcp.md sections 3.20
//! and 5.
//!
//! The documents come from `rendering::render_http_workload` and
//! `materializer_workload::render_materializer`, the derivations of the cluster tests,
//! over `deploy/platform/http-route-workload.example.yaml` and
//! `deploy/platform/materializer.example.yaml`. The program writes
//! `flow-http.yaml` and `materializer.yaml` for Receiving on host group
//! `default`, and `wms-flow-http.yaml` and `wms-materializer.yaml` for WMS on
//! host group `wms`, into the output directory.
//!
//! cargo run -p wamn-test-infrastructure --example workload_files -- \
//!   <output directory> <Receiving flow-http image> <Receiving materializer image> \
//!   <WMS flow-http image> <WMS materializer image>

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_yaml::Value;
use wamn_control_provision::materializer_workload::{
    EventIdentity, FETCH_MS, MaterializerInput, SWEEP_MS, render_materializer,
};
use wamn_test_infrastructure::rendering::{HttpClaims, HttpWorkloadInput, render_http_workload};

const ORG: &str = "dkk";
const ENVIRONMENT: &str = "dev";
const NAMESPACE: &str = "hosts";

/// One application's workloads.
struct Application {
    project: &'static str,
    tenant: &'static str,
    route_host: &'static str,
    event_stream: &'static str,
    host_group: &'static str,
    /// The name of the HTTP workload and its Service.
    http_name: &'static str,
    materializer_name: &'static str,
    http_file: &'static str,
    materializer_file: &'static str,
}

const RECEIVING: Application = Application {
    project: "receiving",
    tenant: "dev",
    route_host: "receiving.wamn.dev",
    event_stream: "EVT_3_dkk_9_receiving_3_dev",
    host_group: "default",
    http_name: "flow-http",
    materializer_name: "receiving-materializer",
    http_file: "flow-http.yaml",
    materializer_file: "materializer.yaml",
};

const WMS: Application = Application {
    project: "wms",
    tenant: "wms",
    route_host: "wms.wamn.dev",
    event_stream: "EVT_3_dkk_3_wms_3_dev",
    host_group: "wms",
    http_name: "wms-flow-http",
    materializer_name: "wms-materializer",
    http_file: "wms-flow-http.yaml",
    materializer_file: "wms-materializer.yaml",
};

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 5,
        "usage: <output directory> <Receiving flow-http image> <Receiving materializer image> <WMS flow-http image> <WMS materializer image>"
    );
    let output = PathBuf::from(&arguments[0]);
    write(&output, &RECEIVING, &arguments[1], &arguments[2])?;
    write(&output, &WMS, &arguments[3], &arguments[4])?;
    println!("{}", output.display());
    Ok(())
}

fn write(
    output: &std::path::Path,
    application: &Application,
    http_image: &str,
    materializer_image: &str,
) -> anyhow::Result<()> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let http = render_http_workload(
        &fs::read_to_string(repository.join("deploy/platform/http-route-workload.example.yaml"))?,
        &HttpWorkloadInput {
            namespace: NAMESPACE.to_owned(),
            image: http_image.to_owned(),
            route_host: application.route_host.to_owned(),
            claims: HttpClaims {
                tenant: application.tenant.to_owned(),
                catalog: "default".to_owned(),
                environment: NAMESPACE.to_owned(),
                project: application.project.to_owned(),
                schema: application.project.to_owned(),
            },
        },
    )?;
    let materializer = render_materializer(
        &fs::read_to_string(repository.join("deploy/platform/materializer.example.yaml"))?,
        &MaterializerInput {
            workload: application.materializer_name.to_owned(),
            namespace: NAMESPACE.to_owned(),
            image: materializer_image.to_owned(),
            tenant: application.tenant.to_owned(),
            event: EventIdentity {
                org: ORG.to_owned(),
                project: application.project.to_owned(),
                environment: ENVIRONMENT.to_owned(),
            },
            event_stream: application.event_stream.to_owned(),
            fetch_ms: FETCH_MS,
            sweep_ms: SWEEP_MS,
        },
    )?;
    let (http, materializer) = if application.host_group == RECEIVING.host_group {
        (http, materializer)
    } else {
        (
            place_http(&http, application)?,
            place(&materializer, application.host_group)?,
        )
    };
    fs::write(output.join(application.http_file), http)?;
    fs::write(output.join(application.materializer_file), materializer)?;
    Ok(())
}

/// Name the Service and the HTTP workload, and select the host group.
fn place_http(documents: &str, application: &Application) -> anyhow::Result<String> {
    let mut rendered = Vec::new();
    for document in serde_yaml::Deserializer::from_str(documents) {
        let mut value = Value::deserialize(document)?;
        value["metadata"]["name"] = application.http_name.into();
        if value["kind"] == "Service" {
            value["metadata"]["labels"]["app"] = application.http_name.into();
        } else {
            value["spec"]["template"]["spec"]["kubernetes"]["service"]["name"] =
                application.http_name.into();
            value = serde_yaml::from_str(&place(
                &serde_yaml::to_string(&value)?,
                application.host_group,
            )?)?;
        }
        rendered.push(serde_yaml::to_string(&value)?);
    }
    Ok(rendered.join("---\n"))
}

/// Select the host group of a workload.
fn place(document: &str, host_group: &str) -> anyhow::Result<String> {
    let mut value: Value = serde_yaml::from_str(document)?;
    let selector = value["spec"]["template"]["spec"]
        .get_mut("hostSelector")
        .context("the workload has a hostSelector")?;
    selector["hostgroup"] = host_group.into();
    Ok(serde_yaml::to_string(&value)?)
}
