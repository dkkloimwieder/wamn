//! Admin-side installation of a single org.

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{net::IpAddr, path::PathBuf, time::Duration};
use wamn_kubernetes::{Target, helm, job, kubectl};
use wamn_postgres::ConnectionConfig;
use wamn_registry::OrgId;

use crate::provision_org::{
    APPLY_SECRET, CONTROL_AUDIENCE, CONTROL_SECRET, INVITATION_ACCOUNT, SQL_ACCOUNT,
};
use crate::release_chart::bundle::POD_REGISTRY;

const PLATFORM: &str = "platform";
const SYSTEM_SQL_CONFIG: &str = "wamn-system-sql";
const PROVISION_JOB: &str = "wamn-provision-org";
const SQL_CONFIG_PATH: &str = "/etc/wamn/system-sql/database.json";
const DEADLINE: Duration = Duration::from_secs(300);

/// Install the org release, provision its logins, then invite its owner.
pub async fn create(org: &OrgId, owner: &str) -> anyhow::Result<()> {
    let chart = org_chart()?;
    // The empty path lets kubectl and Helm use their normal KUBECONFIG loading rules.
    let kubeconfig = PathBuf::new();
    let context = kubectl::current_context(&kubeconfig)?;
    let mut target = Target {
        kubeconfig,
        context,
        namespace: PLATFORM.into(),
    };
    let (inputs, database_config, identity_ca, nats_ca, api_server) = tokio::try_join!(
        config_map(&target, PLATFORM, "wamn-org-inputs"),
        config_map(&target, PLATFORM, SYSTEM_SQL_CONFIG),
        config_map(&target, "hosts", "identity-ca"),
        kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            PLATFORM,
            &[
                "get",
                "secret",
                "wasmcloud-ca",
                "-o",
                r"jsonpath={.data.tls\.crt}"
            ],
            None,
        ),
        api_destinations(&target),
    )?;
    let database: ConnectionConfig =
        serde_json::from_str(field(&database_config["data"], "database.json")?)
            .context("decode the platform SQL Job connection")?;
    let postgres_ca: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        PLATFORM,
        "secret",
        &database.server_ca.name,
    )
    .await?
    .context("public system database CA Secret is absent")?;
    let postgres_ca = STANDARD
        .decode(field(&postgres_ca["data"], "ca.crt")?)
        .context("decode the public system database CA")?;
    ensure!(!nats_ca.is_empty(), "public NATS CA is absent");
    let nats_ca = STANDARD
        .decode(nats_ca)
        .context("decode the public NATS CA")?;
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
    let values_file =
        tempfile::NamedTempFile::new().context("create a file for public org values")?;
    serde_json::to_writer(values_file.as_file(), &values).context("write public org values")?;

    let storage = helm::org_release_namespace(org.as_str());
    kubectl::apply(
        &target.kubeconfig,
        &target.context,
        PLATFORM,
        &json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": storage, "labels": {"app.kubernetes.io/part-of": "wamn"}},
        }),
    )
    .await
    .context("create the org Helm storage namespace")?;
    helm::recover_pending(&target, &storage, "wamn-org", helm::Wait::None).await?;
    helm::upgrade(
        &target,
        &storage,
        &chart,
        "wamn-org",
        values_file.path(),
        helm::Wait::None,
        helm::Install::ExistingNamespace,
    )
    .await?;

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
        {"name": "database", "mountPath": wamn_postgres::DATABASE_DIR, "readOnly": true},
    ]);
    let sql_job_uid = job::run_exclusive(&target, &sql, DEADLINE + Duration::from_secs(30)).await?;

    target.namespace = org.to_string();
    // The control pool reads its mounted password at startup.
    let control = wamn_registry::name::control_resource(org);
    let restart = json!({
        "spec": {"template": {"metadata": {"annotations": {
            "wamn.dev/control-credential-job": sql_job_uid,
        }}}},
    });
    kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &[
            "patch",
            "deployment",
            &control,
            "--type=merge",
            "--patch",
            &restart.to_string(),
        ],
        None,
    )
    .await
    .context("reload the org database logins")?;

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

fn org_values(
    inputs: &Value,
    database: &ConnectionConfig,
    org: &OrgId,
    identity_ca: &str,
    nats_ca: &str,
    postgres_ca: &str,
    api_server: &[Value],
) -> anyhow::Result<Value> {
    let image = field(inputs, "controlImage")?;
    let registry = field(inputs, "registry")?;
    let kind = field(inputs, "provider")? == "kind";
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
        "networkPolicy": {"apiServer": api_server},
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
                "chart": "/usr/local/share/wamn/deploy/charts/application",
                "registry_token_metadata": !kind,
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
    kubectl::get(
        &target.kubeconfig,
        &target.context,
        namespace,
        "configmap",
        name,
    )
    .await?
    .with_context(|| format!("platform configuration {namespace}/{name} is absent"))
}

fn field<'a>(object: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    object[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("platform configuration has no {key}"))
}

async fn api_destinations(target: &Target) -> anyhow::Result<Vec<Value>> {
    let service: Value = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "default",
        "service",
        "kubernetes",
    )
    .await?
    .context("Kubernetes API Service is absent")?;
    let slices = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        "default",
        &[
            "get",
            "endpointslices",
            "-l",
            "kubernetes.io/service-name=kubernetes",
            "-o",
            "json",
        ],
        None,
    )
    .await?;
    let slices: Value = serde_json::from_str(&slices)?;
    let mut destinations = std::collections::BTreeSet::new();
    let mut add = |address: &Value, port: &Value| {
        if let (Some(address), Some(port)) = (address.as_str(), port.as_u64())
            && let Ok(ip) = address.parse::<IpAddr>()
        {
            destinations.insert((ip, port));
        }
    };
    add(
        &service["spec"]["clusterIP"],
        &service["spec"]["ports"][0]["port"],
    );
    for slice in slices["items"].as_array().into_iter().flatten() {
        for port in slice["ports"].as_array().into_iter().flatten() {
            for endpoint in slice["endpoints"].as_array().into_iter().flatten() {
                for address in endpoint["addresses"].as_array().into_iter().flatten() {
                    add(address, &port["port"]);
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
    let input = serde_json::to_vec(manifest)?;
    let created = kubectl::kubectl(
        &target.kubeconfig,
        &target.context,
        &target.namespace,
        &["create", "-f", "-", "-o", "name"],
        Some(&input),
    )
    .await
    .with_context(|| {
        format!(
            "start {} Job in {}",
            manifest["metadata"]["generateName"], target.namespace
        )
    })?;
    let name = created
        .trim()
        .strip_prefix("job.batch/")
        .context("kubectl did not return a Job name")?;
    job::wait(target, name, DEADLINE + Duration::from_secs(30)).await
}
