//! Ownership checks and deletion of project resources.

use anyhow::Context as _;
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Secret};
use kube::{Api, api::ListParams};
use serde_json::Value;
use wamn_kubernetes::{
    client::{delete_named, resource},
    helm,
};
use wamn_registry::{Triple, name};

use super::super::Platform;

/// Refuse foreign resources before the first destructive operation.
pub(super) async fn require_ownership(
    platform: &Platform,
    triple: &Triple,
    database: &str,
) -> anyhow::Result<()> {
    let target = &platform.target;
    let reader = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
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

pub(super) async fn delete_reader(
    platform: &Platform,
    triple: &Triple,
    release: Option<&str>,
) -> anyhow::Result<()> {
    let cdc = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let target = &platform.target;
    let deployments = Api::<Deployment>::namespaced(target.client.client(), &target.namespace);
    if let Some(release) = release
        && deployments.get_opt(&cdc).await?.is_some()
    {
        wamn_kubernetes::platform_environment::update_project(
            &platform.target, &platform.chart, release,
            serde_json::json!({"cdcReader":{"pause":true},"publication":{"reclaimPolicy":"delete"}}),
        ).await?;
    }
    crate::environment::cdc_reader::wait_reader_stopped(&platform.target, triple).await
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
    let release = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    wamn_kubernetes::platform_environment::remove_database(
        &platform.target,
        &platform.chart,
        &release,
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
    let target = &platform.target;
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

pub(super) async fn delete_database_certificate_secrets(
    platform: &Platform,
    database: &str,
) -> anyhow::Result<()> {
    // Also remove older Secrets that lack cert-manager's Certificate owner reference.
    let cluster = name::project_cluster(database);
    let root = format!("{cluster}-login-ca");
    let target = &platform.target;
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
    let secrets: Vec<_> = wamn_provision::WorkloadRoleFamily::ALL
        .into_iter()
        .map(wamn_provision::WorkloadRoleFamily::login_name)
        .chain([wamn_role::CDC_LOGIN])
        .map(|login| name::project_login_certificate(database, login))
        .chain([root, format!("{cluster}-client-repl")])
        .collect();
    let api = Api::<Secret>::namespaced(target.client.client(), &target.namespace);
    for name in secrets {
        delete_named(&api, &name, true).await?;
    }
    Ok(())
}
