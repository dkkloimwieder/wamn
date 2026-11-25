//! Kubernetes operations for the org installer.

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet},
    batch::v1::Job,
    core::v1::{ConfigMap, Namespace, Pod, Secret, Service},
    discovery::v1::EndpointSlice,
};
use kube::{
    Api, ResourceExt as _,
    api::{ListParams, Patch, PatchParams, PostParams},
    core::Selector,
};
use serde::Deserialize as _;
use serde_json::{Value, json};
use std::{collections::BTreeSet, net::IpAddr, time::Duration};
use wamn_registry::{OrgId, name::REGISTRY_PULL};

use crate::{
    Target,
    client::{APPLY_MANAGER, wait_named},
    job,
};

const PLATFORM: &str = "platform";
const CREDENTIAL_JOB: &str = "wamn.dev/control-credential-job";
const REVISION: &str = "deployment.kubernetes.io/revision";

pub async fn ensure_helm_storage_namespace(target: &Target, storage: &str) -> anyhow::Result<()> {
    let apply = PatchParams::apply(APPLY_MANAGER);
    let namespaces = Api::<Namespace>::all(target.client.client());
    namespaces
        .patch(
            storage,
            &apply,
            &Patch::Apply(helm_storage_namespace(storage)),
        )
        .await
        .context("create the org Helm storage namespace")?;
    Ok(())
}

/// Build the Namespace that holds an org's Helm release records.
pub fn helm_storage_namespace(storage: &str) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Namespace",
        "metadata": {"name": storage, "labels": {"app.kubernetes.io/part-of": "wamn"}},
    })
}

pub async fn write_registry_credentials(
    target: &Target,
    org: &OrgId,
    credentials: &[u8],
) -> anyhow::Result<()> {
    let secrets = Api::<Secret>::namespaced(target.client.client(), org.as_str());
    secrets
        .patch(
            REGISTRY_PULL,
            &PatchParams::apply(APPLY_MANAGER),
            &Patch::Apply(registry_credentials(org, credentials)),
        )
        .await
        .with_context(|| format!("apply {REGISTRY_PULL} in {org}"))?;
    Ok(())
}

/// Build the org's image-pull Secret from its Docker configuration bytes.
pub fn registry_credentials(org: &OrgId, credentials: &[u8]) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Secret", "type": "kubernetes.io/dockerconfigjson",
        "metadata": {"name": REGISTRY_PULL, "namespace": org},
        "data": {".dockerconfigjson": STANDARD.encode(credentials)},
    })
}

