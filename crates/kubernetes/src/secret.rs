//! Typed Secret documents.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Kubernetes metadata.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub name: String,
    #[serde(default)]
    pub namespace: String,
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
    /// Build the username and password Secret that CloudNativePG reads.
    pub fn basic_auth(namespace: &str, name: &str, username: &str, password: &str) -> Self {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mut secret = Self::new(namespace, name);
        secret.content_type = "kubernetes.io/basic-auth".into();
        secret
            .data
            .insert("username".into(), STANDARD.encode(username));
        secret
            .data
            .insert("password".into(), STANDARD.encode(password));
        secret.metadata.attributes.insert(
            "labels".into(),
            serde_json::json!({"cnpg.io/reload":"true"}),
        );
        secret
    }

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
