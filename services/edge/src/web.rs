//! Serve bucket files selected by the chart-owned release ConfigMap.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Full};
use hyper::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::service::service_fn;
use hyper::{Method, Response, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use object_store::{ObjectStore, ObjectStoreExt as _};
use rustls::pki_types::{CertificateDer, pem::PemObject as _};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// The platform installer supplies public routing inputs in a mounted file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    pub listen: SocketAddr,
    /// Empty on kind, otherwise `gs://<buckets.web>`.
    pub bucket: String,
    /// Public host to Helm release name. Every release lives in `hosts`.
    pub releases: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LiveRelease {
    host: String,
    digest: String,
    clients: BTreeMap<String, String>,
    platform_version: String,
}

impl LiveRelease {
    fn root(&self, host: &str) -> anyhow::Result<String> {
        ensure!(self.host == host, "the release does not serve this host");
        if !self.platform_version.is_empty() {
            ensure!(segment(&self.platform_version), "invalid platform version");
            return Ok(format!("platform-ui/{}", self.platform_version));
        }
        let package = self
            .clients
            .get(host)
            .context("the host has no web client")?;
        ensure!(segment(package), "invalid client package");
        let hex = self
            .digest
            .strip_prefix("sha256:")
            .context("invalid release digest")?;
        ensure!(
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "invalid release digest"
        );
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

#[derive(Debug)]
struct Releases {
    client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
    endpoint: String,
    token: PathBuf,
}

impl Releases {
    fn in_cluster() -> anyhow::Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_file_iter(Path::new(SERVICE_ACCOUNT).join("ca.crt"))?
        {
            roots.add(certificate?)?;
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http1()
            .build();
        Ok(Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
            endpoint: "https://kubernetes.default.svc".into(),
            token: Path::new(SERVICE_ACCOUNT).join("token"),
        })
    }

    async fn read(&self, name: &str) -> anyhow::Result<LiveRelease> {
        ensure!(segment(name), "invalid release name");
        // Kubernetes rotates projected tokens. Never retain one in memory.
        let token = tokio::fs::read_to_string(&self.token).await?;
        let request = hyper::Request::builder()
            .uri(format!(
                "{}/api/v1/namespaces/hosts/configmaps/{name}",
                self.endpoint
            ))
            .header(
                hyper::header::AUTHORIZATION,
                format!("Bearer {}", token.trim()),
            )
            .body(Empty::new())?;
        let bytes = tokio::time::timeout(Duration::from_secs(10), async {
            let response = self.client.request(request).await?;
            ensure!(
                response.status().is_success(),
                "release ConfigMap read returned {}",
                response.status()
            );
            Ok::<_, anyhow::Error>(response.into_body().collect().await?.to_bytes())
        })
        .await??;
        let object: serde_json::Value = serde_json::from_slice(&bytes)?;
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
    /// The object path includes the package and release digest (or platform version).
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
        let key = format!("{root}/{file}");
        let config = file == "config.json";
        let cached = if config {
            None
        } else {
            self.files.read().await.get(&key).cloned()
        };
        let file = match cached {
            Some(file) => file,
            None => {
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
                    .map_or_else(|| content_type(file).to_owned(), ToString::to_string);
                let file = File {
                    bytes: result.bytes().await?,
                    content_type,
                };
                if !config {
                    self.files.write().await.insert(key, file.clone());
                }
                file
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

/// Run the cluster web server until interrupted. The box's `serve` mode is separate.
///
/// # Errors
/// Returns an error if the configuration, bucket client, Kubernetes CA or listener fails.
pub async fn serve(path: &Path) -> anyhow::Result<()> {
    let config: WebConfig = toml::from_str(&std::fs::read_to_string(path)?)?;
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
    let routes = Arc::new(config.releases);
    let listener = TcpListener::bind(config.listen).await?;
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; break; }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let (web, releases, routes) = (Arc::clone(&web), Arc::clone(&releases), Arc::clone(&routes));
                tasks.spawn(async move {
                    let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let (web, releases, routes) = (Arc::clone(&web), Arc::clone(&releases), Arc::clone(&routes));
                        async move {
                            if request.uri().path() == "/healthz" {
                                return Ok::<_, Infallible>(empty(StatusCode::OK));
                            }
                            let host = request.headers().get(HOST).and_then(|value| value.to_str().ok())
                                .unwrap_or_default().split(':').next().unwrap_or_default().to_ascii_lowercase();
                            let Some(name) = routes.get(&host) else { return Ok(empty(StatusCode::NOT_FOUND)); };
                            let result = async {
                                let row = releases.read(name).await?;
                                web.answer(&row, &host, request.uri().path(), request.method()).await
                            }.await;
                            Ok(match result {
                                Ok(response) => response,
                                Err(error) => {
                                    tracing::warn!(%host, %error, "web request failed");
                                    empty(StatusCode::BAD_GATEWAY)
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

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn row(digit: char) -> LiveRelease {
        LiveRelease {
            host: "app.example.com".into(),
            digest: format!("sha256:{}", digit.to_string().repeat(64)),
            clients: [("app.example.com".into(), "app".into())].into(),
            platform_version: String::new(),
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
    async fn digest_cache_survives_a_bucket_change_but_the_live_row_selects_a_new_bundle() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(store.clone()),
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
            store: Some(store.clone()),
            files: RwLock::new(BTreeMap::new()),
        };
        let row = row('a');
        let root = row.root(&row.host).unwrap();
        for value in ["first", "changed"] {
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
            assert_eq!(body(response).await, value);
        }
        assert!(web.files.read().await.is_empty());
    }

    #[tokio::test]
    async fn platform_version_selects_the_ui_and_unknown_paths_do_not_escape_it() {
        let store = Arc::new(InMemory::new());
        let web = Web {
            store: Some(store.clone()),
            files: RwLock::new(BTreeMap::new()),
        };
        let mut row = row('a');
        row.platform_version = "0.1.68".into();
        put(&store, "platform-ui/0.1.68", "index.html", "platform").await;
        assert_eq!(
            body(
                web.answer(&row, &row.host, "/", &Method::GET)
                    .await
                    .unwrap()
            )
            .await,
            "platform"
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
