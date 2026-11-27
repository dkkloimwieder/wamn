//! Discover web routes and release pins from Service and Namespace metadata.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
    time::Duration,
};

use anyhow::Context as _;
use futures_util::TryStreamExt as _;
use k8s_openapi::api::core::v1::{Namespace, Service};
use kube::{
    Api,
    api::{ListParams, ObjectMeta, WatchEvent, WatchParams},
    core::{ObjectList, PartialObjectMeta},
};
use serde::Deserialize;
use tokio::sync::RwLock;
use wamn_registry::{OrgId, platform_domain::PlatformDomain};
use wamn_routes::valid_route_host;

use crate::client::{KubernetesApi, REQUEST_TIMEOUT};

/// The chart-owned Service annotation with the route and its pinned browser assets.
pub const WEB_RELEASE_ANNOTATION: &str = "wamn.dev/web-release";

/// The provisioner-owned label that assigns an org to a namespace.
pub const ROUTE_ORG_LABEL: &str = "wamn.dev/org-name";

/// The API destination and static pins served from one deployed Service.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveRelease {
    pub host: String,
    pub api: String,
    pub digest: wamn_digest::Digest,
    pub clients: BTreeMap<String, wamn_package::PackageName>,
    pub webs: BTreeMap<String, wamn_digest::Digest>,
    pub platform_ui: Option<wamn_digest::Digest>,
    pub org: OrgId,
    pub project: Option<wamn_registry::ProjectId>,
}

/// The result of an in-memory lookup for a public host.
#[derive(Debug)]
pub enum RouteLookup {
    Loading,
    Missing,
    Found(Arc<LiveRelease>),
}

#[derive(Debug, PartialEq, Eq)]
struct RouteScope {
    org: OrgId,
    control: bool,
}

fn scope(name: &str, metadata: &ObjectMeta) -> Option<RouteScope> {
    let labels = metadata.labels.as_ref()?;
    let control = labels
        .get("wamn.dev/org")
        .is_some_and(|value| value == "true");
    let env = labels
        .get("wamn.dev/env")
        .is_some_and(|value| value == "true")
        && labels
            .get("wamn.dev/org-workloads")
            .is_some_and(|value| value == "true");
    let org: OrgId = labels.get(ROUTE_ORG_LABEL)?.parse().ok()?;
    ((control && name == org.as_str()) || (!control && env)).then_some(RouteScope { org, control })
}

fn service_release(
    (service, namespace): &ServiceKey,
    metadata: &ObjectMeta,
) -> Option<Arc<LiveRelease>> {
    let annotation = metadata.annotations.as_ref()?.get(WEB_RELEASE_ANNOTATION)?;
    match serde_json::from_str(annotation) {
        Ok(release) => Some(Arc::new(release)),
        Err(error) => {
            tracing::warn!(namespace, service, %error, "dropping malformed web release");
            None
        }
    }
}

type ServiceKey = (String, String);
type RouteTable = BTreeMap<String, Arc<LiveRelease>>;

#[derive(Debug, Default)]
struct Discovery {
    scopes: Option<BTreeMap<String, RouteScope>>,
    services: Option<BTreeMap<ServiceKey, Arc<LiveRelease>>>,
    routes: RouteTable,
}

impl Discovery {
    fn namespaces(&mut self, change: MetadataChange) -> bool {
        change.update(
            &mut self.scopes,
            |meta| meta.name.take(),
            |name, meta| scope(name, meta),
        )
    }

    fn services(&mut self, change: MetadataChange) -> bool {
        change.update(
            &mut self.services,
            |meta| Some((meta.name.take()?, meta.namespace.take()?)),
            service_release,
        )
    }

    fn ready(&self) -> bool {
        self.scopes.is_some() && self.services.is_some()
    }

    fn rebuild(&mut self, domain: &PlatformDomain) {
        let (Some(scopes), Some(services)) = (&self.scopes, &self.services) else {
            return;
        };
        self.routes.clear();
        for ((name, namespace), release) in services {
            if !scopes.get(namespace).is_some_and(|scope| {
                valid_route_host(&release.host, scope.org.as_str(), scope.control, domain)
            }) {
                tracing::warn!(namespace, service = name, host = %release.host, "dropping route outside its namespace host pattern");
                continue;
            }
            // Keep the first claim in Service name, then namespace order.
            match self.routes.entry(release.host.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(Arc::clone(release));
                }
                Entry::Occupied(_) => {
                    tracing::warn!(namespace, service = name, host = %release.host, "dropping duplicate web route");
                }
            }
        }
    }
}

