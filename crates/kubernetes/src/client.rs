//! Kubernetes client configuration and shared resource access.

use crate::{Error, ErrorType, Target, failure};
use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited, combinators::UnsyncBoxBody};
use hyper::{Request, Response};
use k8s_openapi::api::{
    authentication::v1::{TokenReview, TokenReviewSpec},
    core::v1::{ConfigMap, Secret},
};
use kube::{
    Api, Client, Config, Resource,
    api::{ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams, PostParams},
    client::{Body, ClientBuilder, DynBody},
    config::{KubeConfigOptions, Kubeconfig},
};
use serde::de::DeserializeOwned;
use std::{
    path::PathBuf,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tower::{BoxError, Service, layer::layer_fn};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// A Kubernetes client with the namespace selected by its configuration.
#[derive(Clone)]
pub struct KubernetesApi {
    client: Client,
    pub namespace: String,
}

impl std::fmt::Debug for KubernetesApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KubernetesApi")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

/// The ServiceAccount authenticated by Kubernetes.
#[derive(Debug)]
pub struct ServiceAccountIdentity {
    pub namespace: String,
    pub name: String,
    pub pod_name: Option<String>,
}

impl KubernetesApi {
    pub fn from_config(config: Config) -> Result<Self, Error> {
        Self::configured(config, BODY_LIMIT)
    }

    fn configured(mut config: Config, body_limit: usize) -> Result<Self, Error> {
        config.default_retry = false;
        let namespace = config.default_namespace.trim().to_owned();
        let client = ClientBuilder::try_from(config)
            .map_err(|error| failure("build client", "k8s", error))?
            .with_layer(&layer_fn(|service| Responses {
                service,
                body_limit,
            }))
            .build();
        Ok(Self { client, namespace })
    }

    pub fn in_cluster() -> Result<Self, Error> {
        Self::from_config(cluster_config()?)
    }

    pub(crate) fn host_credential_secrets() -> anyhow::Result<Api<Secret>> {
        let mut config = Config::incluster().context("load in-cluster credential configuration")?;
        config.default_retry = false;
        let namespace = config.default_namespace.clone();
        let client = Client::try_from(config).context("build in-cluster credential client")?;
        Ok(Api::namespaced(client, &namespace))
    }

    pub(crate) fn into_secrets(self) -> Api<Secret> {
        Api::namespaced(self.client, &self.namespace)
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// Review an opaque token for the receiving service's audience.
    pub async fn review_service_account_token(
        &self,
        token: &str,
        audience: &str,
    ) -> Result<Option<ServiceAccountIdentity>, Error> {
        let reviews = Api::<TokenReview>::all(self.client());
        let review = TokenReview {
            spec: TokenReviewSpec {
                token: Some(token.to_owned()),
                audiences: Some(vec![audience.to_owned()]),
            },
            ..TokenReview::default()
        };
        let reply = tokio::time::timeout(
            REQUEST_TIMEOUT,
            reviews.create(&PostParams::default(), &review),
        )
        .await
        .map_err(|error| failure("review token", audience, error))?
        .map_err(|error| failure("review token", audience, error))?;
        let Some(status) = reply.status else {
            return Err(failure("decode TokenReview", audience, "status is absent"));
        };
        if !status.authenticated.unwrap_or_default()
            || status.error.is_some_and(|error| !error.is_empty())
            || !status
                .audiences
                .is_some_and(|values| values.iter().any(|value| value == audience))
        {
            return Ok(None);
        }
        let Some(user) = status.user else {
            return Ok(None);
        };
        let Some((namespace, name)) = user
            .username
            .as_deref()
            .and_then(|username| username.strip_prefix("system:serviceaccount:"))
            .and_then(|identity| identity.split_once(':'))
            .filter(|(namespace, name)| {
                !namespace.is_empty() && !name.is_empty() && !name.contains(':')
            })
        else {
            return Ok(None);
        };
        let pod_name = user
            .extra
            .and_then(|mut extra| extra.remove("authentication.kubernetes.io/pod-name"))
            .filter(|names| names.len() == 1)
            .and_then(|mut names| names.pop());
        Ok(Some(ServiceAccountIdentity {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            pod_name,
        }))
    }

    /// Read a named Secret in the selected namespace within the request deadline.
    ///
    /// # Errors
    /// Returns the Kubernetes request error or deadline expiry.
    pub async fn secret(&self, name: &str) -> anyhow::Result<Secret> {
        let secrets = Api::<Secret>::namespaced(self.client(), &self.namespace);
        Ok(tokio::time::timeout(REQUEST_TIMEOUT, secrets.get(name)).await??)
    }

    /// Read a public root from the named ConfigMap in the selected namespace.
    ///
    /// # Errors
    /// Returns an error if the request fails or the ConfigMap has no certificate.
    pub async fn public_root(&self, name: &str) -> anyhow::Result<Vec<u8>> {
        let maps = Api::<ConfigMap>::namespaced(self.client(), &self.namespace);
        let map = tokio::time::timeout(REQUEST_TIMEOUT, maps.get(name)).await??;
        Ok(map
            .data
            .and_then(|mut data| data.remove("ca.crt"))
            .filter(|pem| !pem.is_empty())
            .with_context(|| format!("ConfigMap {}/{name} lacks ca.crt", self.namespace))?
            .into_bytes())
    }

    pub async fn patch_secret_data(
        &self,
        name: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), Error> {
        self.patch_namespaced_secret_data(&self.namespace, name, key, value)
            .await
    }

    pub async fn patch_namespaced_secret_data(
        &self,
        namespace: &str,
        name: &str,
        key: &str,
        value: &[u8],
    ) -> Result<(), Error> {
        let secrets = Api::<Secret>::namespaced(self.client(), namespace);
        let patch = serde_json::json!({"data": {key: STANDARD.encode(value)}});
        tokio::time::timeout(
            REQUEST_TIMEOUT,
            secrets.patch(name, &PatchParams::default(), &Patch::Merge(&patch)),
        )
        .await
        .map_err(|error| failure("patch Secret", name, error))?
        .map_err(|error| failure("patch Secret", name, error))?;
        Ok(())
    }
}

fn cluster_config() -> Result<Config, Error> {
    if std::env::var_os("KUBERNETES_SERVICE_HOST").is_none() {
        let mut error = failure("read endpoint", "k8s", std::env::VarError::NotPresent);
        error.type_ = Some(ErrorType::NoCluster);
        return Err(error);
    }
    Config::incluster().map_err(|error| failure("load ServiceAccount configuration", "k8s", error))
}

struct Responses<S> {
    service: S,
    body_limit: usize,
}

impl<S> Service<Request<Body>> for Responses<S>
where
    S: Service<Request<Body>, Response = Response<Box<DynBody>>, Error = BoxError>,
    S::Future: Send + 'static,
{
    type Response = Response<UnsyncBoxBody<Bytes, BoxError>>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(context)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let path = request.uri().path();
        let sensitive = path
            .split('/')
            .any(|segment| matches!(segment, "secrets" | "tokenreviews"));
        let result = self.service.call(request);
        let body_limit = self.body_limit;
        Box::pin(async move {
            let response = match result.await {
                Ok(response) => response,
                Err(error) => return Err(error),
            };
            let (parts, body) = response.into_parts();
            if parts.status.is_redirection() {
                return Err(format!("Kubernetes API answered {}", parts.status).into());
            }
            let body = limited_body(body, body_limit);
            let body = if sensitive
                && (parts.status.is_client_error() || parts.status.is_server_error())
            {
                // Remove details before kube-rs records the error response.
                let bytes = match body.collect().await {
                    Ok(body) => body.to_bytes(),
                    Err(error) => return Err(error),
                };
                let code = parts.status.as_u16();
                let status = serde_json::from_slice::<kube::core::Status>(&bytes).map_or_else(
                    |_| kube::core::Status::failure("", "").with_code(code),
                    |status| kube::core::Status::failure("", &status.reason).with_code(code),
                );
                Full::new(Bytes::from(serde_json::to_vec(&status)?))
                    .map_err(|never| -> BoxError { match never {} })
                    .boxed_unsync()
            } else {
                body
            };
            Ok(Response::from_parts(parts, body))
        })
    }
}

fn limited_body(body: Box<DynBody>, limit: usize) -> UnsyncBoxBody<Bytes, BoxError> {
    Limited::new(body, limit).boxed_unsync()
}

impl Target {
    pub fn in_cluster(
        kubeconfig: PathBuf,
        context: &str,
        namespace: String,
    ) -> Result<Self, Error> {
        Ok(Self {
            kubeconfig,
            context: context.to_owned(),
            namespace,
            client: KubernetesApi::configured(cluster_config()?, usize::MAX)?,
        })
    }

    pub async fn developer(namespace: &str) -> anyhow::Result<Self> {
        let kubeconfig = Kubeconfig::read().context("read kubeconfig")?;
        let context = kubeconfig
            .current_context
            .as_deref()
            .filter(|value| !value.is_empty())
            .context("kubeconfig has no current context")?
            .to_owned();
        let config = Config::from_custom_kubeconfig(
            kubeconfig,
            &KubeConfigOptions {
                context: Some(context.clone()),
                ..KubeConfigOptions::default()
            },
        )
        .await
        .with_context(|| format!("load Kubernetes context {context}"))?;
        Ok(Self {
            kubeconfig: PathBuf::new(),
            context,
            namespace: namespace.to_owned(),
            client: KubernetesApi::configured(config, usize::MAX)?,
        })
    }
}

pub fn status_matches(error: &kube::Error, code: u16, reason: &str) -> bool {
    matches!(error, kube::Error::Api(status) if status.code == code && status.reason == reason)
}

pub fn resource(
    client: Client,
    namespace: &str,
    group: &str,
    version: &str,
    name: &str,
    plural: &str,
) -> Api<DynamicObject> {
    let resource =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(group, version, name), plural);
    Api::namespaced_with(client, namespace, &resource)
}

