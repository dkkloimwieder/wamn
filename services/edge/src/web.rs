//! Route application hosts through chart-owned route and release ConfigMaps.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Full, combinators::UnsyncBoxBody};
use hyper::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::service::service_fn;
use hyper::{Method, Response, StatusCode};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use object_store::{ObjectStore, ObjectStoreExt as _};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[cfg(test)]
mod rollout_tests;

/// The platform installer supplies the listener and bucket in a mounted file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    pub listen: SocketAddr,
    /// Empty on kind, otherwise `gs://<buckets.web>`.
    pub bucket: String,
}

/// One application-owned route, discovered from a labelled ConfigMap in hosts.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    host: String,
    release: String,
    api: String,
    files: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LiveRelease {
    host: String,
    digest: String,
    clients: BTreeMap<String, String>,
    webs: BTreeMap<String, String>,
    platform_ui: String,
    org: String,
}

impl LiveRelease {
    fn root(&self, host: &str) -> anyhow::Result<String> {
        ensure!(self.host == host, "the release does not serve this host");
        if !self.platform_ui.is_empty() {
            return Ok(format!(
                "platform-ui/sha256-{}",
                digest_hex(&self.platform_ui)?
            ));
        }
        let package = self
            .clients
            .get(host)
            .context("the host has no web client")?;
        ensure!(segment(package), "invalid client package");
        let hex = digest_hex(self.webs.get(host).context("the host has no web client")?)?;
        Ok(format!("clients/{package}/{hex}"))
    }
}

fn digest_hex(digest: &str) -> anyhow::Result<&str> {
    let hex = digest
        .strip_prefix("sha256:")
        .context("invalid web digest")?;
    ensure!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid web digest"
    );
    Ok(hex)
}

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[derive(Debug)]
struct Releases {
    api: wamn_kubernetes::kubernetes_api::KubernetesApi,
}
impl Releases {
    fn in_cluster() -> anyhow::Result<Self> {
        Ok(Self {
            api: wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?,
        })
    }
    async fn request(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        Ok(self
            .api
            .get(format!("/api/v1/namespaces/hosts/configmaps{path}"))
            .await?)
    }
    async fn routes(&self) -> anyhow::Result<BTreeMap<String, Route>> {
        let mut objects = self.request("?labelSelector=wamn.route%3Dtrue").await?;
        let objects = objects["items"]
            .as_array_mut()
            .context("route ConfigMap list has no items")?;
        // Kubernetes list order is unspecified. The lowest ConfigMap name wins.
        objects.sort_unstable_by(|a, b| {
            a["metadata"]["name"]
                .as_str()
                .cmp(&b["metadata"]["name"].as_str())
        });
        let mut routes = BTreeMap::new();
        for object in objects {
            let mut route: Route = serde_json::from_str(
                object["data"]["route.json"]
                    .as_str()
                    .context("route ConfigMap lacks route.json")?,
            )?;
            routes
                .entry(std::mem::take(&mut route.host))
                .or_insert(route);
        }
        Ok(routes)
    }

    async fn read(&self, name: &str) -> anyhow::Result<LiveRelease> {
        ensure!(segment(name), "invalid release name");
        let object = self.request(&format!("/{name}")).await?;
        serde_json::from_str(
            object["data"]["release.json"]
                .as_str()
                .context("release ConfigMap lacks release.json")?,
        )
        .context("read the release ConfigMap")
    }
}

#[derive(Debug, Clone)]
struct File {
    bytes: Bytes,
    content_type: String,
}

#[derive(Debug)]
struct Web {
    store: Option<Arc<dyn ObjectStore>>,
    /// The object path includes the pinned web artifact digest.
    files: RwLock<BTreeMap<String, File>>,
}

