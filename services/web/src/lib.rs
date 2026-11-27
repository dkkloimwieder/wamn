//! Route public hosts and application mounts to chart-owned Services.

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
use hyper::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST, LOCATION};
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
use wamn_http_client::{self as http_transport, Protocols, Schemes};
use wamn_kubernetes::{
    client::KubernetesApi,
    web_release::{LiveRelease, Releases, RouteLookup},
};
use wamn_registry::{name::valid_resource_segment, platform_domain::PlatformDomain};

const LISTEN_PORT: u16 = 8080;
const IDENTITY_CA: &str = "/etc/wamn-edge/identity-ca/ca.crt";
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
    /// Base domain used for org and environment public hosts.
    pub platform_domain: PlatformDomain,
    /// Empty on kind, otherwise `gs://<buckets.web>`.
    pub bucket: String,
    /// HTTPS origin of identity's password listener.
    pub identity_password_url: String,
    /// Trusted proxy addresses counted from the direct peer toward the client.
    pub trusted_proxy_hops: usize,
}

fn web_root(row: &LiveRelease) -> Option<String> {
    if let Some(platform_ui) = &row.release.platform_ui {
        return Some(format!("platform-ui/sha256-{}", platform_ui.hex()));
    }
    let client = row.release.client.as_ref()?;
    Some(format!("clients/{}/{}", client.package, client.web.hex()))
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
        path: &str,
        method: &Method,
    ) -> anyhow::Result<Response<Full<Bytes>>> {
        if method != Method::GET && method != Method::HEAD {
            return Ok(empty(StatusCode::METHOD_NOT_ALLOWED));
        }
        let path = path.trim_start_matches('/');
        if path.split('/').any(|part| part == "." || part == "..") || path.contains(['%', '\\']) {
            return Ok(empty(StatusCode::BAD_REQUEST));
        }
        let root = web_root(row);
        let pinned = root.as_ref().and_then(|root| {
            path.strip_prefix(root)
                .and_then(|tail| tail.strip_prefix('/'))
        });
        let (file, pinned) = if let Some(file) = pinned {
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
            let target = if let Some(project) = &row.release.project {
                let Some(client) = &row.release.client else {
                    return Ok(empty(StatusCode::NOT_FOUND));
                };
                serde_json::json!({"org":row.org,"project":project,
                    "release":row.release.digest,"web":client.web})
            } else {
                serde_json::json!({"org":row.org})
            };
            File {
                bytes: serde_json::to_vec(&target)?.into(),
                content_type: "application/json".into(),
            }
        } else {
            let Some(root) = root else {
                return Ok(empty(StatusCode::NOT_FOUND));
            };
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
struct Connector {
    tls: TlsConnector,
    #[cfg(test)]
    test_address: Option<SocketAddr>,
}

impl Connector {
    fn new(tls: TlsConnector) -> Self {
        Self {
            tls,
            #[cfg(test)]
            test_address: None,
        }
    }
}

impl Service<Uri> for Connector {
    type Response = <TlsConnector as Service<Uri>>::Response;
    type Error = <TlsConnector as Service<Uri>>::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let mut connector = self.tls.clone();
        #[cfg(test)]
        let uri = match self.test_address {
            Some(address) => format!("http://{address}")
                .parse()
                .expect("test socket URI"),
            None => uri,
        };
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

/// Match an endpoint and its descendants on a path segment boundary.
fn at_path(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
}

fn upstream_path(path: &str, app: Option<&wamn_registry::AppName>) -> Option<String> {
    if let Some(app) = app {
        if path == "/api/health" || path.starts_with("/wamn_control/") {
            return Some(path.to_owned());
        }
        let tail = path.strip_prefix("/api/")?;
        if at_path(tail, "files") || at_path(tail, "flow") {
            return Some(format!("/api/{app}/{tail}"));
        }
        return Some(format!("/{tail}"));
    }
    if path.starts_with("/api/") {
        return Some(path["/api".len()..].to_owned());
    }
    (path.starts_with("/password/")
        || path.starts_with("/wamn_control/")
        || path == "/oauth/callback")
        .then(|| path.to_owned())
}

fn public_location(location: &str, row: &LiveRelease, api: bool) -> Option<String> {
    let upstream_path = if location.starts_with('/') && !location.starts_with("//") {
        location
    } else {
        let target = location
            .strip_prefix("http://")
            .or_else(|| location.strip_prefix("//"))?;
        let (authority, path) = target.split_at(target.find('/')?);
        let host = authority.strip_suffix(":80").unwrap_or(authority);
        host.eq_ignore_ascii_case(&row.upstream_host)
            .then_some(path)?
    };
    let path = upstream_path.split(['?', '#']).next()?;
    let Some(mount) = &row.mount else {
        return if api {
            Some(format!("/api{upstream_path}"))
        } else {
            (location != upstream_path).then(|| upstream_path.to_owned())
        };
    };
    let app = row.release.app.as_ref()?;
    let mount = mount.trim_end_matches('/');
    if let Some(tail) = path
        .strip_prefix("/api/")
        .and_then(|path| path.strip_prefix(app.as_str()))
        && let Some(tail) = tail.strip_prefix('/')
        && (at_path(tail, "files") || at_path(tail, "flow"))
    {
        let suffix = &upstream_path[path.len()..];
        return Some(format!("{mount}/api/{tail}{suffix}"));
    }
    if path == "/api/health" || (!api && path.starts_with("/wamn_control/")) {
        return Some(format!("{mount}{upstream_path}"));
    }
    Some(format!("{mount}/api{upstream_path}"))
}

async fn respond(
    web: &Web,
    releases: &Releases,
    proxy: &Proxy,
    identity_password_url: &str,
    peer: std::net::IpAddr,
    trusted_proxy_hops: usize,
    mut request: hyper::Request<hyper::body::Incoming>,
) -> anyhow::Result<Response<ProxyBody>> {
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
    let route = match releases.route(&host, request.uri().path()).await {
        RouteLookup::Found(route) => route,
        RouteLookup::Loading => return Ok(boxed(empty(StatusCode::SERVICE_UNAVAILABLE))),
        RouteLookup::Missing => return Ok(boxed(empty(StatusCode::NOT_FOUND))),
    };
    let public_path = request.uri().path();
    let query = request.uri().query();
    let path = if let Some(mount) = &route.mount {
        let prefix = mount.trim_end_matches('/');
        let tail = public_path.strip_prefix(prefix).context("route mount")?;
        if tail.is_empty() {
            let mut response = empty(StatusCode::PERMANENT_REDIRECT);
            let mut location = mount.to_owned();
            if let Some(query) = query {
                location.push('?');
                location.push_str(query);
            }
            response.headers_mut().insert(LOCATION, location.parse()?);
            return Ok(boxed(response));
        }
        tail
    } else {
        public_path
    };
    let api_request = path.starts_with("/api/");
    let password_request = route.mount.is_none() && path.starts_with("/password/");
    if let Some(mut upstream_path) = upstream_path(path, route.release.app.as_ref()) {
        let password_route = browser_session::password_route(&request);
        if let Some(query) = query {
            upstream_path.push('?');
            upstream_path.push_str(query);
        }
        *request.uri_mut() = if password_request {
            format!(
                "{}{upstream_path}",
                identity_password_url.trim_end_matches('/')
            )
        } else {
            format!("http://{}{upstream_path}", route.upstream_host)
        }
        .parse()?;
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
            match browser_session::forward(request, password_route, password_request).await {
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
        let upstream_host = if password_request {
            request
                .uri()
                .authority()
                .context("identity authority")?
                .as_str()
        } else if route.mount.is_some() {
            &route.upstream_host
        } else {
            &host
        };
        let upstream_host = upstream_host.parse()?;
        request.headers_mut().insert(HOST, upstream_host);
        let mut response = proxy.client.request(request).await?;
        remove_hop_headers(response.headers_mut());
        if password_request {
            return browser_session::respond(password_route, response).await;
        }
        if let Some(location) = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| public_location(location, &route, api_request))
        {
            response.headers_mut().insert(LOCATION, location.parse()?);
        }
        return Ok(response.map(http_body_util::BodyExt::boxed_unsync));
    }
    Ok(boxed(web.answer(&route, path, request.method()).await?))
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
        client: http_transport::client(Connector::new(connector), |builder| {
            builder.retry_canceled_requests(false);
        }),
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
        ensure!(valid_resource_segment(bucket), "invalid web bucket");
        Some(Arc::new(
            object_store::gcp::GoogleCloudStorageBuilder::from_env()
                .with_bucket_name(bucket)
                .build()?,
        ))
    };
    let web = Arc::new(Web { store });
    let releases = Arc::new(Releases::new(
        KubernetesApi::in_cluster()?,
        config.platform_domain,
    ));
    ensure!(
        config.identity_password_url.starts_with("https://"),
        "identity password URL must use HTTPS"
    );
    let proxy = Arc::new(proxy(&std::fs::read(IDENTITY_CA)?)?);
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], LISTEN_PORT))).await?;
    let identity_password_url: Arc<str> = config.identity_password_url.into();
    let trusted_proxy_hops = config.trusted_proxy_hops;
    let mut tasks = tokio::task::JoinSet::new();
    let routes = releases.watch();
    tokio::pin!(routes);
    loop {
        tokio::select! {
            () = &mut routes => unreachable!("Route watches run until shutdown"),
            signal = tokio::signal::ctrl_c() => { signal?; break; }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let identity_password_url = Arc::clone(&identity_password_url);
                let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), Arc::clone(&proxy));
                tasks.spawn(async move {
                    let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let identity_password_url = Arc::clone(&identity_password_url);
                        let (web, releases, proxy) = (Arc::clone(&web), Arc::clone(&releases), Arc::clone(&proxy));
                        async move {
                            let mut response = match respond(&web, &releases, &proxy, &identity_password_url, peer.ip(), trusted_proxy_hops, request).await {
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
    let releases = Releases::new(KubernetesApi::in_cluster()?, config.platform_domain);
    let client: Client<HttpConnector, Empty<Bytes>> = http_transport::http_client();
    for (host, rows) in releases.list().await? {
        for row in rows {
            if web_root(&row).is_none() {
                continue;
            }
            let mount = row.mount.as_deref().unwrap_or("/");
            for path in [mount.to_owned(), format!("{mount}config.json")] {
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
    }
    Ok(())
}

#[cfg(test)]
async fn wait_for_route(releases: &Releases, host: &str, path: &str) {
    while matches!(releases.route(host, path).await, RouteLookup::Loading) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(matches!(
        releases.route(host, path).await,
        RouteLookup::Found(_)
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn row(digit: char) -> LiveRelease {
        let org = "acme".parse().unwrap();
        let project = "shop".parse().unwrap();
        let app = "stock".parse().unwrap();
        LiveRelease {
            upstream_host: "release-http.acme--dev.svc.cluster.local".into(),
            org,
            mount: Some(wamn_registry::name::app_mount(&project, &app)),
            release: Arc::new(wamn_kubernetes::web_release::ReleaseAnnotation {
                project: Some(project),
                app: Some(app),
                digest: wamn_digest::bytes_sha256(b"release"),
                client: Some(wamn_registry::configuration::Client {
                    package: "app".parse().unwrap(),
                    web: wamn_digest::bytes_sha256(digit.to_string().as_bytes()),
                }),
                platform_ui: None,
            }),
        }
    }

    #[test]
    fn mounted_native_paths_use_the_workload_paths_and_reverse_their_locations() {
        let row = row('a');
        for (public, native) in [
            ("/api/files", "/api/stock/files"),
            ("/api/files/object", "/api/stock/files/object"),
            ("/api/flow/receive", "/api/stock/flow/receive"),
            ("/api/health", "/api/health"),
            ("/api/files-report", "/files-report"),
            ("/api/flowchart", "/flowchart"),
            ("/api/health/history", "/health/history"),
            ("/api/widget/list", "/widget/list"),
        ] {
            assert_eq!(
                upstream_path(public, row.release.app.as_ref()).as_deref(),
                Some(native)
            );
            let location = format!("http://{}{native}?key=a%2Fb#part", row.upstream_host);
            let mounted = format!("/shop/stock{public}?key=a%2Fb#part");
            assert_eq!(
                public_location(&location, &row, true).as_deref(),
                Some(mounted.as_str())
            );
        }
        assert_eq!(
            public_location("/api/stock/files/object", &row, true).as_deref(),
            Some("/shop/stock/api/files/object")
        );
        let location = format!(
            "http://{}:80/api/stock/flow/receive",
            row.upstream_host.to_ascii_uppercase()
        );
        assert_eq!(
            public_location(&location, &row, true).as_deref(),
            Some("/shop/stock/api/flow/receive")
        );
        for location in [
            "https://example.net/path",
            "//example.net/path",
            "http://release-http.acme--dev.svc.cluster.local.example.net/path",
        ] {
            assert!(
                public_location(location, &row, true).is_none(),
                "{location}"
            );
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
        assert_ne!(web_root(&first).unwrap(), web_root(&second).unwrap());
        for row in [&first, &second] {
            let response = web.answer(row, "/config.json", &Method::GET).await.unwrap();
            assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
            let config: serde_json::Value = serde_json::from_slice(&body(response).await).unwrap();
            assert_eq!(config["org"], serde_json::json!(row.org));
            assert_eq!(config["project"], serde_json::json!(row.release.project));
            assert_eq!(config["release"], serde_json::json!(first.release.digest));
            assert_eq!(
                config["web"],
                serde_json::json!(row.release.client.as_ref().unwrap().web)
            );
        }
    }

    #[tokio::test]
    async fn health_stays_available_while_routes_wait_for_discovery() {
        let releases = Releases::new(
            KubernetesApi::from_config(kube::Config::new("http://127.0.0.1:1".parse().unwrap()))
                .unwrap(),
            "example.test".parse().unwrap(),
        );
        let web = Web { store: None };
        let proxy = Proxy {
            client: http_transport::client(Connector::new(native_connector().unwrap()), |_| {}),
        };
        let (front, back) = tokio::io::duplex(4096);
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let service = service_fn(|request| async {
                Ok::<_, Infallible>(
                    respond(
                        &web,
                        &releases,
                        &proxy,
                        "https://localhost",
                        std::net::Ipv4Addr::LOCALHOST.into(),
                        0,
                        request,
                    )
                    .await
                    .unwrap(),
                )
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(back), service)
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut client, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(front))
                    .await
                    .unwrap();
            tasks.spawn(async move { connection.await.unwrap() });
            for (host, path, status) in [
                ("dev.acme.example.test", "/healthz", StatusCode::OK),
                (
                    "dev.acme.example.test",
                    "/config.json",
                    StatusCode::SERVICE_UNAVAILABLE,
                ),
                (
                    "acme.example.test",
                    "/shop/stock/api/health",
                    StatusCode::SERVICE_UNAVAILABLE,
                ),
            ] {
                let request = hyper::Request::builder()
                    .uri(path)
                    .header(HOST, host)
                    .body(Empty::<Bytes>::new())
                    .unwrap();
                let response = client.send_request(request).await.unwrap();
                assert_eq!(response.status(), status);
                response.into_body().collect().await.unwrap();
            }
        })
        .await
        .expect("answer health and route requests before discovery");
        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn platform_digest_selects_the_ui_and_unknown_paths_do_not_escape_it() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(Arc::<InMemory>::clone(&store)),
        };
        let control = |pin: &[u8]| LiveRelease {
            upstream_host: "c-acme-http.acme.svc.cluster.local".into(),
            org: "acme".parse().unwrap(),
            mount: None,
            release: Arc::new(wamn_kubernetes::web_release::ReleaseAnnotation {
                digest: wamn_digest::bytes_sha256(b"control"),
                project: None,
                app: None,
                client: None,
                platform_ui: Some(wamn_digest::bytes_sha256(pin)),
            }),
        };
        let row = control(b"platform");
        let root = web_root(&row).unwrap();
        put(&store, &root, "index.html", "platform").await;
        let config = web
            .answer(&row, "/config.json", &Method::GET)
            .await
            .unwrap();
        let config: serde_json::Value = serde_json::from_slice(&body(config).await).unwrap();
        assert_eq!(config["org"], row.org.as_str());
        assert!(config.get("project").is_none());
        let new = control(b"new platform");
        let new_root = web_root(&new).unwrap();
        assert_ne!(root, new_root);
        put(&store, &new_root, "index.html", "new platform").await;
        assert_eq!(
            body(web.answer(&new, "/", &Method::GET).await.unwrap()).await,
            "new platform"
        );
        for (path, status) in [
            ("/assets/missing.js", StatusCode::NOT_FOUND),
            ("/assets/../config.json", StatusCode::BAD_REQUEST),
            ("/clients/other/file", StatusCode::NOT_FOUND),
        ] {
            assert_eq!(
                web.answer(&row, path, &Method::GET).await.unwrap().status(),
                status
            );
        }
        assert_eq!(
            web.answer(&row, "/", &Method::POST).await.unwrap().status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