pub async fn reload_control(
    target: &Target,
    org: &OrgId,
    sql_job_uid: &str,
    deadline: Duration,
) -> anyhow::Result<()> {
    let client = &target.client;
    // Control loads the mounted credentials at startup.
    let control = wamn_registry::name::control_resource(org);
    let restart = json!({
        "spec": {"template": {"metadata": {"annotations": {
            (CREDENTIAL_JOB): sql_job_uid,
        }}}},
    });
    let deployments = Api::<Deployment>::namespaced(client.client(), org.as_str());
    let patched = deployments
        .patch(&control, &PatchParams::default(), &Patch::Merge(restart))
        .await
        .context("reload the org database logins")?;
    let generation = patched
        .metadata
        .generation
        .context("control Deployment has no generation")?;
    let selector = Selector::try_from(
        patched
            .spec
            .context("control Deployment has no spec")?
            .selector,
    )
    .context("read the control Deployment selector")?;
    let params = ListParams::default().labels_from(&selector);
    let replicas = Api::<ReplicaSet>::namespaced(client.client(), org.as_str());
    let pods = Api::<Pod>::namespaced(client.client(), org.as_str());
    tokio::time::timeout(deadline, async {
        loop {
            let deployment = wait_named(&deployments, &control, deadline, |object| {
                Ok(object.filter(|deployment| {
                    let Some((spec, status)) =
                        deployment.spec.as_ref().zip(deployment.status.as_ref())
                    else {
                        return false;
                    };
                    let desired = spec.replicas.unwrap_or(1);
                    deployment
                        .metadata
                        .generation
                        .zip(status.observed_generation)
                        .is_some_and(|(current, observed)| {
                            current >= generation && observed >= current
                        })
                        && spec
                            .template
                            .metadata
                            .as_ref()
                            .and_then(|metadata| metadata.annotations.as_ref())
                            .and_then(|annotations| annotations.get(CREDENTIAL_JOB))
                            .map(String::as_str)
                            == Some(sql_job_uid)
                        && status.updated_replicas.unwrap_or_default() == desired
                        && status.replicas.unwrap_or_default() == desired
                        && status.available_replicas.unwrap_or_default() == desired
                }))
            })
            .await?;
            let uid = deployment
                .metadata
                .uid
                .as_deref()
                .context("control Deployment has no UID")?;
            let revision = deployment
                .annotations()
                .get(REVISION)
                .context("control Deployment has no revision")?;
            let (replicas, pods) = tokio::try_join!(replicas.list(&params), pods.list(&params))
                .context("list control ReplicaSets and Pods")?;
            let current = replicas.items.iter().find(|replica| {
                replica.annotations().get(REVISION) == Some(revision)
                    && replica
                        .owner_references()
                        .iter()
                        .any(|owner| owner.controller == Some(true) && owner.uid == uid)
            });
            if let Some(uid) = current.and_then(|replica| replica.metadata.uid.as_deref())
                && pods.items.iter().all(|pod| {
                    pod.owner_references()
                        .iter()
                        .any(|owner| owner.controller == Some(true) && owner.uid == uid)
                })
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .with_context(|| format!("wait for the control Deployment {org}/{control} rollout"))?
}

pub async fn config_map(target: &Target, namespace: &str, name: &str) -> anyhow::Result<Value> {
    let maps = Api::<ConfigMap>::namespaced(target.client.client(), namespace);
    let object = maps
        .get(name)
        .await
        .with_context(|| format!("read platform configuration {namespace}/{name}"))?;
    Ok(serde_json::to_value(object)?)
}

pub async fn secret_entry(target: &Target, name: &str, key: &str) -> anyhow::Result<Vec<u8>> {
    let secrets = Api::<Secret>::namespaced(target.client.client(), PLATFORM);
    let secret = secrets
        .get(name)
        .await
        .with_context(|| format!("read public CA Secret {PLATFORM}/{name}"))?;
    let bytes = secret
        .data
        .and_then(|mut data| data.remove(key))
        .with_context(|| format!("public CA Secret {PLATFORM}/{name} lacks {key}"))?;
    Ok(bytes.0)
}

/// Read the Kubernetes API Service and EndpointSlice addresses.
pub async fn api_addresses(client: kube::Client) -> anyhow::Result<BTreeSet<(IpAddr, i32)>> {
    let services = Api::<Service>::namespaced(client.clone(), "default");
    let endpoints = Api::<EndpointSlice>::namespaced(client, "default");
    let params = ListParams::default().labels("kubernetes.io/service-name=kubernetes");
    let (service, slices) = tokio::try_join!(services.get("kubernetes"), endpoints.list(&params))?;
    let mut destinations = BTreeSet::new();
    let mut add = |address: &str, port: i32| {
        if let Ok(ip) = address.parse::<IpAddr>() {
            destinations.insert((ip, port));
        }
    };
    if let Some(spec) = service.spec
        && let Some(ip) = spec.cluster_ip
        && let Some(port) = spec.ports.as_ref().and_then(|ports| ports.first())
    {
        add(&ip, port.port);
    }
    for slice in slices.items {
        for port in slice
            .ports
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| entry.port)
        {
            for endpoint in &slice.endpoints {
                for address in &endpoint.addresses {
                    add(address, port);
                }
            }
        }
    }
    Ok(destinations)
}

pub async fn run_job(
    target: &Target,
    org: &OrgId,
    manifest: &Value,
    deadline: Duration,
) -> anyhow::Result<()> {
    let jobs = Api::<Job>::namespaced(target.client.client(), org.as_str());
    let job = Job::deserialize(manifest).context("decode invitation Job")?;
    let created = jobs
        .create(&PostParams::default(), &job)
        .await
        .with_context(|| format!("start invitation Job in {org}"))?;
    let name = created
        .metadata
        .name
        .context("Kubernetes did not return a Job name")?;
    job::wait(&jobs, &name, deadline + Duration::from_secs(30)).await
}
