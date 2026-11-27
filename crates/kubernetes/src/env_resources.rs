//! Kubernetes operations used by the operator to provision an env.

use anyhow::{Context as _, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    Api,
    api::{Patch, PatchParams, PostParams},
};
use serde::Deserialize as _;
use serde_json::{Value, json};

use crate::{
    Target,
    client::{APPLY_MANAGER, KubernetesApi, REQUEST_TIMEOUT, resource, status_matches},
};

/// Apply a namespaced env provisioning object.
pub async fn apply(target: &Target, object: &Value) -> anyhow::Result<()> {
    let kind = object["kind"]
        .as_str()
        .context("env resource has no kind")?;
    let (group, plural) = match kind {
        "ServiceAccount" => ("", "serviceaccounts"),
        "ResourceQuota" => ("", "resourcequotas"),
        "LimitRange" => ("", "limitranges"),
        "Role" => ("rbac.authorization.k8s.io", "roles"),
        "RoleBinding" => ("rbac.authorization.k8s.io", "rolebindings"),
        "NetworkPolicy" => ("networking.k8s.io", "networkpolicies"),
        _ => bail!("unsupported env provisioning kind {kind}"),
    };
    let metadata = &object["metadata"];
    let namespace = metadata["namespace"]
        .as_str()
        .context("env resource has no namespace")?;
    let name = metadata["name"]
        .as_str()
        .context("env resource has no name")?;
    let api = resource(target.client.client(), namespace, group, "v1", kind, plural);
    let params = PatchParams::apply(APPLY_MANAGER);
    let patched = async {
        tokio::time::timeout(
            REQUEST_TIMEOUT,
            api.patch(name, &params, &Patch::Apply(object)),
        )
        .await?
        .map_err(anyhow::Error::from)
    };
    patched
        .await
        .with_context(|| format!("apply {kind} {namespace}/{name}"))?;
    Ok(())
}

/// Publish both keys together to the Secret the operator prepared for the Job.
pub async fn publish_login(
    api: &KubernetesApi,
    namespace: &str,
    name: &str,
    login: &str,
    password: &str,
) -> anyhow::Result<()> {
    let secrets = Api::<Secret>::namespaced(api.client(), namespace);
    let patch = json!({
        "data": {"username": STANDARD.encode(login), "password": STANDARD.encode(password)},
    });
    tokio::time::timeout(
        REQUEST_TIMEOUT,
        secrets.patch(name, &PatchParams::default(), &Patch::Merge(&patch)),
    )
    .await?
    .with_context(|| format!("publish worker login to Secret {namespace}/{name}"))?;
    Ok(())
}

/// Create a Secret once, preserving all data if it already exists.
pub async fn create_secret(target: &Target, object: &Value) -> anyhow::Result<()> {
    let secret = Secret::deserialize(object)?;
    let namespace = secret
        .metadata
        .namespace
        .as_deref()
        .context("Secret has no namespace")?;
    let name = secret
        .metadata
        .name
        .as_deref()
        .context("Secret has no name")?;
    let secrets = Api::<Secret>::namespaced(target.client.client(), namespace);
    let params = PostParams::default();
    match tokio::time::timeout(REQUEST_TIMEOUT, secrets.create(&params, &secret)).await? {
        Ok(_) => Ok(()),
        Err(error) if status_matches(&error, 409, "AlreadyExists") => Ok(()),
        Err(error) => Err(error).with_context(|| format!("create Secret {namespace}/{name}")),
    }
}

/// Create an OAuth Secret once and preserve its existing key on a retry.
pub async fn ensure_oauth_key(
    target: &Target,
    namespace: &str,
    name: &str,
    key: &[u8; 32],
) -> anyhow::Result<()> {
    create_secret(
        target,
        &json!({
            "apiVersion":"v1", "kind":"Secret", "type":"Opaque",
            "metadata":{"name":name,"namespace":namespace},
            "data":{"key":STANDARD.encode(key)},
        }),
    )
    .await
}
