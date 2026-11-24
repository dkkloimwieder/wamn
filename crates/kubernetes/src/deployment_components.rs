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
    api::{ListParams, ObjectMeta, Patch, PatchParams},
};
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsStr, sync::Mutex, time::Duration};
use wamn_registry::{ApplicationName, Triple, name};

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
                .and_then(|mut values| values.remove("wamn.environment"))
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

async fn project_release_exists(target: &Target, release: &str) -> anyhow::Result<bool> {
    let namespace = helm::org_release_namespace(&target.namespace);
    let filter = format!("^{release}$");
    let output = helm::run_text(&[
        "list".as_ref(),
        "--filter".as_ref(),
        filter.as_ref(),
        "-o".as_ref(),
        "json".as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        std::ffi::OsStr::new(namespace.as_ref()),
    ])
    .await
    .with_context(|| format!("look up project release {release}"))?;
    let releases: Vec<Value> = serde_json::from_str(&output).context("decode Helm release list")?;
    Ok(releases.iter().any(|entry| entry["name"] == release))
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

async fn wait_workload_ready(target: &Target, name: &str, timeout: Duration) -> anyhow::Result<()> {
    let diagnostics = Mutex::new(String::new());
    let workloads = workloads(target.client.client(), &target.namespace);
    wait_named(&workloads, name, timeout, |object| {
        let Some(object) = object else {
            return Ok(None);
        };
        let conditions = object.data["status"]["conditions"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        if conditions
            .iter()
            .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
        {
            return Ok(Some(()));
        }
        let messages = conditions
            .iter()
            .filter_map(|condition| {
                let type_ = condition["type"].as_str()?;
                let status = condition["status"].as_str()?;
                let message = condition["message"].as_str().unwrap_or_default();
                (type_ == "Ready" || matches!(status, "False" | "Unknown"))
                    .then(|| format!("{type_}={status} {message:?}"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        *diagnostics.lock().expect("lock workload diagnostics") = messages;
        Ok(None)
    })
    .await
    .map_err(|error| {
        if error.is::<tokio::time::error::Elapsed>() {
            let diagnostics = diagnostics.into_inner().expect("read workload diagnostics");
            let diagnostics = if diagnostics.is_empty() {
                "no Ready, False, or Unknown conditions observed"
            } else {
                &diagnostics
            };
            error.context(format!(
                "WorkloadDeployment {name} did not become Ready within {} seconds ({diagnostics})",
                timeout.as_secs()
            ))
        } else {
            error
        }
    })
}

async fn helm(target: &Target, arguments: &[&str]) -> anyhow::Result<std::process::Output> {
    let namespace = helm::org_release_namespace(&target.namespace);
    let mut args = vec![
        OsStr::new("--kubeconfig"),
        target.kubeconfig.as_os_str(),
        OsStr::new("--kube-context"),
        target.context.as_ref(),
        OsStr::new("--namespace"),
        OsStr::new(namespace.as_ref()),
    ];
    args.extend(arguments.iter().map(OsStr::new));
    helm::output(&args).await
}

async fn get(target: &Target, args: &[&str]) -> anyhow::Result<Option<Vec<u8>>> {
    let output = helm(target, args).await?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("release: not found") || error.contains("release not found") {
        return Ok(None);
    }
    anyhow::bail!("helm {} failed: {}", args.join(" "), error.trim())
}

async fn run(target: &Target, release: &str, revision: u32) -> anyhow::Result<()> {
    let output = helm(
        target,
        &[
            "rollback",
            release,
            &revision.to_string(),
            "--server-side=true",
            "--wait=watcher",
            "--timeout",
            "600s",
        ],
    )
    .await?;
    ensure!(
        output.status.success(),
        "helm rollback failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
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

async fn applications(target: &Target, triple: &Triple) -> anyhow::Result<Vec<ApplicationName>> {
    let mut applications = Vec::new();
    let coordinate = triple.to_string();
    let workloads = workloads(target.client.client(), triple.org.as_str());
    for item in workloads.list(&ListParams::default()).await?.items {
        let annotations = item.metadata.annotations.as_ref();
        let annotation = |key| {
            annotations
                .and_then(|values| values.get(key))
                .map(String::as_str)
        };
        if annotation("wamn.environment") == Some(coordinate.as_str()) {
            applications.push(
                annotation("wamn.application")
                    .context("workload has no application annotation")?
                    .parse()?,
            );
        }
    }
    Ok(applications)
}

/// List the host Pods for the specified host group.
async fn host_pods(target: &Target, release_name: &str) -> anyhow::Result<Value> {
    let selector = wamn_registry::name::host_selector(release_name);
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
    let objects = pods.list(&ListParams::default().labels(&selector)).await?;
    serde_json::to_value(objects).context("encode the pod list")
}

async fn workload_annotations(
    triple: &Triple,
    application: &ApplicationName,
) -> anyhow::Result<Option<BTreeMap<String, String>>> {
    let api = KubernetesApi::in_cluster()?;
    let release = wamn_registry::resource_names::application_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
        application,
    );
    let workloads = crate::client::workloads(api.client(), triple.org.as_str());
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
        application: &ApplicationName,
    ) -> anyhow::Result<Option<BTreeMap<String, String>>> {
        workload_annotations(triple, application).await
    }

    pub async fn applications(&self, triple: &Triple) -> anyhow::Result<Vec<ApplicationName>> {
        applications(self, triple).await
    }

    pub async fn wait_workload_ready(&self, name: &str, timeout: Duration) -> anyhow::Result<()> {
        wait_workload_ready(self, name, timeout).await
    }

    pub async fn current_job(&self) -> anyhow::Result<Value> {
        crate::job::current(&KubernetesApi::in_cluster()?).await
    }

    pub async fn release_manifest(
        &self,
        release: &str,
        revision: u32,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        get(
            self,
            &[
                "get",
                "manifest",
                release,
                "--revision",
                &revision.to_string(),
            ],
        )
        .await
    }

    pub async fn release_values(
        &self,
        release: &str,
        revision: u32,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        get(
            self,
            &[
                "get",
                "values",
                release,
                "--revision",
                &revision.to_string(),
                "-o",
                "json",
            ],
        )
        .await
    }

    pub async fn rollback(&self, release: &str, revision: u32) -> anyhow::Result<()> {
        run(self, release, revision).await
    }

    pub async fn uninstall(&self, release: &str) -> anyhow::Result<()> {
        let namespace = helm::org_release_namespace(&self.namespace);
        helm::uninstall(self, &namespace, release, helm::Wait::Watcher).await
    }

    pub async fn require_ownership(&self, triple: &Triple, database: &str) -> anyhow::Result<()> {
        require_ownership(self, triple, database).await
    }

    pub async fn project_release_exists(&self, release: &str) -> anyhow::Result<bool> {
        project_release_exists(self, release).await
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
