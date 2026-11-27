//! Cluster observations and resource mutations for deployment.

use anyhow::{Context as _, ensure};
use k8s_openapi::{
    ByteString,
    api::{
        apps::v1::Deployment,
        core::v1::{ConfigMap, Pod, Secret},
    },
};
use kube::{
    Api,
    api::{DynamicObject, ListParams, ObjectMeta, Patch, PatchParams},
};
use serde::Deserialize as _;
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{Instant, timeout_at};
use wamn_registry::{AppName, Triple, name};

use crate::client::{
    APPLY_MANAGER, KubernetesApi, delete_named, ready, resource, wait_named, workloads,
};
use crate::{Target, helm};

/// Wait for reader pods, including pods left by an interrupted Deployment delete.
async fn wait_reader_stopped(target: &Target, triple: &Triple) -> anyhow::Result<()> {
    let project_env_key = name::project_env_key(&triple.org, &triple.project, &triple.env);
    let selector = format!(
        "app.kubernetes.io/name=wamn-cdc-reader,app.kubernetes.io/instance={project_env_key}"
    );
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
    let params = ListParams::default().labels(&selector);
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if pods.list(&params).await?.items.is_empty() {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("wait for the old CDC reader to stop")??;
    Ok(())
}

async fn wait_reader_ready(target: &Target, triple: &Triple) -> anyhow::Result<()> {
    let name = name::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let deployments = Api::<Deployment>::namespaced(target.client.client(), &target.namespace);
    wait_named(&deployments, &name, Duration::from_secs(120), |object| {
        Ok(object
            .filter(|deployment| {
                let Some(status) = &deployment.status else {
                    return false;
                };
                let desired = deployment
                    .spec
                    .as_ref()
                    .and_then(|spec| spec.replicas)
                    .unwrap_or(1);
                let updated = status.updated_replicas.unwrap_or(0);
                deployment
                    .metadata
                    .generation
                    .zip(status.observed_generation)
                    .is_some_and(|(generation, observed)| generation <= observed)
                    && updated >= desired
                    && status.replicas.unwrap_or(0) <= updated
                    && status.available_replicas.unwrap_or(0) >= updated
            })
            .map(drop))
    })
    .await
}

/// Refuse foreign resources before the first destructive operation.
async fn require_ownership(target: &Target, triple: &Triple, database: &str) -> anyhow::Result<()> {
    let reader = name::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let deployments = Api::<Deployment>::namespaced(target.client.client(), &target.namespace);
    if let Some(object) = deployments.get_opt(&reader).await? {
        let coordinate = triple.to_string();
        anyhow::ensure!(
            object
                .metadata
                .annotations
                .and_then(|mut values| values.remove("wamn.env"))
                .as_deref()
                == Some(coordinate.as_str()),
            "refused: Deployment {reader} does not belong to {triple}"
        );
    }
    let publications = resource(
        target.client.client(),
        &target.namespace,
        "postgresql.cnpg.io",
        "v1",
        "Publication",
        "publications",
    );
    if let Some(object) = publications.get_opt(database).await? {
        let labels = object.metadata.labels.as_ref();
        let label = |key| {
            labels
                .and_then(|values| values.get(key))
                .map(String::as_str)
        };
        anyhow::ensure!(
            label("wamn.org") == Some(triple.org.as_str())
                && label("wamn.project") == Some(triple.project.as_str())
                && label("wamn.env") == Some(triple.env.as_str())
                && object.data["spec"]["dbname"] == database,
            "refused: Publication {database} does not belong to {triple}"
        );
    }
    Ok(())
}

/// Remove chart resources left by uninstalled project generations.
async fn delete_kept_project_resources(
    target: &Target,
    triple: &Triple,
    release: &str,
) -> anyhow::Result<Vec<String>> {
    let namespace = &target.namespace;
    let coordinate = name::project_env_key(&triple.org, &triple.project, &triple.env);
    let mut lines = Vec::new();
    let stores = resource(
        target.client.client(),
        namespace,
        "barmancloud.cnpg.io",
        "v1",
        "ObjectStore",
        "objectstores",
    );
    for (group, type_, plural, is_cluster) in [
        ("postgresql.cnpg.io", "Cluster", "clusters", true),
        ("cert-manager.io", "Certificate", "certificates", false),
        ("cert-manager.io", "Issuer", "issuers", false),
        (
            "networking.k8s.io",
            "NetworkPolicy",
            "networkpolicies",
            false,
        ),
    ] {
        let objects = resource(
            target.client.client(),
            namespace,
            group,
            "v1",
            type_,
            plural,
        );
        let label = format!("{}.{group}", type_.to_ascii_lowercase());
        let list = objects
            .list(&ListParams::default())
            .await
            .with_context(|| format!("list {label} in {namespace}"))?;
        for object in list
            .items
            .iter()
            .filter(|object| kept_by_release(object, release))
        {
            let name = object
                .metadata
                .name
                .as_deref()
                .with_context(|| format!("kept {label} has no name"))?;
            if is_cluster {
                let project = object.data["spec"]["bootstrap"]["initdb"]["database"]
                    .as_str()
                    .and_then(name::project_database_key);
                if project.as_ref() != Some(&coordinate) {
                    continue;
                }
                // Remove the store first so a retry can still find it by the Cluster name.
                let store = format!("{name}-store");
                if let Some(object) = stores.get_opt(&store).await?
                    && kept_by_release(&object, release)
                    && delete_named(&stores, &store, false)
                        .await
                        .with_context(|| {
                            format!("delete kept objectstore.barmancloud.cnpg.io/{store}")
                        })?
                {
                    lines.push(format!("deleted objectstore.barmancloud.cnpg.io/{store}"));
                }
            }
            if delete_named(&objects, name, true)
                .await
                .with_context(|| format!("delete kept {label}/{name}"))?
            {
                lines.push(format!("deleted {label}/{name}"));
            }
        }
    }
    if lines.is_empty() {
        lines.push(format!("no kept project resources found for {triple}"));
    }
    Ok(lines)
}

fn kept_by_release(object: &kube::api::DynamicObject, release: &str) -> bool {
    object
        .metadata
        .annotations
        .as_ref()
        .is_some_and(|annotations| {
            let value = |key| annotations.get(key).map(String::as_str);
            value("meta.helm.sh/release-name") == Some(release)
                && value("helm.sh/resource-policy") == Some("keep")
        })
}

async fn delete_database_certificate_secrets(
    target: &Target,
    database: &str,
    logins: &[&str],
) -> anyhow::Result<()> {
    // Also remove older Secrets that lack cert-manager's Certificate owner reference.
    let cluster = name::project_cluster(database);
    let root = format!("{cluster}-login-ca");
    let certificates = resource(
        target.client.client(),
        &target.namespace,
        "cert-manager.io",
        "v1",
        "Certificate",
        "certificates",
    );
    anyhow::ensure!(
        certificates.get_opt(&root).await?.is_none(),
        "cannot remove database certificate Secrets while Certificate {root} exists"
    );
    let secrets: Vec<_> = logins
        .iter()
        .copied()
        .map(|login| name::project_login_certificate(database, login))
        .chain([root, format!("{cluster}-client-repl")])
        .collect();
    let api = Api::<Secret>::namespaced(target.client.client(), &target.namespace);
    for name in secrets {
        delete_named(&api, &name, true).await?;
    }
    Ok(())
}

pub(crate) async fn wait_app_ready(target: &Target, manifest: &[u8]) -> anyhow::Result<()> {
    let expected = app_workloads(target, manifest)?;
    while !workloads_serve(target, &expected).await? {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

pub(crate) async fn app_serves(target: &Target, manifest: &[u8]) -> anyhow::Result<bool> {
    let expected = app_workloads(target, manifest)?;
    workloads_serve(target, &expected).await
}

fn app_workloads(target: &Target, manifest: &[u8]) -> anyhow::Result<Vec<Value>> {
    let mut expected = Vec::new();
    for document in serde_yaml::Deserializer::from_slice(manifest) {
        let document = Value::deserialize(document).context("read Helm release manifest")?;
        if document["kind"] == "WorkloadDeployment" {
            ensure!(
                document["metadata"]["namespace"] == target.namespace,
                "Helm workload is outside the selected namespace"
            );
            expected.push(document);
        }
    }
    ensure!(
        !expected.is_empty(),
        "Helm release contains no application workloads"
    );
    Ok(expected)
}

async fn workloads_serve(target: &Target, expected: &[Value]) -> anyhow::Result<bool> {
    let deployments = workloads(target.client.client(), &target.namespace);
    let replica_sets = resource(
        target.client.client(),
        &target.namespace,
        "runtime.wasmcloud.dev",
        "v1alpha1",
        "WorkloadReplicaSet",
        "workloadreplicasets",
    );
    for expected in expected {
        let name = expected["metadata"]["name"]
            .as_str()
            .context("Helm workload has no name")?;
        let Some(deployment) = deployments.get_opt(name).await? else {
            return Ok(false);
        };
        let Some(current) = deployment.data["status"]["currentReplicaSet"]["name"]
            .as_str()
            .filter(|name| !name.is_empty())
        else {
            return Ok(false);
        };
        if !replica_sets
            .get_opt(current)
            .await?
            .is_some_and(|set| workload_serves(expected, &deployment, &set))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn workload_serves(expected: &Value, deployment: &DynamicObject, set: &DynamicObject) -> bool {
    let deployment_spec = &deployment.data["spec"];
    let set_spec = &set.data["spec"];
    let revision = &expected["metadata"]["annotations"];
    let Some(annotations) = &deployment.metadata.annotations else {
        return false;
    };
    let Some(uid) = deployment.metadata.uid.as_deref() else {
        return false;
    };
    let Some(generation) = set.metadata.generation.filter(|generation| *generation > 0) else {
        return false;
    };
    ["wamn.package-digest", "wamn.release-revision"]
        .iter()
        .all(|key| {
            revision[key]
                .as_str()
                .zip(annotations.get(*key))
                .is_some_and(|(expected, actual)| expected == actual)
        })
        && deployment.metadata.deletion_timestamp.is_none()
        && set.metadata.deletion_timestamp.is_none()
        && ready(deployment)
        && set
            .metadata
            .owner_references
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|owner| {
                owner.controller == Some(true)
                    && owner.uid == uid
                    && owner.kind == "WorkloadDeployment"
                    && owner.api_version == "runtime.wasmcloud.dev/v1alpha1"
            })
        && deployment_spec["replicas"]
            .as_u64()
            .is_some_and(|replicas| replicas > 0)
        && deployment_spec["replicas"] == set_spec["replicas"]
        && deployment_spec["template"].is_object()
        && same_workload_template(&expected["spec"]["template"], &deployment_spec["template"])
        && same_workload_template(&deployment_spec["template"], &set_spec["template"])
        && set.data["status"]["conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|condition| {
                condition["type"] == "Ready"
                    && condition["status"] == "True"
                    && condition["observedGeneration"].as_i64() == Some(generation)
            })
}

fn same_workload_template(left: &Value, right: &Value) -> bool {
    // The controller serializes these optional zero or empty chart fields as absent.
    let omitted = |key: &str, value: &Value| match key {
        "poolSize" => value.as_u64() == Some(0),
        "allowedIpNameLookups" => value.as_array().is_some_and(Vec::is_empty),
        _ => false,
    };
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            left.iter().all(|(key, value)| {
                right.get(key).map_or_else(
                    || omitted(key, value),
                    |right| same_workload_template(value, right),
                )
            }) && right
                .iter()
                .all(|(key, value)| left.contains_key(key) || omitted(key, value))
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_workload_template(left, right))
        }
        _ => left == right,
    }
}

