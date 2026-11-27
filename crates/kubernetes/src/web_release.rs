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
use wamn_registry::{
    AppName, EnvName, OrgId, ProjectId, configuration::Client, name,
    platform_domain::PlatformDomain,
};

use crate::client::{KubernetesApi, REQUEST_TIMEOUT};

/// The chart-owned Service annotation with the route and its pinned browser assets.
pub const WEB_RELEASE_ANNOTATION: &str = "wamn.dev/web-release";

/// The provisioner-owned label that assigns an org to a namespace.
pub const ROUTE_ORG_LABEL: &str = "wamn.dev/org-name";

/// The coordinates and artifact pins published on a Service.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseAnnotation {
    pub digest: wamn_digest::Digest,
    pub project: Option<ProjectId>,
    pub app: Option<AppName>,
    pub client: Option<Client>,
    pub platform_ui: Option<wamn_digest::Digest>,
}

impl ReleaseAnnotation {
    /// The public mount if this record describes an application.
    pub fn app_mount(&self) -> Option<String> {
        let (project, app) = (self.project.as_ref()?, self.app.as_ref()?);
        (self.platform_ui.is_none() && name::valid_resource_segment(app.as_str()))
            .then(|| name::app_mount(project, app))
    }
}

/// The discovered Service and its release.
#[derive(Debug, PartialEq, Eq)]
pub struct LiveRelease {
    pub upstream_host: String,
    pub org: OrgId,
    pub release: Arc<ReleaseAnnotation>,
    /// The public mount, including its trailing slash.
    pub mount: Option<String>,
}

/// The result of an in-memory lookup for a public host and path.
#[derive(Debug)]
pub enum RouteLookup {
    Loading,
    Missing,
    Found(Arc<LiveRelease>),
}

#[derive(Debug, PartialEq, Eq)]
struct RouteScope {
    org: OrgId,
    env: Option<EnvName>,
    prod: bool,
}

impl RouteScope {
    fn host(&self, domain: &PlatformDomain) -> String {
        match &self.env {
            Some(env) if !self.prod => name::env_host(&self.org, env, domain),
            _ => name::org_host(&self.org, domain),
        }
    }
}

fn scope(namespace: &str, metadata: &ObjectMeta) -> Option<RouteScope> {
    let labels = metadata.labels.as_ref()?;
    let marked = |key| labels.get(key).is_some_and(|value| value == "true");
    let org: OrgId = labels.get(ROUTE_ORG_LABEL)?.parse().ok()?;
    let env = if marked("wamn.dev/org") {
        if namespace != org.as_str() {
            return None;
        }
        None
    } else {
        if !marked("wamn.dev/env") || !marked("wamn.dev/org-workloads") {
            return None;
        }
        let (prefix, env) = namespace.split_once("--")?;
        if prefix != org.as_str() {
            return None;
        }
        Some(env.parse().ok()?)
    };
    Some(RouteScope {
        org,
        env,
        prod: marked("wamn.dev/prod"),
    })
}