enum MetadataChange {
    List(Vec<ObjectMeta>),
    Apply(ObjectMeta),
    Delete(ObjectMeta),
}

impl MetadataChange {
    fn update<K: Ord, V: PartialEq>(
        self,
        index: &mut Option<BTreeMap<K, V>>,
        key: impl Fn(&mut ObjectMeta) -> Option<K>,
        value: impl Fn(&K, &ObjectMeta) -> Option<V>,
    ) -> bool {
        match self {
            Self::List(objects) => {
                let next = objects
                    .into_iter()
                    .filter_map(|mut meta| {
                        let key = key(&mut meta)?;
                        let value = value(&key, &meta)?;
                        Some((key, value))
                    })
                    .collect();
                if index.as_ref() == Some(&next) {
                    return false;
                }
                *index = Some(next);
                true
            }
            Self::Apply(mut meta) => {
                let Some(key) = key(&mut meta) else {
                    return false;
                };
                let Some(index) = index.as_mut() else {
                    return false;
                };
                match value(&key, &meta) {
                    Some(value) if index.get(&key) != Some(&value) => {
                        index.insert(key, value);
                        true
                    }
                    Some(_) => false,
                    None => index.remove(&key).is_some(),
                }
            }
            Self::Delete(mut meta) => key(&mut meta)
                .and_then(|key| index.as_mut()?.remove(&key))
                .is_some(),
        }
    }
}

async fn list_metadata<K: Clone + std::fmt::Debug + serde::de::DeserializeOwned>(
    api: &Api<K>,
    selector: &str,
) -> anyhow::Result<(String, MetadataChange)> {
    // The client caps each response body. Page metadata that can include large annotations.
    let mut params = ListParams::default().labels(selector).limit(10);
    let mut objects = Vec::new();
    loop {
        let list: ObjectList<PartialObjectMeta<K>> =
            tokio::time::timeout(REQUEST_TIMEOUT, api.list_metadata(&params)).await??;
        objects.extend(list.items.into_iter().map(|item| item.metadata));
        if let Some(token) = list.metadata.continue_.filter(|token| !token.is_empty()) {
            params = params.continue_token(&token);
        } else {
            let revision = list
                .metadata
                .resource_version
                .context("metadata list has no resource version")?;
            return Ok((revision, MetadataChange::List(objects)));
        }
    }
}

