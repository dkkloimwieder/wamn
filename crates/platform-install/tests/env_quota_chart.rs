//! The platform chart supplies the initial CPU request to the env SQL Job.

use std::{fs, path::Path};

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_platform_install::env_install::read_env_default_cpu_millicores;

#[tokio::test]
#[ignore = "requires: bin:helm"]
async fn only_kind_supplies_an_initial_env_cpu_request() -> anyhow::Result<()> {
    let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/charts/platform");
    let metadata: Value = serde_yaml::from_slice(&fs::read(chart.join("Chart.yaml"))?)?;
    for (values_file, expected) in [("values-kind.yaml", Some(10)), ("values-gcp.yaml", None)] {
        let values: Value = serde_yaml::from_slice(&fs::read(chart.join(values_file))?)?;
        let objects = wamn_kubernetes::helm::template(&json!({
            "chart": chart, "version": metadata["version"],
            "name": "wamn", "namespace": "platform", "values": values,
        }))?;
        let config = objects
            .iter()
            .find(|object| {
                object["kind"] == "ConfigMap" && object["metadata"]["name"] == "wamn-system-sql"
            })
            .context("find the env SQL Job's ConfigMap")?;
        let mount = tempfile::tempdir()?;
        for (name, value) in config["data"].as_object().context("ConfigMap data")? {
            fs::write(
                mount.path().join(name),
                value.as_str().context("ConfigMap file")?,
            )?;
        }
        let database = mount.path().join("database.json");
        assert_eq!(
            read_env_default_cpu_millicores(&database).await?,
            expected,
            "{values_file}"
        );
    }
    Ok(())
}
