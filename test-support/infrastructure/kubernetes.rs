//! Repository-only Kubernetes adapters used to arrange and observe live proofs.
//!
//! This module can park and restore a deployment around a proof. Production
//! wake actuation remains behind the deployed waker process boundary.

use std::{fmt, path::Path};

use anyhow::{Context as _, bail};

const API_BASE: &str = "https://kubernetes.default.svc";
const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// Desired and observed replica counts from a Deployment `Scale` subresource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentScale {
    /// Desired replicas (`.spec.replicas`).
    pub spec_replicas: i32,
    /// Observed replicas (`.status.replicas`).
    pub status_replicas: i32,
}

/// An in-cluster proof client for a Deployment's Kubernetes `Scale` subresource.
pub struct KubeScale {
    http: reqwest::Client,
    base: String,
    namespace: String,
    token: String,
}

impl fmt::Debug for KubeScale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KubeScale")
            .field("base", &self.base)
            .field("namespace", &self.namespace)
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl KubeScale {
    /// Build from the pod's mounted service-account token, CA, and namespace.
    pub fn in_cluster() -> anyhow::Result<Self> {
        let dir = Path::new(SERVICE_ACCOUNT_DIR);
        let token = std::fs::read_to_string(dir.join("token"))
            .context("read service-account token")?
            .trim()
            .to_string();
        let ca_pem = std::fs::read(dir.join("ca.crt")).context("read service-account CA")?;
        let namespace = std::fs::read_to_string(dir.join("namespace"))
            .context("read service-account namespace")?
            .trim()
            .to_string();

        let certs = reqwest::Certificate::from_pem_bundle(&ca_pem)
            .context("parse service-account CA bundle")?;
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .tls_certs_only(certs)
            .build()
            .context("build Kubernetes HTTPS client")?;
        Ok(Self {
            http,
            base: API_BASE.to_string(),
            namespace,
            token,
        })
    }

    /// Read desired and observed replica counts from a Deployment.
    pub async fn get_scale(&self, deployment: &str) -> anyhow::Result<DeploymentScale> {
        let response = self
            .http
            .get(self.scale_url(deployment))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET scale")?;
        let status = response.status();
        let body = response.text().await.context("read GET scale body")?;
        if !status.is_success() {
            bail!("GET scale {deployment}: {status}: {body}");
        }
        parse_scale(&body)
    }

    /// Set a Deployment's desired replica count with a merge patch.
    pub async fn set_replicas(&self, deployment: &str, replicas: i32) -> anyhow::Result<()> {
        let body = serde_json::json!({ "spec": { "replicas": replicas } }).to_string();
        let response = self
            .http
            .patch(self.scale_url(deployment))
            .bearer_auth(&self.token)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/merge-patch+json",
            )
            .body(body)
            .send()
            .await
            .context("PATCH scale")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("PATCH scale {deployment}={replicas}: {status}: {body}");
        }
        Ok(())
    }

    fn scale_url(&self, deployment: &str) -> String {
        format!(
            "{}/apis/apps/v1/namespaces/{}/deployments/{}/scale",
            self.base, self.namespace, deployment
        )
    }
}

fn parse_scale(body: &str) -> anyhow::Result<DeploymentScale> {
    let value: serde_json::Value = serde_json::from_str(body).context("parse Scale JSON")?;
    Ok(DeploymentScale {
        spec_replicas: value["spec"]["replicas"].as_i64().unwrap_or(0) as i32,
        status_replicas: value["status"]["replicas"].as_i64().unwrap_or(0) as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_scale_reads_desired_and_observed_replicas() {
        let scale = parse_scale(
            r#"{"kind":"Scale","spec":{"replicas":2},"status":{"replicas":1,"selector":"app=runner"}}"#,
        )
        .expect("scale parses");

        assert_eq!(
            scale,
            DeploymentScale {
                spec_replicas: 2,
                status_replicas: 1,
            }
        );
    }

    #[test]
    fn deployment_scale_treats_absent_status_as_zero() {
        let scale = parse_scale(r#"{"spec":{"replicas":0},"status":{}}"#).expect("scale parses");

        assert_eq!(
            scale,
            DeploymentScale {
                spec_replicas: 0,
                status_replicas: 0,
            }
        );
    }
}
