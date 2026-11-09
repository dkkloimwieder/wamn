//! Prepared chart fixtures shared by platform command and render tests.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wamn_release_control::release_chart::{self, ImageSet};

#[derive(Debug)]
pub struct Chart(pub PathBuf);

impl Drop for Chart {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

pub fn prepared_chart(repository: &Path) -> anyhow::Result<Chart> {
    let chart = Chart(std::env::temp_dir().join(format!(
        "wamn-chart-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos(),
    )));
    let digest = "a".repeat(64);
    release_chart::stamp(
        &repository.join("deploy/platform/release"),
        &ImageSet {
            host: format!("localhost:5001/host:host-build@sha256:{digest}"),
            http: format!("registry.example/components/http@sha256:{digest}"),
            materializer: format!("other.example/materializer:component-build@sha256:{digest}"),
        },
        None,
        &chart.0,
    )?;
    for name in ["identity", "edge"] {
        copy_directory(
            &repository.join("deploy/platform").join(name),
            &chart.0.join("platform").join(name),
        )?;
    }
    let resources = chart.0.join("platform/resources");
    std::fs::create_dir_all(&resources)?;
    // Match the existing fixture's source precedence without a file roster.
    for source in [
        "deploy/gcp",
        "deploy/kind",
        "deploy/platform",
        "deploy/infra",
    ] {
        for entry in std::fs::read_dir(repository.join(source))? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "yaml")
            {
                std::fs::copy(entry.path(), resources.join(entry.file_name()))?;
            }
        }
    }
    let path = chart.0.join("values.yaml");
    let mut values: Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    values["platform"]["ctl"] =
        json!({"image":format!("ctl.example/wamn-ctl:ctl-build@sha256:{digest}")});
    values["platform"]["identity"]["image"] =
        json!(format!("identity.example/identity@sha256:{digest}"));
    values["platform"]["edge"]["image"] = json!(format!("edge.example/edge@sha256:{digest}"));
    std::fs::write(path, serde_yaml::to_string(&values)?)?;
    Ok(chart)
}
