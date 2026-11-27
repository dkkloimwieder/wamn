//! One typed Secret per project release, with one entry per credential UUID.

use crate::{
    Error, ErrorType,
    client::{KubernetesApi, REQUEST_TIMEOUT, status_matches},
    failure,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Api,
    api::{Patch, PatchParams},
};
use serde_json::json;
use tokio::sync::OnceCell;
/// A credential Secret name and its environment labels.
#[derive(Debug)]
pub struct CredentialSecret<'a> {
    pub name: String,
    pub org: &'a str,
    pub env: &'a str,
    pub project: &'a str,
}

/// The Kubernetes operations on versioned project Secrets.
#[derive(Clone, Debug)]
pub struct CredentialSecrets {
    secrets: Api<Secret>,
}

impl CredentialSecrets {
    pub fn in_cluster() -> Result<Self, Error> {
        Ok(Self::new(KubernetesApi::in_cluster()?))
    }

    /// Read and patch credential versions in a namespace selected by the caller.
    pub fn in_namespace(namespace: &str) -> Result<Self, Error> {
        let mut api = KubernetesApi::in_cluster()?;
        namespace.clone_into(&mut api.namespace);
        Ok(Self::new(api))
    }

    pub(crate) fn new(api: KubernetesApi) -> Self {
        Self {
            secrets: api.into_secrets(),
        }
    }

    fn secrets(&self, reference: &str) -> Result<&Api<Secret>, Error> {
        if !reference.starts_with("wamn-credential-")
            || !reference
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(failure("resolve", reference, "invalid Secret name"));
        }
        Ok(&self.secrets)
    }

    async fn read(&self, reference: &str) -> Result<Option<Secret>, Error> {
        let secrets = self.secrets(reference)?;
        tokio::time::timeout(REQUEST_TIMEOUT, secrets.get_opt(reference))
            .await
            .map_err(|error| failure("get Secret", reference, error))?
            .map_err(|error| failure("get Secret", reference, error))
    }

    async fn change(
        &self,
        reference: &str,
        version: &str,
        value: Option<&[u8]>,
        target: Option<&CredentialSecret<'_>>,
    ) -> Result<(), Error> {
        uuid::Uuid::parse_str(version).map_err(|e| failure("resolve", reference, e))?;
        let secrets = self.secrets(reference)?;
        let mut patch = json!({"data":{}});
        patch["data"][version] = value.map_or(serde_json::Value::Null, |bytes| {
            STANDARD.encode(bytes).into()
        });
        if let Some(target) = target {
            patch["metadata"] = json!({
                "labels":{"wamn.org":target.org,"wamn.project":target.project,"wamn.env":target.env}
            });
        }
        let params = PatchParams {
            field_manager: Some("wamn".into()),
            ..PatchParams::default()
        };
        match tokio::time::timeout(
            REQUEST_TIMEOUT,
            secrets.patch(reference, &params, &Patch::Merge(&patch)),
        )
        .await
        .map_err(|error| failure("patch Secret", reference, error))?
        {
            Ok(_) => Ok(()),
            Err(error) if status_matches(&error, 404, "NotFound") => {
                if value.is_none() {
                    return Ok(());
                }
                let mut failure = failure("patch Secret", reference, error);
                failure.type_ = Some(ErrorType::ProjectCredentialSecretMissing);
                Err(failure)
            }
            Err(error) => Err(failure("patch Secret", reference, error)),
        }
    }

    pub async fn put(
        &self,
        target: &CredentialSecret<'_>,
        credential_id: uuid::Uuid,
        value: &[u8],
    ) -> Result<String, Error> {
        if [target.org, target.project, target.env]
            .into_iter()
            .any(str::is_empty)
        {
            return Err(failure(
                "put",
                "k8s",
                "the environment coordinate is absent",
            ));
        }
        let version = credential_id.to_string();
        self.change(&target.name, &version, Some(value), Some(target))
            .await?;
        Ok(version)
    }
    pub async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
        let document = self
            .read(reference)
            .await?
            .ok_or_else(|| failure("get", reference, "Secret is absent"))?;
        document
            .data
            .and_then(|mut data| data.remove(version))
            .map(|bytes| bytes.0)
            .ok_or_else(|| failure("get", reference, "credential data is absent"))
    }
    pub async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        self.change(reference, version, None, None).await
    }
}

/// The host's lazy Kubernetes credential transport.
#[derive(Debug)]
pub struct KubernetesCredentials {
    host_store: OnceCell<CredentialSecrets>,
}

impl KubernetesCredentials {
    /// Cache the host's namespace client after the first successful initialization.
    pub fn for_host() -> Self {
        Self {
            host_store: OnceCell::new(),
        }
    }

    /// Read a versioned Secret value through the selected namespace client.
    pub async fn read(&self, reference: &str, version: &str) -> anyhow::Result<Vec<u8>> {
        let store = self
            .host_store
            .get_or_try_init(|| async {
                if std::env::var_os("KUBERNETES_SERVICE_HOST").is_none() {
                    let reason = "no credential store in local mode";
                    tracing::warn!("{reason}");
                    anyhow::bail!(reason);
                }
                Ok::<_, anyhow::Error>(CredentialSecrets {
                    secrets: KubernetesApi::host_credential_secrets()?,
                })
            })
            .await?;
        Ok(store.get(reference, version).await?)
    }
}
