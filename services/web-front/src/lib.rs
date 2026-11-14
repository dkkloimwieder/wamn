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
use rustls::pki_types::pem::PemObject as _;
use serde::Deserialize;
use tokio::net::TcpListener;
#[cfg(test)]
use tokio::sync::RwLock;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[cfg(test)]
mod password_tests;
#[cfg(test)]
mod rollout_tests;

/// The platform installer supplies the listener and bucket in a mounted file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    pub listen: SocketAddr,
    /// Empty on kind, otherwise `gs://<buckets.web>`.
    pub bucket: String,
    /// HTTPS identity endpoint and its private CA.
    pub identity: String,
    pub identity_ca: std::path::PathBuf,
    /// Trusted proxy addresses counted from the direct peer toward the client.
    pub trusted_proxy_hops: usize,
}

/// One application-owned route, discovered from a labelled ConfigMap in hosts.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    #[serde(skip)]
    namespace: String,
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
    project: String,
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
        Ok(self.api.get(path.to_owned()).await?)
    }
    async fn routes(&self) -> anyhow::Result<BTreeMap<String, Route>> {
        let mut objects = self
            .request("/api/v1/configmaps?labelSelector=wamn.route%3Dtrue")
            .await?;
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
            object["metadata"]["namespace"]
                .as_str()
                .context("route namespace is absent")?
                .clone_into(&mut route.namespace);
            routes
                .entry(std::mem::take(&mut route.host))
                .or_insert(route);
        }
        Ok(routes)
    }

    async fn read(&self, namespace: &str, name: &str) -> anyhow::Result<LiveRelease> {
        ensure!(
            segment(namespace) && segment(name),
            "invalid release coordinate"
        );
        let object = self
            .request(&format!("/api/v1/namespaces/{namespace}/configmaps/{name}"))
            .await?;
        serde_json::from_str(
            object["data"]["release.json"]
                .as_str()
                .context("release ConfigMap lacks release.json")?,
        )
        .context("read the release ConfigMap")
    }
}

#[derive(Debug)]
struct File {
    bytes: Bytes,
    content_type: String,
}

