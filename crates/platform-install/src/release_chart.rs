//! Platform chart image selection and registry addresses.

use std::borrow::Cow;

/// Registry Service address for the local cluster.
pub const POD_REGISTRY: &str = "wamn-registry.platform.svc.cluster.local:5000";

/// Replace a loopback OCI registry authority with the registry reachable from pods.
pub fn pod_registry_reference(reference: &str) -> Cow<'_, str> {
    let (scheme, name) = match reference.strip_prefix("oci://") {
        Some(name) => ("oci://", name),
        None => ("", reference),
    };
    if let Some((authority, path)) = name.split_once('/')
        && wamn_registry::registry_is_loopback(authority)
    {
        return Cow::Owned(format!("{scheme}{POD_REGISTRY}/{path}"));
    }
    Cow::Borrowed(reference)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSet {
    /// `<repository>:<tag>@sha256:<hex>`.
    pub host: String,
    /// `<repository>@sha256:<hex>`.
    pub ingress: String,
    /// `<repository>@sha256:<hex>`.
    pub workflow: String,
}

/// The OCI reference checks that a prepared platform chart requires.
pub trait ChartImageValidation {
    fn require_host_tag(&self, image: &str) -> anyhow::Result<()>;
    fn require_image_digest(&self, image: &str) -> anyhow::Result<()>;
}
