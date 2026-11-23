//! Discover web routes and releases from chart-managed ConfigMaps.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, api::ListParams};
use serde::Deserialize;
use wamn_registry::{name::valid_resource_segment, platform_domain::PlatformDomain};
use wamn_routes::valid_route_host;

use crate::client::{KubernetesApi, REQUEST_TIMEOUT};

/// One route discovered from a labelled ConfigMap in an org namespace.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    #[serde(skip)]
    pub namespace: String,
    host: String,
    pub release: String,
    pub api: String,
}

/// The deployed release and its pinned browser assets.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveRelease {
    pub host: String,
    pub digest: wamn_digest::Digest,
    pub clients: BTreeMap<String, wamn_package::PackageName>,
    pub webs: BTreeMap<String, wamn_digest::Digest>,
    pub platform_ui: Option<wamn_digest::Digest>,
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
}

/// Discover routes and read their releases from chart-managed ConfigMaps.
#[derive(Debug)]
pub struct Releases {
    api: KubernetesApi,
    platform_domain: PlatformDomain,
}
impl Releases {
    /// Use the supplied Kubernetes client and platform domain for discovery.
    pub fn new(api: KubernetesApi, platform_domain: PlatformDomain) -> Self {
        Self {
            api,
            platform_domain,
        }
    }

    /// List routes whose hosts match their namespace and platform domain.
    ///
    /// # Errors
    /// Returns an error if Kubernetes cannot list the route ConfigMaps.
    pub async fn routes(&self) -> anyhow::Result<BTreeMap<String, Route>> {
        let maps = Api::<ConfigMap>::all(self.api.client());
        let params = ListParams::default().labels("wamn.route=true");
        let mut objects = tokio::time::timeout(REQUEST_TIMEOUT, maps.list(&params))
            .await??
            .items;
        // Kubernetes list order is unspecified. The lowest ConfigMap name wins.
        objects.sort_unstable_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
        let mut routes = BTreeMap::new();
        for object in objects {
            let namespace = object.metadata.namespace.as_deref().unwrap_or_default();
            let name = object.metadata.name.as_deref().unwrap_or_default();
            let parsed = object
                .data
                .as_ref()
                .and_then(|data| data.get("route.json"))
                .context("route ConfigMap lacks route.json")
                .and_then(|json| serde_json::from_str::<Route>(json).context("invalid route.json"));
            let mut route = match parsed {
                Ok(route) => route,
                Err(error) => {
                    tracing::warn!(namespace, name, %error, "dropping malformed route");
                    continue;
                }
            };
            if !valid_route_host(&route.host, namespace, &self.platform_domain) {
                tracing::warn!(namespace, name, host = %route.host, "dropping route outside its namespace host pattern");
                continue;
            }
            namespace.clone_into(&mut route.namespace);
            routes
                .entry(std::mem::take(&mut route.host))
                .or_insert(route);
        }
        Ok(routes)
    }

    /// Read the release selected by a route.
    ///
    /// # Errors
    /// Returns an error for an invalid coordinate, failed read or invalid release data.
    pub async fn read(&self, namespace: &str, name: &str) -> anyhow::Result<LiveRelease> {
        ensure!(
            valid_resource_segment(namespace) && valid_resource_segment(name),
            "invalid release coordinate"
        );
        let maps = Api::<ConfigMap>::namespaced(self.api.client(), namespace);
        let object = tokio::time::timeout(REQUEST_TIMEOUT, maps.get(name)).await??;
        serde_json::from_str(
            object
                .data
                .as_ref()
                .and_then(|data| data.get("release.json"))
                .context("release ConfigMap lacks release.json")?,
        )
        .context("read the release ConfigMap")
    }
}