async fn postgres_settings(target: &Target) -> anyhow::Result<serde_json::Value> {
    let maps = Api::<ConfigMap>::namespaced(target.client.client(), &target.namespace);
    let configuration = maps
        .get_opt("wamn-platform")
        .await?
        .context("installation configuration is absent")?;
    let postgres = configuration
        .data
        .as_ref()
        .and_then(|data| data.get("postgres.json"))
        .context("installation PostgreSQL settings are absent")?;
    serde_json::from_str(postgres).context("decode installation PostgreSQL settings")
}

async fn apps(target: &Target, triple: &Triple) -> anyhow::Result<Vec<AppName>> {
    let mut apps = Vec::new();
    let coordinate = triple.to_string();
    let workloads = workloads(target.client.client(), &target.namespace);
    for item in workloads.list(&ListParams::default()).await?.items {
        let annotations = item.metadata.annotations.as_ref();
        let annotation = |key| {
            annotations
                .and_then(|values| values.get(key))
                .map(String::as_str)
        };
        if annotation("wamn.env") == Some(coordinate.as_str()) {
            apps.push(
                annotation("wamn.app")
                    .context("workload has no application annotation")?
                    .parse()?,
            );
        }
    }
    Ok(apps)
}

/// List the host Pods for the specified host group.
async fn host_pods(target: &Target, release_name: &str) -> anyhow::Result<Value> {
    let selector = wamn_registry::name::host_selector(release_name);
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
    let objects = pods.list(&ListParams::default().labels(&selector)).await?;
    serde_json::to_value(objects).context("encode the pod list")
}

