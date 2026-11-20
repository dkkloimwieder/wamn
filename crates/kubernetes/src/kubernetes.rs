//! One typed Secret per project release, with one entry per credential UUID.

use crate::{
    CredentialSecret, Error, ErrorType, Store, StoreVersion, failure,
    client::{KubernetesApi, REQUEST_TIMEOUT, status_matches},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Api,
    api::{Patch, PatchParams},
};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct KubernetesStore {
    api: KubernetesApi,
}

impl KubernetesStore {
    pub fn in_cluster() -> Result<Self, Error> {
        Ok(Self {
            api: KubernetesApi::in_cluster()?,
        })
    }
    pub fn new(api: KubernetesApi) -> Self {
        Self { api }
    }

    fn secrets(&self, reference: &str) -> Result<Api<Secret>, Error> {
        if !reference.starts_with("wamn-credential-")
            || !reference
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(failure("resolve", reference, "invalid Secret name"));
        }
        Ok(Api::namespaced(self.api.client(), &self.api.namespace))
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
        patch["data"][version] =
            value.map_or(serde_json::Value::Null, |bytes| STANDARD.encode(bytes).into());
        if let Some(target) = target {
            patch["metadata"] = json!({
                "labels":{"wamn.org":target.org,"wamn.project":target.project,"wamn.env":target.env}
            });
        }
        let params = PatchParams {
            field_manager: Some("wamn-control".into()),
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
}

#[async_trait::async_trait]
impl Store for KubernetesStore {
    async fn put(
        &self,
        target: CredentialSecret<'_>,
        credential_id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, Error> {
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
        self.change(&target.name, &version, Some(value), Some(&target))
            .await?;
        Ok(StoreVersion {
            store: "k8s",
            reference: target.name,
            version,
        })
    }
    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
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
    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        self.change(reference, version, None, None).await
    }
}
