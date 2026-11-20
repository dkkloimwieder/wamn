//! Ownership checks and deletion of project resources.

use anyhow::Context as _;
use serde_json::Value;
use wamn_kubernetes::helm;
use wamn_registry::{Triple, name};

use super::super::Platform;

async fn get(
    platform: &Platform,
    namespace: &str,
    type_: &str,
    name: &str,
) -> anyhow::Result<Option<Value>> {
    wamn_kubernetes::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        type_,
        name,
    )
    .await
}

/// Refuse foreign resources before the first destructive operation.
pub(super) async fn require_ownership(
    platform: &Platform,
    triple: &Triple,
    database: &str,
) -> anyhow::Result<()> {
    let namespace = &platform.target.namespace;
    let reader = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    if let Some(object) = get(platform, namespace, "deployment", &reader).await? {
        anyhow::ensure!(
            object["metadata"]["annotations"]["wamn.environment"].as_str()
                == Some(triple.to_string().as_str()),
            "refused: Deployment {reader} does not belong to {triple}"
        );
    }
    if let Some(object) = get(
        platform,
        &platform.target.namespace,
        "publication",
        database,
    )
    .await?
    {
        let labels = &object["metadata"]["labels"];
        anyhow::ensure!(
            labels["wamn.org"] == triple.org.as_str()
                && labels["wamn.project"] == triple.project.as_str()
                && labels["wamn.env"] == triple.env.as_str()
                && object["spec"]["dbname"] == database,
            "refused: Publication {database} does not belong to {triple}"
        );
    }
    Ok(())
}

pub(super) async fn delete_reader(
    platform: &Platform,
    triple: &Triple,
    release: Option<&str>,
) -> anyhow::Result<()> {
    let cdc = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    if let Some(release) = release
        && get(platform, &platform.target.namespace, "deployment", &cdc)
            .await?
            .is_some()
    {
        wamn_platform_install::environment::update_project(
            &platform.target, &platform.chart, release,
            serde_json::json!({"cdcReader":{"pause":true},"publication":{"reclaimPolicy":"delete"}}),
        ).await?;
    }
    crate::environment::cdc_reader::wait_reader_stopped(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        triple,
    )
    .await
}

pub(super) async fn project_release_exists(
    platform: &Platform,
    release: &str,
) -> anyhow::Result<bool> {
    let target = &platform.target;
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

pub(super) async fn uninstall_project(
    platform: &Platform,
    triple: &Triple,
    data: bool,
    lines: &mut Vec<String>,
) -> anyhow::Result<()> {
    wamn_platform_install::environment::remove_database(
        &platform.target,
        &platform.chart,
        triple,
        data,
    )
    .await?;
    lines.push("uninstalled the project release".into());
    Ok(())
}

/// Remove chart resources left by uninstalled project generations.
pub(super) async fn delete_kept_project_resources(
    platform: &Platform,
    triple: &Triple,
    release: &str,
) -> anyhow::Result<Vec<String>> {
    const CLUSTER: &str = "cluster.postgresql.cnpg.io";
    const STORE: &str = "objectstore.barmancloud.cnpg.io";
    let namespace = &platform.target.namespace;
    let coordinate = name::project_env_key(&triple.org, &triple.project, &triple.env);
    let mut lines = Vec::new();
    for kind in [
        CLUSTER,
        "certificate.cert-manager.io",
        "issuer.cert-manager.io",
    ] {
        let output = super::kubectl(platform, namespace, &["get", kind, "-o", "json"], None)
            .await
            .with_context(|| format!("list {kind} in {namespace}"))?;
        let list: Value =
            serde_json::from_str(&output).with_context(|| format!("decode {kind} list"))?;
        let items = list["items"]
            .as_array()
            .with_context(|| format!("read items in {kind} list"))?;
        for object in items
            .iter()
            .filter(|object| kept_by_release(object, release))
        {
            let name = object["metadata"]["name"]
                .as_str()
                .with_context(|| format!("kept {kind} has no name"))?;
            if kind == CLUSTER {
                let project = object["spec"]["bootstrap"]["initdb"]["database"]
                    .as_str()
                    .and_then(name::project_database_key);
                if project.as_ref() != Some(&coordinate) {
                    continue;
                }
                // Remove the store first so a retry can still find it by the Cluster name.
                let store = format!("{name}-store");
                if let Some(object) = get(platform, namespace, STORE, &store).await?
                    && kept_by_release(&object, release)
                {
                    // Waiting in kubectl would require list and watch on ObjectStores.
                    delete_kept_object(platform, STORE, &store, "--wait=false", &mut lines).await?;
                }
            }
            delete_kept_object(platform, kind, name, "--wait=true", &mut lines).await?;
        }
    }
    if lines.is_empty() {
        lines.push(format!("no kept project resources found for {triple}"));
    }
    Ok(lines)
}

fn kept_by_release(object: &Value, release: &str) -> bool {
    let annotations = &object["metadata"]["annotations"];
    annotations["meta.helm.sh/release-name"] == release
        && annotations["helm.sh/resource-policy"] == "keep"
}

async fn delete_kept_object(
    platform: &Platform,
    kind: &str,
    name: &str,
    wait: &str,
    lines: &mut Vec<String>,
) -> anyhow::Result<()> {
    let removed = super::kubectl(
        platform,
        &platform.target.namespace,
        &[
            "delete",
            kind,
            name,
            "--ignore-not-found",
            wait,
            "-o",
            "name",
        ],
        None,
    )
    .await
    .with_context(|| format!("delete kept {kind}/{name}"))?;
    if !removed.is_empty() {
        lines.push(format!("deleted {removed}"));
    }
    Ok(())
}

pub(super) async fn delete_database_certificate_secrets(
    platform: &Platform,
    database: &str,
) -> anyhow::Result<()> {
    // Also remove older Secrets that lack cert-manager's Certificate owner reference.
    let cluster = name::project_cluster(database);
    let root = format!("{cluster}-login-ca");
    let namespace = &platform.target.namespace;
    anyhow::ensure!(
        get(platform, namespace, "certificate.cert-manager.io", &root)
            .await?
            .is_none(),
        "cannot remove database certificate Secrets while Certificate {root} exists"
    );
    let secrets: Vec<_> = wamn_provision::WorkloadRoleFamily::ALL
        .into_iter()
        .map(wamn_provision::WorkloadRoleFamily::login_name)
        .chain([wamn_role::CDC_LOGIN])
        .map(|login| name::project_login_certificate(database, login))
        .chain([root, format!("{cluster}-client-repl")])
        .collect();
    let mut arguments = vec!["delete", "secret", "--ignore-not-found"];
    arguments.extend(secrets.iter().map(String::as_str));
    super::kubectl(platform, namespace, &arguments, None)
        .await
        .map(drop)
}