async fn workload_annotations(
    target: &Target,
    triple: &Triple,
    app: &AppName,
) -> anyhow::Result<Option<BTreeMap<String, String>>> {
    let release = wamn_registry::resource_names::app_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
        app,
    );
    let workloads = crate::client::workloads(target.client.client(), &target.namespace);
    let workload = tokio::time::timeout(
        crate::client::REQUEST_TIMEOUT,
        workloads.get_opt(&format!("{release}-http")),
    )
    .await??;
    Ok(workload.map(|item| item.metadata.annotations.unwrap_or_default()))
}

/// Copy only `ca.crt` of the cluster's server CA Secret into the ConfigMap
/// that control reads. The Cluster owns the copy, so deleting it removes both.
pub async fn copy_server_ca(
    target: &Target,
    cluster: &str,
    source: &str,
    cluster_uid: String,
) -> anyhow::Result<()> {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;

    let secrets = Api::<Secret>::namespaced(target.client.client(), &target.namespace);
    let pem = secrets
        .get(source)
        .await
        .with_context(|| format!("read Secret {source}"))?
        .data
        .and_then(|mut data| data.remove("ca.crt"))
        .with_context(|| format!("Secret {source} has no ca.crt"))?;
    let name = name::project_server_ca_copy(cluster);
    let copy = ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(target.namespace.clone()),
            owner_references: Some(vec![OwnerReference {
                api_version: "postgresql.cnpg.io/v1".into(),
                kind: "Cluster".into(),
                name: cluster.into(),
                uid: cluster_uid,
                ..OwnerReference::default()
            }]),
            ..ObjectMeta::default()
        },
        data: Some([("ca.crt".into(), String::from_utf8(pem.0)?)].into()),
        ..ConfigMap::default()
    };
    Api::<ConfigMap>::namespaced(target.client.client(), &target.namespace)
        .patch(
            &name,
            &PatchParams::apply(crate::client::APPLY_MANAGER),
            &Patch::Apply(copy),
        )
        .await
        .with_context(|| format!("apply ConfigMap {name}"))?;
    Ok(())
}

