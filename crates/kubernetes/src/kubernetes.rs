//! One typed Secret per environment, with one entry per credential UUID.

use crate::{
    Error, ErrorType, Store, StoreVersion, failure,
    kubernetes_api::{KubernetesApi, successful},
    secret::Secret,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hyper::{Method, StatusCode};
use serde_json::json;
use wamn_registry::{Triple, credential_secret_name};

#[derive(Clone, Debug)]
pub struct KubernetesStore {
    api: KubernetesApi,
}

impl KubernetesStore {
    pub fn in_cluster() -> Result<Self, Error> {
        if std::env::var_os("KUBERNETES_SERVICE_HOST").is_none() {
            let mut error = failure("read endpoint", "k8s", std::env::VarError::NotPresent);
            error.type_ = Some(ErrorType::NoCluster);
            return Err(error);
        }
        Ok(Self {
            api: KubernetesApi::in_cluster()?,
        })
    }
    pub fn new(api: KubernetesApi) -> Self {
        Self { api }
    }

    fn path(&self, reference: &str) -> Result<String, Error> {
        if !reference.starts_with("wamn-credential-")
            || !reference
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(failure("resolve", reference, "invalid Secret name"));
        }
        Ok(format!(
            "/api/v1/namespaces/{}/secrets/{reference}",
            self.api.namespace
        ))
    }

    async fn read(&self, reference: &str) -> Result<Option<Secret>, Error> {
        self.api
            .json(Method::GET, &self.path(reference)?, None)
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| failure("decode Secret", reference, e))
    }

    async fn change(
        &self,
        reference: &str,
        version: &str,
        value: Option<&[u8]>,
        target: Option<&Triple>,
    ) -> Result<(), Error> {
        uuid::Uuid::parse_str(version).map_err(|e| failure("resolve", reference, e))?;
        for _ in 0..5 {
            let existing = self.read(reference).await?;
            let creates = existing.is_none();
            if creates && value.is_none() {
                return Ok(());
            }
            let mut document =
                existing.unwrap_or_else(|| Secret::new(&self.api.namespace, reference));
            if !creates && document.metadata.resource_version.is_none() {
                return Err(failure(
                    "update",
                    reference,
                    "Secret has no resourceVersion",
                ));
            }
            if let Some(target) = target {
                document.metadata.attributes.insert("labels".into(), json!({"wamn.org":target.org,"wamn.project":target.project,"wamn.env":target.env.as_str()}));
            }
            if let Some(value) = value {
                document.data.insert(version.into(), STANDARD.encode(value));
            } else if document.data.remove(version).is_none() {
                return Ok(());
            }
            let path = self.path(reference)?;
            let (method, path) = if creates {
                (
                    Method::POST,
                    path.rsplit_once('/').expect("Secret path").0.to_owned(),
                )
            } else {
                (Method::PUT, path)
            };
            let body = serde_json::to_value(document)
                .map_err(|e| failure("encode Secret", reference, e))?;
            let status = self.api.write(method, &path, &body).await?;
            if status == StatusCode::CONFLICT || (!creates && status == StatusCode::NOT_FOUND) {
                continue;
            }
            successful(status, reference)?;
            return Ok(());
        }
        Err(failure(
            "update",
            reference,
            "Secret changed during five writes",
        ))
    }
}

#[async_trait::async_trait]
impl Store for KubernetesStore {
    async fn put(
        &self,
        target: &Triple,
        credential_id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, Error> {
        if [
            target.org.as_str(),
            target.project.as_str(),
            target.env.as_str(),
        ]
        .into_iter()
        .any(str::is_empty)
        {
            return Err(failure(
                "put",
                "k8s",
                "the environment coordinate is absent",
            ));
        }
        let reference = credential_secret_name(&target.org, &target.project, target.env.as_str());
        let version = credential_id.to_string();
        self.change(&reference, &version, Some(value), Some(target))
            .await?;
        Ok(StoreVersion {
            store: "k8s",
            reference,
            version,
        })
    }
    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
        let document = self
            .read(reference)
            .await?
            .ok_or_else(|| failure("get", reference, "Secret is absent"))?;
        let encoded = document
            .data
            .get(version)
            .ok_or_else(|| failure("get", reference, "credential data is absent"))?;
        STANDARD
            .decode(encoded)
            .map_err(|e| failure("decode", reference, e))
    }
    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        self.change(reference, version, None, None).await
    }
}
