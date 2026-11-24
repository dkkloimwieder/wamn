//! Admin-side installation of a single org.

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use k8s_openapi::api::{
    apps::v1::Deployment,
    batch::v1::Job,
    core::v1::{ConfigMap, Namespace, Secret, Service},
    discovery::v1::EndpointSlice,
};
use kube::{
    Api,
    api::{ListParams, Patch, PatchParams, PostParams},
};
use serde::Deserialize as _;
use serde_json::{Value, json};
use std::{net::IpAddr, path::PathBuf, time::Duration};
use wamn_kubernetes::{Target, client::APPLY_MANAGER, helm, job};
use wamn_oci::package_artifact::registry_credentials::project_registry_credentials;
use wamn_postgres::ConnectionConfig;
use wamn_registry::{OrgId, name::REGISTRY_PULL};

use crate::provision_org::{
    APPLY_SECRET, CONTROL_AUDIENCE, CONTROL_SECRET, INVITATION_ACCOUNT, OAUTH_ENCRYPTION_INPUT,
    ORG_INPUTS_DIR, SQL_ACCOUNT,
};
use crate::release_chart::bundle::POD_REGISTRY;

const PLATFORM: &str = "platform";
const ORG_INPUTS: &str = "wamn-org-inputs";
const SYSTEM_SQL_CONFIG: &str = "wamn-system-sql";
const PROVISION_JOB: &str = "wamn-provision-org";
const SQL_CONFIG_PATH: &str = "/etc/wamn/system-sql/database.json";
const DEADLINE: Duration = Duration::from_secs(300);

/// Install the org release, provision its logins, then invite its owner.
pub async fn create(org: &OrgId, owner: &str) -> anyhow::Result<()> {
    use k8s_openapi::api::{apps::v1::ReplicaSet, core::v1::Pod};
    use kube::{ResourceExt as _, core::Selector};
    use wamn_kubernetes::client::wait_named;

    const CREDENTIAL_JOB: &str = "wamn.dev/control-credential-job";
    const REVISION: &str = "deployment.kubernetes.io/revision";

    let chart = org_chart()?;
    let mut target = Target::developer(PLATFORM).await?;
    let (inputs, database_config, identity_ca, nats_ca, api_server) = tokio::try_join!(
        config_map(&target, PLATFORM, ORG_INPUTS),
        config_map(&target, PLATFORM, SYSTEM_SQL_CONFIG),
        config_map(&target, "hosts", "identity-ca"),
        secret_entry(&target, "wasmcloud-ca", "tls.crt"),
        api_destinations(&target),
    )?;
    let database: ConnectionConfig =
        serde_json::from_str(field(&database_config["data"], "database.json")?)
            .context("decode the platform SQL Job connection")?;
    let postgres_ca = secret_entry(&target, &database.server_ca.name, "ca.crt").await?;
    ensure!(!nats_ca.is_empty(), "public NATS CA is absent");
    let inputs = &inputs["data"];
    let image = field(inputs, "controlImage")?;
    let (_, digest) = image.rsplit_once('@').context(
        "pin systemSql.image in the platform release so org control and its worker use an immutable image",
    )?;
    digest.parse::<wamn_digest::Digest>()?;

    let values = org_values(
        inputs,
        &database,
        org,
        field(&identity_ca["data"], "ca.crt")?,
        std::str::from_utf8(&nats_ca).context("read the public NATS CA as PEM")?,
        std::str::from_utf8(&postgres_ca).context("read the public system database CA as PEM")?,
        &api_server,
    )?;
    let registry_secret = if registry_token_metadata(inputs)? {
        None
    } else {
        Some(registry_pull_secret(
            org,
            field(inputs, "registry")?,
            field(&values, "registry")?,
        )?)
    };
    let values_file =
        tempfile::NamedTempFile::new().context("create a file for public org values")?;
    serde_json::to_writer(values_file.as_file(), &values).context("write public org values")?;

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
    helm::recover_pending(&target, &storage, "wamn-org", helm::Wait::None).await?;
    helm::upgrade(
        &target,
        &storage,
        &chart,
        "wamn-org",
        &[values_file.path()],
        helm::Wait::None,
        helm::Install::ExistingNamespace,
    )
    .await?;

    if let Some(secret) = registry_secret {
        let secrets = Api::<Secret>::namespaced(target.client.client(), org.as_str());
        secrets
            .patch(REGISTRY_PULL, &apply, &Patch::Apply(secret))
            .await
            .with_context(|| format!("apply {REGISTRY_PULL} in {org}"))?;
    }

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
        {"name": "database", "mountPath": wamn_postgres::DATABASE_DIR, "readOnly": true},
    ]);
    let sql_job_uid = job::run_exclusive(&target, &sql, DEADLINE + Duration::from_secs(30)).await?;

    target.namespace = org.to_string();
    // Control loads the mounted credentials at startup.
    let control = wamn_registry::name::control_resource(org);
    let restart = json!({
        "spec": {"template": {"metadata": {"annotations": {
            (CREDENTIAL_JOB): sql_job_uid,
        }}}},
    });
    let deployments = Api::<Deployment>::namespaced(target.client.client(), &target.namespace);
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
    let replicas = Api::<ReplicaSet>::namespaced(target.client.client(), &target.namespace);
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
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
                            == Some(&sql_job_uid)
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
    .with_context(|| format!("wait for the control Deployment {org}/{control} rollout"))??;

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
    run_job(&target, &invitation).await
}

