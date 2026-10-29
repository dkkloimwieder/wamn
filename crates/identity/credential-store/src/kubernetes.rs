//! One Secret per environment, with one data entry per credential UUID.

use crate::{
    Error, Store, StoreVersion, failure,
    kubernetes_api::{KubernetesApi, successful_response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct KubernetesStore {
    api: KubernetesApi,
    reference: String,
    labels: Value,
    #[cfg(test)]
    mock: Option<std::sync::Arc<std::sync::Mutex<tests::Mock>>>,
}

/// The Secret name fixed when the environment is provisioned.
pub fn secret_name(org: &str, project: &str, env: &str) -> String {
    format!("wamn-credential-{org}-{project}-{env}")
}

/// The account used by every host in the environment release.
pub fn service_account_name(org: &str, project: &str, env: &str) -> String {
    format!("wamn-host-{org}-{project}-{env}")
}

impl KubernetesStore {
    pub fn in_cluster(org: &str, project: &str, env: &str) -> Result<Self, Error> {
        Ok(Self {
            api: KubernetesApi::in_cluster()?,
            reference: secret_name(org, project, env),
            labels: json!({"wamn.org": org, "wamn.project": project, "wamn.env": env}),
            #[cfg(test)]
            mock: None,
        })
    }

    fn path(&self, reference: &str) -> Result<String, Error> {
        // References are names, never caller-supplied API paths.
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

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response, Error> {
        let mut request = self
            .api
            .http
            .request(method, format!("{}{path}", self.api.base))
            .bearer_auth(&self.api.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let request = request
            .build()
            .map_err(|source| failure("request", path, source))?;
        #[cfg(test)]
        if let Some(mock) = &self.mock {
            return Ok(mock.lock().unwrap().respond(&request));
        }
        self.api
            .http
            .execute(request)
            .await
            .map_err(|source| failure("request", path, source))
    }

    async fn read(&self, reference: &str) -> Result<Option<Value>, Error> {
        let response = self
            .request(Method::GET, &self.path(reference)?, None)
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = successful_response(response, "get", reference)?;
        response
            .json()
            .await
            .map(Some)
            .map_err(|source| failure("decode", reference, source))
    }

    async fn change(
        &self,
        reference: &str,
        version: &str,
        value: Option<&[u8]>,
    ) -> Result<(), Error> {
        uuid::Uuid::parse_str(version).map_err(|source| failure("resolve", reference, source))?;
        loop {
            let existing = self.read(reference).await?;
            let creates = existing.is_none();
            if creates && value.is_none() {
                return Ok(());
            }
            let mut document = existing.unwrap_or_else(|| json!({
                "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                "metadata": {"name": reference, "namespace": self.api.namespace, "labels": self.labels},
                "data": {}
            }));
            if !creates && document["metadata"]["resourceVersion"].as_str().is_none() {
                return Err(failure(
                    "update",
                    reference,
                    "Secret has no resourceVersion",
                ));
            }
            if document["data"].is_null() {
                document["data"] = json!({});
            }
            let data = document["data"]
                .as_object_mut()
                .ok_or_else(|| failure("decode", reference, "Secret data is not an object"))?;
            if let Some(value) = value {
                data.insert(version.to_owned(), STANDARD.encode(value).into());
            } else if data.remove(version).is_none() {
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
            let response = self.request(method, &path, Some(&document)).await?;
            if response.status() == StatusCode::CONFLICT
                || (!creates && response.status() == StatusCode::NOT_FOUND)
            {
                continue;
            }
            successful_response(response, "update", reference)?;
            return Ok(());
        }
    }
}

#[async_trait::async_trait]
impl Store for KubernetesStore {
    async fn put(&self, credential_id: uuid::Uuid, value: &[u8]) -> Result<StoreVersion, Error> {
        if ["wamn.org", "wamn.project", "wamn.env"]
            .iter()
            .any(|label| self.labels[label].as_str().is_none_or(str::is_empty))
        {
            return Err(failure(
                "put",
                &self.reference,
                "the environment coordinate is absent",
            ));
        }
        let version = credential_id.to_string();
        self.change(&self.reference, &version, Some(value)).await?;
        Ok(StoreVersion {
            store: "k8s",
            reference: self.reference.clone(),
            version,
        })
    }

    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error> {
        let document = self
            .read(reference)
            .await?
            .ok_or_else(|| failure("get", reference, "Secret is absent"))?;
        let encoded = document["data"][version]
            .as_str()
            .ok_or_else(|| failure("get", reference, "credential data is absent"))?;
        STANDARD
            .decode(encoded)
            .map_err(|source| failure("decode", reference, source))
    }

    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error> {
        self.change(reference, version, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    type Requests = Arc<Mutex<Vec<(String, Value)>>>;

    pub(super) struct Mock {
        replies: VecDeque<(u16, Value)>,
        observed: Requests,
    }

    impl std::fmt::Debug for Mock {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Mock")
                .field("remaining", &self.replies.len())
                .finish_non_exhaustive()
        }
    }

    impl Mock {
        pub(super) fn respond(&mut self, request: &reqwest::Request) -> reqwest::Response {
            assert_eq!(
                request.headers()[reqwest::header::AUTHORIZATION],
                "Bearer fixture-token"
            );
            let value = request.body().map_or(Value::Null, |body| {
                serde_json::from_slice(body.as_bytes().unwrap()).unwrap()
            });
            self.observed.lock().unwrap().push((
                format!("{} {} HTTP/1.1", request.method(), request.url().path()),
                value,
            ));
            let (status, body) = self.replies.pop_front().expect("unexpected request");
            http::Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(body.to_string())
                .unwrap()
                .into()
        }
    }

    fn mock(replies: Vec<(u16, Value)>) -> (KubernetesStore, Requests) {
        let requests = Requests::default();
        let store = KubernetesStore {
            api: KubernetesApi {
                http: reqwest::Client::new(),
                base: "https://kubernetes.invalid".into(),
                token: "fixture-token".into(),
                namespace: "hosts".into(),
            },
            reference: secret_name("acme", "orders", "dev"),
            labels: json!({"wamn.org":"acme","wamn.project":"orders","wamn.env":"dev"}),
            mock: Some(Arc::new(Mutex::new(Mock {
                replies: replies.into(),
                observed: requests.clone(),
            }))),
        };
        (store, requests)
    }

    #[tokio::test]
    async fn creates_one_environment_secret_and_reads_the_uuid_entry() {
        let id = uuid::Uuid::new_v4();
        let (store, requests) = mock(vec![
            (404, json!({})),
            (201, json!({})),
            (
                200,
                json!({"data":{id.to_string():STANDARD.encode(b"value")}}),
            ),
        ]);
        let stored = store.put(id, b"value").await.unwrap();
        assert_eq!(stored.store, "k8s");
        assert_eq!(stored.reference, "wamn-credential-acme-orders-dev");
        assert_eq!(stored.version, id.to_string());
        assert_eq!(
            store.get(&stored.reference, &stored.version).await.unwrap(),
            b"value"
        );
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests[1].0,
            "POST /api/v1/namespaces/hosts/secrets HTTP/1.1"
        );
        assert_eq!(requests[1].1["metadata"]["labels"], store.labels);
        assert_eq!(
            requests[1].1["data"][id.to_string()],
            STANDARD.encode(b"value")
        );
    }

    #[tokio::test]
    async fn put_rereads_after_conflict_and_keeps_other_entries() {
        let id = uuid::Uuid::new_v4();
        let (store, requests) = mock(vec![
            (
                200,
                json!({"metadata":{"resourceVersion":"1"},"data":{"other":"b2xk"}}),
            ),
            (409, json!({})),
            (
                200,
                json!({"metadata":{"resourceVersion":"2"},"data":{"other":"bmV3"}}),
            ),
            (200, json!({})),
        ]);
        store.put(id, b"value").await.unwrap();
        let requests = requests.lock().unwrap();
        for index in [1, 3] {
            assert!(requests[index].0.starts_with(
                "PUT /api/v1/namespaces/hosts/secrets/wamn-credential-acme-orders-dev "
            ));
        }
        assert_eq!(requests[1].1["metadata"]["resourceVersion"], "1");
        assert_eq!(requests[3].1["metadata"]["resourceVersion"], "2");
        assert_eq!(requests[3].1["data"]["other"], "bmV3");
    }

    #[tokio::test]
    async fn destroy_retries_conflicts_and_absent_entries_are_done() {
        let id = uuid::Uuid::new_v4().to_string();
        let (store, requests) = mock(vec![
            (
                200,
                json!({"metadata":{"resourceVersion":"1"},"data":{&id:"dmFsdWU=","other":"b2xk"}}),
            ),
            (409, json!({})),
            (
                200,
                json!({"metadata":{"resourceVersion":"2"},"data":{&id:"dmFsdWU=","other":"bmV3"}}),
            ),
            (200, json!({})),
            (200, json!({"metadata":{"resourceVersion":"3"},"data":{}})),
            (404, json!({})),
        ]);
        for _ in 0..3 {
            store.destroy(&store.reference, &id).await.unwrap();
        }
        let requests = requests.lock().unwrap();
        assert_eq!(requests[3].1["metadata"]["resourceVersion"], "2");
        assert_eq!(requests[3].1["data"], json!({"other":"bmV3"}));
        assert_eq!(requests.len(), 6);
    }

    #[tokio::test]
    async fn a_create_race_rereads_and_updates_the_existing_secret() {
        let id = uuid::Uuid::new_v4();
        let (store, requests) = mock(vec![
            (404, json!({})),
            (409, json!({})),
            (
                200,
                json!({"metadata":{"resourceVersion":"1"},"data":{"other":"dmFsdWU="}}),
            ),
            (200, json!({})),
        ]);
        store.put(id, b"value").await.unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[1].0.starts_with("POST "));
        assert!(requests[3].0.starts_with("PUT "));
        assert_eq!(requests[3].1["data"]["other"], "dmFsdWU=");
    }
    #[tokio::test]
    async fn failed_or_redirected_writes_never_return_a_reference() {
        for status in [302, 403, 500] {
            let (store, requests) = mock(vec![(404, json!({})), (status, json!({}))]);
            assert!(store.put(uuid::Uuid::new_v4(), b"value").await.is_err());
            assert_eq!(requests.lock().unwrap().len(), 2);
        }
    }
}
