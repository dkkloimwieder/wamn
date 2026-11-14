//! Installed Helm values are the authority for platform configuration.
use anyhow::{Context as _, ensure};
use serde_json::Value;
use wamn_release_control::release_chart::Target;

#[derive(Debug)]
pub struct InstalledPlatform {
    pub values: Value,
}
impl InstalledPlatform {
    pub async fn read(target: &Target) -> anyhow::Result<Option<Self>> {
        Ok(release_values(target, "platform", "wamn")
            .await?
            .map(|values| Self { values }))
    }
    pub fn control(&self) -> &Value {
        &self.values["control"]
    }
    pub fn identity(&self) -> &Value {
        &self.values["identity"]
    }
}

pub async fn release_values(
    target: &Target,
    namespace: &str,
    name: &str,
) -> anyhow::Result<Option<Value>> {
    let filter = format!("^{name}$");
    let listed = super::resources::helm(
        target,
        &[
            "list".as_ref(),
            "--all".as_ref(),
            "--namespace".as_ref(),
            namespace.as_ref(),
            "--filter".as_ref(),
            filter.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let releases: Vec<Value> = serde_json::from_slice(&listed).context("decode Helm releases")?;
    if !releases.iter().any(|release| release["name"] == name) {
        return Ok(None);
    }
    let bytes = super::resources::helm(
        target,
        &[
            "get".as_ref(),
            "values".as_ref(),
            name.as_ref(),
            "--namespace".as_ref(),
            namespace.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    )
    .await?;
    let values: Value = serde_json::from_slice(&bytes).context("decode Helm values")?;
    ensure!(
        values.is_object(),
        "installed Helm values must be an object"
    );
    Ok(Some(values))
}

pub fn host_image<'a>(installed: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    installed?
        .pointer("/runtime-operator/runtime/hostGroups")?
        .as_array()?
        .iter()
        .find(|group| group["name"] == name)?
        .get("image")
}