async fn watch_metadata<K, F, Fut>(api: Api<K>, selector: &'static str, mut update: F)
where
    K: Clone + std::fmt::Debug + serde::de::DeserializeOwned,
    F: FnMut(MetadataChange) -> Fut,
    Fut: Future<Output = ()> + Send,
{
    loop {
        let result = async {
            let (revision, initial) = list_metadata(&api, selector).await?;
            update(initial).await;
            let params = WatchParams::default().labels(selector);
            let events =
                tokio::time::timeout(REQUEST_TIMEOUT, api.watch_metadata(&params, &revision))
                    .await??;
            tokio::pin!(events);
            while let Some(event) = events.try_next().await? {
                match event {
                    WatchEvent::Added(item) | WatchEvent::Modified(item) => {
                        update(MetadataChange::Apply(item.metadata)).await;
                    }
                    WatchEvent::Deleted(item) => {
                        update(MetadataChange::Delete(item.metadata)).await;
                    }
                    WatchEvent::Bookmark(_) => {}
                    WatchEvent::Error(error) => anyhow::bail!("metadata watch: {error:?}"),
                }
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, resource = std::any::type_name::<K>(), "refresh web route metadata");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Keep a route table from the two metadata watches.
#[derive(Debug)]
pub struct Releases {
    api: KubernetesApi,
    platform_domain: PlatformDomain,
    discovery: RwLock<Discovery>,
}

impl Releases {
    /// Make an empty route table that starts serving after both lists arrive.
    pub fn new(api: KubernetesApi, platform_domain: PlatformDomain) -> Self {
        Self {
            api,
            platform_domain,
            discovery: RwLock::default(),
        }
    }

    /// Resolve a host from memory without waiting for Kubernetes.
    pub async fn route(&self, host: &str) -> RouteLookup {
        let discovery = self.discovery.read().await;
        if !discovery.ready() {
            return RouteLookup::Loading;
        }
        match discovery.routes.get(host) {
            Some(release) => RouteLookup::Found(Arc::clone(release)),
            None => RouteLookup::Missing,
        }
    }

    /// Maintain the route table until the caller stops polling the watches.
    pub async fn watch(&self) {
        tokio::join!(
            watch_metadata(
                Api::<Namespace>::all(self.api.client()),
                ROUTE_ORG_LABEL,
                |change| async move {
                    let mut discovery = self.discovery.write().await;
                    if discovery.namespaces(change) {
                        discovery.rebuild(&self.platform_domain);
                    }
                }
            ),
            watch_metadata(
                Api::<Service>::all(self.api.client()),
                "",
                |change| async move {
                    let mut discovery = self.discovery.write().await;
                    if discovery.services(change) {
                        discovery.rebuild(&self.platform_domain);
                    }
                }
            ),
        );
    }

    /// Resolve the current routes with one metadata list per resource for a one-shot probe.
    ///
    /// # Errors
    /// Returns an error if a Kubernetes list fails.
    pub async fn list(&self) -> anyhow::Result<BTreeMap<String, Arc<LiveRelease>>> {
        let namespaces = Api::<Namespace>::all(self.api.client());
        let services = Api::<Service>::all(self.api.client());
        let ((_, namespaces), (_, services)) = tokio::try_join!(
            list_metadata(&namespaces, ROUTE_ORG_LABEL),
            list_metadata(&services, ""),
        )?;
        let mut discovery = Discovery::default();
        discovery.namespaces(namespaces);
        discovery.services(services);
        discovery.rebuild(&self.platform_domain);
        Ok(discovery.routes)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::{Request, Response, header::ACCEPT};
    use serde_json::{Value, json};
    use tokio::sync::{Notify, mpsc};

    use super::*;

    fn namespace(name: &str, labels: Value) -> ObjectMeta {
        ObjectMeta {
            name: Some(name.to_owned()),
            labels: Some(serde_json::from_value(labels).unwrap()),
            ..ObjectMeta::default()
        }
    }

    fn service(name: &str, namespace: &str, host: &str, pin: &str) -> Value {
        let release = json!({
            "host":host, "api":format!("http://{name}.{namespace}.svc.cluster.local"),
            "org":"acme", "project":"app", "digest":wamn_digest::bytes_sha256(b"release"),
            "clients":{(host):"app"}, "webs":{(host):wamn_digest::bytes_sha256(pin.as_bytes())},
        });
        json!({"metadata":{"name":name,"namespace":namespace,"annotations":{
            (WEB_RELEASE_ANNOTATION):release.to_string()
        }}})
    }

    fn env_labels(org: &str) -> Value {
        json!({(ROUTE_ORG_LABEL):org,"wamn.dev/env":"true","wamn.dev/org-workloads":"true"})
    }

    #[test]
    fn namespace_scope_filters_hosts_and_the_first_service_claim_wins() {
        let mut discovery = Discovery::default();
        let domain = "example.test".parse().unwrap();
        let host = "app--acme.example.test";
        let control_host = "acme.example.test";
        assert!(!discovery.ready());
        discovery.namespaces(MetadataChange::List(vec![
            namespace("acme--dev", env_labels("acme")),
            namespace("other--dev", env_labels("other")),
            namespace(
                "acme",
                json!({(ROUTE_ORG_LABEL):"acme","wamn.dev/org":"true"}),
            ),
        ]));
        assert!(!discovery.ready());
        let primary = service("b-http", "acme--dev", host, "primary");
        let duplicate = service("c-http", "acme--dev", host, "duplicate");
        let other_org = service("a-http", "other--dev", host, "foreign");
        let unknown = service("a-http", "acme--unknown", host, "unscoped");
        let control = service("c-acme-http", "acme", control_host, "control");
        discovery.services(MetadataChange::List(
            [duplicate, other_org, unknown, control, primary]
                .into_iter()
                .map(|mut item| serde_json::from_value(item["metadata"].take()).unwrap())
                .collect(),
        ));
        discovery.rebuild(&domain);
        assert!(discovery.ready());
        assert_eq!(
            discovery.routes[host].webs[host],
            wamn_digest::bytes_sha256(b"primary")
        );
        assert!(discovery.routes.contains_key(control_host));

        assert!(discovery.namespaces(MetadataChange::Apply(namespace(
            "acme--dev",
            env_labels("other"),
        ))));
        discovery.rebuild(&domain);
        assert!(!discovery.routes.contains_key(host));
        assert!(discovery.routes.contains_key(control_host));
    }

    #[tokio::test]
    async fn service_watch_retains_routes_until_relist_and_applies_event_changes() {
        let host = "app--acme.example.test";
        let ns = "acme--dev";
        let first = service("app-http", ns, host, "first");
        let second = service("app-http", ns, host, "updated");
        let mut responses = [
            ("limit=", json!({"metadata":{"resourceVersion":"1","continue":"next"},"items":[]}).to_string()),
            ("continue=next", json!({"metadata":{"resourceVersion":"1"},"items":[first]}).to_string()),
            ("resourceVersion=1", format!("{}\n{}\n",
                json!({"type":"MODIFIED","object":second}),
                json!({"type":"ERROR","object":{"code":410,"reason":"Expired"}}),
            )),
            ("limit=", json!({"metadata":{"resourceVersion":"3"},"items":[service("replacement-http", ns, host, "relisted")]}).to_string()),
            ("resourceVersion=3", format!("{}\n", json!({"type":"DELETED","object":{
                "metadata":{"name":"replacement-http","namespace":ns}
            }}))),
        ].into_iter();
        let relist = Arc::new(Notify::new());
        let resume = Arc::clone(&relist);
        let transport = tower::service_fn(move |request: Request<kube::client::Body>| {
            let response = responses.next();
            let resume = Arc::clone(&resume);
            async move {
                let Some((expected_query, response)) = response else {
                    return std::future::pending().await;
                };
                let query = request.uri().query().unwrap_or_default();
                assert!(query.contains(expected_query), "{query}");
                let accept = request.headers()[ACCEPT].to_str().unwrap();
                if query.contains("watch=true") {
                    assert!(!accept.contains("PartialObjectMetadataList"));
                } else {
                    assert!(accept.contains("PartialObjectMetadataList"));
                    if response.contains("replacement-http") {
                        resume.notified().await;
                    }
                }
                assert!(accept.contains("as=PartialObjectMetadata"));
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(response))))
            }
        });
        let client = kube::Client::new(transport, ns);
        let (changes, mut updates) = mpsc::unbounded_channel();
        let watcher = tokio::spawn(async move {
            watch_metadata(Api::<Service>::all(client), "", |change| {
                changes.send(change).unwrap();
                std::future::ready(())
            })
            .await;
        });

        let mut discovery = Discovery::default();
        let domain = "example.test".parse().unwrap();
        discovery.namespaces(MetadataChange::List(vec![namespace(
            ns,
            env_labels("acme"),
        )]));
        tokio::time::timeout(Duration::from_secs(5), async {
            for pin in [b"first".as_slice(), b"updated"] {
                assert!(discovery.services(updates.recv().await.unwrap()));
                discovery.rebuild(&domain);
                let route = &discovery.routes[host];
                assert_eq!(route.digest, wamn_digest::bytes_sha256(b"release"));
                assert_eq!(route.webs[host], wamn_digest::bytes_sha256(pin));
            }
            relist.notify_one();
            assert!(discovery.services(updates.recv().await.unwrap()));
            discovery.rebuild(&domain);
            assert_eq!(
                discovery.routes[host].webs[host],
                wamn_digest::bytes_sha256(b"relisted")
            );
            assert!(discovery.services(updates.recv().await.unwrap()));
            discovery.rebuild(&domain);
            assert!(!discovery.routes.contains_key(host));
        })
        .await
        .expect("apply watch updates and the replacement list");
        watcher.abort();
    }
}
