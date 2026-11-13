//! Typed Secret reads and optimistic writes preserve concurrent changes.

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

/// Kubernetes metadata, including the version used for optimistic writes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub name: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_version: Option<String>,
    #[serde(default, flatten)]
    pub attributes: BTreeMap<String, Value>,
}

/// A Secret with base64 encoded data. Debug output excludes its values.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Secret {
    pub api_version: String,
    #[serde(rename = "kind")]
    pub resource_type: String,
    pub metadata: Metadata,
    #[serde(rename = "type", default = "opaque")]
    pub content_type: String,
    #[serde(default)]
    pub data: BTreeMap<String, String>,
    #[serde(flatten)]
    pub attributes: BTreeMap<String, Value>,
}
fn opaque() -> String {
    "Opaque".into()
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret")
            .field("name", &self.metadata.name)
            .field("namespace", &self.metadata.namespace)
            .finish_non_exhaustive()
    }
}
impl Secret {
    pub fn new(namespace: &str, name: &str) -> Self {
        Self {
            api_version: "v1".into(),
            resource_type: "Secret".into(),
            metadata: Metadata {
                name: name.into(),
                namespace: namespace.into(),
                ..Metadata::default()
            },
            content_type: opaque(),
            data: BTreeMap::new(),
            attributes: BTreeMap::new(),
        }
    }
}

/// Read, edit and replace a Secret, retrying a concurrent creation or write.
/// Every edit runs against the latest resource version.
pub async fn update(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    name: &str,
    mut edit: impl FnMut(&mut Secret) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Keep callers' suspended state small while the command runner waits for I/O.
    Box::pin(async {
        for attempt in 0..5 {
            let existing: Option<Secret> =
                crate::kubectl::get(kubeconfig, context, namespace, "secret", name).await?;
            let creates = existing.is_none();
            let mut secret = existing.unwrap_or_else(|| Secret::new(namespace, name));
            ensure!(
                creates || secret.metadata.resource_version.is_some(),
                "Secret has no resourceVersion"
            );
            edit(&mut secret)?;
            let body = serde_json::to_vec(&secret).context("encode Secret")?;
            let result = crate::kubectl::kubectl(
                kubeconfig,
                context,
                namespace,
                &[if creates { "create" } else { "replace" }, "-f", "-"],
                Some(&body),
            )
            .await;
            match result {
                Ok(_) => return Ok(()),
                Err(error) if error.is::<crate::kubectl::Conflict>() && attempt < 4 => {}
                Err(error) => return Err(error),
            }
        }
        unreachable!("the last write returns its error")
    })
    .await
}
