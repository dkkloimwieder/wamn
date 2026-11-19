//! Route application hosts through chart-owned route and release ConfigMaps.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Full, combinators::UnsyncBoxBody};
use hyper::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::service::service_fn;
use hyper::{Method, Response, StatusCode, Uri};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioIo;
use object_store::{ObjectStore, ObjectStoreExt as _};
use rustls::pki_types::pem::PemObject as _;
use serde::Deserialize;
use tokio::net::TcpListener;
#[cfg(test)]
use tokio::sync::RwLock;
use tower_service::Service;
use wamn_kubernetes::http_transport::{self, Protocols, Schemes};
use wamn_registry::platform_domain::PlatformDomain;

const LISTEN_PORT: u16 = 8080;
const IDENTITY_CA: &str = "/etc/wamn-edge/identity-ca/ca.crt";
const CSRF_KEY_FILE: &str = "/etc/wamn-edge/csrf/key";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const CONNECT_RETRIES: usize = 2;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

mod browser_session;
#[cfg(test)]
mod rollout_tests;

/// The platform chart supplies web settings in a mounted JSON file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Base domain covered by the platform wildcard certificate.
    pub platform_domain: PlatformDomain,
    /// Empty on kind, otherwise `gs://<buckets.web>`.
    pub bucket: String,
    /// Canonical HTTPS identity issuer.
    pub identity_issuer: String,
    /// Trusted proxy addresses counted from the direct peer toward the client.
    pub trusted_proxy_hops: usize,
}

/// One route discovered from a labelled ConfigMap in an org namespace.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    #[serde(skip)]
    namespace: String,
    host: String,
    release: String,
    api: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LiveRelease {
    host: String,
    digest: wamn_digest::Digest,
    clients: BTreeMap<String, wamn_catalog::PackageName>,
    webs: BTreeMap<String, wamn_digest::Digest>,
    platform_ui: Option<wamn_digest::Digest>,
    org: wamn_registry::OrgId,
    project: wamn_registry::ProjectId,
}