impl Web {
    async fn answer(
        &self,
        row: &LiveRelease,
        host: &str,
        path: &str,
        method: &Method,
    ) -> anyhow::Result<Response<Full<Bytes>>> {
        if method != Method::GET && method != Method::HEAD {
            return Ok(empty(StatusCode::METHOD_NOT_ALLOWED));
        }
        let root = row.root(host)?;
        let Some(store) = &self.store else {
            return Ok(empty(StatusCode::SERVICE_UNAVAILABLE));
        };
        let path = path.trim_start_matches('/');
        if path.split('/').any(|part| part == "." || part == "..") || path.contains(['%', '\\']) {
            return Ok(empty(StatusCode::BAD_REQUEST));
        }
        let (file, pinned) = if let Some(file) = path.strip_prefix(&format!("{root}/")) {
            (file, true)
        } else if path.starts_with("clients/") || path.starts_with("platform-ui/") {
            return Ok(empty(StatusCode::NOT_FOUND));
        } else if path == "config.json" || path.starts_with("assets/") {
            (path, false)
        } else {
            ("index.html", false)
        };
        let config = file == "config.json";
        let key = if config && !row.platform_ui.is_empty() {
            ensure!(segment(&row.org), "invalid platform UI org");
            format!("platform-ui/orgs/{}/config.json", row.org)
        } else {
            format!("{root}/{file}")
        };
        let cached = if config {
            None
        } else {
            self.files.read().await.get(&key).cloned()
        };
        let mut file = if let Some(file) = cached {
            file
        } else {
            let result = match store
                .get(&object_store::path::Path::from(key.clone()))
                .await
            {
                Ok(result) => result,
                Err(object_store::Error::NotFound { .. }) => {
                    return Ok(empty(StatusCode::NOT_FOUND));
                }
                Err(error) => return Err(error.into()),
            };
            let content_type = result
                .attributes
                .get(&object_store::Attribute::ContentType)
                .map_or_else(
                    || content_type(file).to_owned(),
                    |value| value.as_ref().to_owned(),
                );
            let file = File {
                bytes: result.bytes().await?,
                content_type,
            };
            if !config {
                self.files.write().await.insert(key, file.clone());
            }
            file
        };
        if config && row.platform_ui.is_empty() {
            let mut target: serde_json::Map<String, serde_json::Value> =
                serde_json::from_slice(&file.bytes).context("read web config.json")?;
            target.insert("release".into(), row.digest.clone().into());
            target.insert("web".into(), row.webs[host].clone().into());
            file.bytes = serde_json::to_vec(&target)?.into();
        }
        let length = file.bytes.len();
        Ok(Response::builder()
            .header(CONTENT_TYPE, file.content_type)
            .header(CONTENT_LENGTH, length)
            .header(
                CACHE_CONTROL,
                if pinned && !config {
                    IMMUTABLE
                } else {
                    "no-cache"
                },
            )
            .body(Full::new(if method == Method::HEAD {
                Bytes::new()
            } else {
                file.bytes
            }))?)
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn empty(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CACHE_CONTROL, "no-cache")
        .body(Full::new(Bytes::new()))
        .expect("a constant response is valid")
}

type ProxyBody = UnsyncBoxBody<Bytes, hyper::Error>;

fn boxed(response: Response<Full<Bytes>>) -> Response<ProxyBody> {
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

fn remove_hop_headers(headers: &mut hyper::HeaderMap) {
    let named: Vec<_> = headers
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| hyper::header::HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

async fn respond(
    web: &Web,
    releases: &Releases,
    proxy: &Client<HttpConnector, hyper::body::Incoming>,
    mut request: hyper::Request<hyper::body::Incoming>,
) -> anyhow::Result<Response<ProxyBody>> {
    let routes = releases.routes().await?;
    if request.uri().path() == "/healthz" {
        return Ok(boxed(empty(StatusCode::OK)));
    }
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(route) = routes.get(&host) else {
        return Ok(boxed(empty(StatusCode::NOT_FOUND)));
    };
    let path = request.uri().path();
    let file_request = !route.files.is_empty()
        && path
            .strip_prefix("/api/")
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(_, path)| path == "files" || path.starts_with("files/"));
    let api_request = path.starts_with("/api/");
    if api_request || path == "/oauth/callback" {
        let upstream = if file_request {
            &route.files
        } else {
            &route.api
        };
        let path = request
            .uri()
            .path_and_query()
            .context("request path")?
            .as_str();
        let path = if api_request && !file_request {
            path.strip_prefix("/api").expect("API prefix")
        } else {
            path
        };
        *request.uri_mut() = format!("{}{path}", upstream.trim_end_matches('/')).parse()?;
        remove_hop_headers(request.headers_mut());
        // Upstream ingress does not retire pooled connections when its host drains.
        // Select a current Service endpoint for each request, without replaying it.
        request.headers_mut().insert(
            hyper::header::CONNECTION,
            hyper::header::HeaderValue::from_static("close"),
        );
        request.headers_mut().insert(HOST, host.parse()?);
        let mut response = proxy.request(request).await?;
        remove_hop_headers(response.headers_mut());
        if api_request
            && !file_request
            && let Some(location) = response
                .headers()
                .get(hyper::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| {
                    location.strip_prefix(&format!("{}/", route.api.trim_end_matches('/')))
                })
        {
            let location = format!("/api/{location}").parse()?;
            response
                .headers_mut()
                .insert(hyper::header::LOCATION, location);
        }
        return Ok(response.map(http_body_util::BodyExt::boxed_unsync));
    }
    let row = releases.read(&route.release).await?;
    Ok(boxed(
        web.answer(&row, &host, path, request.method()).await?,
    ))
}

/// Run the cluster web server until interrupted. The box's `serve` mode is separate.
///
/// # Errors
/// Returns an error if the configuration, bucket client, Kubernetes CA or listener fails.
pub async fn serve(path: &Path) -> anyhow::Result<()> {
    let config: WebConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    let store: Option<Arc<dyn ObjectStore>> = if config.bucket.is_empty() {
        None
    } else {
        let bucket = config
            .bucket
            .strip_prefix("gs://")
            .context("web bucket must use gs://")?;
        ensure!(segment(bucket), "invalid web bucket");
        Some(Arc::new(
            object_store::gcp::GoogleCloudStorageBuilder::from_env()
                .with_bucket_name(bucket)
                .build()?,
        ))
    };
    let web = Arc::new(Web {
        store,
        files: RwLock::new(BTreeMap::new()),
    });
    let releases = Arc::new(Releases::in_cluster()?);
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(10)));
    let proxy = Client::builder(TokioExecutor::new()).build(connector);
    let listener = TcpListener::bind(config.listen).await?;
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; break; }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), proxy.clone());
                tasks.spawn(async move {
                    let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), proxy.clone());
                        async move {
                            Ok::<_, Infallible>(match respond(&web, &releases, &proxy, request).await {
                                Ok(response) => response,
                                Err(error) => {
                                    tracing::warn!(%error, "edge request failed");
                                    boxed(empty(StatusCode::BAD_GATEWAY))
                                }
                            })
                        }
                    });
                    if let Err(error) = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await {
                        tracing::debug!(%error, "web connection closed");
                    }
                });
            }
        }
    }
    tasks.shutdown().await;
    Ok(())
}

