//! Kubernetes lifecycle for platform initialization and SQL Jobs.

use std::path::Path;

use anyhow::Context as _;
use serde_json::Value;
use wamn_kubernetes::job::{container, manifest, run};
use wamn_kubernetes::kubectl;
use wamn_package_control::migrations::{MigrationResolution, Resolution};
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::bootstrap::{self, SecretData};

/// Run one system SQL action through the existing bounded Job lifecycle.
pub async fn system_schema(
    target: &Target,
    directory: &Path,
    chart: &SelectedChart,
    apply: bool,
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<String> {
    let superuser: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-pg-superuser",
    )
    .await?
    .context("missing Secret platform/wamn-pg-superuser")?;
    let mut job = system_schema_job(chart.image("ctl")?, apply, resolution);
    let admin = bootstrap::administrator(&mut job, &superuser)?;
    run(target, directory, &mut job, &mut [admin]).await
}

fn system_schema_job(image: &str, apply: bool, resolution: Option<&MigrationResolution>) -> Value {
    let mut job = manifest(
        "platform-system-schema",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
            "system-schema",
            if apply { "apply" } else { "diff" },
            "--image",
            image,
        ],
    );
    if let Some(resolution) = resolution {
        container(&mut job)["args"].as_array_mut().unwrap().extend([
            "--resolve".into(),
            format!(
                "{}={}",
                resolution.path,
                match resolution.action {
                    Resolution::Applied => "applied",
                    Resolution::Retry => "retry",
                }
            )
            .into(),
        ]);
    }
    job
}