impl LiveRelease {
    fn root(&self, host: &str) -> anyhow::Result<String> {
        ensure!(self.host == host, "the release does not serve this host");
        if let Some(platform_ui) = &self.platform_ui {
            return Ok(format!("platform-ui/sha256-{}", platform_ui.hex()));
        }
        let package = self
            .clients
            .get(host)
            .context("the host has no web client")?;
        let hex = self
            .webs
            .get(host)
            .context("the host has no web client")?
            .hex();
        Ok(format!("clients/{package}/{hex}"))
    }
}

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn valid_route_host(host: &str, namespace: &str, base: &PlatformDomain) -> bool {
    let Some((label, domain)) = host.split_once('.') else {
        return false;
    };
    if domain != base.as_str() || namespace.is_empty() || label.len() > 63 || host.len() > 253 {
        return false;
    }
    label
        .strip_suffix(namespace)
        .and_then(|prefix| prefix.strip_suffix("--"))
        .is_some_and(|prefix| {
            !prefix.is_empty()
                && !prefix.starts_with('-')
                && !prefix.ends_with('-')
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

#[derive(Debug)]
struct Releases {
    api: wamn_kubernetes::kubernetes_api::KubernetesApi,
    platform_domain: PlatformDomain,
}
impl Releases {
    fn in_cluster(platform_domain: PlatformDomain) -> anyhow::Result<Self> {
        Ok(Self {
            api: wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?,
            platform_domain,
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
            let namespace = object["metadata"]["namespace"].as_str().unwrap_or_default();
            let name = object["metadata"]["name"].as_str().unwrap_or_default();
            let parsed = object["data"]["route.json"]
                .as_str()
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
            let target = if row.platform_ui.is_none() {
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

type TlsConnector = hyper_rustls::HttpsConnector<HttpConnector>;

/// Hyper hands the connector only the URI. All attempts finish before it
/// hands the upstream connection the request, including streaming bodies.
#[derive(Clone)]
struct Connector(TlsConnector);

impl Service<Uri> for Connector {
    type Response = <TlsConnector as Service<Uri>>::Response;
    type Error = <TlsConnector as Service<Uri>>::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let mut connector = self.0.clone();
        Box::pin(async move {
            let mut retries = CONNECT_RETRIES;
            loop {
                let result = tokio::time::timeout(CONNECT_TIMEOUT, async {
                    poll_fn(|cx| connector.poll_ready(cx)).await?;
                    connector.call(uri.clone()).await
                })
                .await
                .unwrap_or_else(|elapsed| Err(elapsed.into()));
                if result.is_ok() || retries == 0 {
                    return result;
                }
                retries -= 1;
            }
        })
    }
}
type ProxyBody = UnsyncBoxBody<Bytes, hyper::Error>;

struct Proxy {
    client: Client<Connector, ProxyBody>,
    csrf_key: ring::hmac::Key,
}

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
    let native_api_request = path
        .strip_prefix("/api/")
        .and_then(|path| path.split_once('/'))
        .is_some_and(|(_, path)| {
            matches!(path, "files" | "flow")
                || path.starts_with("files/")
                || path.starts_with("flow/")
        });
    let api_request = path.starts_with("/api/");
    let password_request = path.starts_with("/password/");
    if api_request
        || password_request
        || path.starts_with("/wamn_control/")
        || path == "/oauth/callback"
    {
        let password_route = browser_session::password_route(&request);
        let upstream = if password_request {
            identity
        } else {
            route.api.as_str()
        };
        let path = request
            .uri()
            .path_and_query()
            .context("request path")?
            .as_str();
        let path = if api_request && !native_api_request {
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
        let mut request =
            match browser_session::forward(request, &proxy.csrf_key, password_route, password_request)
                .await
            {
                Ok(request) => request,
                Err(status) => return Ok(boxed(empty(status))),
            };
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
        let mut response = proxy.client.request(request).await?;
        remove_hop_headers(response.headers_mut());
        if password_request {
            return browser_session::respond(password_route, response, &proxy.csrf_key).await;
        }
        if api_request
            && !native_api_request
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

fn proxy(ca: &[u8], csrf_key: &[u8; 32]) -> anyhow::Result<Proxy> {
    let connector = HttpConnector::new();
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
    let connector =
        http_transport::tls_connector(tls, connector, Schemes::HttpAndHttps, Protocols::Http1);
    Ok(Proxy {
        client: http_transport::client(Connector(connector), |builder| {
            builder.retry_canceled_requests(false);
        }),
        csrf_key: ring::hmac::Key::new(ring::hmac::HMAC_SHA256, csrf_key),
    })
}

#[cfg(test)]
fn native_connector() -> anyhow::Result<TlsConnector> {
    use hyper_rustls::ConfigBuilderExt as _;
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_native_roots()?
    .with_no_client_auth();
    Ok(http_transport::tls_connector(
        tls,
        HttpConnector::new(),
        Schemes::HttpAndHttps,
        Protocols::Http1,
    ))
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
    let releases = Arc::new(Releases::in_cluster(config.platform_domain)?);
    ensure!(
        config.identity_issuer.starts_with("https://"),
        "identity must use HTTPS"
    );
    let csrf_key = std::fs::read(CSRF_KEY_FILE).context("read web CSRF key file")?;
    let csrf_key = csrf_key
        .as_slice()
        .try_into()
        .context("web CSRF key must contain 32 bytes")?;
    let proxy = Arc::new(proxy(&std::fs::read(IDENTITY_CA)?, csrf_key)?);
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], LISTEN_PORT))).await?;
    let identity: Arc<str> = config.identity_issuer.into();
    let trusted_proxy_hops = config.trusted_proxy_hops;
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; break; }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let identity = Arc::clone(&identity);
                let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), Arc::clone(&proxy));
                tasks.spawn(async move {
                    let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let identity = Arc::clone(&identity);
                        let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), Arc::clone(&proxy));
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
    let config: WebConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    let releases = Releases::in_cluster(config.platform_domain)?;
    let client: Client<HttpConnector, Empty<Bytes>> = http_transport::http_client();
    for (host, route) in releases.routes().await? {
        let row = releases.read(&route.namespace, &route.release).await?;
        if row.platform_ui.is_none() && !row.clients.contains_key(&host) {
            continue;
        }
        for path in ["/", "/config.json"] {
            let request = hyper::Request::builder()
                .uri(format!("http://127.0.0.1:{LISTEN_PORT}{path}"))
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
            digest: (format!("sha256:{}", "c".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            clients: [("app.example.com".into(), "app".parse().unwrap())].into(),
            webs: [(
                "app.example.com".into(),
                format!("sha256:{}", digit.to_string().repeat(64))
                    .parse()
                    .unwrap(),
            )]
            .into(),
            platform_ui: None,
            org: "acme".parse().unwrap(),
            project: "app".parse().unwrap(),
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
            assert_eq!(config["release"], serde_json::json!(first.digest));
            assert_eq!(config["web"], serde_json::json!(row.webs[&row.host]));
        }
    }

    #[tokio::test]
    async fn discovered_routes_keep_services_and_web_pins_independent() {
        use serde_json::json;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let route = json!({"host":"app--acme.example", "release":"app", "api":endpoint});
        let web_digest = (format!("sha256:{}", "a".repeat(64)))
            .parse::<wamn_digest::Digest>()
            .unwrap();
        let release = json!({"host":"app--acme.example", "digest":wamn_digest::bytes_sha256(b"release"), "clients":{"app--acme.example":"client"},
            "webs":{"app--acme.example":web_digest}, "platformUi":null, "org":"acme", "project":"app"});
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let service = service_fn(|request: hyper::Request<hyper::body::Incoming>| {
                    let (route, release) = (&route, &release);
                    async move {
                        let (status, value) = match request.uri().path() {
                            "/api/v1/configmaps" => (StatusCode::OK,
                                json!({"items":[{"metadata":{"name":"app","namespace":"acme"},"data":{"route.json":route.to_string()}}]})),
                            "/api/v1/namespaces/acme/configmaps/app" => (StatusCode::OK,
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
        let connector = native_connector().unwrap();
        let proxy = Proxy {
            client: http_transport::client(Connector(connector.clone()), |_| {}),
            csrf_key: ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &[0; 32]),
        };
        let releases = Releases {
            platform_domain: "example".parse().unwrap(),
            api: wamn_kubernetes::kubernetes_api::KubernetesApi::with_client(
                http_transport::client(connector, |_| {}),
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
        let client: Client<HttpConnector, Full<Bytes>> = http_transport::http_client();
        for path in ["/api/query", "/config.json"] {
            let request = hyper::Request::builder()
                .uri(format!("{front}{path}"))
                .header(HOST, "app--acme.example")
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
                assert_eq!(
                    config["release"],
                    serde_json::json!(wamn_digest::bytes_sha256(b"release"))
                );
                assert_eq!(config["web"], serde_json::json!(web_digest));
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
        row.platform_ui = Some(
            (format!("sha256:{}", "a".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
        );
        row.org = "acme".parse().unwrap();
        let root = row.root(&row.host).unwrap();
        row.webs.clear();
        put(&store, &root, "index.html", "platform").await;

        let config = web
            .answer(&row, &row.host, "/config.json", &Method::GET)
            .await
            .unwrap();
        let config: serde_json::Value = serde_json::from_slice(&body(config).await).unwrap();
        assert_eq!(config["org"], row.org.as_str());
        assert!(config.get("project").is_none());
        let original_root = root;
        row.platform_ui = Some(
            (format!("sha256:{}", "b".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
        );
        let root = row.root(&row.host).unwrap();
        assert_ne!(original_root, root);
        put(&store, &root, "index.html", "new platform").await;
        row.platform_ui = Some(
            (format!("sha256:{}", "b".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
        );
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