/// Require the live index and configuration of every declared host through the Service.
///
/// # Errors
/// Returns an error naming the host and path if a response is not successful.
pub async fn check(path: &Path) -> anyhow::Result<()> {
    let _: WebConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    let releases = Releases::in_cluster()?;
    let client: Client<HttpConnector, Empty<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    for (host, route) in releases.routes().await? {
        let row = releases.read(&route.release).await?;
        if row.platform_ui.is_empty() && !row.clients.contains_key(&host) {
            continue;
        }
        for path in ["/", "/config.json"] {
            let request = hyper::Request::builder()
                .uri(format!("http://127.0.0.1:8080{path}"))
                .header(HOST, &host)
                .body(Empty::new())?;
            tokio::time::timeout(Duration::from_secs(30), async {
                let response = client.request(request).await?;
                ensure!(
                    response.status() == StatusCode::OK,
                    "web check {host}{path}: {}",
                    response.status()
                );
                response.into_body().collect().await?;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .with_context(|| format!("web check {host}{path} timed out"))??;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn row(digit: char) -> LiveRelease {
        LiveRelease {
            host: "app.example.com".into(),
            digest: format!("sha256:{}", "c".repeat(64)),
            clients: [("app.example.com".into(), "app".into())].into(),
            webs: [(
                "app.example.com".into(),
                format!("sha256:{}", digit.to_string().repeat(64)),
            )]
            .into(),
            platform_ui: String::new(),
            org: String::new(),
        }
    }

    async fn put(store: &InMemory, root: &str, file: &str, bytes: &'static str) {
        store
            .put(
                &format!("{root}/{file}").into(),
                Bytes::from_static(bytes.as_bytes()).into(),
            )
            .await
            .unwrap();
    }

    async fn body(response: Response<Full<Bytes>>) -> Bytes {
        response.into_body().collect().await.unwrap().to_bytes()
    }

    #[tokio::test]
    async fn web_only_apply_changes_the_bundle_and_config_but_keeps_the_release() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(Arc::<InMemory>::clone(&store)),
            files: RwLock::new(BTreeMap::new()),
        };
        let first = row('a');
        let second = row('b');
        assert_ne!(
            first.root(&first.host).unwrap(),
            second.root(&second.host).unwrap()
        );
        for row in [&first, &second] {
            put(
                &store,
                &row.root(&row.host).unwrap(),
                "config.json",
                r#"{"org":"acme","project":"app","release":"stale","web":"stale"}"#,
            )
            .await;
            let response = web
                .answer(row, &row.host, "/config.json", &Method::GET)
                .await
                .unwrap();
            assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
            let config: serde_json::Value = serde_json::from_slice(&body(response).await).unwrap();
            assert_eq!(config["org"], "acme");
            assert_eq!(config["project"], "app");
            assert_eq!(config["release"], first.digest);
            assert_eq!(config["web"], row.webs[&row.host]);
        }
    }

    #[tokio::test]
    async fn discovered_routes_keep_services_and_web_pins_independent() {
        use serde_json::json;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let applications = BTreeMap::from([
            ("orders.example", "orders-release"),
            ("receiving.example", "receiving-release"),
        ]);
        let rows = Arc::new(RwLock::new(BTreeMap::new()));
        let store = Arc::new(InMemory::new());
        let mut objects = Vec::new();
        for (host, name) in &applications {
            let row = json!({"host":host, "digest":name, "clients":{*host:name},
                "webs":{*host:format!("sha256:{}", "a".repeat(64))}, "platformUi":"", "org":""});
            let release = LiveRelease::deserialize(&row).unwrap();
            put(&store, &release.root(host).unwrap(), "config.json", "{}").await;
            rows.write().await.insert(name.to_string(), row);
            let route = json!({"host":host,"release":name,
                "api":format!("{endpoint}/{name}-api"),"files":format!("{endpoint}/{name}-files")});
            objects.push(json!({"metadata":{"name":format!("{name}-route")},"data":{"route.json":route.to_string()}}));
        }
        // List order does not decide conflicts. The smallest ConfigMap name wins.
        let conflict = json!({"host":"orders.example","release":"wrong","api":format!("{endpoint}/wrong"),"files":""});
        objects.push(
            json!({"metadata":{"name":"z-conflict"},"data":{"route.json":conflict.to_string()}}),
        );
        objects.reverse();
        let mut tasks = tokio::task::JoinSet::new();
        let server_rows = Arc::clone(&rows);
        tasks.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let rows = Arc::clone(&server_rows);
                let objects = &objects;
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let rows = Arc::clone(&rows);
                    async move {
                        let path = request.uri().path();
                        let response = if path == "/api/v1/namespaces/hosts/configmaps" {
                            assert_eq!(request.uri().query(), Some("labelSelector=wamn.route%3Dtrue"));
                            json!({"items":objects})
                        } else if let Some(name) = path.strip_prefix("/api/v1/namespaces/hosts/configmaps/") {
                            json!({"data":{"release.json":rows.read().await[name].to_string()}})
                        } else {
                            let method = request.method().to_string();
                            let uri = request.uri().to_string();
                            let host = request.headers()[HOST].to_str().unwrap().to_owned();
                            let bytes = request.into_body().collect().await.unwrap().to_bytes();
                            json!({"method":method,"uri":uri,"host":host,"body":std::str::from_utf8(&bytes).unwrap()})
                        };
                        Ok::<_, Infallible>(Response::builder()
                            .header(hyper::header::CONNECTION, "close")
                            .body(Full::new(Bytes::from(response.to_string()))).unwrap())
                    }
                });
                hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await.unwrap();
            }
        });
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::aws_lc_rs::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
            )
            .https_or_http()
            .enable_http1()
            .build();
        let releases = Releases {
            api: wamn_kubernetes::kubernetes_api::KubernetesApi::with_client(
                Client::builder(TokioExecutor::new()).build(connector),
                endpoint,
                String::new(),
                "hosts".into(),
            ),
        };
        let web = Web {
            store: Some(store),
            files: RwLock::new(BTreeMap::new()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let edge = format!("http://{}", listener.local_addr().unwrap());
        tasks.spawn(async move {
            let proxy = Client::builder(TokioExecutor::new()).build_http();
            let (stream, _) = listener.accept().await.unwrap();
            let service = service_fn(|request| async {
                Ok::<_, Infallible>(respond(&web, &releases, &proxy, request).await.unwrap())
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        });
        let client: Client<HttpConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        for revision in ["orders-release", "orders-updated", "orders-release"] {
            rows.write().await.get_mut("orders-release").unwrap()["digest"] = revision.into();
            for (host, name) in &applications {
                for (path, prefix) in [
                    ("/api/query?escaped=%2F", "api"),
                    ("/api/package/files?name=a", "files"),
                ] {
                    let request = hyper::Request::builder()
                        .uri(format!("{edge}{path}"))
                        .method(Method::POST)
                        .header(HOST, *host)
                        .body(Full::new(Bytes::from_static(b"payload")))
                        .unwrap();
                    let response = client.request(request).await.unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    let bytes = response.into_body().collect().await.unwrap().to_bytes();
                    let routed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    let suffix = if prefix == "api" {
                        path.strip_prefix("/api").unwrap()
                    } else {
                        path
                    };
                    assert_eq!(
                        routed,
                        json!({"method":"POST", "uri":format!("/{name}-{prefix}{suffix}"),"host":host,"body":"payload"})
                    );
                }
                let request = hyper::Request::builder()
                    .uri(format!("{edge}/config.json"))
                    .header(HOST, *host)
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                let response = client.request(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    config["release"],
                    if *name == "orders-release" {
                        revision
                    } else {
                        name
                    }
                );
                assert_eq!(config["web"], format!("sha256:{}", "a".repeat(64)));
            }
        }
        let request = hyper::Request::builder()
            .uri(format!("{edge}/api/query"))
            .header(HOST, "unknown.example")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert_eq!(
            client.request(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        tasks.shutdown().await;
    }

    #[test]
    fn application_root_requires_a_package_and_a_valid_web_digest() {
        let mut row = row('a');
        assert_eq!(
            row.root(&row.host).unwrap(),
            format!("clients/app/{}", "a".repeat(64))
        );
        for digest in [
            "a".repeat(64),
            format!("sha512:{}", "a".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            format!("sha256:../{}", "a".repeat(61)),
        ] {
            row.webs.insert(row.host.clone(), digest);
            assert_eq!(
                row.root(&row.host).unwrap_err().to_string(),
                "invalid web digest"
            );
        }
        row.webs.clear();
        assert_eq!(
            row.root(&row.host).unwrap_err().to_string(),
            "the host has no web client"
        );
        row.webs
            .insert(row.host.clone(), format!("sha256:{}", "a".repeat(64)));
        row.clients.clear();
        assert_eq!(
            row.root(&row.host).unwrap_err().to_string(),
            "the host has no web client"
        );
    }

    #[tokio::test]
    async fn digest_cache_survives_a_bucket_change_but_the_live_row_selects_a_new_bundle() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(Arc::<InMemory>::clone(&store)),
            files: RwLock::new(BTreeMap::new()),
        };
        let first = row('a');
        let second = row('b');
        for (row, value) in [(&first, "first"), (&second, "second")] {
            put(&store, &row.root(&row.host).unwrap(), "index.html", value).await;
        }
        let response = web
            .answer(&first, &first.host, "/", &Method::GET)
            .await
            .unwrap();
        assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
        assert_eq!(body(response).await, "first");
        put(
            &store,
            &first.root(&first.host).unwrap(),
            "index.html",
            "changed",
        )
        .await;
        assert_eq!(
            body(
                web.answer(&second, &second.host, "/page", &Method::GET)
                    .await
                    .unwrap()
            )
            .await,
            "second"
        );
        assert_eq!(
            body(
                web.answer(&first, &first.host, "/", &Method::GET)
                    .await
                    .unwrap()
            )
            .await,
            "first"
        );
        let path = format!("/{}/index.html", first.root(&first.host).unwrap());
        let response = web
            .answer(&first, &first.host, &path, &Method::HEAD)
            .await
            .unwrap();
        assert_eq!(response.headers()[CACHE_CONTROL], IMMUTABLE);
        assert_eq!(response.headers()[CONTENT_LENGTH], "5");
        assert!(body(response).await.is_empty());
    }

    #[tokio::test]
    async fn config_is_fetched_again_and_never_immutable_even_at_a_digest_path() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(Arc::<InMemory>::clone(&store)),
            files: RwLock::new(BTreeMap::new()),
        };
        let row = row('a');
        let root = row.root(&row.host).unwrap();
        for (project, value) in [
            ("first", r#"{"org":"acme","project":"first"}"#),
            ("changed", r#"{"org":"acme","project":"changed"}"#),
        ] {
            put(&store, &root, "config.json", value).await;
            let response = web
                .answer(
                    &row,
                    &row.host,
                    &format!("/{root}/config.json"),
                    &Method::GET,
                )
                .await
                .unwrap();
            assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
            let length = response.headers()[CONTENT_LENGTH]
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let bytes = body(response).await;
            assert_eq!(length, bytes.len());
            let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(config["project"], project);
            assert_eq!(config["release"], row.digest);
            assert_eq!(config["web"], row.webs[&row.host]);
        }
        assert!(web.files.read().await.is_empty());
        let response = web
            .answer(&row, &row.host, "/config.json", &Method::HEAD)
            .await
            .unwrap();
        assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
        assert!(body(response).await.is_empty());
    }

    #[tokio::test]
    async fn platform_digest_selects_the_ui_and_unknown_paths_do_not_escape_it() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(Arc::<InMemory>::clone(&store)),
            files: RwLock::new(BTreeMap::new()),
        };
        let mut row = row('a');
        row.platform_ui = format!("sha256:{}", "a".repeat(64));
        row.org = "acme".into();
        let root = row.root(&row.host).unwrap();
        row.webs.clear();
        put(&store, &root, "index.html", "platform").await;
        let config = r#"{"org":"acme"}"#;
        put(&store, "platform-ui/orgs/acme", "config.json", config).await;
        assert_eq!(
            body(
                web.answer(&row, &row.host, "/config.json", &Method::GET)
                    .await
                    .unwrap()
            )
            .await,
            config
        );
        assert_eq!(
            body(
                web.answer(&row, &row.host, "/", &Method::GET)
                    .await
                    .unwrap()
            )
            .await,
            "platform"
        );
        let original_root = root;
        row.platform_ui = format!("sha256:{}", "b".repeat(64));
        let root = row.root(&row.host).unwrap();
        assert_ne!(original_root, root);
        put(&store, &root, "index.html", "new platform").await;
        assert_eq!(
            body(
                web.answer(&row, &row.host, "/", &Method::GET)
                    .await
                    .unwrap()
            )
            .await,
            "new platform"
        );
        for digest in ["0.1.68", "sha256:short", "../../config.json"] {
            row.platform_ui = digest.into();
            assert!(row.root(&row.host).is_err());
        }
        row.platform_ui = format!("sha256:{}", "b".repeat(64));
        assert!(
            web.answer(&row, "other.example.com", "/", &Method::GET)
                .await
                .is_err()
        );
        for (path, status) in [
            ("/assets/missing.js", StatusCode::NOT_FOUND),
            ("/assets/../config.json", StatusCode::BAD_REQUEST),
            ("/clients/other/file", StatusCode::NOT_FOUND),
        ] {
            assert_eq!(
                web.answer(&row, &row.host, path, &Method::GET)
                    .await
                    .unwrap()
                    .status(),
                status
            );
        }
        assert_eq!(
            web.answer(&row, &row.host, "/", &Method::POST)
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
