//! The Kubernetes HTTP client shared with environment status.

use crate::{Error, failure};
use serde_json::Value;
use std::time::Duration;

const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The Kubernetes API of the pod's cluster, with the pod's ServiceAccount.
#[derive(Clone)]
pub struct KubernetesApi {
    pub(crate) http: reqwest::Client,
    pub(crate) base: String,
    pub(crate) token: String,
    /// The pod's own namespace, the release namespace of every environment.
    pub namespace: String,
}

impl KubernetesApi {
    /// Use an existing transport and endpoint, including a recorded API in tests.
    pub fn with_client(
        http: reqwest::Client,
        base: String,
        token: String,
        namespace: String,
    ) -> Self {
        Self {
            http,
            base,
            token,
            namespace,
        }
    }

    /// Read the pod's API endpoint, token, CA and namespace.
    pub fn in_cluster() -> Result<Self, Error> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|source| failure("read endpoint", "k8s", source))?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_owned());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let token = std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/token"))
            .map_err(|source| failure("read token", "k8s", source))?;
        let namespace = std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/namespace"))
            .map_err(|source| failure("read namespace", "k8s", source))?;
        let ca = std::fs::read(format!("{SERVICE_ACCOUNT}/ca.crt"))
            .map_err(|source| failure("read CA", "k8s", source))?;
        let roots = reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|source| failure("decode CA", "k8s", source))?;
        if roots.is_empty() {
            return Err(failure(
                "decode CA",
                "k8s",
                "the ServiceAccount CA is empty",
            ));
        }
        let http = reqwest::Client::builder()
            .tls_backend_rustls()
            .tls_certs_only(roots)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|source| failure("build client", "k8s", source))?;
        Ok(Self {
            http,
            base: format!("https://{host}:{port}"),
            token: token.trim().to_owned(),
            namespace: namespace.trim().to_owned(),
        })
    }

    /// Read and decode one API path.
    pub async fn get(&self, path: String) -> Result<Value, Error> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|source| failure("GET", &path, source))?;
        let response = successful_response(response, "GET", &path)?;
        let body = response
            .bytes()
            .await
            .map_err(|source| failure("read GET", &path, source))?;
        serde_json::from_slice(&body).map_err(|source| failure("decode GET", &path, source))
    }

    /// Submit one server-built Job without retries.
    pub async fn create_job(&self, namespace: &str, job: &Value) -> Result<Value, Error> {
        let path = format!("/apis/batch/v1/namespaces/{namespace}/jobs");
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(job)
            .send()
            .await
            .map_err(|source| failure("create Job", &path, source))?;
        successful_response(response, "create Job", &path)?
            .json()
            .await
            .map_err(|source| failure("decode Job", &path, source))
    }

    /// Read one Job. The caller supplies validated Kubernetes names.
    pub async fn get_job(&self, namespace: &str, name: &str) -> Result<Option<Value>, Error> {
        let path = format!("/apis/batch/v1/namespaces/{namespace}/jobs/{name}");
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|source| failure("get Job", &path, source))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        successful_response(response, "get Job", &path)?
            .json()
            .await
            .map(Some)
            .map_err(|source| failure("decode Job", &path, source))
    }

    /// Read at most 32 KiB and 100 lines from one pod without following its log.
    pub async fn pod_log(&self, namespace: &str, pod: &str) -> Result<String, Error> {
        const LIMIT: usize = 32 * 1024;
        let path = format!(
            "/api/v1/namespaces/{namespace}/pods/{pod}/log?container=operation&tailLines=100&limitBytes={LIMIT}"
        );
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|source| failure("pod log", &path, source))?;
        let mut response = successful_response(response, "pod log", &path)?;
        let mut bytes = Vec::new();
        while bytes.len() < LIMIT {
            let Some(chunk) = response
                .chunk()
                .await
                .map_err(|source| failure("read log", &path, source))?
            else {
                break;
            };
            bytes.extend_from_slice(&chunk[..chunk.len().min(LIMIT - bytes.len())]);
        }
        let mut log = String::from_utf8_lossy(&bytes).into_owned();
        if log.len() > LIMIT {
            let mut end = LIMIT;
            while !log.is_char_boundary(end) {
                end -= 1;
            }
            log.truncate(end);
        }
        Ok(log)
    }
}

impl std::fmt::Debug for KubernetesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KubernetesApi")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

pub(crate) fn successful_response(
    response: reqwest::Response,
    operation: &'static str,
    reference: &str,
) -> Result<reqwest::Response, Error> {
    let status = response.status();
    if !status.is_success() {
        return Err(failure(
            operation,
            reference,
            format!("Kubernetes API answered {status}"),
        ));
    }
    Ok(response)
}