impl Target {
    pub async fn wait_database_cluster_uid(&self, cluster: &str) -> anyhow::Result<String> {
        let clusters = resource(
            self.client.client(),
            &self.namespace,
            "postgresql.cnpg.io",
            "v1",
            "Cluster",
            "clusters",
        );
        wait_named(&clusters, cluster, Duration::from_secs(120), |object| {
            Ok(object.filter(ready).map(|object| object.metadata.uid))
        })
        .await?
        .with_context(|| format!("Cluster {cluster} has no uid"))
    }

    pub async fn has_reader_deployment(&self, triple: &Triple) -> Result<bool, kube::Error> {
        let name = name::cdc_reader_name(&triple.org, &triple.project, &triple.env);
        let deployments = Api::<Deployment>::namespaced(self.client.client(), &self.namespace);
        Ok(deployments.get_opt(&name).await?.is_some())
    }

    pub async fn wait_database_certificates(
        &self,
        owner: &str,
        capture: &str,
    ) -> anyhow::Result<()> {
        let certificates = resource(
            self.client.client(),
            &self.namespace,
            "cert-manager.io",
            "v1",
            "Certificate",
            "certificates",
        );
        let wait_certificate = |name| {
            wait_named(&certificates, name, Duration::from_secs(120), |object| {
                Ok(object.filter(ready).map(drop))
            })
        };
        tokio::try_join!(wait_certificate(owner), wait_certificate(capture),)?;
        Ok(())
    }
}

