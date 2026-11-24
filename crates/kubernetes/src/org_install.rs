//! Kubernetes operations for installing an org.

use anyhow::{Context as _, ensure};
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
use std::{net::IpAddr, path::PathBuf, time::Duration};
use wamn_platform_install::{
    database::{ConnectionConfig, DATABASE_DIR},
    org::{OrgInputs, OrgInstallation, field},
    provision_org::{CONTROL_AUDIENCE, INVITATION_ACCOUNT, ORG_INPUTS_DIR, SQL_ACCOUNT},
};
use wamn_registry::{OrgId, name::REGISTRY_PULL};

use crate::{
    Target,
    client::{APPLY_MANAGER, wait_named},
    helm, job,
};

const PLATFORM: &str = "platform";
const ORG_INPUTS: &str = "wamn-org-inputs";
const SYSTEM_SQL_CONFIG: &str = "wamn-system-sql";
const PROVISION_JOB: &str = "wamn-provision-org";
const SQL_CONFIG_PATH: &str = "/etc/wamn/system-sql/database.json";
const DEADLINE: Duration = Duration::from_secs(300);

const CREDENTIAL_JOB: &str = "wamn.dev/control-credential-job";
const REVISION: &str = "deployment.kubernetes.io/revision";

/// The org chart and the cluster selected by the administrator.
#[derive(Debug)]
pub struct OrgInstaller {
    chart: PathBuf,
    target: Target,
}

impl OrgInstaller {
    /// Locate the org chart and connect to the selected cluster.
    pub async fn developer() -> anyhow::Result<Self> {
        let chart = org_chart()?;
        let target = Target::developer(PLATFORM).await?;
        Ok(Self { chart, target })
    }
}

impl OrgInstallation for OrgInstaller {
    async fn inputs(&self) -> anyhow::Result<OrgInputs> {
        let target = &self.target;
        let (inputs, database_config, identity_ca, nats_ca, api_server) = tokio::try_join!(
            config_map(target, PLATFORM, ORG_INPUTS),
            config_map(target, PLATFORM, SYSTEM_SQL_CONFIG),
            config_map(target, "hosts", "identity-ca"),
            secret_entry(target, "wasmcloud-ca", "tls.crt"),
            api_destinations(target),
        )?;
        let database: ConnectionConfig =
            serde_json::from_str(field(&database_config["data"], "database.json")?)
                .context("decode the platform SQL Job connection")?;
        let postgres_ca = secret_entry(target, &database.server_ca.name, "ca.crt").await?;
        Ok(OrgInputs {
            values: inputs,
            database,
            identity_ca,
            nats_ca,
            postgres_ca,
            api_server,
        })
    }

    async fn install_release(&self, org: &OrgId, values: &Value) -> anyhow::Result<()> {
        let target = &self.target;
        let values_file =
            tempfile::NamedTempFile::new().context("create a file for public org values")?;
        serde_json::to_writer(values_file.as_file(), values).context("write public org values")?;

        let storage = helm::org_release_namespace(org.as_str());
        let apply = PatchParams::apply(APPLY_MANAGER);
        let namespaces = Api::<Namespace>::all(target.client.client());
        namespaces
            .patch(
                &storage,
                &apply,
                &Patch::Apply(json!({
                    "apiVersion": "v1", "kind": "Namespace",
                    "metadata": {"name": storage, "labels": {"app.kubernetes.io/part-of": "wamn"}},
                })),
            )
            .await
            .context("create the org Helm storage namespace")?;
        helm::recover_pending(target, &storage, "wamn-org", helm::Wait::None).await?;
        helm::upgrade(
            target,
            &storage,
            &self.chart,
            "wamn-org",
            &[values_file.path()],
            helm::Wait::None,
            helm::Install::ExistingNamespace,
        )
        .await
    }

    async fn write_registry_credentials(
        &self,
        org: &OrgId,
        credentials: &[u8],
    ) -> anyhow::Result<()> {
        let secret = json!({
            "apiVersion": "v1", "kind": "Secret", "type": "kubernetes.io/dockerconfigjson",
            "metadata": {"name": REGISTRY_PULL, "namespace": org},
            "data": {".dockerconfigjson": STANDARD.encode(credentials)},
        });
        let secrets = Api::<Secret>::namespaced(self.target.client.client(), org.as_str());
        secrets
            .patch(
                REGISTRY_PULL,
                &PatchParams::apply(APPLY_MANAGER),
                &Patch::Apply(secret),
            )
            .await
            .with_context(|| format!("apply {REGISTRY_PULL} in {org}"))?;
        Ok(())
    }

