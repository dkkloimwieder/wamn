//! Kubernetes lifecycle for platform initialization and SQL Jobs.

use std::path::Path;

use serde_json::Value;
use wamn_kubernetes::job::{container, manifest, run};
use wamn_release_control::release_chart::Target;

use super::artifact_plan::SelectedChart;
use super::bootstrap::{self};

/// Run one system SQL action through the existing bounded Job lifecycle.
pub async fn system_schema(
    target: &Target,
    directory: &Path,
    chart: &SelectedChart,
    apply: bool,
) -> anyhow::Result<String> {
    let mut job = system_schema_job(chart.image("ctl")?, apply);
    let input = crate::provision_project_env::workload::configuration(
        &target.namespace,
        "wamn-pg",
        bootstrap::database_host(&target.namespace),
        5432,
        "wamn_system".into(),
        "postgres".into(),
        "wamn-pg-superuser".into(),
    )
    .to_json()?;
    let migration =
        wamn_control_provision::migrations::MigrationDatabase::System.configuration(&input)?;
    job["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    container(&mut job)["args"]
        .as_array_mut()
        .unwrap()
        .extend(["--system-url".into(), migration.into()]);
    run(target, directory, &mut job, &mut []).await
}

fn system_schema_job(image: &str, apply: bool) -> Value {
    manifest(
        "platform-system-schema",
        image,
        &["/usr/local/bin/wamn-ctl"],
        &[
            "system-schema",
            if apply { "apply" } else { "diff" },
            "--image",
            image,
        ],
    )
}