async fn write_credentials(
    target: &Target,
    name: &str,
    key: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let secrets = Api::<Secret>::namespaced(target.client.client(), &target.namespace);
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(target.namespace.clone()),
            ..ObjectMeta::default()
        },
        type_: Some("Opaque".into()),
        data: Some([(key.into(), ByteString(bytes.to_vec()))].into()),
        ..Secret::default()
    };
    secrets
        .patch(
            name,
            &PatchParams::apply(APPLY_MANAGER),
            &Patch::Apply(secret),
        )
        .await?;
    Ok(())
}

async fn read_credentials(
    target: &Target,
    name: &str,
    key: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let secrets = Api::<Secret>::namespaced(target.client.client(), &target.namespace);
    let Some(secret) = secrets.get_opt(name).await? else {
        return Ok(None);
    };
    let bytes = secret
        .data
        .and_then(|mut data| data.remove(key))
        .context("broker Secret is missing its credentials")?;
    Ok(Some(bytes.0))
}

impl Target {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub async fn host_pods(&self, name: &str) -> anyhow::Result<Value> {
        host_pods(self, name).await
    }

    pub async fn postgres_settings(&self) -> anyhow::Result<Value> {
        postgres_settings(self).await
    }

    pub async fn wait_reader_stopped(&self, triple: &Triple) -> anyhow::Result<()> {
        wait_reader_stopped(self, triple).await
    }

    pub async fn wait_reader_ready(&self, triple: &Triple) -> anyhow::Result<()> {
        wait_reader_ready(self, triple).await
    }

    pub async fn reader_exists(&self, triple: &Triple) -> anyhow::Result<bool> {
        let reader = name::cdc_reader_name(&triple.org, &triple.project, &triple.env);
        self.has_reader_deployment(triple)
            .await
            .with_context(|| format!("read CDC reader Deployment {reader}"))
    }

    pub async fn publication_exists(&self, database: &str) -> anyhow::Result<bool> {
        Ok(resource(
            self.client.client(),
            &self.namespace,
            "postgresql.cnpg.io",
            "v1",
            "Publication",
            "publications",
        )
        .get_opt(database)
        .await?
        .is_some())
    }

    pub async fn write_credentials(
        &self,
        name: &str,
        key: &str,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        write_credentials(self, name, key, bytes).await
    }

    pub async fn read_credentials(&self, name: &str, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        read_credentials(self, name, key).await
    }

    pub async fn delete_secret(&self, name: &str) -> anyhow::Result<bool> {
        let secrets = Api::<Secret>::namespaced(self.client.client(), &self.namespace);
        delete_named(&secrets, name, true).await
    }

    pub async fn workload_annotations(
        &self,
        triple: &Triple,
        app: &AppName,
    ) -> anyhow::Result<Option<BTreeMap<String, String>>> {
        workload_annotations(self, triple, app).await
    }

    pub async fn apps(&self, triple: &Triple) -> anyhow::Result<Vec<AppName>> {
        apps(self, triple).await
    }

    pub async fn current_job(&self) -> anyhow::Result<Value> {
        crate::job::current(&KubernetesApi::in_cluster()?).await
    }

    pub async fn release_manifest(
        &self,
        release: &str,
        revision: u32,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        helm::get(self, "manifest", release, revision).await
    }

    pub async fn release_values(
        &self,
        release: &str,
        revision: u32,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        helm::get(self, "values", release, revision).await
    }

