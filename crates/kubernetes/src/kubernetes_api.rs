//! The shared Kubernetes HTTP client with a rotating ServiceAccount token.

use crate::{Error, failure};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Method, Request, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use rustls::pki_types::{CertificateDer, pem::PemObject as _};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};

const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_LIMIT: usize = 16 * 1024 * 1024;

type Http = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// The pod's Kubernetes API. Debug output excludes credentials.
#[derive(Clone)]
pub struct KubernetesApi {
    http: Http,
    base: String,
    token: Token,
    pub namespace: String,
}

#[derive(Clone)]
enum Token {
    File(PathBuf),
    Fixed(String),
}

impl std::fmt::Debug for KubernetesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KubernetesApi")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl KubernetesApi {
    /// Use an explicit transport and endpoint, including an API fixture.
    pub fn with_client(http: Http, base: String, token: String, namespace: String) -> Self {
        Self {
            http,
            base,
            token: Token::Fixed(token),
            namespace,
        }
    }

    /// Read the pod's endpoint, CA, namespace and projected token path.
    pub fn in_cluster() -> Result<Self, Error> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|e| failure("read endpoint", "k8s", e))?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let namespace = std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/namespace"))
            .map_err(|e| failure("read namespace", "k8s", e))?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(format!("{SERVICE_ACCOUNT}/ca.crt"))
            .map_err(|e| failure("read CA", "k8s", e))?
        {
            roots
                .add(cert.map_err(|e| failure("decode CA", "k8s", e))?)
                .map_err(|e| failure("decode CA", "k8s", e))?;
        }
        if roots.is_empty() {
            return Err(failure(
                "decode CA",
                "k8s",
                "the ServiceAccount CA is empty",
            ));
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| failure("build client", "k8s", e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http1()
            .build();
        Ok(Self {
            http: Client::builder(TokioExecutor::new()).build(connector),
            base: format!("https://{host}:{port}"),
            token: Token::File(format!("{SERVICE_ACCOUNT}/token").into()),
            namespace: namespace.trim().into(),
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Error> {
        let projected;
        let token = match &self.token {
            Token::File(path) => {
                projected = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| failure("read token", "k8s", e))?;
                &projected
            }
            Token::Fixed(token) => token,
        };
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| failure("encode", path, e))?
            .unwrap_or_default();
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header(
                hyper::header::AUTHORIZATION,
                format!("Bearer {}", token.trim()),
            )
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| failure("request", path, e))?;
        self.http
            .request(request)
            .await
            .map_err(|e| failure("request", path, e))
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        limit: usize,
    ) -> Result<(StatusCode, Bytes), Error> {
        let operation = async {
            let response = self.send(method, path, body).await?;
            let status = response.status();
            let bytes = Limited::new(response.into_body(), limit)
                .collect()
                .await
                .map_err(|e| failure("read response", path, e))?
                .to_bytes();
            Ok((status, bytes))
        };
        tokio::time::timeout(REQUEST_TIMEOUT, operation)
            .await
            .map_err(|e| failure("request timeout", path, e))?
    }

    pub(crate) async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Option<Value>, Error> {
        let (status, bytes) = self.request(method, path, body, BODY_LIMIT).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        successful(status, path)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| failure("decode", path, e))
    }

    /// Write an object without reading its successful response document.
    pub(crate) async fn write(
        &self,
        method: Method,
        path: &str,
        body: &Value,
    ) -> Result<StatusCode, Error> {
        let operation = async {
            let response = self.send(method, path, Some(body)).await?;
            let status = response.status();
            if !status.is_success() {
                Limited::new(response.into_body(), 1024 * 1024)
                    .collect()
                    .await
                    .map_err(|e| failure("read response", path, e))?;
            }
            Ok(status)
        };
        tokio::time::timeout(REQUEST_TIMEOUT, operation)
            .await
            .map_err(|e| failure("request timeout", path, e))?
    }

    /// Read one API path. An absent object is an error.
    pub async fn get(&self, path: String) -> Result<Value, Error> {
        self.json(Method::GET, &path, None)
            .await?
            .ok_or_else(|| failure("GET", &path, "object is absent"))
    }

    /// Submit a server-built Job once.
    pub async fn create_job(&self, namespace: &str, job: &Value) -> Result<Value, Error> {
        let path = format!("/apis/batch/v1/namespaces/{namespace}/jobs");
        self.json(Method::POST, &path, Some(job))
            .await?
            .ok_or_else(|| failure("create Job", &path, "endpoint is absent"))
    }

    pub async fn get_job(&self, namespace: &str, name: &str) -> Result<Option<Value>, Error> {
        self.json(
            Method::GET,
            &format!("/apis/batch/v1/namespaces/{namespace}/jobs/{name}"),
            None,
        )
        .await
    }

    /// Read bounded pod output without following the log.
    pub async fn pod_log(&self, namespace: &str, pod: &str) -> Result<String, Error> {
        const LIMIT: usize = 32 * 1024;
        let path = format!(
            "/api/v1/namespaces/{namespace}/pods/{pod}/log?container=operation&tailLines=100&limitBytes={LIMIT}"
        );
        // Enforce the local limit even if a server ignores limitBytes.
        let operation = async {
            let response = self.send(Method::GET, &path, None).await?;
            successful(response.status(), &path)?;
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while bytes.len() < LIMIT {
                let Some(frame) = body.frame().await else {
                    break;
                };
                if let Ok(data) = frame
                    .map_err(|e| failure("read log", &path, e))?
                    .into_data()
                {
                    bytes.extend_from_slice(&data[..data.len().min(LIMIT - bytes.len())]);
                }
            }
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            while text.len() > LIMIT {
                text.pop();
            }
            Ok(text)
        };
        tokio::time::timeout(REQUEST_TIMEOUT, operation)
            .await
            .map_err(|e| failure("log timeout", &path, e))?
    }
}

pub(crate) fn successful(status: StatusCode, path: &str) -> Result<(), Error> {
    if status.is_success() {
        Ok(())
    } else {
        Err(failure(
            "request",
            path,
            format!("Kubernetes API answered {status}"),
        ))
    }
}
