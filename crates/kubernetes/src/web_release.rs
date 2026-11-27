//! Discover web routes and releases from chart-managed ConfigMaps.

use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context as _, ensure};
use futures_util::TryStreamExt as _;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{
    Api,
    api::{ListParams, WatchEvent, WatchParams},
};
use serde::Deserialize;
use tokio::sync::RwLock;
use wamn_registry::{name::valid_resource_segment, platform_domain::PlatformDomain};
use wamn_routes::valid_route_host;

use crate::client::{KubernetesApi, REQUEST_TIMEOUT};

/// One route discovered from a labeled ConfigMap in an org or environment namespace.
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

/// The provisioner-owned labels that assign an org and a route kind to a namespace.
pub const ROUTE_ORG_LABEL: &str = "wamn.dev/org-name";

#[derive(Debug)]
struct RouteScope {
    org: wamn_registry::OrgId,
    control: bool,
}

fn update_scope(scopes: &mut BTreeMap<String, RouteScope>, namespace: Namespace) {
    let Some(name) = namespace.metadata.name else {
        return;
    };
    let labels = namespace.metadata.labels.unwrap_or_default();
    let control = labels
        .get("wamn.dev/org")
        .is_some_and(|value| value == "true");
    let env = labels
        .get("wamn.dev/env")
        .is_some_and(|value| value == "true")
        && labels
            .get("wamn.dev/org-workloads")
            .is_some_and(|value| value == "true");
    let org = labels
        .get(ROUTE_ORG_LABEL)
        .and_then(|value| value.parse().ok());
    if let Some(org) = org
        .filter(|org: &wamn_registry::OrgId| (control && name == org.as_str()) || (!control && env))
    {
        scopes.insert(name, RouteScope { org, control });
    } else {
        scopes.remove(&name);
    }
}

/// Discover routes and read their releases from chart-managed ConfigMaps.
#[derive(Debug)]
pub struct Releases {
    api: KubernetesApi,
    platform_domain: PlatformDomain,
    scopes: RwLock<BTreeMap<String, RouteScope>>,
}
impl Releases {
    /// Load the initial route scopes from the provisioner-owned Namespace labels.
    ///
    /// # Errors
    /// Returns an error if Kubernetes cannot list those namespaces.
    pub async fn new(api: KubernetesApi, platform_domain: PlatformDomain) -> anyhow::Result<Self> {
        let releases = Self {
            api,
            platform_domain,
            scopes: RwLock::default(),
        };
        releases.list_namespaces().await?;
        Ok(releases)
    }

    async fn list_namespaces(&self) -> anyhow::Result<String> {
        let api = Api::<Namespace>::all(self.api.client());
        let params = ListParams::default().labels(ROUTE_ORG_LABEL);
        let list = tokio::time::timeout(REQUEST_TIMEOUT, api.list(&params)).await??;
        let mut scopes = BTreeMap::new();
        for namespace in list.items {
            update_scope(&mut scopes, namespace);
        }
        *self.scopes.write().await = scopes;
        Ok(list.metadata.resource_version.unwrap_or_default())
    }

    /// Refresh Namespace labels for route discovery until the server shuts down.
    pub async fn watch_namespaces(&self) {
        loop {
            if let Err(error) = self.watch_namespace_changes().await {
                tracing::warn!(%error, "refresh web route namespace labels");
                self.scopes.write().await.clear();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    async fn watch_namespace_changes(&self) -> anyhow::Result<()> {
        let revision = self.list_namespaces().await?;
        let api = Api::<Namespace>::all(self.api.client());
        let params = WatchParams::default().labels(ROUTE_ORG_LABEL);
        let changes =
            tokio::time::timeout(REQUEST_TIMEOUT, api.watch(&params, &revision)).await??;
        tokio::pin!(changes);
        while let Some(event) = changes.try_next().await? {
            match event {
                WatchEvent::Added(namespace) | WatchEvent::Modified(namespace) => {
                    update_scope(&mut *self.scopes.write().await, namespace);
                }
                WatchEvent::Deleted(namespace) => {
                    if let Some(name) = namespace.metadata.name {
                        self.scopes.write().await.remove(&name);
                    }
                }
                WatchEvent::Bookmark(_) => {}
                WatchEvent::Error(error) => anyhow::bail!("Namespace watch: {error:?}"),
            }
        }
        Ok(())
    }

    /// List routes whose hosts match their namespace labels and the platform domain.
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
        objects.sort_unstable_by(|a, b| {
            (&a.metadata.name, &a.metadata.namespace)
                .cmp(&(&b.metadata.name, &b.metadata.namespace))
        });
        let mut routes = BTreeMap::new();
        let scopes = self.scopes.read().await;
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
            if !scopes.get(namespace).is_some_and(|scope| {
                valid_route_host(
                    &route.host,
                    scope.org.as_str(),
                    scope.control,
                    &self.platform_domain,
                )
            }) {
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

#[cfg(test)]
mod tests {
    use super::{Namespace, ROUTE_ORG_LABEL, RouteScope, update_scope};
    use std::collections::BTreeMap;

    #[test]
    fn updating_namespace_labels_replaces_and_removes_the_route_scope() -> anyhow::Result<()> {
        let name = "acme--dev";
        let mut scopes: BTreeMap<String, RouteScope> = BTreeMap::new();
        for org in ["acme", "another-org"] {
            update_scope(
                &mut scopes,
                serde_json::from_value(serde_json::json!({"metadata":{
                    "name":name,
                    "labels":{
                        "wamn.dev/env":"true", "wamn.dev/org-workloads":"true",
                        (ROUTE_ORG_LABEL):org,
                    }
                }}))?,
            );
            let scope = scopes.get(name).expect("environment route scope");
            assert_eq!(scope.org.as_str(), org);
            assert!(!scope.control);
        }
        update_scope(
            &mut scopes,
            serde_json::from_value::<Namespace>(serde_json::json!({"metadata":{"name":name}}))?,
        );
        assert!(!scopes.contains_key(name));
        Ok(())
    }
}