fn service_release(
    (service, namespace): &ServiceKey,
    metadata: &ObjectMeta,
) -> Option<Arc<ReleaseAnnotation>> {
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

#[derive(Debug, Default)]
struct HostRoutes {
    control: Option<Arc<LiveRelease>>,
    // Use the mount without its final slash as the key so both forms resolve.
    apps: BTreeMap<String, Arc<LiveRelease>>,
}

impl HostRoutes {
    fn route(&self, path: &str) -> Option<&Arc<LiveRelease>> {
        let control = self.control.as_ref();
        let Some(path) = path.strip_prefix('/') else {
            return control;
        };
        let Some((project, rest)) = path.split_once('/') else {
            return control;
        };
        let (app, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if matches!(
            project,
            "api" | "password" | "oauth" | "wamn_control" | "assets" | "clients" | "platform-ui"
        ) {
            return control;
        }
        let key = &path[..project.len() + 1 + app.len()];
        match self.apps.get(key) {
            Some(route) => Some(route),
            // A missing application API must not fall through to the org's web client.
            None if (tail.starts_with("api/") || tail.starts_with("wamn_control/"))
                && project.parse::<ProjectId>().is_ok() =>
            {
                None
            }
            None => control,
        }
    }
}

#[derive(Debug, Default)]
struct Discovery {
    scopes: Option<BTreeMap<String, RouteScope>>,
    services: Option<BTreeMap<ServiceKey, Arc<ReleaseAnnotation>>>,
    routes: BTreeMap<String, HostRoutes>,
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
        let mut controls = BTreeMap::new();
        for ((service, namespace), release) in services {
            let Some(scope) = scopes.get(namespace) else {
                continue;
            };
            let mount = if scope.env.is_some() && service.ends_with("-http") {
                match release.app_mount() {
                    Some(mount) => Some(mount),
                    None => continue,
                }
            } else if scope.env.is_none()
                && *service == name::resource_suffix(&name::control_resource(&scope.org), "http")
                && release.project.is_none()
                && release.app.is_none()
                && release.client.is_none()
            {
                None
            } else {
                tracing::warn!(
                    namespace,
                    service,
                    "dropping release outside its namespace scope"
                );
                continue;
            };
            let route = Arc::new(LiveRelease {
                upstream_host: name::service_host(service, namespace),
                org: scope.org.clone(),
                release: Arc::clone(release),
                mount,
            });
            if let Some(mount) = &route.mount {
                let host = scope.host(domain);
                if host.len() > 253 {
                    continue;
                }
                let apps = &mut self.routes.entry(host).or_default().apps;
                // Keep the first claim in Service name, then namespace order.
                match apps.entry(mount[1..mount.len() - 1].to_owned()) {
                    Entry::Vacant(entry) => {
                        entry.insert(route);
                    }
                    Entry::Occupied(_) => {
                        tracing::warn!(namespace, service, mount, "dropping duplicate web route");
                    }
                }
            } else {
                controls.insert(&scope.org, route);
            }
        }
        for scope in scopes.values() {
            let Some(control) = controls.get(&scope.org) else {
                continue;
            };
            let host = scope.host(domain);
            if host.len() <= 253 {
                self.routes.entry(host).or_default().control = Some(Arc::clone(control));
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

    /// Resolve a host and path from memory without waiting for Kubernetes.
    pub async fn route(&self, host: &str, path: &str) -> RouteLookup {
        let discovery = self.discovery.read().await;
        if !discovery.ready() {
            return RouteLookup::Loading;
        }
        match discovery
            .routes
            .get(host)
            .and_then(|routes| routes.route(path))
        {
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
    pub async fn list(&self) -> anyhow::Result<BTreeMap<String, Vec<Arc<LiveRelease>>>> {
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
        Ok(discovery
            .routes
            .into_iter()
            .map(|(host, routes)| {
                let releases = routes
                    .control
                    .into_iter()
                    .chain(routes.apps.into_values())
                    .collect();
                (host, releases)
            })
            .collect())
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

    fn annotated_service(name: &str, namespace: &str, release: &Value) -> Value {
        json!({"metadata":{"name":name,"namespace":namespace,"annotations":{
            (WEB_RELEASE_ANNOTATION):release.to_string()
        }}})
    }

    fn service(name: &str, namespace: &str, app: &str, pin: &str) -> Value {
        annotated_service(
            name,
            namespace,
            &json!({
                "project":"shop", "app":app, "digest":wamn_digest::bytes_sha256(b"release"),
                "client":{"package":"app","web":wamn_digest::bytes_sha256(pin.as_bytes())}
            }),
        )
    }

    fn env_labels(org: &str) -> Value {
        json!({(ROUTE_ORG_LABEL):org,"wamn.dev/env":"true","wamn.dev/org-workloads":"true"})
    }

    #[test]
    fn namespace_coordinates_scope_apps_and_share_control_across_the_org_hosts() {
        let mut discovery = Discovery::default();
        let domain = "example.test".parse().unwrap();
        let dev = "dev.acme.example.test";
        let apex = "acme.example.test";
        let mut prod_labels = env_labels("acme");
        prod_labels["wamn.dev/prod"] = json!("true");
        discovery.namespaces(MetadataChange::List(vec![
            namespace("acme--dev", env_labels("acme")),
            namespace("acme--test", env_labels("acme")),
            namespace("acme--main", prod_labels),
            namespace("other--dev", env_labels("acme")),
            namespace(
                "acme",
                json!({(ROUTE_ORG_LABEL):"acme","wamn.dev/org":"true"}),
            ),
        ]));
        assert!(!discovery.ready());
        let objects = [
            service("b-http", "acme--dev", "stock", "first"),
            service("c-http", "acme--dev", "stock", "duplicate"),
            service("d-http", "acme--dev", "orders", "orders"),
            service("a-http", "acme--main", "stock", "prod"),
            service("a-http", "other--dev", "stock", "foreign"),
            annotated_service(
                "c-acme-http",
                "acme",
                &json!({"digest":wamn_digest::bytes_sha256(b"control")}),
            ),
            annotated_service(
                "e-http",
                "acme--test",
                &json!({
                    "host":apex, "digest":wamn_digest::bytes_sha256(b"bad"),
                    "project":"shop", "app":"stock"
                }),
            ),
        ];
        discovery.services(MetadataChange::List(
            objects
                .into_iter()
                .map(|mut item| serde_json::from_value(item["metadata"].take()).unwrap())
                .collect(),
        ));
        discovery.rebuild(&domain);
        assert!(discovery.ready());
        let routes = &discovery.routes;
        for (host, path, pin, upstream) in [
            (
                dev,
                "/shop/stock",
                "first",
                "b-http.acme--dev.svc.cluster.local",
            ),
            (
                dev,
                "/shop/orders/api/health",
                "orders",
                "d-http.acme--dev.svc.cluster.local",
            ),
            (
                apex,
                "/shop/stock/",
                "prod",
                "a-http.acme--main.svc.cluster.local",
            ),
        ] {
            let route = routes[host].route(path).unwrap();
            assert_eq!(
                route.release.client.as_ref().unwrap().web,
                wamn_digest::bytes_sha256(pin.as_bytes())
            );
            assert_eq!(route.upstream_host, upstream);
        }
        for host in [apex, dev, "test.acme.example.test"] {
            for path in [
                "/",
                "/config.json",
                "/password/login",
                "/oauth/callback",
                "/wamn_control/control/mine",
            ] {
                let route = routes[host].route(path).unwrap();
                assert!(route.mount.is_none());
                assert_eq!(route.upstream_host, "c-acme-http.acme.svc.cluster.local");
            }
        }
        assert!(routes[apex].route("/shop/orders/api/health").is_none());
        assert!(routes[apex].route("/shop/orders/").unwrap().mount.is_none());
        assert!(
            routes[dev]
                .route("/shop/stockroom/")
                .unwrap()
                .mount
                .is_none()
        );
        assert!(routes["test.acme.example.test"].apps.is_empty());
        assert!(!routes.contains_key("other.example.test"));
        assert!(!routes.contains_key("main.acme.example.test"));
        assert!(!routes.contains_key("stock--acme.example.test"));

        assert!(discovery.namespaces(MetadataChange::Apply(namespace(
            "acme--dev",
            env_labels("other")
        ))));
        discovery.rebuild(&domain);
        assert!(!discovery.routes.contains_key(dev));
        assert!(discovery.routes.contains_key(apex));
    }

    #[tokio::test]
    async fn service_watch_retains_routes_until_relist_and_applies_event_changes() {
        let host = "dev.acme.example.test";
        let ns = "acme--dev";
        let first = service("app-http", ns, "app", "first");
        let second = service("app-http", ns, "app", "updated");
        let mut responses = [
            ("limit=", json!({"metadata":{"resourceVersion":"1","continue":"next"},"items":[]}).to_string()),
            ("continue=next", json!({"metadata":{"resourceVersion":"1"},"items":[first]}).to_string()),
            ("resourceVersion=1", format!("{}\n{}\n",
                json!({"type":"MODIFIED","object":second}),
                json!({"type":"ERROR","object":{"code":410,"reason":"Expired"}}),
            )),
            ("limit=", json!({"metadata":{"resourceVersion":"3"},"items":[service("replacement-http", ns, "app", "relisted")]}).to_string()),
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
                let route = discovery.routes[host].route("/shop/app/").unwrap();
                assert_eq!(route.release.digest, wamn_digest::bytes_sha256(b"release"));
                assert_eq!(
                    route.release.client.as_ref().unwrap().web,
                    wamn_digest::bytes_sha256(pin)
                );
            }
            relist.notify_one();
            assert!(discovery.services(updates.recv().await.unwrap()));
            discovery.rebuild(&domain);
            assert_eq!(
                discovery.routes[host]
                    .route("/shop/app/")
                    .unwrap()
                    .release
                    .client
                    .as_ref()
                    .unwrap()
                    .web,
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