#[derive(Debug)]
struct Web {
    store: Option<Arc<dyn ObjectStore>>,
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
        let file = if config {
            ensure!(segment(&row.org), "invalid web org");
            let target = if row.platform_ui.is_empty() {
                ensure!(segment(&row.project), "invalid web project");
                serde_json::json!({"org":row.org,"project":row.project,
                    "release":row.digest,"web":row.webs[host]})
            } else {
                serde_json::json!({"org":row.org})
            };
            File {
                bytes: serde_json::to_vec(&target)?.into(),
                content_type: "application/json".into(),
            }
        } else {
            let Some(store) = &self.store else {
                return Ok(empty(StatusCode::SERVICE_UNAVAILABLE));
            };
            let key = format!("{root}/{file}");
            let result = match store.get(&object_store::path::Path::from(key)).await {
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
            File {
                bytes: result.bytes().await?,
                content_type,
            }
        };
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

type Connector = hyper_rustls::HttpsConnector<HttpConnector>;
type Proxy = Client<Connector, hyper::body::Incoming>;

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
    proxy: &Proxy,
    identity: &str,
    peer: std::net::IpAddr,
    trusted_proxy_hops: usize,
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
    let password_request = path.starts_with("/password/");
    if api_request
        || password_request
        || path.starts_with("/wamn_control/")
        || path == "/oauth/callback"
    {
        let upstream = if password_request {
            identity
        } else if file_request {
            route.files.as_str()
        } else {
            route.api.as_str()
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
        let address =
            if trusted_proxy_hops == 0 || !request.headers().contains_key("x-forwarded-for") {
                peer
            } else {
                let address = request
                    .headers()
                    .get_all("x-forwarded-for")
                    .iter()
                    .rev()
                    .flat_map(|value| value.as_bytes().rsplit(|byte| *byte == b','))
                    .nth(trusted_proxy_hops - 1)
                    .context("forwarded client address is absent from the trusted proxy chain")?;
                std::str::from_utf8(address)?
                    .trim()
                    .parse::<std::net::IpAddr>()?
            };
        remove_hop_headers(request.headers_mut());
        request
            .headers_mut()
            .insert("x-forwarded-for", address.to_string().parse()?);
        // Upstream ingress does not retire pooled connections when its host drains.
        // Select a current Service endpoint for each request, without replaying it.
        request.headers_mut().insert(
            hyper::header::CONNECTION,
            hyper::header::HeaderValue::from_static("close"),
        );
        if password_request {
            let authority = request
                .uri()
                .authority()
                .context("identity authority")?
                .as_str()
                .parse()?;
            request.headers_mut().insert(HOST, authority);
        } else {
            request.headers_mut().insert(HOST, host.parse()?);
        }
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
    let row = releases.read(&route.namespace, &route.release).await?;
    Ok(boxed(
        web.answer(&row, &host, path, request.method()).await?,
    ))
}

fn response_headers(headers: &mut hyper::HeaderMap) {
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "same-origin"),
        ("strict-transport-security", "max-age=31536000"),
    ] {
        headers.insert(
            hyper::header::HeaderName::from_static(name),
            hyper::header::HeaderValue::from_static(value),
        );
    }
}

fn proxy(ca: &[u8]) -> anyhow::Result<Proxy> {
    let mut connector = HttpConnector::new();
    connector.enforce_http(false);
    connector.set_connect_timeout(Some(Duration::from_secs(10)));
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(ca) {
        roots.add(cert?)?;
    }
    ensure!(!roots.is_empty(), "identity CA is empty");
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .wrap_connector(connector);
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

/// Run the cluster web server until interrupted.
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
    let web = Arc::new(Web { store });
    let releases = Arc::new(Releases::in_cluster()?);
    ensure!(
        config.identity.starts_with("https://"),
        "identity must use HTTPS"
    );
    let proxy = proxy(&std::fs::read(&config.identity_ca)?)?;
    let listener = TcpListener::bind(config.listen).await?;
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; break; }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let identity = config.identity.clone();
                let trusted_proxy_hops = config.trusted_proxy_hops;
                let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), proxy.clone());
                tasks.spawn(async move {
                    let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let identity = identity.clone();
                        let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), proxy.clone());
                        async move {
                            let mut response = match respond(&web, &releases, &proxy, &identity, peer.ip(), trusted_proxy_hops, request).await {
                                Ok(response) => response,
                                Err(error) => {
                                    tracing::warn!(%error, "edge request failed");
                                    boxed(empty(StatusCode::BAD_GATEWAY))
                                }
                            };
                            response_headers(response.headers_mut());
                            Ok::<_, Infallible>(response)
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
pub async fn probe(path: &Path) -> anyhow::Result<()> {
    let _: WebConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    let releases = Releases::in_cluster()?;
    let client: Client<HttpConnector, Empty<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    for (host, route) in releases.routes().await? {
        let row = releases.read(&route.namespace, &route.release).await?;
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
                    "web probe {host}{path}: {}",
                    response.status()
                );
                response.into_body().collect().await?;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .with_context(|| format!("web probe {host}{path} timed out"))??;
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
            org: "acme".into(),
            project: "app".into(),
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
        };
        let first = row('a');
        let second = row('b');
        assert_ne!(
            first.root(&first.host).unwrap(),
            second.root(&second.host).unwrap()
        );
        for row in [&first, &second] {
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
        let route = json!({"host":"app.example", "release":"app", "api":endpoint, "files":""});
        let web_digest = format!("sha256:{}", "a".repeat(64));
        let release = json!({"host":"app.example", "digest":"release", "clients":{"app.example":"client"},
            "webs":{"app.example":web_digest}, "platformUi":"", "org":"acme", "project":"app"});
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let service = service_fn(|request: hyper::Request<hyper::body::Incoming>| {
                    let (route, release) = (&route, &release);
                    async move {
                        let (status, value) = match request.uri().path() {
                            "/api/v1/namespaces/hosts/configmaps" => (StatusCode::OK,
                                json!({"items":[{"metadata":{"name":"app"},"data":{"route.json":route.to_string()}}]})),
                            "/api/v1/namespaces/hosts/configmaps/app" => (StatusCode::OK,
                                json!({"data":{"release.json":release.to_string()}})),
                            _ => (StatusCode::ACCEPTED, json!(null)),
                        };
                        Ok::<_, Infallible>(Response::builder().status(status)
                            .header(hyper::header::CONNECTION, "close")
                            .body(Full::new(Bytes::from(value.to_string()))).unwrap())
                    }
                });
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service).await.unwrap();
            }
        });
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_native_roots(rustls::crypto::aws_lc_rs::default_provider())
            .unwrap()
            .https_or_http()
            .enable_http1()
            .build();
        let proxy = Client::builder(TokioExecutor::new()).build(connector.clone());
        let releases = Releases {
            api: wamn_kubernetes::kubernetes_api::KubernetesApi::with_client(
                Client::builder(TokioExecutor::new()).build(connector),
                endpoint,
                String::new(),
                "hosts".into(),
            ),
        };
        let web = Web { store: None };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let front = format!("http://{}", listener.local_addr().unwrap());
        tasks.spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let service = service_fn(|request| async {
                Ok::<_, Infallible>(
                    respond(
                        &web,
                        &releases,
                        &proxy,
                        "https://localhost",
                        peer.ip(),
                        0,
                        request,
                    )
                    .await
                    .unwrap(),
                )
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        });
        let client: Client<HttpConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        for path in ["/api/query", "/config.json"] {
            let request = hyper::Request::builder()
                .uri(format!("{front}{path}"))
                .header(HOST, "app.example")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = client.request(request).await.unwrap();
            if path == "/api/query" {
                assert_eq!(response.status(), StatusCode::ACCEPTED);
                response.into_body().collect().await.unwrap();
            } else {
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(config["release"], "release");
                assert_eq!(config["web"], web_digest);
            }
        }
        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn platform_digest_selects_the_ui_and_unknown_paths_do_not_escape_it() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(Arc::<InMemory>::clone(&store)),
        };
        let mut row = row('a');
        row.platform_ui = format!("sha256:{}", "a".repeat(64));
        row.org = "acme".into();
        let root = row.root(&row.host).unwrap();
        row.webs.clear();
        put(&store, &root, "index.html", "platform").await;

        let config = web
            .answer(&row, &row.host, "/config.json", &Method::GET)
            .await
            .unwrap();
        let config: serde_json::Value = serde_json::from_slice(&body(config).await).unwrap();
        assert_eq!(config["org"], row.org);
        assert!(config.get("project").is_none());
        let original_root = root;
        row.platform_ui = format!("sha256:{}", "b".repeat(64));
        let root = row.root(&row.host).unwrap();
        assert_ne!(original_root, root);
        put(&store, &root, "index.html", "new platform").await;
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