    /// Restore the saved application release and wait for its template to serve.
    pub async fn rollback(
        &self,
        release: &str,
        revision: u32,
        deadline: Instant,
        readiness: Duration,
    ) -> anyhow::Result<()> {
        let namespace = helm::org_release_namespace(&self.namespace);
        timeout_at(
            deadline,
            helm::rollback(
                self,
                &namespace,
                release,
                Some(revision),
                helm::Wait::WatcherUntil {
                    deadline,
                    readiness,
                },
            ),
        )
        .await
        .with_context(|| {
            format!("Job deadline reached rolling back Helm release {namespace}/{release}")
        })?
    }

    pub async fn uninstall(&self, release: &str) -> anyhow::Result<()> {
        let namespace = helm::org_release_namespace(&self.namespace);
        helm::uninstall(self, &namespace, release, helm::Wait::Watcher).await
    }

    pub async fn require_ownership(&self, triple: &Triple, database: &str) -> anyhow::Result<()> {
        require_ownership(self, triple, database).await
    }

    pub async fn project_release_exists(&self, release: &str) -> anyhow::Result<bool> {
        helm::project_release_exists(self, release).await
    }

    pub async fn delete_kept_project_resources(
        &self,
        triple: &Triple,
        release: &str,
    ) -> anyhow::Result<Vec<String>> {
        delete_kept_project_resources(self, triple, release).await
    }

    pub async fn delete_database_certificate_secrets(
        &self,
        database: &str,
        logins: &[&str],
    ) -> anyhow::Result<()> {
        delete_database_certificate_secrets(self, database, logins).await
    }

    pub async fn database_exists(&self, database: &str) -> anyhow::Result<bool> {
        Ok(resource(
            self.client.client(),
            &self.namespace,
            "postgresql.cnpg.io",
            "v1",
            "Database",
            "databases",
        )
        .get_opt(database)
        .await?
        .is_some())
    }

    pub async fn database_cluster_exists(&self, name: &str) -> anyhow::Result<bool> {
        Ok(resource(
            self.client.client(),
            &self.namespace,
            "postgresql.cnpg.io",
            "v1",
            "Cluster",
            "clusters",
        )
        .get_opt(name)
        .await?
        .is_some())
    }
}

#[cfg(test)]
mod rollout_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_owned_current_template_ready_at_its_generation_serves() {
        let expected = json!({
            "metadata":{"annotations":{"wamn.package-digest":"new","wamn.release-revision":"2"}},
            "spec":{"replicas":1,"template":{"spec":{"components":[{"image":"new","poolSize":0,"localResources":{"allowedIpNameLookups":[]}}]}}},
        });
        let deployment: DynamicObject = serde_json::from_value(json!({
            "metadata":{"uid":"deployment","annotations":expected["metadata"]["annotations"]},
            "spec":expected["spec"],
            "status":{"conditions":[{"type":"Ready","status":"True"}]},
        }))
        .unwrap();
        let mut set: DynamicObject = serde_json::from_value(json!({
            "metadata":{"generation":3,"ownerReferences":[{"apiVersion":"runtime.wasmcloud.dev/v1alpha1","kind":"WorkloadDeployment","name":"app","uid":"deployment","controller":true}]},
            "spec":{"replicas":1,"template":{"spec":{"components":[{"image":"old","localResources":{}}]}}},
            "status":{"conditions":[{"type":"Ready","status":"True","observedGeneration":3}]},
        })).unwrap();
        assert!(
            !workload_serves(&expected, &deployment, &set),
            "old template is still Ready"
        );
        set.data["spec"]["template"]["spec"]["components"][0]["image"] = json!("new");
        assert!(workload_serves(&expected, &deployment, &set));
        set.metadata.generation = Some(4);
        assert!(
            !workload_serves(&expected, &deployment, &set),
            "previous generation is still Ready"
        );
        set.data["status"]["conditions"][0]["observedGeneration"] = json!(4);
        set.metadata.owner_references.as_mut().unwrap()[0].uid = "another-deployment".into();
        assert!(
            !workload_serves(&expected, &deployment, &set),
            "another deployment owns the set"
        );
    }
}
