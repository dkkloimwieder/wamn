//! Render and observe named Helm and CloudNativePG resources.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

use crate::Target;

pub fn entry(chart: &Path, namespace: &str, name: &str, values: Value) -> anyhow::Result<Value> {
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    let mut entry =
        json!({"chart":chart,"name":name,"namespace":namespace,"version":metadata["version"]});
    entry["values"] = values;
    Ok(entry)
}

pub fn resource(objects: Vec<Value>, type_: &str) -> anyhow::Result<Value> {
    objects
        .into_iter()
        .find(|object| object["kind"] == type_)
        .context("chart did not render the requested resource")
}

/// Wait for CloudNativePG to apply the current generation of a named database.
///
/// # Errors
/// Names the Database if Kubernetes cannot read it or it stays unapplied.
pub async fn wait_database_applied(
    target: &Target,
    database: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    use crate::client::{resource, wait_named};
    let databases = resource(
        target.client.client(),
        &target.namespace,
        "postgresql.cnpg.io",
        "v1",
        "Database",
        "databases",
    );
    wait_named(&databases, database, timeout, |object| {
        Ok(object
            .filter(|object| {
                object.data["status"]["applied"] == true
                    && object.metadata.generation.is_some_and(|generation| {
                        object.data["status"]["observedGeneration"].as_i64() == Some(generation)
                    })
            })
            .map(drop))
    })
    .await
}