    async fn provision_logins(
        &self,
        org: &OrgId,
        image: &str,
        database: &ConnectionConfig,
    ) -> anyhow::Result<String> {
        let login = &database.credentials.name;
        let mut sql = org_job(
            SQL_ACCOUNT,
            image,
            &["provision-org", org.as_str(), "--database", SQL_CONFIG_PATH],
        );
        sql["metadata"] = json!({"name": PROVISION_JOB});
        let spec = &mut sql["spec"]["template"]["spec"];
        spec["automountServiceAccountToken"] = true.into();
        spec["volumes"] = json!([
            {"name": "configuration", "configMap": {"name": SYSTEM_SQL_CONFIG}},
            {"name": "org-inputs", "configMap": {"name": ORG_INPUTS}},
            {"name": "database", "projected": {"defaultMode": 256, "sources": [
                {"secret": {"name": login, "items": [
                    {"key": "tls.crt", "path": format!("{login}/tls.crt")},
                    {"key": "tls.key", "path": format!("{login}/tls.key")},
                ]}},
                {"secret": {"name": database.server_ca.name, "items": [
                    {"key": "ca.crt", "path": format!("{login}/ca.crt")},
                ]}},
            ]}},
        ]);
        spec["containers"][0]["volumeMounts"] = json!([
            {"name": "configuration", "mountPath": "/etc/wamn/system-sql", "readOnly": true},
            {"name": "org-inputs", "mountPath": ORG_INPUTS_DIR, "readOnly": true},
            {"name": "database", "mountPath": DATABASE_DIR, "readOnly": true},
        ]);
        job::run_exclusive(&self.target, &sql, DEADLINE + Duration::from_secs(30)).await
    }

    async fn reload_control(&self, org: &OrgId, sql_job_uid: &str) -> anyhow::Result<()> {
        let client = &self.target.client;
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
        tokio::time::timeout(DEADLINE, async {
            loop {
                let deployment = wait_named(&deployments, &control, DEADLINE, |object| {
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

    async fn invite_owner(&self, org: &OrgId, image: &str, owner: &str) -> anyhow::Result<()> {
        let mut invitation = org_job(
            INVITATION_ACCOUNT,
            image,
            &[
                "invite-org-owner",
                org.as_str(),
                &format!("--owner={owner}"),
            ],
        );
        invitation["spec"]["backoffLimit"] = 5.into();
        let spec = &mut invitation["spec"]["template"]["spec"];
        spec["volumes"] = json!([
            {"name": "control-token", "projected": {"defaultMode": 256, "sources": [
                {"serviceAccountToken": {"audience": CONTROL_AUDIENCE, "expirationSeconds": 600, "path": "token"}},
            ]}},
        ]);
        spec["containers"][0]["volumeMounts"] = json!([
            {"name": "control-token", "mountPath": "/var/run/secrets/wamn-control", "readOnly": true},
        ]);
        run_job(&self.target, org, &invitation).await
    }
}

fn org_chart() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .map(|dir| dir.join("deploy/charts/org"))
        .chain([PathBuf::from("/usr/local/share/wamn/deploy/charts/org")])
        .find(|dir| dir.join("Chart.yaml").is_file())
        .context("locate deploy/charts/org in the repository or installed chart directory")
}

async fn config_map(target: &Target, namespace: &str, name: &str) -> anyhow::Result<Value> {
    let maps = Api::<ConfigMap>::namespaced(target.client.client(), namespace);
    let object = maps
        .get(name)
        .await
        .with_context(|| format!("read platform configuration {namespace}/{name}"))?;
    Ok(serde_json::to_value(object)?)
}

async fn secret_entry(target: &Target, name: &str, key: &str) -> anyhow::Result<Vec<u8>> {
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

async fn api_destinations(target: &Target) -> anyhow::Result<Vec<Value>> {
    let services = Api::<Service>::namespaced(target.client.client(), "default");
    let endpoints = Api::<EndpointSlice>::namespaced(target.client.client(), "default");
    let params = ListParams::default().labels("kubernetes.io/service-name=kubernetes");
    let (service, slices) = tokio::try_join!(services.get("kubernetes"), endpoints.list(&params))?;
    let mut destinations = std::collections::BTreeSet::new();
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
    ensure!(
        !destinations.is_empty(),
        "no Kubernetes API address for the control NetworkPolicy"
    );
    Ok(destinations
        .into_iter()
        .map(|(ip, port)| {
            let mask = if ip.is_ipv4() { 32 } else { 128 };
            json!({"cidr": format!("{ip}/{mask}"), "port": port})
        })
        .collect())
}

fn org_job(account: &str, image: &str, args: &[&str]) -> Value {
    json!({
        "apiVersion": "batch/v1", "kind": "Job", "metadata": {"generateName": format!("{account}-")},
        "spec": {
            "activeDeadlineSeconds": DEADLINE.as_secs(), "ttlSecondsAfterFinished": 3600,
            "backoffLimit": 0,
            "template": {
                "metadata": {"labels": {"app.kubernetes.io/component": account}},
                "spec": {
                    "serviceAccountName": account, "automountServiceAccountToken": false,
                    "restartPolicy": "Never",
                    "containers": [{"name": account, "image": image, "args": args}],
                },
            },
        },
    })
}

async fn run_job(target: &Target, org: &OrgId, manifest: &Value) -> anyhow::Result<()> {
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
    job::wait(&jobs, &name, DEADLINE + Duration::from_secs(30)).await
}