fn registry_pull_secret(org: &OrgId, registry: &str, pod_registry: &str) -> anyhow::Result<Value> {
    let path = std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .context("registry-credentials-file-required: set WAMN_REGISTRY_AUTH_FILE")?;
    let registry = registry.split('/').next().unwrap_or_default();
    let pod_registry = pod_registry.split('/').next().unwrap_or_default();
    let config = project_registry_credentials(&path, registry, pod_registry)?;
    let data = STANDARD.encode(serde_json::to_vec(&config)?);
    Ok(json!({
        "apiVersion": "v1", "kind": "Secret", "type": "kubernetes.io/dockerconfigjson",
        "metadata": {"name": REGISTRY_PULL, "namespace": org},
        "data": {".dockerconfigjson": data},
    }))
}

fn registry_token_metadata(inputs: &Value) -> anyhow::Result<bool> {
    Ok(field(inputs, "provider")? != "kind")
}

/// The org chart values from the platform's public org inputs.
pub fn org_values(
    inputs: &Value,
    database: &ConnectionConfig,
    org: &OrgId,
    identity_ca: &str,
    nats_ca: &str,
    postgres_ca: &str,
    api_server: &[Value],
) -> anyhow::Result<Value> {
    let image = field(inputs, "controlImage")?;
    let application_chart = field(inputs, "applicationChart")?;
    let (_, digest) = application_chart
        .strip_prefix("oci://")
        .and_then(|reference| reference.rsplit_once('@'))
        .filter(|(repository, _)| !repository.is_empty())
        .context("pin applicationChart in the platform release to an OCI chart digest")?;
    digest
        .parse::<wamn_digest::Digest>()
        .context("invalid applicationChart digest in the platform release")?;
    let registry = field(inputs, "registry")?;
    let registry_egress: Value = serde_json::from_str(field(inputs, "registryEgress")?)
        .context("decode registryEgress from wamn-org-inputs")?;
    let registry_egress = registry_egress
        .as_array()
        .filter(|destinations| !destinations.is_empty())
        .context("registryEgress in wamn-org-inputs must be a nonempty list")?;
    for (index, destination) in registry_egress.iter().enumerate() {
        let valid_shape = destination
            .as_object()
            .is_some_and(|fields| fields.len() == 2);
        let cidr = destination["cidr"]
            .as_str()
            .and_then(|cidr| cidr.split_once('/'));
        let valid_cidr = cidr.is_some_and(|(address, prefix)| {
            let (Ok(address), Ok(bits)) = (address.parse::<IpAddr>(), prefix.parse::<u8>()) else {
                return false;
            };
            prefix.bytes().all(|byte| byte.is_ascii_digit())
                && bits <= if address.is_ipv4() { 32 } else { 128 }
        });
        let valid_port = destination["port"]
            .as_u64()
            .is_some_and(|port| (1..=u64::from(u16::MAX)).contains(&port));
        ensure!(
            valid_shape && valid_cidr && valid_port,
            "registryEgress entry {index} in wamn-org-inputs must contain a valid CIDR and TCP port"
        );
    }
    let oauth_encryption: Value = serde_json::from_str(field(inputs, OAUTH_ENCRYPTION_INPUT)?)
        .context("decode the platform OAuth encryption configuration")?;
    let internal_registry = registry
        .strip_prefix("localhost:5001")
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .map_or_else(
            || registry.to_owned(),
            |path| format!("{POD_REGISTRY}{path}"),
        );
    let connection = |host: &str, username: &str, secret: &str| {
        json!({
            "host": host, "port": database.port, "database": database.database,
            "username": username, "credentials": {"namespace": org, "name": secret},
            "server_ca": {"namespace": org, "name": "wamn-pg-ca"},
        })
    };
    let control = crate::provision_org::control_login(org, &database.database);
    let apply = crate::provision_org::apply_login(org, &database.database);
    Ok(json!({
        "org": org, "base": field(inputs, "base")?, "registry": internal_registry, "identityCa": identity_ca, "natsCa": nats_ca, "postgresCa": postgres_ca,
        "postgres": {"image": field(inputs, "postgresImage")?},
        "networkPolicy": {"apiServer": api_server, "registry": registry_egress},
        "control": {
            "image": image,
            "configuration": {
                "org": org,
                "database": connection(field(inputs, "controlDatabaseHost")?, control.as_str(), CONTROL_SECRET),
                "apply_database": connection(field(inputs, "applyDatabaseHost")?, apply.as_str(), APPLY_SECRET),
                "identity_issuer": field(inputs, "identityIssuer")?,
                "nats_url": field(inputs, "natsUrl")?,
                "registry": internal_registry,
                "worker_image": image,
                "chart": application_chart,
                "registry_token_metadata": registry_token_metadata(inputs)?,
                "events": {"stream_replicas": 1, "dup_window_secs": 120},
                "oauth_encryption": oauth_encryption,
            },
        },
    }))
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

fn field<'a>(object: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    object[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("platform configuration has no {key}"))
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

async fn run_job(target: &Target, manifest: &Value) -> anyhow::Result<()> {
    let jobs = Api::<Job>::namespaced(target.client.client(), &target.namespace);
    let job = Job::deserialize(manifest).context("decode invitation Job")?;
    let created = jobs
        .create(&PostParams::default(), &job)
        .await
        .with_context(|| format!("start invitation Job in {}", target.namespace))?;
    let name = created
        .metadata
        .name
        .context("Kubernetes did not return a Job name")?;
    job::wait(&jobs, &name, DEADLINE + Duration::from_secs(30)).await
}