pub fn workloads(client: Client, namespace: &str) -> Api<DynamicObject> {
    resource(
        client,
        namespace,
        "runtime.wasmcloud.dev",
        "v1alpha1",
        "WorkloadDeployment",
        "workloaddeployments",
    )
}

/// Poll a named object with an overall deadline using only GET.
pub async fn wait_named<K, T>(
    api: &Api<K>,
    name: &str,
    timeout: Duration,
    mut ready: impl FnMut(Option<K>) -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T>
where
    K: Clone + std::fmt::Debug + DeserializeOwned + Resource,
{
    let path = api.resource_url();
    tokio::time::timeout(timeout, async {
        loop {
            let object = api.get_opt(name).await?;
            if let Some(result) = ready(object)? {
                return Ok(result);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .flatten()
    .with_context(|| format!("wait for {path}/{name}"))
}

pub fn ready(object: &DynamicObject) -> bool {
    object.data["status"]["conditions"]
        .as_array()
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
        })
}

/// Delete a resource by name, optionally waiting for the deleted UID to disappear.
pub async fn delete_named<K>(api: &Api<K>, name: &str, wait: bool) -> anyhow::Result<bool>
where
    K: Clone + std::fmt::Debug + DeserializeOwned + Resource,
{
    use kube::api::{DeleteParams, PropagationPolicy};

    let params = DeleteParams {
        propagation_policy: Some(PropagationPolicy::Background),
        ..DeleteParams::default()
    };
    let uid = match api.delete(name, &params).await {
        Ok(reply) => reply.either(
            |mut object| object.meta_mut().uid.take(),
            |status| {
                status
                    .details
                    .map(|details| details.uid)
                    .filter(|uid| !uid.is_empty())
            },
        ),
        Err(error) if status_matches(&error, 404, "NotFound") => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if wait {
        // Preserve the default deletion deadline and wait only for the deleted object.
        wait_named(api, name, Duration::from_hours(168), |object| {
            let absent = object.is_none_or(|object| {
                uid.as_ref()
                    .is_some_and(|uid| object.meta().uid.as_ref() != Some(uid))
            });
            Ok(absent.then_some(()))
        })
        .await?;
    }
    Ok(true)
}

pub const APPLY_MANAGER: &str = "wamn";

/// Borrow a decoded entry from a Kubernetes Secret.
pub fn secret_entry<'a>(secret: &'a Secret, key: &str) -> anyhow::Result<&'a [u8]> {
    secret
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .map(|bytes| bytes.0.as_slice())
        .with_context(|| format!("database Secret key {key} is absent"))
}
