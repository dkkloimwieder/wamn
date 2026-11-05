//! Registry inputs and credentials for artifact consumption.

use std::path::PathBuf;

use anyhow::Context as _;
use wamn_package_artifact::PackageRegistry;

use crate::environment::document::EnvironmentDocument;
use crate::kubectl;
use wamn_release_control::release_chart::Target;

use super::document::ClusterDocument;
use super::install::{SecretData, administrator, secret_text};
use super::job::{JobRequest, JobSecret};

pub(super) fn registry(document: &ClusterDocument) -> anyhow::Result<PackageRegistry> {
    Ok(PackageRegistry {
        artifact_base: format!("{}/packages", document.registry),
        registry_auth_file: std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
            .map(PathBuf::from)
            .context("set WAMN_REGISTRY_AUTH_FILE for published package and release access")?,
        insecure_registry: false,
        oci_ca_paths: std::env::var_os("WAMN_OCI_CA_PATHS")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default(),
    })
}

pub(super) fn artifact_base(
    registry: &PackageRegistry,
    repository: &str,
) -> anyhow::Result<String> {
    let base = registry
        .artifact_base
        .strip_suffix("/packages")
        .context("platform package base must end with /packages")?;
    Ok(format!("{base}/{repository}"))
}

pub(super) fn registry_inputs(
    request: &mut JobRequest,
    registry: &PackageRegistry,
) -> anyhow::Result<()> {
    request.environment.insert(
        "WAMN_REGISTRY_AUTH_FILE".into(),
        "/inputs/registry.json".into(),
    );
    request.files.insert(
        "registry.json".into(),
        std::fs::read_to_string(&registry.registry_auth_file)
            .context("read registry credentials")?,
    );
    let mut paths = Vec::new();
    for (index, path) in registry.oci_ca_paths.iter().enumerate() {
        let name = format!("registry-ca-{index}.crt");
        paths.push(format!("/inputs/{name}"));
        request.files.insert(
            name,
            std::fs::read_to_string(path).context("read registry CA")?,
        );
    }
    // An empty CA list sets no variable, because an empty value names one empty path.
    if !paths.is_empty() {
        let paths = paths.join(":");
        request
            .environment
            .insert("WAMN_OCI_CA_PATHS".into(), paths.clone());
        request
            .environment
            .insert("WASH_OCI_CA_PATHS".into(), paths.replace(':', ","));
    }
    Ok(())
}

pub(super) async fn credentials(
    target: &Target,
    environment: &EnvironmentDocument,
) -> anyhow::Result<[JobSecret; 2]> {
    let secret: SecretData = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "platform",
        "secret",
        "wamn-pg-superuser",
    )
    .await?
    .context("missing Secret platform/wamn-pg-superuser")?;
    let admin = administrator(&secret)?;
    let name = wamn_control_provision::workload_secret_name(
        wamn_control_provision::WorkloadRoleFamily::App,
        &environment.org,
        &environment.project,
        &environment.env,
    );
    let secret = kubectl::get(
        &target.kubeconfig,
        &target.context,
        "hosts",
        "secret",
        &name,
    )
    .await?
    .context("missing environment application Secret")?;
    credentials_from(&admin, &secret_text(&secret, "url")?)
}

pub(super) fn credentials_from(
    admin: &JobSecret,
    application_url: &str,
) -> anyhow::Result<[JobSecret; 2]> {
    let application = url::Url::parse(application_url)
        .map_err(|_| anyhow::anyhow!("parse application database URL"))?;
    let mut database = url::Url::parse(&admin.value)
        .map_err(|_| anyhow::anyhow!("parse administrator database URL"))?;
    database.set_path(application.path());
    Ok([
        admin.clone(),
        JobSecret {
            name: "platform-package-admin".into(),
            key: "url".into(),
            value: database.into(),
            environment: "WAMN_PG_ADMIN_URL".into(),
        },
    ])
}
